//! Owner sign-in rules for a Cloudflare Access token. Pure: no I/O, no
//! dependencies. The Worker parses the token and fetches keys; every decision
//! about whether it is acceptable is made here.

/// Seconds of clock difference we tolerate on `exp` and `nbf`.
pub const CLOCK_LEEWAY_SECS: i64 = 30;
/// Longest token we will look at, in bytes.
pub const MAX_TOKEN_BYTES: usize = 8192;
/// Longest base64url text `decode_b64url` accepts (a bit over the largest token).
pub const MAX_B64_CHARS: usize = MAX_TOKEN_BYTES;
/// Smallest RSA modulus we accept, in bytes (2048 bits).
pub const MIN_MODULUS_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    MissingToken,
    Malformed,
    UnsupportedAlg,
    BadKeyId,
    UnknownKey,
    BadSignature,
    WrongIssuer,
    WrongAudience,
    Expired,
    NotYetValid,
    NotOwner,
    NotConfigured,
    KeysUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthConfigError {
    BadTeamDomain,
    BadAudience,
    BadOwnerEmail,
}

/// What the Worker was told about this deployment's Access application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessConfig {
    team_domain: String,
    audience: String,
    owner_email: String,
}

impl AccessConfig {
    pub fn new(
        team_domain: &str,
        audience: &str,
        owner_email: &str,
    ) -> Result<Self, AuthConfigError> {
        if !valid_team_domain(team_domain) {
            return Err(AuthConfigError::BadTeamDomain);
        }
        if !(1..=128).contains(&audience.len())
            || !audience.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(AuthConfigError::BadAudience);
        }
        if !valid_email(owner_email) {
            return Err(AuthConfigError::BadOwnerEmail);
        }
        Ok(Self {
            team_domain: team_domain.to_string(),
            audience: audience.to_string(),
            owner_email: owner_email.to_string(),
        })
    }

    /// The `iss` value Access puts in its tokens.
    pub fn issuer(&self) -> String {
        format!("https://{}", self.team_domain)
    }

    /// Where the public keys live. The only URL the Worker ever fetches keys from.
    pub fn certs_url(&self) -> String {
        format!("https://{}/cdn-cgi/access/certs", self.team_domain)
    }
}

fn valid_team_domain(text: &str) -> bool {
    let Some(label) = text.strip_suffix(".cloudflareaccess.com") else {
        return false;
    };
    (1..=63).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_email(text: &str) -> bool {
    if !(3..=254).contains(&text.len()) || text.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let mut sides = text.split('@');
    matches!(
        (sides.next(), sides.next(), sides.next()),
        (Some(l), Some(r), None) if !l.is_empty() && !r.is_empty()
    )
}

/// Splits `header.payload.signature`: exactly three non-empty parts.
pub fn split_token(token: &str) -> Result<(&str, &str, &str), AuthError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(AuthError::Malformed);
    }
    let mut parts = token.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() && !s.is_empty() => {
            Ok((h, p, s))
        }
        _ => Err(AuthError::Malformed),
    }
}

