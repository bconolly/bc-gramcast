//! OAuth token model and the ports around it. Tokens are secrets: the Debug
//! output is redacted so a stray `{:?}` or `assert_eq!` failure never leaks one.

use crate::app::PortError;
use std::fmt;

/// Refresh this long before the real expiry, so a token never dies mid-upload.
pub const REFRESH_MARGIN_SECS: i64 = 5 * 60;

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
#[allow(async_fn_in_trait)]
pub trait TokenStore {
    /// `None` until the owner has connected the account.
    async fn load(&self) -> Result<Option<StoredToken>, PortError>;
    async fn save(&self, token: &StoredToken) -> Result<(), PortError>;
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::cell::RefCell;

    /// In-memory store for tests.
    #[derive(Default)]
    pub struct FakeTokenStore {
        pub token: RefCell<Option<StoredToken>>,
    }
    impl TokenStore for FakeTokenStore {
        async fn load(&self) -> Result<Option<StoredToken>, PortError> {
            Ok(self.token.borrow().clone())
        }
        async fn save(&self, token: &StoredToken) -> Result<(), PortError> {
            *self.token.borrow_mut() = Some(token.clone());
            Ok(())
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
        pollster::block_on(s.save(&token(1))).unwrap();
        assert_eq!(pollster::block_on(s.load()), Ok(Some(token(1))));
    }
}
