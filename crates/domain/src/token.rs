//! OAuth token model and the ports around it. Tokens are secrets: the Debug
//! output is redacted so a stray `{:?}` or `assert_eq!` failure never leaks one.

use crate::app::PortError;
use std::fmt;

/// Refresh this long before the real expiry, so a token never dies mid-upload.
pub const REFRESH_MARGIN_SECS: i64 = 5 * 60;

// Do NOT derive or implement Serialize (or Display) for this type: it would let the
// tokens reach logs or responses. Debug is hand-written to redact them. The storage
// adapter must write the fields itself, to the secret store only.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    /// Expiry as Unix seconds (UTC).
    pub expires_at: i64,
}

impl StoredToken {
    /// True once `now` is within the safety margin of expiry (or past it).
    pub fn needs_refresh(&self, now: i64) -> bool {
        now.saturating_add(REFRESH_MARGIN_SECS) >= self.expires_at
    }
}

impl fmt::Debug for StoredToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredToken")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Persistence port for the owner's token (one per account).
///
/// One store value serves one run. `load` remembers which stored version it read
/// (an opaque marker, such as the row's nonce); `save` and `mark_needs_reconnect`
/// then apply only if the row is still that version. If the owner reconnected in
/// the meantime the write is skipped and returns `Ok`: a lost race is harmless, and
/// the owner's new token and cleared mark must survive a scheduled run that started
/// before it. Called before any `load`, both return an error. `connect` is
/// unconditional: it is the owner's own act.
#[allow(async_fn_in_trait)]
pub trait TokenStore {
    /// `None` until the owner has connected the account.
    async fn load(&self) -> Result<Option<StoredToken>, PortError>;
    /// Stores a refreshed token, if the row is still the version `load` read. Used by
    /// the scheduled refresh path. Must NOT clear the needs-reconnect mark: only the
    /// owner reconnecting does that (see `connect`).
    async fn save(&self, token: &StoredToken) -> Result<(), PortError>;
    /// Stores a token from a new owner sign-in AND clears the needs-reconnect mark.
    /// Callable only from the owner's OAuth callback, never from the scheduled run.
    async fn connect(&self, token: &StoredToken) -> Result<(), PortError>;
    /// Records that the provider keeps refusing the token `load` read, so runs stop at
    /// once instead of retrying. Skipped if the owner has reconnected since. Cleared
    /// only by `connect`.
    async fn mark_needs_reconnect(&self) -> Result<(), PortError>;
    async fn needs_reconnect(&self) -> Result<bool, PortError>;
    /// What the Connections screen needs, without reading the token: `None` when
    /// there is no stored token, otherwise the needs-reconnect mark. Never decrypts,
    /// so a row that can no longer be read (a lost key) still shows as connected.
    async fn connection_mark(&self) -> Result<Option<bool>, PortError>;
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// In-memory store for tests. `version` changes on every stored write; `load`
    /// remembers it, like the real store.
    #[derive(Default)]
    pub struct FakeTokenStore {
        pub token: RefCell<Option<StoredToken>>,
        pub reconnect: Cell<bool>,
        pub version: Cell<u64>,
        pub loaded: Cell<Option<u64>>,
    }
    impl FakeTokenStore {
        fn still_loaded(&self) -> Result<bool, PortError> {
            let loaded = self
                .loaded
                .get()
                .ok_or_else(|| PortError("token not loaded".into()))?;
            Ok(loaded == self.version.get())
        }
    }
    impl TokenStore for FakeTokenStore {
        async fn load(&self) -> Result<Option<StoredToken>, PortError> {
            self.loaded.set(Some(self.version.get()));
            Ok(self.token.borrow().clone())
        }
        async fn save(&self, token: &StoredToken) -> Result<(), PortError> {
            if self.still_loaded()? {
                *self.token.borrow_mut() = Some(token.clone());
                self.version.set(self.version.get() + 1);
                self.loaded.set(Some(self.version.get()));
            }
            Ok(())
        }
        async fn connect(&self, token: &StoredToken) -> Result<(), PortError> {
            *self.token.borrow_mut() = Some(token.clone());
            self.reconnect.set(false);
            self.version.set(self.version.get() + 1);
            Ok(())
        }
        async fn mark_needs_reconnect(&self) -> Result<(), PortError> {
            if self.still_loaded()? {
                self.reconnect.set(true);
            }
            Ok(())
        }
        async fn needs_reconnect(&self) -> Result<bool, PortError> {
            Ok(self.reconnect.get())
        }
        async fn connection_mark(&self) -> Result<Option<bool>, PortError> {
            let present = self.token.borrow().is_some();
            Ok(present.then(|| self.reconnect.get()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::FakeTokenStore;
    use super::*;
    fn token(expires_at: i64) -> StoredToken {
        StoredToken {
            access_token: "ya29.SECRET-ACCESS".into(),
            refresh_token: "1//SECRET-REFRESH".into(),
            expires_at,
        }
    }

    #[test]
    fn needs_refresh_inside_the_margin() {
        let t = token(10_000);
        assert!(!t.needs_refresh(10_000 - REFRESH_MARGIN_SECS - 1));
        assert!(t.needs_refresh(10_000 - REFRESH_MARGIN_SECS));
        assert!(t.needs_refresh(10_000));
        assert!(t.needs_refresh(20_000));
        assert!(token(i64::MAX).needs_refresh(i64::MAX));
    }

    #[test]
    fn debug_output_hides_both_secrets() {
        let t = token(42);
        for out in [
            format!("{t:?}"),
            format!("{t:#?}"),
            format!("{:?}", Some(&t)),
        ] {
            assert!(!out.contains("SECRET"), "{out}");
            assert!(!out.contains("ya29"), "{out}");
            assert!(out.contains("[redacted]"));
            assert!(out.contains("42"));
        }
    }

    #[test]
    fn the_fake_store_round_trips() {
        let s = FakeTokenStore::default();
        assert_eq!(pollster::block_on(s.load()), Ok(None));
        pollster::block_on(s.connect(&token(1))).unwrap();
        assert_eq!(pollster::block_on(s.load()), Ok(Some(token(1))));
        pollster::block_on(s.save(&token(2))).unwrap();
        assert_eq!(pollster::block_on(s.load()), Ok(Some(token(2))));
    }

    #[test]
    fn a_refresh_save_keeps_the_mark_and_only_connect_clears_it() {
        let s = FakeTokenStore::default();
        pollster::block_on(s.connect(&token(0))).unwrap();
        pollster::block_on(s.load()).unwrap();
        assert_eq!(pollster::block_on(s.needs_reconnect()), Ok(false));
        pollster::block_on(s.mark_needs_reconnect()).unwrap();
        pollster::block_on(s.save(&token(1))).unwrap();
        assert_eq!(pollster::block_on(s.needs_reconnect()), Ok(true));
        pollster::block_on(s.connect(&token(2))).unwrap();
        assert_eq!(pollster::block_on(s.needs_reconnect()), Ok(false));
        assert_eq!(pollster::block_on(s.load()), Ok(Some(token(2))));
    }

    #[test]
    fn a_reconnect_between_load_and_save_survives_the_refresh() {
        let s = FakeTokenStore::default();
        pollster::block_on(s.connect(&token(1))).unwrap();
        pollster::block_on(s.load()).unwrap(); // the scheduled run starts here
        pollster::block_on(s.mark_needs_reconnect()).unwrap();
        pollster::block_on(s.connect(&token(2))).unwrap(); // the owner reconnects
        assert_eq!(
            pollster::block_on(s.save(&token(99))),
            Ok(()),
            "a lost race is not an error"
        );
        assert_eq!(s.token.borrow().clone(), Some(token(2)));
        assert!(!s.reconnect.get());
    }

    #[test]
    fn a_reconnect_between_load_and_mark_keeps_the_mark_cleared() {
        let s = FakeTokenStore::default();
        pollster::block_on(s.connect(&token(1))).unwrap();
        pollster::block_on(s.load()).unwrap();
        pollster::block_on(s.connect(&token(2))).unwrap();
        assert_eq!(pollster::block_on(s.mark_needs_reconnect()), Ok(()));
        assert!(!s.reconnect.get());
        assert_eq!(s.token.borrow().clone(), Some(token(2)));
    }

    #[test]
    fn after_a_saved_refresh_the_run_may_save_or_mark_again() {
        let s = FakeTokenStore::default();
        pollster::block_on(s.connect(&token(1))).unwrap();
        pollster::block_on(s.load()).unwrap();
        pollster::block_on(s.save(&token(2))).unwrap();
        pollster::block_on(s.save(&token(3))).unwrap();
        pollster::block_on(s.mark_needs_reconnect()).unwrap();
        assert_eq!(s.token.borrow().clone(), Some(token(3)));
        assert!(s.reconnect.get());
    }

    #[test]
    fn save_and_mark_need_a_load_first() {
        let s = FakeTokenStore::default();
        assert!(pollster::block_on(s.save(&token(1))).is_err());
        assert!(pollster::block_on(s.mark_needs_reconnect()).is_err());
    }

    #[test]
    fn the_connection_mark_needs_no_load_and_none_means_no_token() {
        let s = FakeTokenStore::default();
        s.reconnect.set(true);
        assert_eq!(pollster::block_on(s.connection_mark()), Ok(None));
        pollster::block_on(s.connect(&token(1))).unwrap();
        assert_eq!(pollster::block_on(s.connection_mark()), Ok(Some(false)));
        s.reconnect.set(true);
        assert_eq!(pollster::block_on(s.connection_mark()), Ok(Some(true)));
    }
}
