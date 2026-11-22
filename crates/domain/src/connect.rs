//! The owner's YouTube connect callback: turning Google's token reply into a
//! `StoredToken`, and the use case that ties state, code exchange and storage together.
//!
//! Provider text (the `error` field, bodies) is only ever compared with fixed
//! strings here and never copied into an error, a log or a redirect. The owner
//! sees one of the fixed codes in `ConnectError::code`.

use crate::app::PortError;
use crate::oauth::{self, StateError, StateStore, YOUTUBE_UPLOAD_SCOPE};
use crate::token::{StoredToken, TokenStore};
use std::fmt;

/// Longest access or refresh token accepted. Our own sanity bound.
pub const MAX_TOKEN_LEN: usize = 8192;
/// Longest `expires_in` accepted (7 days). Our own sanity bound; Google's tokens are far shorter.
pub const MAX_EXPIRES_IN_SECS: i64 = 7 * 24 * 60 * 60;
/// Longest authorisation `code` we will forward. Our own sanity bound.
pub const MAX_CODE_LEN: usize = 4096;

/// The fields of the token endpoint's answer, as the Worker read them (all optional:
/// the domain decides what is acceptable). Holds secrets, so Debug is redacted.
#[derive(Clone)]
pub struct TokenReply {
    pub status: u16,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub token_type: Option<String>,
    /// Space-separated scopes Google says were granted, when it says.
    pub scope: Option<String>,
    /// The provider's `error` code, only compared with `invalid_grant`.
    pub error: Option<String>,
}

impl fmt::Debug for TokenReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenReply")
            .field("status", &self.status)
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// First authorisation: Google must hand back a refresh token.
    Code,
    /// Refresh: the reply has no refresh token field, so ours is empty and the
    /// caller keeps the one it already holds. An oversized one is `Malformed`.
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeError {
    /// Google says the code or refresh token is expired or invalidated (`invalid_grant`).
    InvalidGrant,
    /// Any other refusal.
    Rejected,
    /// Rate limit or provider outage: worth trying again later.
    Unavailable,
    /// A success status with an unusable body.
    Malformed,
    /// A code exchange that returned no refresh token.
    MissingRefreshToken,
}

fn bounded(token: Option<&String>) -> Option<&str> {
    token
        .map(String::as_str)
        .filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN_LEN)
}

fn failure(reply: &TokenReply) -> ExchangeError {
    match reply.status {
        429 | 500..=599 => ExchangeError::Unavailable,
        _ if reply.error.as_deref() == Some("invalid_grant") => ExchangeError::InvalidGrant,
        _ => ExchangeError::Rejected,
    }
}

/// Checks a token endpoint reply. `expires_at` is `now + expires_in`.
pub fn parse_token_reply(
    reply: &TokenReply,
    grant: Grant,
    now: i64,
) -> Result<StoredToken, ExchangeError> {
    if !(200..300).contains(&reply.status) {
        return Err(failure(reply));
    }
    let access = bounded(reply.access_token.as_ref()).ok_or(ExchangeError::Malformed)?;
    if reply
        .token_type
        .as_deref()
        .is_some_and(|t| !t.eq_ignore_ascii_case("bearer"))
    {
        return Err(ExchangeError::Malformed);
    }
    // When Google says what was granted, it must include the upload scope. A reply
    // with no scope field is accepted.
    if reply.scope.as_deref().is_some_and(|granted| {
        !granted
            .split_whitespace()
            .any(|s| s == YOUTUBE_UPLOAD_SCOPE)
    }) {
        return Err(ExchangeError::Rejected);
    }
    let expires_in = reply
        .expires_in
        .filter(|n| (1..=MAX_EXPIRES_IN_SECS).contains(n))
        .ok_or(ExchangeError::Malformed)?;
    let refresh = match (grant, reply.refresh_token.as_ref()) {
        (Grant::Code, Some(t)) if t.len() > MAX_TOKEN_LEN => return Err(ExchangeError::Malformed),
        (Grant::Code, r) => bounded(r).ok_or(ExchangeError::MissingRefreshToken)?,
        // A refresh reply may leave it out (or send it empty), but not send a huge one.
        (Grant::Refresh, Some(t)) if t.len() > MAX_TOKEN_LEN => {
            return Err(ExchangeError::Malformed)
        }
        (Grant::Refresh, r) => r.map_or("", String::as_str),
    };
    Ok(StoredToken {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_at: now.saturating_add(expires_in),
    })
}

