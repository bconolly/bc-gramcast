//! JSON shapes for the API, and mapping to and from the domain. No logic here.

use domain::app::{AppError, InsertError, PortError, PostChanges, UpdateError};
use domain::auth::{self, AuthError, Claims, JwtHeader};
use domain::media::UploadError;
use domain::queue::{Platform, QueueError, QueuedPost};
use domain::slots::{SlotError, SlotPattern, SlotRule, Weekday};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartUploadBody {
    pub file_name: String,
    pub content_type: String,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuePostBody {
    pub video_key: String,
    pub title: String,
    pub caption: String,
    pub platforms: Vec<String>,
    pub slot_at: i64,
}

/// Body of `PATCH /api/posts/{id}`: every editable field, nothing else. The id
/// and video are not editable, so naming them is refused like any unknown field.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchPostBody {
    pub title: String,
    pub caption: String,
    pub platforms: Vec<String>,
    pub slot_at: i64,
}

/// Returns `None` if any platform name is unknown.
pub fn to_changes(body: PatchPostBody) -> Option<PostChanges> {
    let platforms = body
        .platforms
        .iter()
        .map(|p| platform_from_str(p))
        .collect::<Option<Vec<_>>>()?;
    Some(PostChanges {
        title: body.title,
        caption: body.caption,
        platforms,
        slot_at: body.slot_at,
    })
}

/// Logged when a post was deleted but its video object could not be removed.
pub const VIDEO_LEFT_BEHIND_LINE: &str = "post deleted; video object not removed";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PostJson {
    pub id: String,
    pub video_key: String,
    pub title: String,
    pub caption: String,
    pub platforms: Vec<String>,
    pub slot_at: i64,
    pub status: String,
}

pub fn platform_from_str(s: &str) -> Option<Platform> {
    match s {
        "instagram" => Some(Platform::Instagram),
        "youtube" => Some(Platform::YouTube),
        _ => None,
    }
}

pub fn platform_to_str(p: Platform) -> &'static str {
    match p {
        Platform::Instagram => "instagram",
        Platform::YouTube => "youtube",
    }
}

/// Returns `None` if any platform name is unknown.
pub fn to_domain(id: String, body: QueuePostBody) -> Option<QueuedPost> {
    let platforms = body
        .platforms
        .iter()
        .map(|p| platform_from_str(p))
        .collect::<Option<Vec<_>>>()?;
    Some(QueuedPost {
        id,
        video_key: body.video_key,
        title: body.title,
        caption: body.caption,
        platforms,
        slot_at: body.slot_at,
    })
}

pub fn to_json(p: &QueuedPost) -> PostJson {
    PostJson {
        id: p.id.clone(),
        video_key: p.video_key.clone(),
        title: p.title.clone(),
        caption: p.caption.clone(),
        platforms: p
            .platforms
            .iter()
            .map(|x| platform_to_str(*x).into())
            .collect(),
        slot_at: p.slot_at,
        status: match p.status() {
            domain::queue::PostStatus::Ready => "ready".into(),
            domain::queue::PostStatus::NeedsCaption => "needs_caption".into(),
        },
    }
}

pub const DEFAULT_NEXT_COUNT: usize = 5;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuleJson {
    pub weekday: String,
    pub time: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PatternJson {
    pub utc_offset_minutes: i32,
    pub rules: Vec<RuleJson>,
}

/// Splits the body into what `app::set_pattern` takes. An unknown weekday or a
/// badly formed time is `BadTime`, and more than `MAX_RULES` rules is
/// `TooManyRules`, checked first.
pub fn pattern_from_json(body: PatternJson) -> Result<(i32, Vec<SlotRule>), SlotError> {
    // Refuse an oversized list before doing any work per rule.
    if body.rules.len() > domain::slots::MAX_RULES {
        return Err(SlotError::TooManyRules);
    }
    let rules = body
        .rules
        .iter()
        .map(|r| {
            Ok(SlotRule {
                weekday: Weekday::parse(&r.weekday).ok_or(SlotError::BadTime)?,
                minute_of_day: domain::slots::parse_hhmm(&r.time)?,
            })
        })
        .collect::<Result<Vec<_>, SlotError>>()?;
    Ok((body.utc_offset_minutes, rules))
}

pub fn pattern_to_json(p: &SlotPattern) -> PatternJson {
    PatternJson {
        utc_offset_minutes: p.utc_offset_minutes,
        rules: p
            .rules
            .iter()
            .map(|r| RuleJson {
                weekday: r.weekday.as_str().into(),
                time: domain::slots::format_hhmm(r.minute_of_day),
            })
            .collect(),
    }
}

/// Reads `count` from a raw query string: absent means 5, otherwise plain
/// ASCII digits in 1..=20. Anything else (repeated, empty, signed, encoded,
/// non-numeric) is `None`. Never echoed back.
pub fn parse_next_count(query: Option<&str>) -> Option<usize> {
    let mut values = query
        .unwrap_or("")
        .split('&')
        // A bare `count` with no `=` is a present-but-empty value, so it is refused.
        .filter_map(|pair| {
            if pair == "count" {
                Some("")
            } else {
                pair.strip_prefix("count=")
            }
        });
    let Some(value) = values.next() else {
        return Some(DEFAULT_NEXT_COUNT);
    };
    if values.next().is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=domain::slots::MAX_NEXT_SLOTS).contains(n))
}

