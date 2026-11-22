//! The owner's YouTube connect callback: turning Google's token reply into a
//! `StoredToken` and the fixed error codes shown to the owner.
//!
//! Provider text (the `error` field, bodies) is only ever compared with fixed
//! strings here and never copied into an error, a log or a redirect. The owner
//! sees one of the fixed codes in `ConnectError::code`.

use crate::token::StoredToken;
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
    /// caller keeps the one it already holds.
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
    let expires_in = reply
        .expires_in
        .filter(|n| (1..=MAX_EXPIRES_IN_SECS).contains(n))
        .ok_or(ExchangeError::Malformed)?;
    let refresh = match (grant, reply.refresh_token.as_ref()) {
        (Grant::Code, r) => bounded(r).ok_or(match r {
            Some(t) if t.len() > MAX_TOKEN_LEN => ExchangeError::Malformed,
            _ => ExchangeError::MissingRefreshToken,
        })?,
        (Grant::Refresh, None) => "",
        (Grant::Refresh, r) => bounded(r).unwrap_or(""),
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

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 5_000_000;

    fn ok_reply() -> TokenReply {
        TokenReply {
            status: 200,
            access_token: Some("ya29.SECRET-ACCESS".into()),
            refresh_token: Some("1//SECRET-REFRESH".into()),
            expires_in: Some(3599),
            token_type: Some("Bearer".into()),
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
    fn failure_statuses_are_sorted_without_reading_provider_text() {
        let err = |status, error: Option<&str>| TokenReply {
            status,
            access_token: None,
            refresh_token: None,
            expires_in: None,
            token_type: None,
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
}
