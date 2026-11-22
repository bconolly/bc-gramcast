//! OAuth connect flow, pure parts: the single-use state and the Google
//! authorisation URL. Randomness and storage are ports; the Worker supplies them.
//!
//! The state is the CSRF and replay protection for the callback (no PKCE: support
//! for this flow was not confirmed). It is 256 random bits, valid for ten minutes,
//! and `take` removes it, so a replayed, expired, unknown or malformed state is
//! refused the same way. Never log a state: it is a credential until used.

use crate::app::PortError;
use crate::codec;

/// How long a state stays valid after it is issued.
pub const STATE_TTL_SECS: i64 = 10 * 60;
const STATE_BYTES: usize = 32;
const STATE_HEX_LEN: usize = STATE_BYTES * 2;

pub const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// The only scope asked for: upload videos. Nothing else may be added.
pub const YOUTUBE_UPLOAD_SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";
pub const CALLBACK_PATH: &str = "/api/connect/youtube/callback";

/// Cryptographically secure random bytes (Web Crypto `getRandomValues` in the Worker).
pub trait RandomSource {
    fn fill(&self, buf: &mut [u8]) -> Result<(), PortError>;
}

/// Storage for pending states.
#[allow(async_fn_in_trait)]
pub trait StateStore {
    /// Saves a state. `now` lets the adapter drop rows that have already expired.
    async fn put(&self, state: &str, now: i64, expires_at: i64) -> Result<(), PortError>;
    /// Removes the state and returns its expiry, or `None` if there is no such row.
    /// Must be atomic: of two racing calls for one state, at most one gets `Some`.
    async fn take(&self, state: &str) -> Result<Option<i64>, PortError>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum StateError {
    /// Unknown, replayed, expired or malformed. Deliberately not told apart.
    Invalid,
    Port(PortError),
}

/// Creates a state, stores it, and returns it for the authorisation URL.
pub async fn issue_state<R: RandomSource, S: StateStore>(
    rng: &R,
    store: &S,
    now: i64,
) -> Result<String, PortError> {
    let mut bytes = [0u8; STATE_BYTES];
    rng.fill(&mut bytes)?;
    let state = codec::hex_encode(&bytes);
    store
        .put(&state, now, now.saturating_add(STATE_TTL_SECS))
        .await?;
    Ok(state)
}

/// Uses a state up. Ok only for the first call within the ten minutes. An expired
/// state is removed too, so nothing outlives its first use.
pub async fn consume_state<S: StateStore>(
    store: &S,
    state: &str,
    now: i64,
) -> Result<(), StateError> {
    if !codec::is_lower_hex(state, STATE_HEX_LEN) {
        return Err(StateError::Invalid);
    }
    match store.take(state).await.map_err(StateError::Port)? {
        Some(expires_at) if now < expires_at => Ok(()),
        _ => Err(StateError::Invalid),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    ClientId,
    BaseUrl,
}

/// What the authorisation URL needs. The client secret is not here: only the
/// token exchange uses it.
#[derive(Debug)]
pub struct OAuthConfig {
    client_id: String,
    redirect_uri: String,
}

/// `host` or `host:port`: letters, digits, `.` and `-` in the host (not starting with
/// `.` or `-`), and at most one `:` followed by 1 to 5 digits.
fn host_ok(rest: &str) -> bool {
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    let host_ok = !host.is_empty()
        && !host.starts_with(['.', '-'])
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    let port_ok =
        port.is_none_or(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()));
    host_ok && port_ok
}

fn is_loopback_http(rest: &str) -> bool {
    let host = rest.split(':').next().unwrap_or("");
    host == "localhost" || host == "127.0.0.1"
}

/// `https://host[:port]` (or `http://` for a loopback host), with no path, query,
/// fragment or user info. One trailing slash is tolerated. Returns it without the slash.
fn checked_base(url: &str) -> Option<&str> {
    let base = url.strip_suffix('/').unwrap_or(url);
    let (https, rest) = match base.strip_prefix("https://") {
        Some(rest) => (true, rest),
        None => (false, base.strip_prefix("http://")?),
    };
    (host_ok(rest) && (https || is_loopback_http(rest))).then_some(base)
}

impl OAuthConfig {
    pub fn new(client_id: &str, public_base_url: &str) -> Result<Self, ConfigError> {
        let id_ok = !client_id.is_empty()
            && client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !id_ok {
            return Err(ConfigError::ClientId);
        }
        let base = checked_base(public_base_url).ok_or(ConfigError::BaseUrl)?;
        Ok(Self {
            client_id: client_id.to_string(),
            redirect_uri: format!("{base}{CALLBACK_PATH}"),
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Form body for the code exchange at `GOOGLE_TOKEN_URL`. Holds the client secret
    /// and the code, so it must never be logged.
    pub fn token_request_body(&self, code: &str, client_secret: &str) -> String {
        let enc = codec::percent_encode;
        format!(
            "grant_type=authorization_code&code={}&client_id={}&client_secret={}&redirect_uri={}",
            enc(code),
            enc(&self.client_id),
            enc(client_secret),
            enc(&self.redirect_uri),
        )
    }

    /// The Google consent URL. `access_type=offline` and `prompt=consent` are what
    /// make Google return a refresh token on every authorisation.
    pub fn authorization_url(&self, state: &str) -> String {
        let enc = codec::percent_encode;
        format!(
            "{GOOGLE_AUTH_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}\
             &access_type=offline&prompt=consent&state={}",
            enc(&self.client_id),
            enc(&self.redirect_uri),
            enc(YOUTUBE_UPLOAD_SCOPE),
            enc(state),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    /// Hands out a different byte pattern on every call.
    #[derive(Default)]
    struct FakeRandom {
        calls: Cell<u8>,
        fail: bool,
    }
    impl RandomSource for FakeRandom {
        fn fill(&self, buf: &mut [u8]) -> Result<(), PortError> {
            if self.fail {
                return Err(PortError("rng down".into()));
            }
            let n = self.calls.get().wrapping_add(1);
            self.calls.set(n);
            for (i, b) in buf.iter_mut().enumerate() {
                *b = n.wrapping_mul(31).wrapping_add(i as u8);
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeStates {
        rows: RefCell<HashMap<String, i64>>,
        takes: Cell<u32>,
        fail: bool,
    }
    impl StateStore for FakeStates {
        async fn put(&self, state: &str, _now: i64, expires_at: i64) -> Result<(), PortError> {
            if self.fail {
                return Err(PortError("d1 down".into()));
            }
            self.rows.borrow_mut().insert(state.to_string(), expires_at);
            Ok(())
        }
        async fn take(&self, state: &str) -> Result<Option<i64>, PortError> {
            self.takes.set(self.takes.get() + 1);
            if self.fail {
                return Err(PortError("d1 down".into()));
            }
            Ok(self.rows.borrow_mut().remove(state))
        }
    }

    const NOW: i64 = 1_000_000;

    fn issue(rng: &FakeRandom, store: &FakeStates, now: i64) -> Result<String, PortError> {
        pollster::block_on(issue_state(rng, store, now))
    }
    fn consume(store: &FakeStates, state: &str, now: i64) -> Result<(), StateError> {
        pollster::block_on(consume_state(store, state, now))
    }

    #[test]
    fn a_state_is_64_hex_characters_and_expires_in_ten_minutes() {
        let (rng, store) = (FakeRandom::default(), FakeStates::default());
        let state = issue(&rng, &store, NOW).unwrap();
        assert!(codec::is_lower_hex(&state, 64), "{state}");
        assert_eq!(STATE_TTL_SECS, 600);
        assert_eq!(store.rows.borrow().get(&state), Some(&(NOW + 600)));
    }

    #[test]
    fn two_states_differ() {
        let (rng, store) = (FakeRandom::default(), FakeStates::default());
        assert_ne!(
            issue(&rng, &store, NOW).unwrap(),
            issue(&rng, &store, NOW).unwrap()
        );
    }

    #[test]
    fn a_state_works_once() {
        let (rng, store) = (FakeRandom::default(), FakeStates::default());
        let state = issue(&rng, &store, NOW).unwrap();
        assert_eq!(consume(&store, &state, NOW + 1), Ok(()));
        assert_eq!(consume(&store, &state, NOW + 2), Err(StateError::Invalid));
    }

    #[test]
    fn an_expired_state_is_refused_and_used_up() {
        let (rng, store) = (FakeRandom::default(), FakeStates::default());
        let state = issue(&rng, &store, NOW).unwrap();
        // The last second it is valid is NOW + 599.
        let at_expiry = NOW + STATE_TTL_SECS;
        assert_eq!(consume(&store, &state, at_expiry), Err(StateError::Invalid));
        assert!(store.rows.borrow().is_empty());
        assert_eq!(consume(&store, &state, NOW), Err(StateError::Invalid));
    }

    #[test]
    fn the_last_second_is_still_valid() {
        let (rng, store) = (FakeRandom::default(), FakeStates::default());
        let state = issue(&rng, &store, NOW).unwrap();
        assert_eq!(consume(&store, &state, NOW + STATE_TTL_SECS - 1), Ok(()));
    }

    #[test]
    fn an_unknown_state_is_refused() {
        let store = FakeStates::default();
        let unknown = "a".repeat(64);
        assert_eq!(consume(&store, &unknown, NOW), Err(StateError::Invalid));
    }

    #[test]
    fn a_malformed_state_is_refused_without_a_lookup() {
        let store = FakeStates::default();
        for bad in ["", "abc", &"A".repeat(64), &"a".repeat(65), &"g".repeat(64)] {
            assert_eq!(consume(&store, bad, NOW), Err(StateError::Invalid), "{bad}");
        }
        assert_eq!(store.takes.get(), 0);
    }

    #[test]
    fn storage_and_random_failures_are_port_errors_not_invalid() {
        let down = FakeStates {
            fail: true,
            ..Default::default()
        };
        let state = "a".repeat(64);
        assert_eq!(
            consume(&down, &state, NOW),
            Err(StateError::Port(PortError("d1 down".into())))
        );
        assert_eq!(
            issue(&FakeRandom::default(), &down, NOW),
            Err(PortError("d1 down".into()))
        );
        let bad_rng = FakeRandom {
            fail: true,
            ..Default::default()
        };
        assert_eq!(
            issue(&bad_rng, &FakeStates::default(), NOW),
            Err(PortError("rng down".into()))
        );
    }

    fn config() -> OAuthConfig {
        OAuthConfig::new(
            "client-1.apps.example",
            "https://gramcast.gramcast.workers.dev",
        )
        .unwrap()
    }

    #[test]
    fn the_redirect_uri_is_the_base_plus_the_callback_path() {
        assert_eq!(
            config().redirect_uri(),
            "https://gramcast.gramcast.workers.dev/api/connect/youtube/callback"
        );
        let slash = OAuthConfig::new("c", "https://example.com/").unwrap();
        assert_eq!(
            slash.redirect_uri(),
            "https://example.com/api/connect/youtube/callback"
        );
    }

    #[test]
    fn the_authorisation_url_has_exactly_the_agreed_parameters() {
        let url = config().authorization_url(&"ab".repeat(32));
        assert_eq!(
            url,
            format!(
                "https://accounts.google.com/o/oauth2/v2/auth?response_type=code\
                 &client_id=client-1.apps.example\
                 &redirect_uri=https%3A%2F%2Fgramcast.gramcast.workers.dev%2Fapi%2Fconnect%2Fyoutube%2Fcallback\
                 &scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fyoutube.upload\
                 &access_type=offline&prompt=consent&state={}",
                "ab".repeat(32)
            )
        );
    }

    #[test]
    fn the_scope_is_only_youtube_upload() {
        assert_eq!(
            YOUTUBE_UPLOAD_SCOPE,
            "https://www.googleapis.com/auth/youtube.upload"
        );
        let url = config().authorization_url("s");
        assert_eq!(url.matches("scope=").count(), 1);
        assert!(
            !url.contains("%20"),
            "a second scope would be space separated"
        );
    }

    #[test]
    fn values_are_percent_encoded() {
        // The port's colon and a state full of special characters must both be encoded.
        let c = OAuthConfig::new("client-1", "https://example.com:8443").unwrap();
        let url = c.authorization_url("a b&c=d#e/+%");
        assert!(
            url.contains("&redirect_uri=https%3A%2F%2Fexample.com%3A8443%2Fapi%2Fconnect%2Fyoutube%2Fcallback&"),
            "{url}"
        );
        assert!(url.ends_with("&state=a%20b%26c%3Dd%23e%2F%2B%25"), "{url}");
        // Exactly one query value per parameter: nothing raw leaked into the structure.
        assert_eq!(url.matches('&').count(), 6, "{url}");
        assert_eq!(url.matches('#').count(), 0, "{url}");
    }

    #[test]
    fn config_refuses_bad_values() {
        for bad in ["", " ", "a b", "a&b", "a\nb", "a=b"] {
            assert_eq!(
                OAuthConfig::new(bad, "https://example.com").unwrap_err(),
                ConfigError::ClientId,
                "{bad:?}"
            );
        }
        for bad in [
            "",
            "example.com",
            "http://example.com",
            "ftp://example.com",
            "https://",
            "https://example.com/path",
            "https://example.com?x=1",
            "https://example.com#frag",
            "https://user@example.com",
            "https://exa mple.com",
            "https://example.com//",
            "https://exa\nmple.com",
            "https://example.com:",
            "https://a:b:c",
            "https://example.com:123456",
            "https://example.com:80a",
            "https://example.com:8080:9",
            "https://:8080",
        ] {
            assert_eq!(
                OAuthConfig::new("c", bad).unwrap_err(),
                ConfigError::BaseUrl,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_token_request_is_a_form_with_every_value_encoded() {
        let body = config().token_request_body("4/0A+b/c=d", "s3&cret=x y");
        assert_eq!(
            body,
            "grant_type=authorization_code&code=4%2F0A%2Bb%2Fc%3Dd\
             &client_id=client-1.apps.example\
             &client_secret=s3%26cret%3Dx%20y\
             &redirect_uri=https%3A%2F%2Fgramcast.gramcast.workers.dev%2Fapi%2Fconnect%2Fyoutube%2Fcallback"
        );
    }

    #[test]
    fn plain_http_is_allowed_only_for_loopback_hosts() {
        for ok in [
            "http://localhost:8787",
            "http://127.0.0.1:8787",
            "http://localhost",
        ] {
            assert!(OAuthConfig::new("c", ok).is_ok(), "{ok}");
        }
        assert!(OAuthConfig::new("c", "https://example.com:8443").is_ok());
        assert!(OAuthConfig::new("c", "http://localhost.evil.com").is_err());
        assert!(OAuthConfig::new("c", "http://127.0.0.1.evil.com").is_err());
    }
}