/// Maps the text of a D1 failure to an `InsertError`. UNIQUE violations on the
/// slot or the video become their own cases; anything else stays a port error
/// (which the API never shows to the client).
pub fn classify_insert_error(text: &str) -> InsertError {
    if text.contains("UNIQUE constraint failed: posts.slot_at") {
        InsertError::SlotTaken
    } else if text.contains("UNIQUE constraint failed: posts.video_key") {
        InsertError::VideoAlreadyQueued
    } else {
        InsertError::Port(PortError(text.to_string()))
    }
}

/// Maps the text of a D1 failure on update. Only the slot UNIQUE violation is
/// special; anything else stays a port error the API never shows.
pub fn classify_update_error(text: &str) -> UpdateError {
    if text.contains("UNIQUE constraint failed: posts.slot_at") {
        UpdateError::SlotTaken
    } else {
        UpdateError::Port(PortError(text.to_string()))
    }
}

/// CSRF guard: a cross-site form cannot send `application/json`, so POSTs must.
pub fn is_json_content_type(header: Option<&str>) -> bool {
    let Some(value) = header else {
        return false;
    };
    let media_type = value.split(';').next().unwrap_or("").trim();
    media_type.eq_ignore_ascii_case("application/json")
}

/// HTTP status and a fixed message per error. Never built from Debug output or
/// client-sent values, and port errors never leak their detail.
pub fn error_status(e: &AppError) -> (u16, &'static str) {
    match e {
        AppError::NotFound => (404, "post not found"),
        AppError::VideoNotUploaded => (422, "video has not been uploaded"),
        AppError::Upload(u) => (
            422,
            match u {
                UploadError::NotMp4 => "file must be an MP4",
                UploadError::Empty => "file is empty",
                UploadError::TooLarge { .. } => "file is too large",
                UploadError::BadFileName => "file name is not allowed",
                UploadError::BadId => "upload id is not valid",
            },
        ),
        AppError::Queue(q) => match q {
            QueueError::SlotTaken { .. } => (409, "that slot is already taken"),
            QueueError::VideoAlreadyQueued => (409, "that video is already queued"),
            QueueError::NoPlatforms => (422, "choose at least one platform"),
            QueueError::EmptyTitle => (422, "title is required"),
            QueueError::BadVideoKey => (422, "video key is not valid"),
            QueueError::TooLong => (422, "title or caption is too long"),
            QueueError::DuplicatePlatform => (422, "each platform can be chosen once"),
            QueueError::SlotInPast => (422, "slot must be in the future"),
            QueueError::SlotOutOfRange => (422, "slot is out of range"),
        },
        AppError::Slots(s) => (
            422,
            match s {
                SlotError::OffsetOutOfRange => "UTC offset is out of range",
                SlotError::TooManyRules => "too many slot rules",
                SlotError::DuplicateRule => "each slot can be set once",
                SlotError::BadTime => "weekday or time is not valid",
            },
        ),
        AppError::Port(_) => (500, "storage error"),
    }
}

/// A token split into what the domain checks, plus the bytes the signature covers.
#[derive(Debug, PartialEq, Eq)]
pub struct TokenParts {
    pub header: JwtHeader,
    pub claims: Claims,
    /// `header.payload` exactly as sent, which is what the signature covers.
    pub signing_input: Vec<u8>,
    pub signature: Vec<u8>,
}

/// Splits and decodes a token. Nothing is trusted yet: the signature is checked
/// later, by the domain. Every failure is `Malformed`.
pub fn parse_token(token: &str) -> Result<TokenParts, AuthError> {
    let (h, p, sig) = domain::auth::split_token(token)?;
    let header: HeaderWire = decode_object(h)?;
    let payload: ClaimsWire = decode_object(p)?;
    let signature = domain::auth::decode_b64url(sig).ok_or(AuthError::Malformed)?;
    // `h` and `p` are slices of `token` that run up to the last dot.
    let signing_input = token.as_bytes()[..h.len() + 1 + p.len()].to_vec();
    Ok(TokenParts {
        header: JwtHeader {
            alg: header.alg,
            kid: header.kid,
        },
        claims: Claims {
            iss: payload.iss,
            aud: match payload.aud {
                Audience::One(a) => vec![a],
                Audience::Many(a) => a,
            },
            exp: payload.exp,
            nbf: payload.nbf,
            email: payload.email,
        },
        signing_input,
        signature,
    })
}

