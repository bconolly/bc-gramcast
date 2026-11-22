//! What the Connections screen shows. Never exposes a token.
use crate::app::PortError;
use crate::token::TokenStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub needs_reconnect: bool,
}

/// Connected means a token is stored. The needs-reconnect mark only counts for a
/// stored token. A failed read of the token or the mark is an error (fail closed),
/// never a guess.
pub async fn connection_status<T: TokenStore>(tokens: &T) -> Result<ConnectionStatus, PortError> {
    let connected = tokens.load().await?.is_some();
    let needs_reconnect = connected && tokens.needs_reconnect().await?;
    Ok(ConnectionStatus {
        connected,
        needs_reconnect,
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

    struct Failing {
        load_fails: bool,
    }
    impl TokenStore for Failing {
        async fn load(&self) -> Result<Option<StoredToken>, PortError> {
            if self.load_fails {
                Err(PortError("load".into()))
            } else {
                Ok(Some(token()))
            }
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
            Err(PortError("mark".into()))
        }
    }

    #[test]
    fn a_failed_read_is_an_error_not_a_default() {
        assert_eq!(
            status(&Failing { load_fails: true }),
            Err(PortError("load".into()))
        );
        assert_eq!(
            status(&Failing { load_fails: false }),
            Err(PortError("mark".into()))
        );
    }
}
