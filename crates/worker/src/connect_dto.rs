//! Mapping for the YouTube connect routes: what Google's redirect and token reply
//! look like on the wire, and where the owner is sent afterwards. No decisions
//! here; those are `domain::connect`.

use domain::app::PortError;
use domain::connect::{CallbackParams, ConnectError, TokenReply};
use domain::connection::ConnectionStatus;
use serde::Deserialize;

/// Largest token endpoint body we will parse. Bigger is treated as unreadable.
pub const MAX_TOKEN_BODY_BYTES: usize = 16 * 1024;

/// The query Google redirected back with. Holds the one-time code and the state, so
/// it has no `Display` and must not be logged.
#[derive(Default)]
pub struct CallbackQuery {
    pub state: Option<String>,
    pub code: Option<String>,
    pub error: Option<String>,
}

impl CallbackQuery {
    pub fn as_params(&self) -> CallbackParams<'_> {
        CallbackParams {
            state: self.state.as_deref(),
            code: self.code.as_deref(),
            error: self.error.as_deref(),
        }
    }
}

/// Reads `state`, `code` and `error` from decoded query pairs. A parameter that
/// appears twice is unusable: `state` and `code` become absent, and `error` becomes
/// an empty value, which the domain treats as a failure but never as a denial.
pub fn callback_query(pairs: impl IntoIterator<Item = (String, String)>) -> CallbackQuery {
    let mut q = CallbackQuery::default();
    let (mut seen_state, mut seen_code, mut seen_error) = (false, false, false);
    for (key, value) in pairs {
        let (slot, seen) = match key.as_str() {
            "state" => (&mut q.state, &mut seen_state),
            "code" => (&mut q.code, &mut seen_code),
            "error" => (&mut q.error, &mut seen_error),
            _ => continue,
        };
        if *seen {
            *slot = if key == "error" {
                Some(String::new())
            } else {
                None
            };
        } else {
            *seen = true;
            *slot = Some(value);
        }
    }
    q
}

#[derive(Deserialize)]
struct TokenJson {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    token_type: Option<String>,
    scope: Option<String>,
    error: Option<String>,
}

/// Builds the reply from the HTTP status and body. An oversized, malformed or
/// wrongly typed body keeps only the status, so the domain refuses it.
pub fn reply_from_body(status: u16, body: &str) -> TokenReply {
    let parsed = (body.len() <= MAX_TOKEN_BODY_BYTES)
        .then(|| serde_json::from_str::<TokenJson>(body).ok())
        .flatten();
    match parsed {
        Some(j) => TokenReply {
            status,
            access_token: j.access_token,
            refresh_token: j.refresh_token,
            expires_in: j.expires_in,
            token_type: j.token_type,
            scope: j.scope,
            error: j.error,
        },
        None => TokenReply {
            status,
            access_token: None,
            refresh_token: None,
            expires_in: None,
            token_type: None,
            scope: None,
            error: None,
        },
    }
}

/// Where the callback sends the owner: the app's Settings page. Only fixed codes.
pub fn redirect_location(result: Result<(), ConnectError>) -> String {
    match result {
        Ok(()) => "/settings?connected=1".to_string(),
        Err(e) => format!("/settings?connect_error={}", e.code()),
    }
}

/// The only thing `GET /api/connect/status` returns.
pub fn status_json(s: ConnectionStatus) -> serde_json::Value {
    serde_json::json!({
        "youtube": { "connected": s.connected, "needs_reconnect": s.needs_reconnect }
    })
}