fn b64_value(b: u8) -> Option<u32> {
    match b {
        b'A'..=b'Z' => Some(u32::from(b - b'A')),
        b'a'..=b'z' => Some(u32::from(b - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(b - b'0') + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

/// Strict base64url without padding. `None` for any other character, a bad
/// length, non-canonical trailing bits, empty input or input over the cap.
pub fn decode_b64url(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_B64_CHARS || bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut acc: u32 = 0;
        for &b in chunk {
            acc = (acc << 6) | b64_value(b)?;
        }
        // Left-align the bits to a 24-bit group, then keep whole bytes only.
        let pad_bits = 6 * (4 - chunk.len() as u32);
        acc <<= pad_bits;
        let n = chunk.len() * 6 / 8;
        let group = acc.to_be_bytes();
        // Bits beyond the whole bytes must be zero, or the text is not canonical.
        if n < 3 && group[1 + n..4].iter().any(|&x| x != 0) {
            return None;
        }
        out.extend_from_slice(&group[1..1 + n]);
    }
    Some(out)
}

/// Refuses RSA keys under 2048 bits. Leading zero bytes are padding, not key
/// size, so they are not counted.
pub fn check_modulus_len(modulus: &[u8]) -> bool {
    strip_leading_zeros(modulus).len() >= MIN_MODULUS_BYTES
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    &bytes[start..]
}

/// Whether a published key may be used to check tokens: an RSA key marked for
/// RS256 (or not marked), with a plain key id, a modulus of at least 2048 bits
/// and an odd public exponent of at least 3 that fits in 8 bytes.
pub fn accept_jwk(kty: &str, alg: Option<&str>, kid: &str, n: &[u8], e: &[u8]) -> bool {
    let exponent = strip_leading_zeros(e);
    let exponent_ok = (1..=8).contains(&exponent.len()) && {
        let value = exponent.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
        value % 2 == 1 && value >= 3
    };
    kty == "RSA"
        && alg.is_none_or(|a| a == "RS256")
        && valid_kid(kid)
        && check_modulus_len(n)
        && exponent_ok
}

/// The parts of the token header we look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtHeader {
    pub alg: String,
    pub kid: String,
}

/// The parts of the token payload we look at. Everything else is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub iss: String,
    pub aud: Vec<String>,
    pub exp: i64,
    pub nbf: Option<i64>,
    pub email: Option<String>,
}

/// Only RS256 is accepted (so `none`, `HS256` and `rs256` are refused), and the
/// key id must be a short plain identifier.
pub fn check_header(header: &JwtHeader) -> Result<(), AuthError> {
    if header.alg != "RS256" {
        return Err(AuthError::UnsupportedAlg);
    }
    if valid_kid(&header.kid) {
        Ok(())
    } else {
        Err(AuthError::BadKeyId)
    }
}

/// A key id is 1 to 128 plain characters: letters, digits, `.`, `_` and `-`.
pub fn valid_kid(kid: &str) -> bool {
    (1..=128).contains(&kid.len())
        && kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Checks the claims against this deployment. `now` is unix seconds.
pub fn check_claims(claims: &Claims, config: &AccessConfig, now: i64) -> Result<(), AuthError> {
    if claims.iss != config.issuer() {
        return Err(AuthError::WrongIssuer);
    }
    if !claims.aud.contains(&config.audience) {
        return Err(AuthError::WrongAudience);
    }
    // `saturating_sub` and `saturating_add` keep extreme values from wrapping.
    if claims.exp <= now.saturating_sub(CLOCK_LEEWAY_SECS) {
        return Err(AuthError::Expired);
    }
    if claims
        .nbf
        .is_some_and(|nbf| nbf > now.saturating_add(CLOCK_LEEWAY_SECS))
    {
        return Err(AuthError::NotYetValid);
    }
    match &claims.email {
        Some(e) if e.eq_ignore_ascii_case(&config.owner_email) => Ok(()),
        _ => Err(AuthError::NotOwner),
    }
}

/// DNS-rebinding guard for local dev: only a Host of exactly `localhost` or
/// `127.0.0.1`, with an optional port (1 to 65535), may use the no-auth mode.
pub fn is_loopback_host(host_header: Option<&str>) -> bool {
    let Some(value) = host_header else {
        return false;
    };
    let (host, port) = match value.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (value, None),
    };
    // 1..=65535: digits only (parse alone would accept a leading `+`), and never port 0.
    let port_ok = port.is_none_or(|p| {
        p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n != 0)
    });
    port_ok && (host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1")
}

/// Whether the no-sign-in local dev mode applies. All three must hold: the flag
/// is exactly `true`, no team domain is configured (a configured one means
/// production, so the flag is ignored), and the Host is loopback.
pub fn local_dev_bypass(
    flag: Option<&str>,
    team_domain: Option<&str>,
    host_is_loopback: bool,
) -> bool {
    flag == Some("true") && team_domain.is_none_or(str::is_empty) && host_is_loopback
}

/// CSRF guard for requests that change state. A browser always sends `Origin`
/// on these, so when it is there it must be this site: `https://<host>`, or
/// `http://<host>` only when the Host is loopback (local dev). A request with no
/// `Origin` does not come from a browser page, so it cannot be cross-site forgery.
pub fn check_origin(
    method: &str,
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
    host: Option<&str>,
) -> bool {
    let mutating = ["POST", "PUT", "PATCH", "DELETE"]
        .iter()
        .any(|m| method.eq_ignore_ascii_case(m));
    if !mutating {
        return true;
    }
    if sec_fetch_site.is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {
        return false;
    }
    let Some(origin) = origin else {
        return true;
    };
    let Some(host) = host.filter(|h| !h.is_empty()) else {
        return false;
    };
    origin.eq_ignore_ascii_case(&format!("https://{host}"))
        || (is_loopback_host(Some(host)) && origin.eq_ignore_ascii_case(&format!("http://{host}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Result<AccessConfig, AuthConfigError> {
        AccessConfig::new(
            "team-x.cloudflareaccess.com",
            "abc123DEF",
            "owner@example.com",
        )
    }

    #[test]
    fn config_accepts_good_values_and_builds_urls() {
        let c = cfg().unwrap();
        assert_eq!(c.issuer(), "https://team-x.cloudflareaccess.com");
        assert_eq!(
            c.certs_url(),
            "https://team-x.cloudflareaccess.com/cdn-cgi/access/certs"
        );
    }

    #[test]
    fn config_refuses_bad_team_domains() {
        let long = format!("{}.cloudflareaccess.com", "a".repeat(64));
        for bad in [
            "",
            "cloudflareaccess.com",
            ".cloudflareaccess.com",
            "x.cloudflareaccess.com.evil.com",
            "evil.com",
            "a.b.cloudflareaccess.com",
            "-x.cloudflareaccess.com",
            "x-.cloudflareaccess.com",
            "X.cloudflareaccess.com",
            "x_y.cloudflareaccess.com",
            "x.cloudflareaccess.com/path",
            "https://x.cloudflareaccess.com",
            "x.cloudflareaccess.com ",
            long.as_str(),
        ] {
            assert_eq!(
                AccessConfig::new(bad, "abc", "o@e.co"),
                Err(AuthConfigError::BadTeamDomain),
                "{bad:?}"
            );
        }
        let max = format!("{}.cloudflareaccess.com", "a".repeat(63));
        assert!(AccessConfig::new(&max, "abc", "o@e.co").is_ok());
    }

    #[test]
    fn config_refuses_bad_audiences() {
        let long = "a".repeat(129);
        for bad in ["", "ab-c", "ab c", "ab.c", long.as_str()] {
            assert_eq!(
                AccessConfig::new("t.cloudflareaccess.com", bad, "o@e.co"),
                Err(AuthConfigError::BadAudience),
                "{bad:?}"
            );
        }
        assert!(AccessConfig::new("t.cloudflareaccess.com", &"a".repeat(128), "o@e.co").is_ok());
    }

    #[test]
    fn config_refuses_bad_owner_emails() {
        let long = format!("{}@e.co", "a".repeat(250));
        for bad in [
            "",
            "a@",
            "@b",
            "ab",
            "a@@b",
            "a@b@c",
            "a b@c.d",
            "a@b.c\n",
            "a\t@b.c",
            "a\u{0}@b.c",
            long.as_str(),
        ] {
            assert_eq!(
                AccessConfig::new("t.cloudflareaccess.com", "abc", bad),
                Err(AuthConfigError::BadOwnerEmail),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn split_token_needs_three_non_empty_parts() {
        assert_eq!(split_token("a.b.c"), Ok(("a", "b", "c")));
        for bad in [
            "", "a", "a.b", "a.b.c.d", ".b.c", "a..c", "a.b.", "..", "...",
        ] {
            assert_eq!(split_token(bad), Err(AuthError::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn split_token_caps_total_size() {
        let at_cap = format!("a.b.{}", "c".repeat(MAX_TOKEN_BYTES - 4));
        assert_eq!(at_cap.len(), MAX_TOKEN_BYTES);
        assert!(split_token(&at_cap).is_ok());
        let over = format!("{at_cap}c");
        assert_eq!(split_token(&over), Err(AuthError::Malformed));
    }

    #[test]
    fn b64url_matches_rfc4648_vectors_without_padding() {
        for (plain, enc) in [
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(decode_b64url(enc), Some(plain.as_bytes().to_vec()), "{enc}");
        }
    }

    #[test]
    fn b64url_uses_the_url_alphabet() {
        assert_eq!(decode_b64url("-_8"), Some(vec![0xfb, 0xff]));
        assert_eq!(decode_b64url("-_-_"), Some(vec![0xfb, 0xff, 0xbf]));
    }

    #[test]
    fn b64url_refuses_everything_else() {
        for bad in [
            "Zg==", "Zm8=", "Z", "Zm9vY", "+/8", "ab/c", "ab+c", "ab cd", "ab\ncd", "abc!", "",
            "Zh", "Zm9", "é",
        ] {
            assert_eq!(decode_b64url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn b64url_is_bounded() {
        assert!(decode_b64url(&"A".repeat(MAX_B64_CHARS)).is_some());
        assert_eq!(decode_b64url(&"A".repeat(MAX_B64_CHARS + 4)), None);
    }

    #[test]
    fn modulus_under_2048_bits_is_refused() {
        assert!(!check_modulus_len(&[]));
        assert!(!check_modulus_len(&[1; 255]));
        assert!(check_modulus_len(&[1; 256]));
        assert!(check_modulus_len(&[1; 512]));
    }

    fn header(alg: &str, kid: &str) -> JwtHeader {
        JwtHeader {
            alg: alg.into(),
            kid: kid.into(),
        }
    }

    #[test]
    fn header_accepts_rs256_with_a_plain_kid() {
        assert_eq!(check_header(&header("RS256", "abc.DEF_1-2")), Ok(()));
        assert_eq!(check_header(&header("RS256", &"k".repeat(128))), Ok(()));
    }

    #[test]
    fn header_refuses_every_other_algorithm() {
        for alg in [
            "none", "None", "HS256", "rs256", "RS384", "ES256", "", "RS256 ",
        ] {
            assert_eq!(
                check_header(&header(alg, "k1")),
                Err(AuthError::UnsupportedAlg),
                "{alg:?}"
            );
        }
    }

    #[test]
    fn header_refuses_bad_key_ids() {
        let long = "k".repeat(129);
        for kid in [
            "",
            "a b",
            "a/b",
            "a\u{0}",
            "../x",
            "k\n",
            "é",
            long.as_str(),
        ] {
            assert_eq!(
                check_header(&header("RS256", kid)),
                Err(AuthError::BadKeyId),
                "{kid:?}"
            );
        }
    }

    #[test]
    fn header_checks_alg_before_kid() {
        assert_eq!(
            check_header(&header("none", "")),
            Err(AuthError::UnsupportedAlg)
        );
    }

    const NOW: i64 = 1_800_000_000;

    fn claims() -> Claims {
        Claims {
            iss: "https://team-x.cloudflareaccess.com".into(),
            aud: vec!["other".into(), "abc123DEF".into()],
            exp: NOW + 600,
            nbf: Some(NOW - 5),
            email: Some("Owner@Example.com".into()),
        }
    }

    fn check(c: &Claims) -> Result<(), AuthError> {
        check_claims(c, &cfg().unwrap(), NOW)
    }

    #[test]
    fn claims_accept_a_good_token_for_the_owner_ignoring_email_case() {
        assert_eq!(check(&claims()), Ok(()));
        let mut c = claims();
        c.nbf = None;
        assert_eq!(check(&c), Ok(()));
    }

    #[test]
    fn claims_refuse_a_wrong_issuer() {
        for iss in [
            "",
            "https://team-x.cloudflareaccess.com/",
            "https://TEAM-X.cloudflareaccess.com",
            "http://team-x.cloudflareaccess.com",
            "https://other.cloudflareaccess.com",
        ] {
            let mut c = claims();
            c.iss = iss.into();
            assert_eq!(check(&c), Err(AuthError::WrongIssuer), "{iss:?}");
        }
    }

    #[test]
    fn claims_refuse_a_missing_or_different_audience() {
        for aud in [
            vec![],
            vec!["nope".to_string()],
            vec!["ABC123DEF".to_string()],
        ] {
            let mut c = claims();
            c.aud = aud;
            assert_eq!(check(&c), Err(AuthError::WrongAudience));
        }
    }

    #[test]
    fn claims_expiry_has_a_thirty_second_leeway() {
        let mut c = claims();
        c.exp = NOW - CLOCK_LEEWAY_SECS + 1;
        assert_eq!(check(&c), Ok(()));
        c.exp = NOW - CLOCK_LEEWAY_SECS;
        assert_eq!(check(&c), Err(AuthError::Expired));
        c.exp = i64::MIN;
        assert_eq!(check(&c), Err(AuthError::Expired));
    }

    #[test]
    fn claims_not_before_has_a_thirty_second_leeway() {
        let mut c = claims();
        c.nbf = Some(NOW + CLOCK_LEEWAY_SECS);
        assert_eq!(check(&c), Ok(()));
        c.nbf = Some(NOW + CLOCK_LEEWAY_SECS + 1);
        assert_eq!(check(&c), Err(AuthError::NotYetValid));
        c.nbf = Some(i64::MAX);
        assert_eq!(check(&c), Err(AuthError::NotYetValid));
    }

    #[test]
    fn claims_need_the_owner_email() {
        for email in [
            None,
            Some("".to_string()),
            Some("someone@example.com".into()),
        ] {
            let mut c = claims();
            c.email = email;
            assert_eq!(check(&c), Err(AuthError::NotOwner));
        }
    }

    #[test]
    fn claims_report_the_first_failed_check_in_a_fixed_order() {
        let mut c = claims();
        c.iss = "x".into();
        c.aud = vec![];
        c.exp = 0;
        c.email = None;
        assert_eq!(check(&c), Err(AuthError::WrongIssuer));
        c.iss = claims().iss;
        assert_eq!(check(&c), Err(AuthError::WrongAudience));
        c.aud = claims().aud;
        assert_eq!(check(&c), Err(AuthError::Expired));
        c.exp = claims().exp;
        c.nbf = Some(i64::MAX);
        assert_eq!(check(&c), Err(AuthError::NotYetValid));
        c.nbf = None;
        assert_eq!(check(&c), Err(AuthError::NotOwner));
    }
    #[test]
    fn loopback_hosts_are_exact() {
        for ok in [
            "localhost",
            "localhost:8787",
            "127.0.0.1",
            "127.0.0.1:8787",
            "LOCALHOST:8787",
        ] {
            assert!(is_loopback_host(Some(ok)), "{ok}");
        }
        for bad in [
            "",
            "evil.com",
            "localhost.evil.com",
            "evil.com:8787",
            "localhost:",
            "localhost:abc",
            "localhost:8787@evil.com",
            "user@localhost",
            "127.0.0.1.evil.com",
            "127.0.0.2",
            "[::1]",
            "localhost:8787:1",
            "localhost:0",
            "localhost:00000",
            "localhost:65536",
            "127.0.0.1:99999",
            " localhost",
        ] {
            assert!(!is_loopback_host(Some(bad)), "{bad:?}");
        }
        assert!(!is_loopback_host(None));
    }

    #[test]
    fn local_dev_bypass_needs_flag_no_team_and_loopback() {
        assert!(local_dev_bypass(Some("true"), None, true));
        assert!(local_dev_bypass(Some("true"), Some(""), true));
        assert!(!local_dev_bypass(
            Some("true"),
            Some("t.cloudflareaccess.com"),
            true
        ));
        assert!(!local_dev_bypass(Some("true"), None, false));
        for flag in [
            None,
            Some(""),
            Some("TRUE"),
            Some("True"),
            Some("1"),
            Some("false"),
            Some("true "),
        ] {
            assert!(!local_dev_bypass(flag, None, true), "{flag:?}");
        }
    }
    #[test]
    fn origin_allows_safe_methods_whatever_the_headers() {
        for m in ["GET", "HEAD", "OPTIONS"] {
            assert!(check_origin(
                m,
                Some("https://evil.example"),
                Some("cross-site"),
                Some("a.example")
            ));
        }
    }

    #[test]
    fn origin_allows_a_matching_origin_on_mutating_methods() {
        for m in ["POST", "PUT", "PATCH", "DELETE"] {
            assert!(
                check_origin(
                    m,
                    Some("https://app.example"),
                    Some("same-origin"),
                    Some("app.example")
                ),
                "{m}"
            );
            assert!(
                check_origin(
                    m,
                    Some("http://127.0.0.1:8787"),
                    None,
                    Some("127.0.0.1:8787")
                ),
                "{m}"
            );
            assert!(
                check_origin(m, Some("HTTPS://APP.example"), None, Some("app.EXAMPLE")),
                "{m}"
            );
        }
    }

    #[test]
    fn origin_allows_a_missing_origin() {
        assert!(check_origin("POST", None, None, Some("app.example")));
        assert!(check_origin(
            "POST",
            None,
            Some("same-origin"),
            Some("app.example")
        ));
        assert!(check_origin("POST", None, Some("none"), None));
    }

    #[test]
    fn origin_refuses_cross_site_fetches() {
        assert!(!check_origin(
            "POST",
            None,
            Some("cross-site"),
            Some("app.example")
        ));
        assert!(!check_origin(
            "POST",
            None,
            Some("Cross-Site"),
            Some("app.example")
        ));
        // Even a matching Origin does not rescue a cross-site fetch.
        assert!(!check_origin(
            "POST",
            Some("https://app.example"),
            Some("cross-site"),
            Some("app.example")
        ));
    }

    #[test]
    fn origin_refuses_other_origins_and_odd_values() {
        for origin in [
            "null",
            "",
            "https://evil.example",
            "https://app.example.evil.example",
            "https://evil.example/app.example",
            "https://app.example/",
            "https://app.example:8443",
            "ftp://app.example",
            "app.example",
            "https://app.example@evil.example",
            "https://app.example ",
        ] {
            assert!(
                !check_origin("POST", Some(origin), None, Some("app.example")),
                "{origin:?}"
            );
        }
    }

    #[test]
    fn origin_refuses_an_origin_when_the_host_is_missing() {
        assert!(!check_origin(
            "POST",
            Some("https://app.example"),
            None,
            None
        ));
    }

    #[test]
    fn origin_accepts_http_only_for_a_loopback_host() {
        assert!(check_origin(
            "POST",
            Some("http://localhost:8787"),
            None,
            Some("localhost:8787")
        ));
        assert!(!check_origin(
            "POST",
            Some("http://app.example"),
            None,
            Some("app.example")
        ));
        assert!(!check_origin(
            "PUT",
            Some("http://app.example:8443"),
            None,
            Some("app.example:8443")
        ));
        assert!(check_origin(
            "PUT",
            Some("https://localhost:8787"),
            None,
            Some("localhost:8787")
        ));
    }

    #[test]
    fn modulus_length_ignores_leading_zero_bytes() {
        let mut padded = vec![0u8; 3];
        padded.extend([1u8; 255]);
        assert!(!check_modulus_len(&padded));
        let mut ok = vec![0u8];
        ok.extend([1u8; 256]);
        assert!(check_modulus_len(&ok));
        assert!(!check_modulus_len(&[0u8; 400]));
    }

    #[test]
    fn accept_jwk_takes_a_normal_key() {
        let n = [0xABu8; 256];
        assert!(accept_jwk("RSA", Some("RS256"), "k1", &n, &[1, 0, 1]));
        assert!(accept_jwk("RSA", None, "k1", &n, &[3]));
        assert!(accept_jwk("RSA", None, "k1", &n, &[0, 1, 0, 1]));
    }

    #[test]
    fn accept_jwk_refuses_each_bad_part() {
        let n = [0xABu8; 256];
        let e = [1u8, 0, 1];
        assert!(!accept_jwk("EC", None, "k", &n, &e));
        assert!(!accept_jwk("RSA", Some("HS256"), "k", &n, &e));
        assert!(!accept_jwk("RSA", Some("rs256"), "k", &n, &e));
        assert!(!accept_jwk("RSA", None, "bad kid", &n, &e));
        assert!(!accept_jwk("RSA", None, "", &n, &e));
        assert!(!accept_jwk("RSA", None, "k", &n[..255], &e));
    }

    #[test]
    fn accept_jwk_needs_an_odd_exponent_of_at_least_three() {
        let n = [0xABu8; 256];
        for bad in [
            &[][..],
            &[0],
            &[1],
            &[2],
            &[4],
            &[1, 0, 0],
            &[0, 0, 0],
            &[1; 9],
        ] {
            assert!(!accept_jwk("RSA", None, "k", &n, bad), "{bad:?}");
        }
        assert!(accept_jwk("RSA", None, "k", &n, &[1; 8]));
    }
}
