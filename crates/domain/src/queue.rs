//! Scheduling domain: the queue of posts waiting for a slot.

use crate::media::is_safe_id;

/// YouTube title limit. Confirm against YouTube's current docs before release.
pub const MAX_TITLE_CHARS: usize = 100;
/// Instagram caption limit. Confirm against Meta's current docs before release.
pub const MAX_CAPTION_CHARS: usize = 2_200;

/// Latest allowed slot: 01/01/2100 UTC, in Unix seconds. Keeps slot_at well
/// inside the range a JavaScript number (as D1 binds it) holds exactly.
pub const MAX_SLOT_SECS: i64 = 4_102_444_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Instagram,
    YouTube,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostStatus {
    Ready,
    NeedsCaption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPost {
    pub id: String,
    pub video_key: String,
    pub title: String,
    pub caption: String,
    pub platforms: Vec<Platform>,
    /// Slot time as Unix seconds (UTC).
    pub slot_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum QueueError {
    NoPlatforms,
    EmptyTitle,
    SlotTaken { slot_at: i64 },
    VideoAlreadyQueued,
    BadVideoKey,
    TooLong,
    DuplicatePlatform,
    SlotInPast,
    SlotOutOfRange,
}

/// Checks a new slot is a sensible future time. Applies to new posts only, so
/// stored posts whose slot has since passed still load.
pub fn validate_slot(slot_at: i64, now: i64) -> Result<(), QueueError> {
    if !(0..=MAX_SLOT_SECS).contains(&slot_at) {
        return Err(QueueError::SlotOutOfRange);
    }
    if slot_at < now {
        return Err(QueueError::SlotInPast);
    }
    Ok(())
}

/// The rules for the editable fields of a post, shared by new and edited posts.
pub fn validate_fields(
    title: &str,
    caption: &str,
    platforms: &[Platform],
) -> Result<(), QueueError> {
    if platforms.is_empty() {
        return Err(QueueError::NoPlatforms);
    }
    let mut seen = Vec::new();
    for p in platforms {
        if seen.contains(p) {
            return Err(QueueError::DuplicatePlatform);
        }
        seen.push(*p);
    }
    if title.trim().is_empty() {
        return Err(QueueError::EmptyTitle);
    }
    if title.chars().count() > MAX_TITLE_CHARS || caption.chars().count() > MAX_CAPTION_CHARS {
        return Err(QueueError::TooLong);
    }
    Ok(())
}

/// A post id as the Worker makes it (a UUID in simple form: lowercase hex),
/// capped at 64 characters. Checked before an id from a URL reaches storage.
pub fn is_post_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Exactly `videos/` + 1 to 64 of `[A-Za-z0-9_-]` + `.mp4`.
fn is_valid_video_key(key: &str) -> bool {
    let Some(name) = key
        .strip_prefix("videos/")
        .and_then(|rest| rest.strip_suffix(".mp4"))
    else {
        return false;
    };
    is_safe_id(name)
}

impl QueuedPost {
    pub fn status(&self) -> PostStatus {
        if self.caption.trim().is_empty() {
            PostStatus::NeedsCaption
        } else {
            PostStatus::Ready
        }
    }
}

#[derive(Debug, Default)]
pub struct Queue {
    posts: Vec<QueuedPost>,
}

impl Queue {
    /// Rebuilds a queue from storage without validating. Rules are for new
    /// posts only, so older rows can never lock the queue.
    pub fn from_stored(posts: Vec<QueuedPost>) -> Self {
        Self { posts }
    }

    pub fn add(&mut self, post: QueuedPost) -> Result<(), QueueError> {
        validate_fields(&post.title, &post.caption, &post.platforms)?;
        if !is_valid_video_key(&post.video_key) {
            return Err(QueueError::BadVideoKey);
        }
        if self.posts.iter().any(|p| p.slot_at == post.slot_at) {
            return Err(QueueError::SlotTaken {
                slot_at: post.slot_at,
            });
        }
        if self.posts.iter().any(|p| p.video_key == post.video_key) {
            return Err(QueueError::VideoAlreadyQueued);
        }
        self.posts.push(post);
        Ok(())
    }

    /// Posts in slot order, earliest first.
    pub fn upcoming(&self, now: i64) -> Vec<&QueuedPost> {
        let mut out: Vec<&QueuedPost> = self.posts.iter().filter(|p| p.slot_at >= now).collect();
        out.sort_by_key(|p| p.slot_at);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str, slot_at: i64) -> QueuedPost {
        QueuedPost {
            id: id.into(),
            video_key: format!("videos/{id}.mp4"),
            title: "A title".into(),
            caption: "A caption".into(),
            platforms: vec![Platform::Instagram, Platform::YouTube],
            slot_at,
        }
    }

    #[test]
    fn post_ids_are_the_hex_the_generator_makes() {
        assert!(is_post_id("0123456789abcdef0123456789abcdef"));
        assert!(is_post_id("a"));
        for bad in ["", "A", "a-b", "a/b", "a b", "g", "a'--", &"a".repeat(65)] {
            assert!(!is_post_id(bad), "{bad}");
        }
    }

    #[test]
    fn validate_fields_is_the_shared_rule_set() {
        use Platform::*;
        assert_eq!(validate_fields("T", "", &[Instagram]), Ok(()));
        assert_eq!(validate_fields("T", "", &[]), Err(QueueError::NoPlatforms));
        assert_eq!(
            validate_fields("T", "", &[YouTube, YouTube]),
            Err(QueueError::DuplicatePlatform)
        );
        assert_eq!(
            validate_fields(" ", "", &[YouTube]),
            Err(QueueError::EmptyTitle)
        );
        assert_eq!(
            validate_fields(&"x".repeat(101), "", &[YouTube]),
            Err(QueueError::TooLong)
        );
    }

    #[test]
    fn adds_a_post_to_an_empty_queue() {
        let mut q = Queue::default();
        q.add(post("a", 100)).unwrap();
        assert_eq!(q.upcoming(0).len(), 1);
    }

    #[test]
    fn needs_at_least_one_platform() {
        let mut p = post("a", 100);
        p.platforms.clear();
        assert_eq!(Queue::default().add(p), Err(QueueError::NoPlatforms));
    }

    #[test]
    fn needs_a_title() {
        let mut p = post("a", 100);
        p.title = "   ".into();
        assert_eq!(Queue::default().add(p), Err(QueueError::EmptyTitle));
    }

    #[test]
    fn one_post_per_slot() {
        let mut q = Queue::from_stored(vec![post("a", 100)]);
        assert_eq!(
            q.add(post("b", 100)),
            Err(QueueError::SlotTaken { slot_at: 100 })
        );
    }

    #[test]
    fn upcoming_is_in_slot_order_and_skips_the_past() {
        let q = Queue::from_stored(vec![post("late", 300), post("past", 50), post("soon", 200)]);
        let ids: Vec<&str> = q.upcoming(100).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["soon", "late"]);
    }

    #[test]
    fn one_post_per_video() {
        let mut q = Queue::from_stored(vec![post("a", 100)]);
        let mut dup = post("b", 200);
        dup.video_key = "videos/a.mp4".into();
        assert_eq!(q.add(dup), Err(QueueError::VideoAlreadyQueued));
    }

    #[test]
    fn from_stored_does_not_validate_old_rows() {
        let mut bad = post("a", 100);
        bad.video_key = "legacy/odd key".into();
        bad.platforms.clear();
        bad.title.clear();
        let q = Queue::from_stored(vec![bad, post("b", 100)]);
        assert_eq!(q.upcoming(0).len(), 2);
    }

    #[test]
    fn video_key_must_be_in_the_videos_prefix() {
        let mut p = post("a", 100);
        p.video_key = "secrets/x.mp4".into();
        assert_eq!(Queue::default().add(p), Err(QueueError::BadVideoKey));
    }

    #[test]
    fn video_key_must_match_the_exact_shape() {
        for bad in [
            "videos/.mp4",
            "videos/a.MP4",
            "videos/a/b.mp4",
            "videos/a b.mp4",
            "videos/a.mp4\n",
            "videos/a..b.mp4",
            "videos/é.mp4",
            "videos/a.mp4.exe",
            "/videos/a.mp4",
        ] {
            let mut p = post("a", 100);
            p.video_key = bad.into();
            assert_eq!(
                Queue::default().add(p),
                Err(QueueError::BadVideoKey),
                "{bad}"
            );
        }
        let mut p = post("a", 100);
        p.video_key = format!("videos/{}.mp4", "A-z_9".repeat(12) + "abcd");
        assert_eq!(p.video_key.len(), 7 + 64 + 4);
        assert!(Queue::default().add(p).is_ok());
        let mut p = post("a", 100);
        p.video_key = format!("videos/{}.mp4", "a".repeat(65));
        assert_eq!(Queue::default().add(p), Err(QueueError::BadVideoKey));
    }

    #[test]
    fn duplicate_platforms_are_rejected() {
        let mut p = post("a", 100);
        p.platforms = vec![Platform::YouTube, Platform::YouTube];
        assert_eq!(Queue::default().add(p), Err(QueueError::DuplicatePlatform));
    }

    #[test]
    fn slot_must_be_in_range_and_not_in_the_past() {
        assert_eq!(validate_slot(-1, 0), Err(QueueError::SlotOutOfRange));
        assert_eq!(
            validate_slot(MAX_SLOT_SECS + 1, 0),
            Err(QueueError::SlotOutOfRange)
        );
        assert_eq!(validate_slot(99, 100), Err(QueueError::SlotInPast));
        assert_eq!(validate_slot(100, 100), Ok(()));
        assert_eq!(validate_slot(MAX_SLOT_SECS, 100), Ok(()));
    }

    #[test]
    fn caps_title_and_caption_length() {
        let mut p = post("a", 100);
        p.title = "t".repeat(MAX_TITLE_CHARS + 1);
        assert_eq!(Queue::default().add(p), Err(QueueError::TooLong));
        let mut p = post("b", 200);
        p.caption = "c".repeat(MAX_CAPTION_CHARS + 1);
        assert_eq!(Queue::default().add(p), Err(QueueError::TooLong));
    }

    #[test]
    fn blank_caption_needs_caption() {
        let mut p = post("a", 100);
        p.caption = String::new();
        assert_eq!(p.status(), PostStatus::NeedsCaption);
        assert_eq!(post("b", 100).status(), PostStatus::Ready);
    }
}