/// Reads the stored `needs_reconnect` column. Anything but exactly 0 or 1 is an
/// error, so a damaged row fails closed instead of reading as "fine".
pub fn flag_from_row(value: f64) -> Result<bool, PortError> {
    if value == 0.0 {
        Ok(false)
    } else if value == 1.0 {
        Ok(true)
    } else {
        Err(PortError("bad needs_reconnect value".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_callback_query_reads_state_code_and_error() {
        let q = callback_query(pairs(&[("state", "s"), ("code", "4/0A"), ("scope", "x")]));
        assert_eq!(q.state.as_deref(), Some("s"));
        assert_eq!(q.code.as_deref(), Some("4/0A"));
        assert_eq!(q.error, None);
        let d = callback_query(pairs(&[("error", "access_denied"), ("state", "s")]));
        assert_eq!(d.error.as_deref(), Some("access_denied"));
        assert_eq!(d.code, None);
        let none = callback_query(pairs(&[]));
        assert_eq!((none.state, none.code, none.error), (None, None, None));
    }

    #[test]
    fn a_repeated_parameter_cannot_be_used() {
        let q = callback_query(pairs(&[
            ("state", "a"),
            ("state", "b"),
            ("code", "1"),
            ("code", "2"),
        ]));
        assert_eq!((q.state, q.code), (None, None));
        // A repeated error is still an error, but never "access_denied".
        let e = callback_query(pairs(&[
            ("error", "access_denied"),
            ("error", "access_denied"),
        ]));
        assert_eq!(e.error.as_deref(), Some(""));
    }

    #[test]
    fn the_borrowed_params_match_the_query() {
        let q = callback_query(pairs(&[("state", "s"), ("code", "c")]));
        let p = q.as_params();
        assert_eq!((p.state, p.code, p.error), (Some("s"), Some("c"), None));
    }

    #[test]
    fn the_token_body_is_read_into_a_reply() {
        let body = r#"{"access_token":"A","expires_in":3599,"refresh_token":"R","scope":"x","token_type":"Bearer"}"#;
        let r = reply_from_body(200, body);
        assert_eq!(r.status, 200);
        assert_eq!(r.access_token.as_deref(), Some("A"));
        assert_eq!(r.refresh_token.as_deref(), Some("R"));
        assert_eq!(r.expires_in, Some(3599));
        assert_eq!(r.token_type.as_deref(), Some("Bearer"));
        assert_eq!(r.scope.as_deref(), Some("x"));
        let err = reply_from_body(
            400,
            r#"{"error":"invalid_grant","error_description":"Bad Request"}"#,
        );
        assert_eq!(err.error.as_deref(), Some("invalid_grant"));
        assert_eq!(err.access_token, None);
    }

    #[test]
    fn an_unreadable_body_keeps_only_the_status() {
        let huge = format!(
            r#"{{"access_token":"{}"}}"#,
            "a".repeat(MAX_TOKEN_BODY_BYTES)
        );
        for body in [
            "",
            "not json",
            "[]",
            r#"{"expires_in":"3599"}"#,
            huge.as_str(),
        ] {
            let r = reply_from_body(200, body);
            assert_eq!(r.status, 200);
            assert_eq!(r.access_token, None, "{body:.20}");
            assert_eq!(r.expires_in, None);
        }
    }

    #[test]
    fn the_redirect_goes_to_settings_with_a_fixed_code() {
        assert_eq!(redirect_location(Ok(())), "/settings?connected=1");
        for (e, code) in [
            (ConnectError::Denied, "access_denied"),
            (ConnectError::BadState, "bad_state"),
            (ConnectError::ExchangeFailed, "exchange_failed"),
            (ConnectError::Server, "server_error"),
        ] {
            assert_eq!(
                redirect_location(Err(e)),
                format!("/settings?connect_error={code}")
            );
        }
    }

    #[test]
    fn the_status_json_has_only_two_flags() {
        let s = domain::connection::ConnectionStatus {
            connected: true,
            needs_reconnect: false,
        };
        assert_eq!(
            status_json(s).to_string(),
            r#"{"youtube":{"connected":true,"needs_reconnect":false}}"#
        );
    }

    #[test]
    fn the_reconnect_flag_must_be_exactly_zero_or_one() {
        assert_eq!(flag_from_row(0.0), Ok(false));
        assert_eq!(flag_from_row(1.0), Ok(true));
        for bad in [2.0, -1.0, 0.5, f64::NAN, f64::INFINITY] {
            assert!(flag_from_row(bad).is_err(), "{bad}");
        }
    }
}
