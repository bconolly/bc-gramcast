//! Policy for caching the Access public keys per isolate. Pure: the adapter
//! owns the fetching and the key objects, this decides when to fetch.

use crate::app::{PortError, VerifyError};

/// How long fetched keys are used without a refresh.
pub const KEY_TTL_SECS: i64 = 3600;
/// How many times a request waits for another request's fetch, and for how long each time.
pub const WAIT_POLLS: u32 = 8;
pub const WAIT_STEP_MS: u64 = 250;
/// Past this age, cached keys are no longer used even when a refresh keeps failing.
pub const MAX_STALE_SECS: i64 = 24 * 3600;
/// A fetch with no answer after this long is treated as lost.
pub const FETCH_TIMEOUT_SECS: i64 = 30;
/// Fetches are at least this far apart, so a flood of bad key ids cannot
/// become a flood of requests to the key endpoint.
pub const MIN_FETCH_GAP_SECS: i64 = 60;

#[derive(Debug, PartialEq, Eq)]
pub enum KeyLookup<K> {
    /// Verify with this key.
    Use(K),
    /// Fetch the keys now (the attempt is already recorded), then look again.
    Fetch,
    /// Another request is fetching the keys: wait a moment and look again.
    Wait,
    /// No usable key has this id, and a fetch happened too recently to try
    /// again. Maps to "sign-in unavailable", since a retry may well succeed.
    Throttled,
    /// No usable keys (never loaded, or too old) and a fetch is not allowed yet.
    Unavailable,
}

/// The sign-in error for a lookup that gives no key. `None` for `Use` and for
/// `Fetch` and `Wait`, which are not failures yet.
pub fn lookup_failure<K>(lookup: &KeyLookup<K>) -> Option<VerifyError> {
    match lookup {
        KeyLookup::Use(_) | KeyLookup::Fetch | KeyLookup::Wait => None,
        KeyLookup::Throttled => Some(VerifyError::Unavailable(PortError(
            "key not found, refresh throttled".into(),
        ))),
        KeyLookup::Unavailable => {
            Some(VerifyError::Unavailable(PortError("no usable keys".into())))
        }
    }
}

#[derive(Debug)]
pub struct KeyCache<K> {
    keys: Vec<(String, K)>,
    loaded_at: Option<i64>,
    last_fetch: Option<i64>,
    fetching_since: Option<i64>,
}

impl<K> KeyCache<K> {
    /// A `const` constructor, for a `thread_local!` initialiser.
    pub const fn new_const() -> Self {
        Self {
            keys: Vec::new(),
            loaded_at: None,
            last_fetch: None,
            fetching_since: None,
        }
    }
}

impl<K: Clone> Default for KeyCache<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Clone> KeyCache<K> {
    pub fn new() -> Self {
        Self::new_const()
    }

    /// `now` is unix seconds.
    pub fn lookup(&mut self, kid: &str, now: i64) -> KeyLookup<K> {
        let age = self.loaded_at.map(|t| now.saturating_sub(t));
        let fresh = age.is_some_and(|a| a < KEY_TTL_SECS);
        let stale_ok = age.is_some_and(|a| a < MAX_STALE_SECS);
        let found = self.keys.iter().find(|(k, _)| k == kid).map(|(_, v)| v);
        if let (true, Some(key)) = (fresh, found) {
            return KeyLookup::Use(key.clone());
        }
        let fetching = self
            .fetching_since
            .is_some_and(|t| now.saturating_sub(t) < FETCH_TIMEOUT_SECS);
        if fetching {
            return match (found, stale_ok) {
                (Some(key), true) => KeyLookup::Use(key.clone()),
                _ => KeyLookup::Wait,
            };
        }
        let may_fetch = self
            .last_fetch
            .is_none_or(|t| now.saturating_sub(t) >= MIN_FETCH_GAP_SECS);
        if may_fetch {
            self.last_fetch = Some(now);
            self.fetching_since = Some(now);
            return KeyLookup::Fetch;
        }
        // Throttled: a stale key (under a day old) beats no key.
        match (found, stale_ok) {
            (Some(key), true) => KeyLookup::Use(key.clone()),
            (_, false) => KeyLookup::Unavailable,
            (None, true) => KeyLookup::Throttled,
        }
    }

