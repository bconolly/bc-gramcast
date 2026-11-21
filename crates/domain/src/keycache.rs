//! Policy for caching the Access public keys per isolate. Pure: the adapter
//! owns the fetching and the key objects, this decides when to fetch.

use crate::app::{PortError, VerifyError};

/// How long fetched keys are used without a refresh.
pub const KEY_TTL_SECS: i64 = 3600;
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

    /// The fetch that `lookup` asked for failed. Keeps the old keys and the
    /// one-a-minute gap, and ends the wait for other requests.
    pub fn fetch_failed(&mut self, now: i64) {
        self.fetching_since = None;
        self.last_fetch = Some(now);
    }

    /// Replaces the cached keys after a successful fetch.
    pub fn store(&mut self, keys: Vec<(String, K)>, now: i64) {
        self.fetching_since = None;
        self.keys = keys;
        self.loaded_at = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(now: i64) -> KeyCache<u32> {
        let mut c = KeyCache::new();
        assert_eq!(c.lookup("a", now), KeyLookup::Fetch);
        c.store(vec![("a".into(), 1), ("b".into(), 2)], now);
        c
    }

    #[test]
    fn an_empty_cache_asks_for_a_fetch_once_then_is_unavailable_for_a_minute() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        c.fetch_failed(1000);
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
        c.store(vec![("a".into(), 1)], 1002);
        assert_eq!(c.lookup("a", 1003), KeyLookup::Use(1));
    }

    #[test]
    fn a_failed_fetch_ends_the_wait() {
        let mut c: KeyCache<u32> = KeyCache::new();
        assert_eq!(c.lookup("a", 1000), KeyLookup::Fetch);
        c.fetch_failed(1001);
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
        c.fetch_failed(1060);
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
        c.store(vec![("c".into(), 3)], t);
        // Rotation: the old ids are gone, the new one is used.
        assert_eq!(c.lookup("c", t + 1), KeyLookup::Use(3));
        assert_eq!(c.lookup("a", t + 1), KeyLookup::Throttled);
    }

    #[test]
    fn stale_keys_still_serve_while_a_refresh_is_throttled() {
        let mut c = loaded(1000);
        let stale = 1000 + KEY_TTL_SECS + 5;
        assert_eq!(c.lookup("a", stale), KeyLookup::Fetch);
        c.fetch_failed(stale);
        assert_eq!(c.lookup("a", stale + 1), KeyLookup::Use(1));
        assert_eq!(c.lookup("a", stale + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn stale_keys_stop_serving_after_a_day() {
        let mut c = loaded(1000);
        let last_ok = 1000 + MAX_STALE_SECS - 1;
        assert_eq!(c.lookup("a", last_ok), KeyLookup::Fetch);
        c.fetch_failed(last_ok);
        assert_eq!(c.lookup("a", last_ok + 1), KeyLookup::Unavailable);
        // One second earlier the key would still have served.
        let mut c = loaded(1000);
        let t = 1000 + MAX_STALE_SECS - 2;
        assert_eq!(c.lookup("a", t), KeyLookup::Fetch);
        c.fetch_failed(t);
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
}
