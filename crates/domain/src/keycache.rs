//! Policy for caching the Access public keys per isolate. Pure: the adapter
//! owns the fetching and the key objects, this decides when to fetch.

/// How long fetched keys are used without a refresh.
pub const KEY_TTL_SECS: i64 = 3600;
/// Fetches are at least this far apart, so a flood of bad key ids cannot
/// become a flood of requests to the key endpoint.
pub const MIN_FETCH_GAP_SECS: i64 = 60;

#[derive(Debug, PartialEq, Eq)]
pub enum KeyLookup<K> {
    /// Verify with this key.
    Use(K),
    /// Fetch the keys now (the attempt is already recorded), then look again.
    Fetch,
    /// Keys are loaded but none has this id.
    Unknown,
    /// No keys have ever been loaded, and a fetch is not allowed yet.
    Unavailable,
}

#[derive(Debug)]
pub struct KeyCache<K> {
    keys: Vec<(String, K)>,
    loaded_at: Option<i64>,
    last_fetch: Option<i64>,
}

impl<K> KeyCache<K> {
    /// A `const` constructor, for a `thread_local!` initialiser.
    pub const fn new_const() -> Self {
        Self {
            keys: Vec::new(),
            loaded_at: None,
            last_fetch: None,
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
        let found = self.keys.iter().find(|(k, _)| k == kid).map(|(_, v)| v);
        let fresh = self
            .loaded_at
            .is_some_and(|t| now.saturating_sub(t) < KEY_TTL_SECS);
        if let (true, Some(key)) = (fresh, found) {
            return KeyLookup::Use(key.clone());
        }
        let may_fetch = self
            .last_fetch
            .is_none_or(|t| now.saturating_sub(t) >= MIN_FETCH_GAP_SECS);
        if may_fetch {
            self.last_fetch = Some(now);
            return KeyLookup::Fetch;
        }
        // Throttled: a stale key beats no key.
        match (found, self.loaded_at) {
            (Some(key), _) => KeyLookup::Use(key.clone()),
            (None, Some(_)) => KeyLookup::Unknown,
            (None, None) => KeyLookup::Unavailable,
        }
    }

    /// Replaces the cached keys after a successful fetch.
    pub fn store(&mut self, keys: Vec<(String, K)>, now: i64) {
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
        // The fetch failed (nothing stored): no second fetch inside the gap.
        assert_eq!(
            c.lookup("a", 1000 + MIN_FETCH_GAP_SECS - 1),
            KeyLookup::Unavailable
        );
        assert_eq!(c.lookup("a", 1000 + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn fresh_keys_are_used_without_fetching() {
        let mut c = loaded(1000);
        assert_eq!(c.lookup("a", 1010), KeyLookup::Use(1));
        assert_eq!(c.lookup("b", 1000 + KEY_TTL_SECS - 1), KeyLookup::Use(2));
    }

    #[test]
    fn an_unknown_kid_fetches_at_most_once_a_minute() {
        let mut c = loaded(1000);
        // Within a minute of the load: no fetch, just unknown.
        assert_eq!(c.lookup("zzz", 1030), KeyLookup::Unknown);
        assert_eq!(c.lookup("zzz", 1000 + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
        // That attempt is recorded, so a flood of bad kids does not fetch again.
        for t in 0..MIN_FETCH_GAP_SECS {
            assert_eq!(c.lookup("zzz", 1060 + t), KeyLookup::Unknown, "t={t}");
        }
        assert_eq!(c.lookup("zzz", 1060 + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn keys_older_than_an_hour_are_refreshed() {
        let mut c = loaded(1000);
        assert_eq!(c.lookup("a", 1000 + KEY_TTL_SECS), KeyLookup::Fetch);
        c.store(vec![("c".into(), 3)], 1000 + KEY_TTL_SECS);
        // Rotation: the old ids are gone, the new one is used.
        assert_eq!(c.lookup("c", 1000 + KEY_TTL_SECS + 1), KeyLookup::Use(3));
        assert_eq!(c.lookup("a", 1000 + KEY_TTL_SECS + 1), KeyLookup::Unknown);
    }

    #[test]
    fn stale_keys_still_serve_while_a_refresh_is_throttled() {
        let mut c = loaded(1000);
        let stale = 1000 + KEY_TTL_SECS + 5;
        assert_eq!(c.lookup("a", stale), KeyLookup::Fetch);
        // The refresh failed: keep serving the old key until the next try.
        assert_eq!(c.lookup("a", stale + 1), KeyLookup::Use(1));
        assert_eq!(c.lookup("a", stale + MIN_FETCH_GAP_SECS), KeyLookup::Fetch);
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_panic_or_loop_fetches() {
        let mut c = loaded(1000);
        assert_eq!(c.lookup("a", 900), KeyLookup::Use(1));
        assert_eq!(c.lookup("zzz", i64::MIN), KeyLookup::Unknown);
        assert_eq!(c.lookup("zzz", i64::MAX), KeyLookup::Fetch);
    }
}