/// Why a callback did not connect. The codes are the only thing shown to the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    /// The owner said no on Google's screen.
    Denied,
    /// Missing, unknown, replayed, expired or malformed state.
    BadState,
    /// Google refused or could not be reached, or sent nothing usable.
    ExchangeFailed,
    /// Our own storage failed.
    Server,
}

impl ConnectError {
    pub fn code(self) -> &'static str {
        match self {
            ConnectError::Denied => "access_denied",
            ConnectError::BadState => "bad_state",
            ConnectError::ExchangeFailed => "exchange_failed",
            ConnectError::Server => "server_error",
        }
    }
}

/// The query parameters Google sent back. Borrowed, never stored or logged.
pub struct CallbackParams<'a> {
    pub state: Option<&'a str>,
    pub code: Option<&'a str>,
    pub error: Option<&'a str>,
}

/// Port: trades the authorisation code for tokens at Google's token endpoint.
#[allow(async_fn_in_trait)]
pub trait CodeExchanger {
    async fn exchange(&self, code: &str) -> Result<TokenReply, PortError>;
}

/// Owner OAuth callback. The state is consumed first, so a replay or forgery stops
/// before anything is sent to Google; a denial also uses the state up. Only a good
/// exchange reaches `TokenStore::connect`, which clears the needs-reconnect mark.
pub async fn connect_youtube_callback<S: StateStore, E: CodeExchanger, T: TokenStore>(
    states: &S,
    exchanger: &E,
    tokens: &T,
    params: &CallbackParams<'_>,
    now: i64,
) -> Result<(), ConnectError> {
    let state = params.state.ok_or(ConnectError::BadState)?;
    oauth::consume_state(states, state, now)
        .await
        .map_err(|e| match e {
            StateError::Invalid => ConnectError::BadState,
            StateError::Port(_) => ConnectError::Server,
        })?;
    match params.error {
        Some("access_denied") => return Err(ConnectError::Denied),
        Some(_) => return Err(ConnectError::ExchangeFailed),
        None => {}
    }
    let code = params
        .code
        .filter(|c| !c.is_empty() && c.len() <= MAX_CODE_LEN)
        .ok_or(ConnectError::ExchangeFailed)?;
    let reply = exchanger
        .exchange(code)
        .await
        .map_err(|_| ConnectError::ExchangeFailed)?;
    let token =
        parse_token_reply(&reply, Grant::Code, now).map_err(|_| ConnectError::ExchangeFailed)?;
    tokens
        .connect(&token)
        .await
        .map_err(|_| ConnectError::Server)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::STATE_TTL_SECS;
    use crate::token::fakes::FakeTokenStore;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    const NOW: i64 = 5_000_000;

    fn ok_reply() -> TokenReply {
        TokenReply {
            status: 200,
            access_token: Some("ya29.SECRET-ACCESS".into()),
            refresh_token: Some("1//SECRET-REFRESH".into()),
            expires_in: Some(3599),
            token_type: Some("Bearer".into()),
            scope: Some(format!("openid {YOUTUBE_UPLOAD_SCOPE}")),
            error: None,
        }
    }
    fn parse(r: &TokenReply, g: Grant) -> Result<StoredToken, ExchangeError> {
        parse_token_reply(r, g, NOW)
    }

    #[test]
    fn a_code_exchange_gives_a_token_that_expires_in_expires_in_seconds() {
        let t = parse(&ok_reply(), Grant::Code).unwrap();
        assert_eq!(t.access_token, "ya29.SECRET-ACCESS");
        assert_eq!(t.refresh_token, "1//SECRET-REFRESH");
        assert_eq!(t.expires_at, NOW + 3599);
    }

    #[test]
    fn a_code_exchange_needs_a_refresh_token() {
        for missing in [None, Some(String::new())] {
            let r = TokenReply {
                refresh_token: missing,
                ..ok_reply()
            };
            assert_eq!(
                parse(&r, Grant::Code),
                Err(ExchangeError::MissingRefreshToken)
            );
        }
    }

    #[test]
    fn a_refresh_reply_may_have_no_refresh_token() {
        let r = TokenReply {
            refresh_token: None,
            ..ok_reply()
        };
        let t = parse(&r, Grant::Refresh).unwrap();
        assert_eq!(t.refresh_token, "");
        assert_eq!(t.expires_at, NOW + 3599);
    }

    #[test]
    fn a_bad_success_body_is_malformed() {
        let cases = [
            TokenReply {
                access_token: None,
                ..ok_reply()
            },
            TokenReply {
                access_token: Some(String::new()),
                ..ok_reply()
            },
            TokenReply {
                access_token: Some("a".repeat(MAX_TOKEN_LEN + 1)),
                ..ok_reply()
            },
            TokenReply {
                refresh_token: Some("r".repeat(MAX_TOKEN_LEN + 1)),
                ..ok_reply()
            },
            TokenReply {
                expires_in: None,
                ..ok_reply()
            },
            TokenReply {
                expires_in: Some(0),
                ..ok_reply()
            },
            TokenReply {
                expires_in: Some(-5),
                ..ok_reply()
            },
            TokenReply {
                expires_in: Some(MAX_EXPIRES_IN_SECS + 1),
                ..ok_reply()
            },
            TokenReply {
                token_type: Some("MAC".into()),
                ..ok_reply()
            },
        ];
        for r in cases {
            assert_eq!(
                parse(&r, Grant::Code),
                Err(ExchangeError::Malformed),
                "{r:?}"
            );
        }
        let lower = TokenReply {
            token_type: Some("bearer".into()),
            ..ok_reply()
        };
        assert!(parse(&lower, Grant::Code).is_ok());
        let none = TokenReply {
            token_type: None,
            ..ok_reply()
        };
        assert!(parse(&none, Grant::Code).is_ok());
    }

    #[test]
    fn a_granted_scope_must_include_youtube_upload() {
        let with = |scope: Option<&str>| TokenReply {
            scope: scope.map(str::to_string),
            ..ok_reply()
        };
        for grant in [Grant::Code, Grant::Refresh] {
            for good in [
                None,
                Some(YOUTUBE_UPLOAD_SCOPE),
                Some("a  b\t  https://www.googleapis.com/auth/youtube.upload"),
            ] {
                assert!(parse(&with(good), grant).is_ok(), "{good:?}");
            }
            for bad in [
                "",
                "openid email",
                "https://www.googleapis.com/auth/youtube",
                "https://www.googleapis.com/auth/youtube.upload.evil",
                "xhttps://www.googleapis.com/auth/youtube.upload",
            ] {
                assert_eq!(
                    parse(&with(Some(bad)), grant),
                    Err(ExchangeError::Rejected),
                    "{bad:?}"
                );
            }
        }
    }

    #[test]
    fn a_refresh_reply_with_an_oversized_refresh_token_is_malformed() {
        let huge = Some("r".repeat(MAX_TOKEN_LEN + 1));
        let r = TokenReply {
            refresh_token: huge,
            ..ok_reply()
        };
        assert_eq!(parse(&r, Grant::Refresh), Err(ExchangeError::Malformed));
        // An empty one is the same as none.
        let empty = TokenReply {
            refresh_token: Some(String::new()),
            ..ok_reply()
        };
        assert_eq!(parse(&empty, Grant::Refresh).unwrap().refresh_token, "");
    }

    #[test]
    fn failure_statuses_are_sorted_without_reading_provider_text() {
        let err = |status, error: Option<&str>| TokenReply {
            status,
            access_token: None,
            refresh_token: None,
            expires_in: None,
            token_type: None,
            scope: None,
            error: error.map(str::to_string),
        };
        assert_eq!(
            parse(&err(400, Some("invalid_grant")), Grant::Code),
            Err(ExchangeError::InvalidGrant)
        );
        assert_eq!(
            parse(&err(400, Some("invalid_grant")), Grant::Refresh),
            Err(ExchangeError::InvalidGrant)
        );
        assert_eq!(
            parse(&err(400, Some("invalid_client")), Grant::Code),
            Err(ExchangeError::Rejected)
        );
        assert_eq!(
            parse(&err(401, None), Grant::Code),
            Err(ExchangeError::Rejected)
        );
        assert_eq!(
            parse(&err(302, None), Grant::Code),
            Err(ExchangeError::Rejected)
        );
        assert_eq!(
            parse(&err(429, None), Grant::Code),
            Err(ExchangeError::Unavailable)
        );
        assert_eq!(
            parse(&err(500, None), Grant::Code),
            Err(ExchangeError::Unavailable)
        );
        assert_eq!(
            parse(&err(503, None), Grant::Code),
            Err(ExchangeError::Unavailable)
        );
        // A failure status wins even when the body also carries tokens.
        let both = TokenReply {
            status: 400,
            ..ok_reply()
        };
        assert_eq!(parse(&both, Grant::Code), Err(ExchangeError::Rejected));
    }

    #[test]
    fn the_reply_debug_output_hides_secrets() {
        let out = format!("{:?}", ok_reply());
        assert!(!out.contains("SECRET") && !out.contains("ya29"), "{out}");
        assert!(out.contains("[redacted]"));
    }

    #[test]
    fn connect_errors_have_fixed_codes() {
        assert_eq!(ConnectError::Denied.code(), "access_denied");
        assert_eq!(ConnectError::BadState.code(), "bad_state");
        assert_eq!(ConnectError::ExchangeFailed.code(), "exchange_failed");
        assert_eq!(ConnectError::Server.code(), "server_error");
    }

    // ---- the callback use case ----

    #[derive(Default)]
    struct FakeStates {
        rows: RefCell<HashMap<String, i64>>,
        fail: bool,
    }
    impl StateStore for FakeStates {
        async fn put(&self, s: &str, _now: i64, exp: i64) -> Result<(), PortError> {
            self.rows.borrow_mut().insert(s.to_string(), exp);
            Ok(())
        }
        async fn take(&self, s: &str) -> Result<Option<i64>, PortError> {
            if self.fail {
                return Err(PortError("d1 down".into()));
            }
            Ok(self.rows.borrow_mut().remove(s))
        }
    }

    struct FakeExchanger {
        reply: Result<TokenReply, PortError>,
        calls: Cell<u32>,
        last_code: RefCell<String>,
    }
    impl FakeExchanger {
        fn new(reply: Result<TokenReply, PortError>) -> Self {
            Self {
                reply,
                calls: Cell::new(0),
                last_code: RefCell::new(String::new()),
            }
        }
    }
    impl CodeExchanger for FakeExchanger {
        async fn exchange(&self, code: &str) -> Result<TokenReply, PortError> {
            self.calls.set(self.calls.get() + 1);
            *self.last_code.borrow_mut() = code.to_string();
            match &self.reply {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(PortError(e.0.clone())),
            }
        }
    }

    struct FailingTokens;
    impl TokenStore for FailingTokens {
        async fn load(&self) -> Result<Option<StoredToken>, PortError> {
            Ok(None)
        }
        async fn save(&self, _: &StoredToken) -> Result<(), PortError> {
            Ok(())
        }
        async fn connect(&self, _: &StoredToken) -> Result<(), PortError> {
            Err(PortError("d1 down".into()))
        }
        async fn mark_needs_reconnect(&self) -> Result<(), PortError> {
            Ok(())
        }
        async fn needs_reconnect(&self) -> Result<bool, PortError> {
            Ok(false)
        }
        async fn connection_mark(&self) -> Result<Option<bool>, PortError> {
            Ok(None)
        }
    }

    fn state() -> String {
        "c".repeat(64)
    }
    fn states_with(s: &str) -> FakeStates {
        let st = FakeStates::default();
        st.rows
            .borrow_mut()
            .insert(s.to_string(), NOW + STATE_TTL_SECS);
        st
    }
    fn params<'a>(
        state: Option<&'a str>,
        code: Option<&'a str>,
        error: Option<&'a str>,
    ) -> CallbackParams<'a> {
        CallbackParams { state, code, error }
    }
    fn run<T: TokenStore>(
        states: &FakeStates,
        ex: &FakeExchanger,
        tokens: &T,
        p: &CallbackParams<'_>,
    ) -> Result<(), ConnectError> {
        pollster::block_on(connect_youtube_callback(states, ex, tokens, p, NOW))
    }

    #[test]
    fn a_good_callback_stores_the_token_and_clears_the_reconnect_mark() {
        let (st, s) = (states_with(&state()), state());
        let ex = FakeExchanger::new(Ok(ok_reply()));
        let tokens = FakeTokenStore::default();
        tokens.reconnect.set(true);
        assert_eq!(
            run(&st, &ex, &tokens, &params(Some(&s), Some("the-code"), None)),
            Ok(())
        );
        assert_eq!(*ex.last_code.borrow(), "the-code");
        let stored = tokens.token.borrow().clone().unwrap();
        assert_eq!(stored.expires_at, NOW + 3599);
        assert!(!tokens.reconnect.get(), "connect must clear the mark");
        assert!(st.rows.borrow().is_empty(), "the state is used up");
    }

    #[test]
    fn a_replayed_callback_is_refused_and_exchanges_nothing() {
        let (st, s) = (states_with(&state()), state());
        let ex = FakeExchanger::new(Ok(ok_reply()));
        let tokens = FakeTokenStore::default();
        let p = params(Some(&s), Some("c"), None);
        assert_eq!(run(&st, &ex, &tokens, &p), Ok(()));
        assert_eq!(run(&st, &ex, &tokens, &p), Err(ConnectError::BadState));
        assert_eq!(ex.calls.get(), 1);
    }

    #[test]
    fn a_bad_state_never_reaches_the_exchange_or_the_store() {
        let ex = FakeExchanger::new(Ok(ok_reply()));
        let tokens = FakeTokenStore::default();
        let known = state();
        let expired = FakeStates::default();
        expired.rows.borrow_mut().insert(known.clone(), NOW);
        let cases: Vec<(FakeStates, CallbackParams<'_>)> = vec![
            (FakeStates::default(), params(None, Some("c"), None)),
            (FakeStates::default(), params(Some(&known), Some("c"), None)),
            (
                FakeStates::default(),
                params(Some("short"), Some("c"), None),
            ),
            (expired, params(Some(&known), Some("c"), None)),
            // Google's error redirect with an unknown state is a bad state, not a denial.
            (
                FakeStates::default(),
                params(Some(&known), None, Some("access_denied")),
            ),
        ];
        for (st, p) in &cases {
            assert_eq!(run(st, &ex, &tokens, p), Err(ConnectError::BadState));
        }
        assert_eq!(ex.calls.get(), 0);
        assert!(tokens.token.borrow().is_none());
    }

    #[test]
    fn a_denial_uses_up_the_state_and_exchanges_nothing() {
        let (st, s) = (states_with(&state()), state());
        let ex = FakeExchanger::new(Ok(ok_reply()));
        let tokens = FakeTokenStore::default();
        let r = run(
            &st,
            &ex,
            &tokens,
            &params(Some(&s), None, Some("access_denied")),
        );
        assert_eq!(r, Err(ConnectError::Denied));
        assert!(st.rows.borrow().is_empty());
        assert_eq!(ex.calls.get(), 0);
    }

    #[test]
    fn another_provider_error_is_an_exchange_failure() {
        let (st, s) = (states_with(&state()), state());
        let ex = FakeExchanger::new(Ok(ok_reply()));
        let r = run(
            &st,
            &ex,
            &FakeTokenStore::default(),
            &params(Some(&s), Some("c"), Some("<script>x</script>")),
        );
        assert_eq!(r, Err(ConnectError::ExchangeFailed));
        assert_eq!(ex.calls.get(), 0);
    }

    #[test]
    fn a_missing_or_oversized_code_is_an_exchange_failure() {
        let big = "x".repeat(MAX_CODE_LEN + 1);
        for code in [None, Some(""), Some(big.as_str())] {
            let (st, s) = (states_with(&state()), state());
            let ex = FakeExchanger::new(Ok(ok_reply()));
            let r = run(
                &st,
                &ex,
                &FakeTokenStore::default(),
                &params(Some(&s), code, None),
            );
            assert_eq!(r, Err(ConnectError::ExchangeFailed));
            assert_eq!(ex.calls.get(), 0);
        }
    }

    #[test]
    fn exchange_failures_keep_the_old_token_and_mark() {
        let failures = [
            Err(PortError("network".into())),
            Ok(TokenReply {
                status: 400,
                error: Some("invalid_grant".into()),
                ..ok_reply()
            }),
            Ok(TokenReply {
                status: 500,
                ..ok_reply()
            }),
            Ok(TokenReply {
                refresh_token: None,
                ..ok_reply()
            }),
        ];
        for reply in failures {
            let (st, s) = (states_with(&state()), state());
            let ex = FakeExchanger::new(reply);
            let tokens = FakeTokenStore::default();
            tokens.reconnect.set(true);
            let r = run(&st, &ex, &tokens, &params(Some(&s), Some("c"), None));
            assert_eq!(r, Err(ConnectError::ExchangeFailed));
            assert!(tokens.token.borrow().is_none());
            assert!(tokens.reconnect.get());
        }
    }

    #[test]
    fn storage_failures_are_server_errors() {
        let (st, s) = (states_with(&state()), state());
        let ex = FakeExchanger::new(Ok(ok_reply()));
        let r = run(&st, &ex, &FailingTokens, &params(Some(&s), Some("c"), None));
        assert_eq!(r, Err(ConnectError::Server));

        let down = FakeStates {
            fail: true,
            ..Default::default()
        };
        let r = run(
            &down,
            &ex,
            &FakeTokenStore::default(),
            &params(Some(&s), Some("c"), None),
        );
        assert_eq!(r, Err(ConnectError::Server));
    }
}