    /// A cached key that is not too old to use, without touching any state.
    fn cached(&self, kid: &str, now: i64) -> Option<K> {
        let age = self.loaded_at.map(|t| now.saturating_sub(t))?;
        if age >= MAX_STALE_SECS {
            return None;
        }
        self.keys
            .iter()
            .find(|(k, _)| k == kid)
            .map(|(_, v)| v.clone())
    }

    /// The fetch that `lookup` asked for (it returned `Fetch` at `started`)
    /// failed. Keeps the old keys and the one-a-minute gap, and ends the wait
    /// for other requests. Ignored if a newer fetch has since begun.
    pub fn fetch_failed(&mut self, started: i64, now: i64) -> bool {
        if self.fetching_since != Some(started) {
            return false;
        }
        self.fetching_since = None;
        self.last_fetch = Some(now);
        true
    }

    /// Replaces the cached keys after a successful fetch begun at `started`.
    /// Returns false, changing nothing, if that fetch was superseded by a newer one.
    pub fn store(&mut self, keys: Vec<(String, K)>, started: i64, now: i64) -> bool {
        if self.fetching_since != Some(started) {
            return false;
        }
        self.fetching_since = None;
        // Counts as a fetch attempt at `now`, so a slow fetch leaves no gap in
        // which the next lookup starts another one at once.
        self.last_fetch = Some(now);
        self.keys = keys;
        self.loaded_at = Some(now);
        true
    }

    /// Records the outcome of the fetch begun at `started` (`None` if it
    /// failed) and answers the request that did the fetch. After a good fetch
    /// a missing key id is definitely unknown (401). After a failed one a
    /// stale key still serves, and otherwise the lookup's failure applies (503).
    /// A fetch that was superseded by a newer one changes nothing: it answers
    /// with the cached key if there is a usable one, else 503.
    pub fn fetched(
        &mut self,
        kid: &str,
        started: i64,
        keys: Option<Vec<(String, K)>>,
        now: i64,
    ) -> Result<K, VerifyError> {
        let good = keys.is_some();
        let applied = match keys {
            Some(keys) => self.store(keys, started, now),
            None => self.fetch_failed(started, now),
        };
        if !applied {
            // A newer fetch owns the marker, so only read the cache (a lookup
            // could start yet another fetch).
            return self
                .cached(kid, now)
                .ok_or_else(|| unavailable("fetch superseded"));
        }
        match self.lookup(kid, now) {
            KeyLookup::Use(key) => Ok(key),
            _ if good => Err(VerifyError::UnknownKey),
            other => Err(lookup_failure(&other).unwrap_or_else(|| unavailable("keys unavailable"))),
        }
    }
}

