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

/// Refuses RSA keys under 2048 bits (decoded modulus under 256 bytes).
pub fn check_modulus_len(modulus: &[u8]) -> bool {
    modulus.len() >= MIN_MODULUS_BYTES
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
}
