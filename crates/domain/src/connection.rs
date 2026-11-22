//! What the Connections screen shows. Never exposes a token.
use crate::app::PortError;
use crate::token::TokenStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub needs_reconnect: bool,
}

/// Connected means a token row exists. The needs-reconnect mark only counts for a
/// stored token. This never reads the token itself, so a row that can no longer be
/// decrypted (a lost or changed key) still shows as connected, and the owner can
/// reconnect. A failed read is an error (fail closed), never a guess.
pub async fn connection_status<T: TokenStore>(tokens: &T) -> Result<ConnectionStatus, PortError> {
    let mark = tokens.connection_mark().await?;
    Ok(ConnectionStatus {
        connected: mark.is_some(),
        needs_reconnect: mark == Some(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::fakes::FakeTokenStore;
    use crate::token::StoredToken;

    fn token() -> StoredToken {
        StoredToken {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1,
        }
    }
    fn status(t: &impl TokenStore) -> Result<ConnectionStatus, PortError> {
        pollster::block_on(connection_status(t))
    }

    #[test]
    fn nothing_stored_means_not_connected() {
        let s = FakeTokenStore::default();
        assert_eq!(
            status(&s),
            Ok(ConnectionStatus {
                connected: false,
                needs_reconnect: false
            })
        );
    }

    #[test]
    fn a_stored_token_is_connected_and_may_need_reconnecting() {
        let s = FakeTokenStore::default();
        pollster::block_on(s.connect(&token())).unwrap();
        assert_eq!(
            status(&s),
            Ok(ConnectionStatus {
                connected: true,
                needs_reconnect: false
            })
        );
        pollster::block_on(s.load()).unwrap();
        pollster::block_on(s.mark_needs_reconnect()).unwrap();
        assert_eq!(
            status(&s),
            Ok(ConnectionStatus {
                connected: true,
                needs_reconnect: true
            })
        );
    }

    #[test]
    fn a_mark_without_a_token_is_not_shown() {
        let s = FakeTokenStore::default();
        s.reconnect.set(true);
        assert_eq!(
            status(&s),
            Ok(ConnectionStatus {
                connected: false,
                needs_reconnect: false
            })
        );
    }

    /// A store whose token cannot be read (a lost key): only the row flags work.
    struct Unreadable {
        mark: Result<Option<bool>, PortError>,
    }
    impl TokenStore for Unreadable {
        async fn load(&self) -> Result<Option<StoredToken>, PortError> {
            panic!("the status must not read the token");
        }
        async fn save(&self, _: &StoredToken) -> Result<(), PortError> {
            Ok(())
        }
        async fn connect(&self, _: &StoredToken) -> Result<(), PortError> {
            Ok(())
        }
        async fn mark_needs_reconnect(&self) -> Result<(), PortError> {
            Ok(())
        }
        async fn needs_reconnect(&self) -> Result<bool, PortError> {
            panic!("the status asks the mark once, with the row");
        }
        async fn connection_mark(&self) -> Result<Option<bool>, PortError> {
            match &self.mark {
                Ok(m) => Ok(*m),
                Err(e) => Err(PortError(e.0.clone())),
            }
        }
    }

    #[test]
    fn an_unreadable_token_still_shows_as_connected() {
        for (mark, need) in [(Some(false), false), (Some(true), true)] {
            let s = Unreadable { mark: Ok(mark) };
            assert_eq!(
                status(&s),
                Ok(ConnectionStatus {
                    connected: true,
                    needs_reconnect: need
                })
            );
        }
    }

    #[test]
    fn a_failed_read_is_an_error_not_a_default() {
        let s = Unreadable {
            mark: Err(PortError("d1 down".into())),
        };
        assert_eq!(status(&s), Err(PortError("d1 down".into())));
    }
}