#[derive(Deserialize)]
struct HeaderWire {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

/// Unknown fields are ignored on purpose: real Access tokens carry many.
#[derive(Deserialize)]
struct ClaimsWire {
    iss: String,
    aud: Audience,
    exp: i64,
    nbf: Option<i64>,
    email: Option<String>,
}

/// Decodes one base64url part as a JSON object. Going through `Value` stops
/// serde reading a JSON array as a struct.
fn decode_object<T: serde::de::DeserializeOwned>(part: &str) -> Result<T, AuthError> {
    let bytes = domain::auth::decode_b64url(part).ok_or(AuthError::Malformed)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| AuthError::Malformed)?;
    if !value.is_object() {
        return Err(AuthError::Malformed);
    }
    serde_json::from_value(value).map_err(|_| AuthError::Malformed)
}

/// HTTP status and a fixed message per sign-in error. Every token problem shares
/// one message, so a response never says which check failed.
pub fn auth_error_status(e: &AuthError) -> (u16, &'static str) {
    match e {
        AuthError::NotOwner => (403, "not allowed"),
        AuthError::NotConfigured => (503, "sign-in not configured yet"),
        AuthError::KeysUnavailable => (503, "sign-in unavailable"),
        AuthError::MissingToken
        | AuthError::Malformed
        | AuthError::UnsupportedAlg
        | AuthError::BadKeyId
        | AuthError::UnknownKey
        | AuthError::BadSignature
        | AuthError::WrongIssuer
        | AuthError::WrongAudience
        | AuthError::Expired
        | AuthError::NotYetValid => (401, "sign-in required"),
    }
}

/// A fixed code for the log line when sign-in is refused. It names the check
/// that failed and nothing from the request: never the token, claims or email.
pub fn auth_reason_code(e: &AuthError) -> &'static str {
    match e {
        AuthError::MissingToken => "missing_token",
        AuthError::Malformed => "malformed",
        AuthError::UnsupportedAlg => "unsupported_alg",
        AuthError::BadKeyId => "bad_key_id",
        AuthError::UnknownKey => "unknown_key",
        AuthError::BadSignature => "bad_signature",
        AuthError::WrongIssuer => "wrong_issuer",
        AuthError::WrongAudience => "wrong_audience",
        AuthError::Expired => "expired",
        AuthError::NotYetValid => "not_yet_valid",
        AuthError::NotOwner => "not_owner",
        AuthError::NotConfigured => "not_configured",
        AuthError::KeysUnavailable => "keys_unavailable",
    }
}

/// Log line for a failed key refresh. `reason` is one of the fixed `PortError`
/// texts from `AccessVerifier::refresh`: never a response body, token, email or URL.
pub fn key_refresh_failed_line(reason: &str) -> String {
    format!("key refresh failed: {reason}")
}

/// Log line for the key import step: counts only, never key material or kid.
pub fn key_import_counts_line(parsed: usize, imported: usize) -> String {
    format!("key import: parsed={parsed} imported={imported}")
}

/// Headers added to every response the Worker returns, denials included.
pub const API_SECURITY_HEADERS: [(&str, &str); 4] = [
    ("X-Content-Type-Options", "nosniff"),
    ("Cache-Control", "no-store"),
    ("Referrer-Policy", "no-referrer"),
    (
        "Content-Security-Policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
];

/// Largest key-set response we will read, in bytes.
pub const MAX_JWKS_BYTES: usize = 64 * 1024;
/// Most keys a key set may list. Access publishes two at a time.
pub const MAX_JWKS_KEYS: usize = 10;

/// Whether a key-set response status is acceptable. Only 200 is: the key
/// endpoint never redirects, so any 3xx (seen because redirects are not
/// followed) is refused along with every other status.
pub fn jwks_status_ok(status: u16) -> bool {
    status == 200
}

/// Whether a key-set response may be read, judging by its `Content-Length`
/// header alone. No header is fine (the size is checked again once read); an
/// unreadable or too-large one is not.
pub fn jwks_length_ok(content_length: Option<&str>) -> bool {
    content_length.is_none_or(|v| {
        v.bytes().all(|b| b.is_ascii_digit())
            && v.parse::<usize>().is_ok_and(|n| n <= MAX_JWKS_BYTES)
    })
}

/// An RSA public key from the key set, ready for Web Crypto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jwk {
    pub kid: String,
    /// base64url modulus (at least 2048 bits).
    pub n: String,
    /// base64url exponent.
    pub e: String,
}

/// The key set could not be used at all.
#[derive(Debug, PartialEq, Eq)]
pub struct JwksError;

#[derive(Deserialize)]
struct JwkWire {
    kty: String,
    kid: Option<String>,
    n: Option<String>,
    e: Option<String>,
    alg: Option<String>,
}

