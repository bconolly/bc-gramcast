//! Scheduling domain: the queue of posts waiting for a slot.

/// YouTube title limit. Confirm against YouTube's current docs before release.
pub const MAX_TITLE_CHARS: usize = 100;
/// Instagram caption limit. Confirm against Meta's current docs before release.
pub const MAX_CAPTION_CHARS: usize = 2_200;

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
    BadVideoKey,
    TooLong,
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
    /// Rebuilds a queue from storage, applying the same rules as `add`.
    pub fn from_posts(posts: Vec<QueuedPost>) -> Result<Self, QueueError> {
        let mut q = Self::default();
        for p in posts {
            q.add(p)?;
        }
        Ok(q)
    }

    pub fn add(&mut self, post: QueuedPost) -> Result<(), QueueError> {
        if post.platforms.is_empty() {
            return Err(QueueError::NoPlatforms);
        }
        if post.title.trim().is_empty() {
            return Err(QueueError::EmptyTitle);
        }
        if post.title.chars().count() > MAX_TITLE_CHARS
            || post.caption.chars().count() > MAX_CAPTION_CHARS
        {
            return Err(QueueError::TooLong);
        }
        if !post.video_key.starts_with("videos/") || post.video_key.contains("..") {
            return Err(QueueError::BadVideoKey);
        }
        if self.posts.iter().any(|p| p.slot_at == post.slot_at) {
            return Err(QueueError::SlotTaken {
                slot_at: post.slot_at,
            });
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
        let mut q = Queue::from_posts(vec![post("a", 100)]).unwrap();
        assert_eq!(
            q.add(post("b", 100)),
            Err(QueueError::SlotTaken { slot_at: 100 })
        );
    }

    #[test]
    fn upcoming_is_in_slot_order_and_skips_the_past() {
        let q = Queue::from_posts(vec![post("late", 300), post("past", 50), post("soon", 200)])
            .unwrap();
        let ids: Vec<&str> = q.upcoming(100).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["soon", "late"]);
    }

    #[test]
    fn from_posts_applies_the_same_rules_as_add() {
        let result = Queue::from_posts(vec![post("a", 100), post("b", 100)]);
        assert_eq!(result.err(), Some(QueueError::SlotTaken { slot_at: 100 }));
    }

    #[test]
    fn video_key_must_be_in_the_videos_prefix() {
        let mut p = post("a", 100);
        p.video_key = "secrets/x.mp4".into();
        assert_eq!(Queue::default().add(p), Err(QueueError::BadVideoKey));
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
