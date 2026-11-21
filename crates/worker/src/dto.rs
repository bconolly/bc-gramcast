//! JSON shapes for the API, and mapping to and from the domain. No logic here.

use domain::app::AppError;
use domain::queue::{Platform, QueueError, QueuedPost};
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

/// True when D1's error text says the slot's UNIQUE constraint failed.
pub fn is_slot_conflict(text: &str) -> bool {
    text.contains("UNIQUE constraint failed: posts.slot_at")
}

/// HTTP status and a safe message. Port errors never leak their detail.
pub fn error_status(e: &AppError) -> (u16, String) {
    match e {
        AppError::Upload(u) => (422, format!("upload rejected: {u:?}")),
        AppError::Queue(QueueError::SlotTaken { .. }) => (409, "slot already taken".into()),
        AppError::Queue(q) => (422, format!("post rejected: {q:?}")),
        AppError::Port(_) => (500, "storage error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::app::PortError;

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
        assert_eq!((code, msg.as_str()), (500, "storage error"));
    }

    #[test]
    fn slot_taken_is_409() {
        let (code, _) = error_status(&AppError::Queue(QueueError::SlotTaken { slot_at: 5 }));
        assert_eq!(code, 409);
    }

    #[test]
    fn detects_a_d1_slot_conflict_by_its_text() {
        assert!(is_slot_conflict(
            "D1_ERROR: UNIQUE constraint failed: posts.slot_at: SQLITE_CONSTRAINT"
        ));
        assert!(!is_slot_conflict(
            "UNIQUE constraint failed: posts.video_key"
        ));
        assert!(!is_slot_conflict("network down"));
    }

    #[test]
    fn rule_errors_are_422() {
        let (code, _) = error_status(&AppError::Queue(QueueError::NoPlatforms));
        assert_eq!(code, 422);
    }
}