fn unavailable(text: &str) -> VerifyError {
    VerifyError::Unavailable(PortError(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(now: i64) -> KeyCache<u32> {
        let mut c = KeyCache::new();
        assert_eq!(c.lookup("a", now), KeyLookup::Fetch);
        c.store(vec![("a".into(), 1), ("b".into(), 2)], now, now);
        c
    }

    #[test]
    fn an_empty_cache_asks_for_a_fetch_once_then_is_unavailable_for_a_minute() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        c.fetch_failed(1000, 1000);
        assert_eq!(
            c.lookup("a", 1000 + MIN_FETCH_GAP_SECS - 1),
            KeyLookup::Unavailable
        );
        assert_eq!(c.lookup("a", 1000 + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn requests_during_a_cold_fetch_wait_instead_of_failing() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        assert_eq!(c.lookup("a", 1001), KeyLookup::Wait);
        assert_eq!(c.lookup("zzz", 1002), KeyLookup::Wait);
        c.store(vec![("a".into(), 1)], 1000, 1002);
        assert_eq!(c.lookup("a", 1003), KeyLookup::Use(1));
    }

    #[test]
    fn a_failed_fetch_ends_the_wait() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        c.fetch_failed(1000, 1001);
        assert_eq!(c.lookup("a", 1002), KeyLookup::Unavailable);
    }

    #[test]
    fn a_lost_fetch_stops_blocking_after_the_timeout() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        assert_eq!(
            c.lookup("a", 1000 + FETCH_TIMEOUT_SECS - 1),
            KeyLookup::Wait
        );
        // No longer waiting, but the one-a-minute gap still holds.
        assert_eq!(
            c.lookup("a", 1000 + FETCH_TIMEOUT_SECS),
            KeyLookup::Unavailable
        );
        assert_eq!(c.lookup("a", 1000 + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn during_a_refresh_known_keys_still_serve_and_unknown_ones_wait() {
        let mut c = loaded(1000);
        let t = 1000 + KEY_TTL_SECS;
        assert_eq!(c.lookup("a", t), KeyLookup::Fetch);
        assert_eq!(c.lookup("a", t + 1), KeyLookup::Use(1));
        assert_eq!(c.lookup("zzz", t + 1), KeyLookup::Wait);
    }

    #[test]
    fn fresh_keys_are_used_without_fetching() {
        let mut c = loaded(1000);
        assert_eq!(c.lookup("a", 1010), KeyLookup::Use(1));
        assert_eq!(c.lookup("b", 1000 + KEY_TTL_SECS - 1), KeyLookup::Use(2));
    }

    #[test]
    fn an_unknown_kid_fetches_at_most_once_a_minute_and_is_throttled_between() {
        let mut c = loaded(1000);
        assert_eq!(c.lookup("zzz", 1030), KeyLookup::Throttled);
        assert_eq!(c.lookup("zzz", 1000 + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
        c.fetch_failed(1060, 1060);
        for t in 0..MIN_FETCH_GAP_SECS {
            assert_eq!(c.lookup("zzz", 1060 + t), KeyLookup::Throttled, "t={t}");
        }
        assert_eq!(c.lookup("zzz", 1060 + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn keys_older_than_an_hour_are_refreshed() {
        let mut c = loaded(1000);
        let t = 1000 + KEY_TTL_SECS;
        assert_eq!(c.lookup("a", t), KeyLookup::Fetch);
        c.store(vec![("c".into(), 3)], t, t);
        // Rotation: the old ids are gone, the new one is used.
        assert_eq!(c.lookup("c", t + 1), KeyLookup::Use(3));
        assert_eq!(c.lookup("a", t + 1), KeyLookup::Throttled);
    }

    #[test]
    fn stale_keys_still_serve_while_a_refresh_is_throttled() {
        let mut c = loaded(1000);
        let stale = 1000 + KEY_TTL_SECS + 5;
        assert_eq!(c.lookup("a", stale), KeyLookup::Fetch);
        c.fetch_failed(stale, stale);
        assert_eq!(c.lookup("a", stale + 1), KeyLookup::Use(1));
        assert_eq!(c.lookup("a", stale + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn stale_keys_stop_serving_after_a_day() {
        let mut c = loaded(1000);
        let last_ok = 1000 + MAX_STALE_SECS - 1;
        assert_eq!(c.lookup("a", last_ok), KeyLookup::Fetch);
        c.fetch_failed(last_ok, last_ok);
        assert_eq!(c.lookup("a", last_ok + 1), KeyLookup::Unavailable);
        // One second earlier the key would still have served.
        let mut c = loaded(1000);
        let t = 1000 + MAX_STALE_SECS - 2;
        assert_eq!(c.lookup("a", t), KeyLookup::Fetch);
        c.fetch_failed(t, t);
        assert_eq!(c.lookup("a", t + 1), KeyLookup::Use(1));
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_panic_or_loop_fetches() {
        let mut c = loaded(1000);
        assert_eq!(c.lookup("a", 900), KeyLookup::Use(1));
        assert_eq!(c.lookup("zzz", i64::MIN), KeyLookup::Throttled);
        assert_eq!(c.lookup("zzz", i64::MAX), KeyLookup::Fetch);
    }

    #[test]
    fn lookup_failures_map_to_sign_in_errors() {
        assert_eq!(lookup_failure(&KeyLookup::<u32>::Use(1)), None);
        assert_eq!(lookup_failure(&KeyLookup::<u32>::Fetch), None);
        assert_eq!(lookup_failure(&KeyLookup::<u32>::Wait), None);
        // A throttled unknown kid is "unavailable" (503), not "unknown key" (401).
        for l in [KeyLookup::<u32>::Throttled, KeyLookup::Unavailable] {
            assert!(
                matches!(lookup_failure(&l), Some(VerifyError::Unavailable(_))),
                "{l:?}"
            );
        }
    }

    #[test]
    fn a_cached_kid_older_than_a_day_waits_during_a_refresh() {
        let mut c = loaded(1000);
        let t = 1000 + MAX_STALE_SECS + 10;
        assert_eq!(c.lookup("a", t), KeyLookup::Fetch);
        assert_eq!(c.lookup("a", t + 1), KeyLookup::Wait);
    }

    #[test]
    fn a_slow_old_fetch_cannot_disturb_a_newer_one() {
        let mut c: KeyCache<u32> = KeyCache::new();
        // Fetch A starts, then hangs.
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        // Fetch B starts.
        assert_eq!(c.lookup("a", 1060), KeyLookup::Fetch);
        // A fails late: B's marker stays, so requests still wait for B.
        c.fetch_failed(1000, 1070);
        assert_eq!(c.lookup("a", 1071), KeyLookup::Wait);
        // B lands, then A's late result must not overwrite B's keys.
        assert!(c.store(vec![("new".into(), 2)], 1060, 1075));
        assert!(!c.store(vec![("old".into(), 1)], 1000, 1080));
        assert_eq!(c.lookup("new", 1081), KeyLookup::Use(2));
        assert_eq!(c.lookup("old", 1081), KeyLookup::Throttled);
    }

    #[test]
    fn fetched_with_the_kid_present_returns_the_key() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        let keys = vec![("a".to_string(), 1), ("b".to_string(), 2)];
        assert_eq!(c.fetched("a", 1000, Some(keys), 1001), Ok(1));
    }

    #[test]
    fn fetched_with_the_kid_missing_is_an_unknown_key() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("zzz", 1000), KeyLookup::Fetch);
        let keys = vec![("a".to_string(), 1)];
        assert_eq!(
            c.fetched("zzz", 1000, Some(keys), 1001),
            Err(VerifyError::UnknownKey)
        );
    }

    #[test]
    fn a_failed_fetch_serves_a_stale_key() {
        let mut c = loaded(1000);
        let t = 1000 + KEY_TTL_SECS + 5;
        assert_eq!(c.lookup("a", t), KeyLookup::Fetch);
        assert_eq!(c.fetched("a", t, None, t + 1), Ok(1));
    }

    #[test]
    fn a_failed_fetch_on_a_cold_cache_is_unavailable() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        let r = c.fetched("a", 1000, None, 1001);
        assert!(matches!(r, Err(VerifyError::Unavailable(_))), "{r:?}");
    }

    #[test]
    fn a_failed_fetch_with_an_unknown_kid_is_unavailable_not_unknown() {
        let mut c = loaded(1000);
        let t = 1000 + KEY_TTL_SECS + 5;
        assert_eq!(c.lookup("zzz", t), KeyLookup::Fetch);
        let r = c.fetched("zzz", t, None, t + 1);
        assert!(matches!(r, Err(VerifyError::Unavailable(_))), "{r:?}");
    }

    #[test]
    fn a_slow_good_fetch_leaves_no_orphan_marker() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("zzz", 1000), KeyLookup::Fetch);
        let keys = vec![("a".to_string(), 1)];
        // The fetch took over a minute.
        assert_eq!(
            c.fetched("zzz", 1000, Some(keys), 1061),
            Err(VerifyError::UnknownKey)
        );
        assert_ne!(c.lookup("zzz", 1062), KeyLookup::Wait);
        assert_eq!(c.lookup("zzz", 1062), KeyLookup::Throttled);
    }

    #[test]
    fn a_superseded_fetch_answers_from_the_cache_and_keeps_the_newer_marker() {
        // Key absent: 503.
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch); // A
        assert_eq!(c.lookup("a", 1060), KeyLookup::Fetch); // B
        let r = c.fetched("a", 1000, Some(vec![("a".into(), 9)]), 1070);
        assert!(matches!(r, Err(VerifyError::Unavailable(_))), "{r:?}");
        assert_eq!(c.lookup("zzz", 1071), KeyLookup::Wait, "B's marker stays");

        // Key cached: returned.
        let mut c = loaded(1000);
        let t = 1000 + KEY_TTL_SECS;
        assert_eq!(c.lookup("a", t), KeyLookup::Fetch); // A (then lost)
        assert_eq!(c.lookup("a", t + MIN_FETCH_GAP_SECS), KeyLookup::Fetch); // B
        assert_eq!(c.fetched("a", t, None, t + 70), Ok(1));
        assert_eq!(c.lookup("zzz", t + 71), KeyLookup::Wait, "B's marker stays");
    }
}