/// Reads the key set from `/cdn-cgi/access/certs`. Keys that are not usable RSA
/// signing keys are skipped one by one; the whole set is refused when it is too
/// big, too long, not the expected shape, or has no usable key.
pub fn parse_jwks(body: &str) -> Result<Vec<Jwk>, JwksError> {
    if body.len() > MAX_JWKS_BYTES {
        return Err(JwksError);
    }
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| JwksError)?;
    let listed = value
        .get("keys")
        .and_then(serde_json::Value::as_array)
        .filter(|k| k.len() <= MAX_JWKS_KEYS)
        .ok_or(JwksError)?;
    let mut keys: Vec<Jwk> = Vec::new();
    for item in listed {
        let Some(jwk) = usable_key(item) else {
            continue;
        };
        if !keys.iter().any(|k| k.kid == jwk.kid) {
            keys.push(jwk);
        }
    }
    if keys.is_empty() {
        Err(JwksError)
    } else {
        Ok(keys)
    }
}

fn usable_key(item: &serde_json::Value) -> Option<Jwk> {
    let wire: JwkWire = serde_json::from_value(item.clone()).ok()?;
    let (kid, n, e) = (wire.kid?, wire.n?, wire.e?);
    let n_bytes = auth::decode_b64url(&n)?;
    let e_bytes = auth::decode_b64url(&e)?;
    auth::accept_jwk(&wire.kty, wire.alg.as_deref(), &kid, &n_bytes, &e_bytes).then_some(Jwk {
        kid,
        n,
        e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::app::{InsertError, PortError};
    use domain::media::UploadError;

    fn body() -> QueuePostBody {
        QueuePostBody {
            video_key: "videos/a.mp4".into(),
            title: "T".into(),
            caption: "C".into(),
            platforms: vec!["instagram".into(), "youtube".into()],
            slot_at: 100,
        }
    }

    #[test]
    fn maps_a_body_to_the_domain_and_back() {
        let post = to_domain("id1".into(), body()).unwrap();
        let json = to_json(&post);
        assert_eq!(json.platforms, ["instagram", "youtube"]);
        assert_eq!(json.status, "ready");
    }

    #[test]
    fn unknown_platform_is_rejected() {
        let mut b = body();
        b.platforms = vec!["tiktok".into()];
        assert!(to_domain("id".into(), b).is_none());
    }

    #[test]
    fn unknown_json_fields_are_rejected() {
        let r: Result<StartUploadBody, _> = serde_json::from_str(
            r#"{"file_name":"a.mp4","content_type":"video/mp4","size_bytes":1,"admin":true}"#,
        );
        assert!(r.is_err());
    }

    fn patch_json() -> &'static str {
        r#"{"title":"T2","caption":"C2","platforms":["youtube"],"slot_at":500}"#
    }

    #[test]
    fn parses_a_patch_body_into_domain_changes() {
        let b: PatchPostBody = serde_json::from_str(patch_json()).unwrap();
        let c = to_changes(b).unwrap();
        assert_eq!(c.title, "T2");
        assert_eq!(c.platforms, [Platform::YouTube]);
        assert_eq!(c.slot_at, 500);
    }

    #[test]
    fn a_patch_body_cannot_name_the_immutable_fields_or_extras() {
        for extra in ["video_key", "id", "admin"] {
            let text = patch_json().replace('}', &format!(r#","{extra}":"x"}}"#));
            assert!(
                serde_json::from_str::<PatchPostBody>(&text).is_err(),
                "{extra}"
            );
        }
    }

    #[test]
    fn a_patch_body_must_have_every_editable_field() {
        let r = serde_json::from_str::<PatchPostBody>(r#"{"title":"T"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn a_patch_with_an_unknown_platform_is_rejected() {
        let mut b: PatchPostBody = serde_json::from_str(patch_json()).unwrap();
        b.platforms = vec!["tiktok".into()];
        assert!(to_changes(b).is_none());
    }

    #[test]
    fn classifies_update_errors_by_the_unique_slot_message() {
        assert_eq!(
            classify_update_error("UNIQUE constraint failed: posts.slot_at | D1_ERROR"),
            UpdateError::SlotTaken
        );
        let other = classify_update_error("secret detail");
        assert_eq!(other, UpdateError::Port(PortError("secret detail".into())));
    }

    #[test]
    fn the_leftover_video_log_line_is_fixed() {
        assert_eq!(
            VIDEO_LEFT_BEHIND_LINE,
            "post deleted; video object not removed"
        );
    }

    #[test]
    fn a_missing_post_is_a_fixed_404() {
        assert_eq!(error_status(&AppError::NotFound), (404, "post not found"));
    }

    #[test]
    fn port_errors_do_not_leak_detail() {
        let (code, msg) = error_status(&AppError::Port(PortError("D1: secret table".into())));
        assert_eq!((code, msg), (500, "storage error"));
    }

    #[test]
    fn every_error_has_a_fixed_message_and_status() {
        let cases = [
            (
                AppError::Upload(UploadError::NotMp4),
                422,
                "file must be an MP4",
            ),
            (AppError::Upload(UploadError::Empty), 422, "file is empty"),
            (
                AppError::VideoNotUploaded,
                422,
                "video has not been uploaded",
            ),
            (
                AppError::Upload(UploadError::TooLarge {
                    size_bytes: 123_456_789,
                }),
                422,
                "file is too large",
            ),
            (
                AppError::Upload(UploadError::BadFileName),
                422,
                "file name is not allowed",
            ),
            (
                AppError::Upload(UploadError::BadId),
                422,
                "upload id is not valid",
            ),
            (
                AppError::Queue(QueueError::NoPlatforms),
                422,
                "choose at least one platform",
            ),
            (
                AppError::Queue(QueueError::EmptyTitle),
                422,
                "title is required",
            ),
            (
                AppError::Queue(QueueError::SlotTaken { slot_at: 777 }),
                409,
                "that slot is already taken",
            ),
            (
                AppError::Queue(QueueError::VideoAlreadyQueued),
                409,
                "that video is already queued",
            ),
            (
                AppError::Queue(QueueError::BadVideoKey),
                422,
                "video key is not valid",
            ),
            (
                AppError::Queue(QueueError::TooLong),
                422,
                "title or caption is too long",
            ),
            (
                AppError::Queue(QueueError::DuplicatePlatform),
                422,
                "each platform can be chosen once",
            ),
            (
                AppError::Queue(QueueError::SlotInPast),
                422,
                "slot must be in the future",
            ),
            (
                AppError::Queue(QueueError::SlotOutOfRange),
                422,
                "slot is out of range",
            ),
            (
                AppError::Slots(SlotError::OffsetOutOfRange),
                422,
                "UTC offset is out of range",
            ),
            (
                AppError::Slots(SlotError::TooManyRules),
                422,
                "too many slot rules",
            ),
            (
                AppError::Slots(SlotError::DuplicateRule),
                422,
                "each slot can be set once",
            ),
            (
                AppError::Slots(SlotError::BadTime),
                422,
                "weekday or time is not valid",
            ),
        ];
        for (err, code, msg) in cases {
            assert_eq!(error_status(&err), (code, msg), "{err:?}");
        }
    }

    #[test]
    fn classifies_d1_unique_violations_by_column() {
        assert_eq!(
            classify_insert_error(
                "D1_ERROR: UNIQUE constraint failed: posts.slot_at: SQLITE_CONSTRAINT"
            ),
            InsertError::SlotTaken
        );
        assert_eq!(
            classify_insert_error("UNIQUE constraint failed: posts.video_key"),
            InsertError::VideoAlreadyQueued
        );
        assert_eq!(
            classify_insert_error("network down"),
            InsertError::Port(PortError("network down".into()))
        );
    }

    #[test]
    fn only_json_content_types_pass_the_csrf_check() {
        assert!(is_json_content_type(Some("application/json")));
        assert!(is_json_content_type(Some(
            "Application/JSON; charset=utf-8"
        )));
        assert!(!is_json_content_type(None));
        assert!(!is_json_content_type(Some("text/plain")));
        assert!(!is_json_content_type(Some(
            "application/x-www-form-urlencoded"
        )));
        assert!(!is_json_content_type(Some(
            "multipart/form-data; boundary=x"
        )));
        assert!(!is_json_content_type(Some("application/jsonp")));
        assert!(!is_json_content_type(Some("")));
    }

    fn rule_json(weekday: &str, time: &str) -> RuleJson {
        RuleJson {
            weekday: weekday.into(),
            time: time.into(),
        }
    }

    #[test]
    fn pattern_json_maps_to_domain_and_back() {
        let body = PatternJson {
            utc_offset_minutes: 600,
            rules: vec![rule_json("wed", "19:00"), rule_json("mon", "06:05")],
        };
        let (offset, rules) = pattern_from_json(body).unwrap();
        let pattern = SlotPattern::new(offset, rules).unwrap();
        assert_eq!(
            pattern_to_json(&pattern),
            PatternJson {
                utc_offset_minutes: 600,
                rules: vec![rule_json("mon", "06:05"), rule_json("wed", "19:00")],
            }
        );
    }

    #[test]
    fn pattern_json_rejects_bad_weekdays_and_times() {
        for (day, time) in [
            ("Mon", "06:00"),
            ("monday", "06:00"),
            ("mon", "6:00"),
            ("mon", "24:00"),
            ("mon", ""),
        ] {
            let body = PatternJson {
                utc_offset_minutes: 0,
                rules: vec![rule_json(day, time)],
            };
            assert_eq!(
                pattern_from_json(body),
                Err(SlotError::BadTime),
                "{day} {time}"
            );
        }
    }

    #[test]
    fn pattern_json_rejects_too_many_rules_before_mapping_them() {
        // The rules are not valid weekdays: if they were mapped first the error
        // would be BadTime, so TooManyRules proves the count is checked first.
        let rules = (0..=domain::slots::MAX_RULES)
            .map(|_| rule_json("nope", "zz"))
            .collect();
        let body = PatternJson {
            utc_offset_minutes: 0,
            rules,
        };
        assert_eq!(pattern_from_json(body), Err(SlotError::TooManyRules));
        let at_limit = PatternJson {
            utc_offset_minutes: 0,
            rules: (0..domain::slots::MAX_RULES)
                .map(|_| rule_json("mon", "06:00"))
                .collect(),
        };
        assert!(pattern_from_json(at_limit).is_ok());
    }

    #[test]
    fn pattern_json_rejects_unknown_fields() {
        let r: Result<PatternJson, _> =
            serde_json::from_str(r#"{"utc_offset_minutes":0,"rules":[],"x":1}"#);
        assert!(r.is_err());
        let r: Result<PatternJson, _> = serde_json::from_str(
            r#"{"utc_offset_minutes":0,"rules":[{"weekday":"mon","time":"06:00","x":1}]}"#,
        );
        assert!(r.is_err());
    }

    #[test]
    fn the_default_pattern_is_brisbane_with_no_rules() {
        let json = serde_json::to_string(&pattern_to_json(&SlotPattern::default())).unwrap();
        assert_eq!(json, r#"{"utc_offset_minutes":600,"rules":[]}"#);
    }

    #[test]
    fn next_count_is_a_plain_integer_from_one_to_twenty() {
        assert_eq!(parse_next_count(None), Some(5));
        assert_eq!(parse_next_count(Some("")), Some(5));
        assert_eq!(parse_next_count(Some("other=1")), Some(5));
        assert_eq!(parse_next_count(Some("count=1")), Some(1));
        assert_eq!(parse_next_count(Some("count=20")), Some(20));
        assert_eq!(parse_next_count(Some("x=1&count=7")), Some(7));
        for bad in [
            "count=0",
            "count=21",
            "count=",
            "count",
            "x=1&count",
            "count&count=2",
            "count=-1",
            "count=+3",
            "count=3.0",
            "count=abc",
            "count=%33",
            "count=1&count=2",
            "count=99999999999999999999",
            "count= 3",
            "count=003x",
        ] {
            assert_eq!(parse_next_count(Some(bad)), None, "{bad:?}");
        }
    }

    fn b64(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n =
                chunk.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
            for i in 0..=chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    /// A synthetic payload shaped like a real Access token: many extra fields.
    fn payload(aud: &str, extra: &str) -> String {
        format!(
            r#"{{"aud":{aud},"email":"owner@example.com","exp":1800000600,"iat":1800000000,"nbf":1799999995,"iss":"https://team-x.cloudflareaccess.com","type":"app","identity_nonce":"n0nce","sub":"00000000-0000-0000-0000-000000000000","country":"AU"{extra}}}"#
        )
    }

    fn token(header: &str, payload: &str) -> String {
        format!(
            "{}.{}.{}",
            b64(header.as_bytes()),
            b64(payload.as_bytes()),
            b64(b"sig-bytes")
        )
    }

    const HDR: &str = r#"{"alg":"RS256","kid":"key1","typ":"JWT"}"#;

    #[test]
    fn parse_token_reads_a_realistic_access_token() {
        let t = token(HDR, &payload(r#"["abc123"]"#, ""));
        let parts = parse_token(&t).unwrap();
        assert_eq!(
            parts.header,
            JwtHeader {
                alg: "RS256".into(),
                kid: "key1".into()
            }
        );
        assert_eq!(
            parts.claims,
            Claims {
                iss: "https://team-x.cloudflareaccess.com".into(),
                aud: vec!["abc123".into()],
                exp: 1_800_000_600,
                nbf: Some(1_799_999_995),
                email: Some("owner@example.com".into()),
            }
        );
        assert_eq!(parts.signature, b"sig-bytes");
        let cut = t.rfind('.').unwrap();
        assert_eq!(parts.signing_input, t.as_bytes()[..cut]);
    }

    #[test]
    fn parse_token_accepts_aud_as_a_string_or_an_array() {
        let one = parse_token(&token(HDR, &payload(r#""abc123""#, ""))).unwrap();
        assert_eq!(one.claims.aud, vec!["abc123".to_string()]);
        let two = parse_token(&token(HDR, &payload(r#"["a","b"]"#, ""))).unwrap();
        assert_eq!(two.claims.aud, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_token_allows_a_missing_nbf_and_email() {
        let p = r#"{"aud":["a"],"iss":"i","exp":5}"#;
        let parts = parse_token(&token(HDR, p)).unwrap();
        assert_eq!((parts.claims.nbf, parts.claims.email), (None, None));
    }

    #[test]
    fn parse_token_refuses_bad_shapes_as_malformed() {
        let good_p = payload(r#"["a"]"#, "");
        let bad_payloads = [
            r#"{"aud":["a"],"iss":"i"}"#.to_string(),
            r#"{"aud":["a"],"iss":"i","exp":"5"}"#.to_string(),
            r#"{"aud":["a"],"iss":"i","exp":5.5}"#.to_string(),
            r#"{"aud":["a"],"iss":"i","exp":5,"nbf":"1"}"#.to_string(),
            r#"{"aud":["a"],"iss":"i","exp":5,"email":7}"#.to_string(),
            r#"{"aud":[1],"iss":"i","exp":5}"#.to_string(),
            r#"{"aud":7,"iss":"i","exp":5}"#.to_string(),
            r#"{"aud":["a"],"exp":5}"#.to_string(),
            r#"{"iss":"i","exp":5}"#.to_string(),
            r#"["not","an","object"]"#.to_string(),
            r#"[["a"],"i",5]"#.to_string(),
            "null".to_string(),
            "not json".to_string(),
        ];
        for p in &bad_payloads {
            assert_eq!(
                parse_token(&token(HDR, p)),
                Err(AuthError::Malformed),
                "{p}"
            );
        }
        for h in [
            r#"{"kid":"k"}"#,
            r#"{"alg":"RS256"}"#,
            r#"{"alg":5,"kid":"k"}"#,
            r#"{"alg":"RS256","kid":5}"#,
            r#"["RS256","k"]"#,
            "nope",
        ] {
            assert_eq!(
                parse_token(&token(h, &good_p)),
                Err(AuthError::Malformed),
                "{h}"
            );
        }
    }

    #[test]
    fn parse_token_refuses_bad_encoding_and_structure() {
        let good = token(HDR, &payload(r#"["a"]"#, ""));
        let (h, rest) = good.split_once('.').unwrap();
        let (p, _) = rest.split_once('.').unwrap();
        for bad in [
            String::new(),
            "a.b".to_string(),
            format!("{h}.{p}"),
            format!("{h}.{p}.***"),
            format!("{h}.{p}.Zg=="),
            format!("{h}.{p}.{}", "A".repeat(9000)),
            format!("{h}!.{p}.c2ln"),
            format!("{h}.{p}.c2ln.extra"),
        ] {
            assert_eq!(parse_token(&bad), Err(AuthError::Malformed), "{bad:.40}");
        }
    }

    #[test]
    fn auth_errors_map_to_fixed_statuses_and_messages() {
        let token_problems = [
            AuthError::MissingToken,
            AuthError::Malformed,
            AuthError::UnsupportedAlg,
            AuthError::BadKeyId,
            AuthError::UnknownKey,
            AuthError::BadSignature,
            AuthError::WrongIssuer,
            AuthError::WrongAudience,
            AuthError::Expired,
            AuthError::NotYetValid,
        ];
        for e in token_problems {
            assert_eq!(auth_error_status(&e), (401, "sign-in required"), "{e:?}");
        }
        assert_eq!(
            auth_error_status(&AuthError::NotOwner),
            (403, "not allowed")
        );
        assert_eq!(
            auth_error_status(&AuthError::NotConfigured),
            (503, "sign-in not configured yet")
        );
        assert_eq!(
            auth_error_status(&AuthError::KeysUnavailable),
            (503, "sign-in unavailable")
        );
    }

    #[test]
    fn security_headers_are_the_expected_set() {
        let find = |n: &str| {
            API_SECURITY_HEADERS
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| *v)
        };
        assert_eq!(find("X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(find("Cache-Control"), Some("no-store"));
        assert_eq!(find("Referrer-Policy"), Some("no-referrer"));
        assert_eq!(
            find("Content-Security-Policy"),
            Some("default-src 'none'; frame-ancestors 'none'")
        );
        assert_eq!(API_SECURITY_HEADERS.len(), 4);
    }

    fn key_json(kid: &str, n_bytes: usize, extra: &str) -> String {
        format!(
            r#"{{"kid":"{kid}","kty":"RSA","alg":"RS256","use":"sig","e":"AQAB","n":"{}"{extra}}}"#,
            b64(&vec![0xAB; n_bytes])
        )
    }

    fn jwks(keys: &[String]) -> String {
        format!(
            r#"{{"keys":[{}],"public_cert":{{"kid":"x","cert":"-----BEGIN CERTIFICATE-----"}},"public_certs":[]}}"#,
            keys.join(",")
        )
    }

    #[test]
    fn parse_jwks_reads_a_realistic_key_set() {
        let body = jwks(&[key_json("k1", 256, ""), key_json("k2", 384, "")]);
        let keys = parse_jwks(&body).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].kid, "k1");
        assert_eq!(keys[0].e, "AQAB");
        assert_eq!(keys[0].n, b64(&[0xAB; 256]));
        assert_eq!(keys[1].kid, "k2");
    }

    #[test]
    fn parse_jwks_skips_each_unusable_key_and_keeps_the_good_one() {
        let bad = [
            key_json("short", 255, ""),
            key_json("bad kid", 256, ""),
            key_json("hs", 256, "").replace("RS256", "HS256"),
            key_json("ec", 256, "").replace("RSA", "EC"),
            key_json("nokid", 256, "").replace(r#""kid":"nokid","#, ""),
            key_json("badn", 256, "").replace(&b64(&[0xAB; 256]), "***"),
            key_json("bade", 256, "").replace("AQAB", "A+B="),
            key_json("nile", 256, "").replace(r#""e":"AQAB""#, r#""e":"""#),
            key_json("bige", 256, "").replace("AQAB", "AQABAQABAQAB"),
            "7".to_string(),
            "null".to_string(),
        ];
        for b in bad {
            let keys = parse_jwks(&jwks(&[b.clone(), key_json("good", 256, "")])).unwrap();
            assert_eq!(keys.len(), 1, "{b}");
            assert_eq!(keys[0].kid, "good");
        }
    }

    #[test]
    fn parse_jwks_accepts_a_key_without_alg_and_keeps_the_first_of_a_repeated_kid() {
        let no_alg = key_json("k", 256, "").replace(r#""alg":"RS256","#, "");
        let other = key_json("k", 300, "");
        let keys = parse_jwks(&jwks(&[no_alg, other])).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].n, b64(&[0xAB; 256]));
    }

    #[test]
    fn parse_jwks_refuses_a_set_with_no_usable_key() {
        assert_eq!(
            parse_jwks(&jwks(&[key_json("short", 128, "")])),
            Err(JwksError)
        );
        assert_eq!(parse_jwks(&jwks(&[])), Err(JwksError));
    }

    #[test]
    fn parse_jwks_refuses_wrong_shapes() {
        for body in [
            "",
            "null",
            "[]",
            r#"{"keys":7}"#,
            r#"{"keys":{}}"#,
            r#"{"public_cert":{}}"#,
            "not json",
        ] {
            assert_eq!(parse_jwks(body), Err(JwksError), "{body}");
        }
    }

    #[test]
    fn parse_jwks_caps_the_key_count_and_the_body_size() {
        let ten: Vec<String> = (0..MAX_JWKS_KEYS)
            .map(|i| key_json(&format!("k{i}"), 256, ""))
            .collect();
        assert_eq!(parse_jwks(&jwks(&ten)).unwrap().len(), MAX_JWKS_KEYS);
        let eleven: Vec<String> = (0..=MAX_JWKS_KEYS)
            .map(|i| key_json(&format!("k{i}"), 256, ""))
            .collect();
        assert_eq!(parse_jwks(&jwks(&eleven)), Err(JwksError));
        let padded = format!(
            "{}{}",
            jwks(&[key_json("k", 256, "")]),
            " ".repeat(MAX_JWKS_BYTES)
        );
        assert_eq!(parse_jwks(&padded), Err(JwksError));
    }

    #[test]
    fn reason_codes_are_fixed_and_distinct() {
        let all = [
            AuthError::MissingToken,
            AuthError::Malformed,
            AuthError::UnsupportedAlg,
            AuthError::BadKeyId,
            AuthError::UnknownKey,
            AuthError::BadSignature,
            AuthError::WrongIssuer,
            AuthError::WrongAudience,
            AuthError::Expired,
            AuthError::NotYetValid,
            AuthError::NotOwner,
            AuthError::NotConfigured,
            AuthError::KeysUnavailable,
        ];
        let codes: Vec<&str> = all.iter().map(auth_reason_code).collect();
        assert_eq!(codes[0], "missing_token");
        assert_eq!(codes[10], "not_owner");
        let mut unique = codes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), all.len());
        assert!(codes
            .iter()
            .all(|c| c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')));
    }

    #[test]
    fn key_refresh_log_lines_are_fixed_text_and_numbers() {
        assert_eq!(
            key_refresh_failed_line("key fetch status"),
            "key refresh failed: key fetch status"
        );
        assert_eq!(
            key_import_counts_line(3, 0),
            "key import: parsed=3 imported=0"
        );
    }

    #[test]
    fn only_a_200_key_response_is_accepted() {
        assert!(jwks_status_ok(200));
        for refused in [204, 301, 302, 303, 307, 308, 404, 500] {
            assert!(!jwks_status_ok(refused), "{refused}");
        }
    }

    #[test]
    fn jwks_content_length_is_checked_early() {
        assert!(jwks_length_ok(None));
        assert!(jwks_length_ok(Some("0")));
        assert!(jwks_length_ok(Some("65536")));
        assert!(!jwks_length_ok(Some("65537")));
        for bad in [
            "",
            "-1",
            "abc",
            "1e3",
            " 10",
            "+5",
            "99999999999999999999999",
        ] {
            assert!(!jwks_length_ok(Some(bad)), "{bad:?}");
        }
    }
}
