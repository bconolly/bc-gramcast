//! What YouTube's requests and answers look like on the wire. No decisions here; those
//! are `domain::youtube` and `domain::upload`. Nothing read from a response is kept
//! except a few short, checked tokens: the error reason codes and the video id. Message
//! text, URLs and header values are never copied into an error or a log.

use domain::youtube::Privacy;
use serde::Deserialize;

/// Largest error body we will parse. Bigger is treated as having no reasons.
pub const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
/// Largest video resource we will parse for its id.
pub const MAX_VIDEO_BODY_BYTES: usize = 64 * 1024;
/// Most reason codes read from one error body, and their longest length.
const MAX_REASONS: usize = 8;
const MAX_REASON_LEN: usize = 64;

/// The JSON body that starts the upload. The title and description are the post's own
/// (the domain already limits them); nothing is appended. There is no category: the
/// field is optional when inserting.
pub fn initiate_body(title: &str, description: &str, privacy: Privacy) -> String {
    serde_json::json!({
        "snippet": { "title": title, "description": description },
        "status": { "privacyStatus": privacy.status(), "selfDeclaredMadeForKids": false },
    })
    .to_string()
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    errors: Option<Vec<ErrorItem>>,
}

#[derive(Deserialize)]
struct ErrorItem {
    reason: Option<String>,
}

/// The `error.errors[].reason` codes of a Google API error body: short, plain tokens
/// only. Messages are never read. Anything unparsable gives none.
pub fn error_reasons(body: &str) -> Vec<String> {
    if body.len() > MAX_ERROR_BODY_BYTES {
        return Vec::new();
    }
    let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(body) else {
        return Vec::new();
    };
    let items = envelope.error.and_then(|e| e.errors).unwrap_or_default();
    items
        .into_iter()
        .filter_map(|i| i.reason)
        .filter(|r| is_plain_token(r))
        .take(MAX_REASONS)
        .collect()
}

fn is_plain_token(s: &str) -> bool {
    (1..=MAX_REASON_LEN).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[derive(Deserialize)]
struct VideoResource {
    id: Option<String>,
}

/// The `id` of a video resource (the answer to the final chunk). The domain still
/// checks its shape; this only refuses a body that is too big or not JSON.
pub fn video_id(body: &str) -> Option<String> {
    if body.len() > MAX_VIDEO_BODY_BYTES {
        return None;
    }
    serde_json::from_str::<VideoResource>(body).ok()?.id
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn the_initiation_body_is_the_posts_own_text_and_a_privacy_and_not_for_kids() {
        let body = initiate_body("My \"title\"", "Line 1\nLine 2 <b> é", Privacy::Unlisted);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v,
            json!({
                "snippet": {"title": "My \"title\"", "description": "Line 1\nLine 2 <b> é"},
                "status": {"privacyStatus": "unlisted", "selfDeclaredMadeForKids": false}
            })
        );
        // No category and nothing appended to the title or description.
        assert!(v["snippet"].get("categoryId").is_none());
        let private: Value =
            serde_json::from_str(&initiate_body("T", "", Privacy::Private)).unwrap();
        assert_eq!(private["status"]["privacyStatus"], "private");
        assert_eq!(private["snippet"]["description"], "");
    }

    #[test]
    fn error_reasons_are_read_without_the_messages() {
        let body = r#"{"error":{"code":403,"message":"SECRET-MESSAGE https://x/?sig=1",
            "errors":[{"domain":"youtube.quota","reason":"quotaExceeded","message":"SECRET"},
                      {"reason":"rateLimitExceeded"}]}}"#;
        let reasons = error_reasons(body);
        assert_eq!(reasons, ["quotaExceeded", "rateLimitExceeded"]);
        assert!(!format!("{reasons:?}").contains("SECRET"));
    }

    #[test]
    fn odd_error_bodies_give_no_reasons() {
        let long = "x".repeat(65);
        let long_reason = format!(r#"{{"error":{{"errors":[{{"reason":"{long}"}}]}}}}"#);
        let too_big = format!(
            r#"{{"error":{{"errors":[{{"reason":"quotaExceeded"}}]}},"pad":"{}"}}"#,
            "x".repeat(MAX_ERROR_BODY_BYTES)
        );
        for body in [
            "",
            "not json",
            "{}",
            r#"{"error":"invalid_grant"}"#,
            r#"{"error":{"errors":"nope"}}"#,
            r#"{"error":{"errors":[{"reason":5},{"reason":"has space"},{"reason":""},{"reason":"a/b"}]}}"#,
            &long_reason,
            &too_big,
        ] {
            assert_eq!(error_reasons(body), Vec::<String>::new(), "{body}");
        }
    }

    #[test]
    fn only_the_first_few_reasons_are_read() {
        let items: Vec<String> = (0..20).map(|i| format!(r#"{{"reason":"r{i}"}}"#)).collect();
        let body = format!(r#"{{"error":{{"errors":[{}]}}}}"#, items.join(","));
        assert_eq!(error_reasons(&body).len(), MAX_REASONS);
    }

    #[test]
    fn the_video_id_comes_from_the_id_field_only() {
        assert_eq!(
            video_id(r#"{"kind":"youtube#video","id":"dQw4w9WgXcQ","snippet":{"title":"x"}}"#),
            Some("dQw4w9WgXcQ".to_string())
        );
        let too_big = format!(
            r#"{{"id":"abc","pad":"{}"}}"#,
            "x".repeat(MAX_VIDEO_BODY_BYTES)
        );
        for bad in ["", "junk", "{}", r#"{"id":7}"#, r#"{"id":null}"#, &too_big] {
            assert_eq!(video_id(bad), None, "{bad}");
        }
    }
}
