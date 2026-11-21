//! JSON shapes for the API, and mapping to and from the domain. No logic here.

use domain::app::{AppError, InsertError, PortError};
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
/// badly formed time is `BadTime`.
pub fn pattern_from_json(body: PatternJson) -> Result<(i32, Vec<SlotRule>), SlotError> {
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

/// DNS-rebinding guard for local dev: only a Host of exactly `localhost` or
/// `127.0.0.1`, with an optional port (1 to 65535), may use the no-auth mode.
pub fn is_local_host(host_header: Option<&str>) -> bool {
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
    fn local_dev_only_answers_to_loopback_hosts() {
        for ok in [
            "localhost",
            "localhost:8787",
            "127.0.0.1",
            "127.0.0.1:8787",
            "LOCALHOST:8787",
        ] {
            assert!(is_local_host(Some(ok)), "{ok}");
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
            assert!(!is_local_host(Some(bad)), "{bad:?}");
        }
        assert!(!is_local_host(None));
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
}
