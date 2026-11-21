//! Use cases. They talk to the outside world only through the ports below,
//! so the Worker (and later an AWS adapter) plugs in without changing them.

use crate::media::{UploadError, VideoUpload, MAX_UPLOAD_BYTES};
use crate::queue::{validate_slot, Queue, QueueError, QueuedPost};
use crate::slots::{next_free_slots, SlotError, SlotPattern};

/// How long an upload link stays valid.
pub const UPLOAD_LINK_SECS: u32 = 15 * 60;

#[derive(Debug, PartialEq, Eq)]
pub struct PortError(pub String);

/// Storage port: signs a short-lived upload link for one object key.
pub trait UploadSigner {
    fn put_url(
        &self,
        key: &str,
        content_type: &str,
        expires_secs: u32,
    ) -> Result<String, PortError>;
}

/// Why an insert failed. `SlotTaken` and `VideoAlreadyQueued` are the
/// lost-race cases: another request took the slot or video between our check
/// and the insert.
#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    SlotTaken,
    VideoAlreadyQueued,
    Port(PortError),
}

/// Persistence port for queued posts.
#[allow(async_fn_in_trait)]
pub trait PostRepository {
    async fn all(&self) -> Result<Vec<QueuedPost>, PortError>;
    /// Must fail if the slot or video is already taken, so two requests can't race.
    async fn insert(&self, post: &QueuedPost) -> Result<(), InsertError>;
}

/// Persistence port for the owner's weekly slot pattern (one per account).
#[allow(async_fn_in_trait)]
pub trait SlotPatternRepository {
    /// `None` until the owner has saved a pattern.
    async fn get(&self) -> Result<Option<SlotPattern>, PortError>;
    async fn save(&self, pattern: &SlotPattern) -> Result<(), PortError>;
}

/// Storage port: what is actually in the bucket. The signed upload link can't
/// enforce a size, so we check after the upload.
#[allow(async_fn_in_trait)]
pub trait UploadStore {
    /// Size in bytes of the stored object, or `None` if there is no such object.
    async fn object_size(&self, key: &str) -> Result<Option<u64>, PortError>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppError {
    /// The post names a video that is not in storage.
    VideoNotUploaded,
    Upload(UploadError),
    Queue(QueueError),
    Slots(SlotError),
    Port(PortError),
}

#[derive(Debug, PartialEq, Eq)]
pub struct UploadTicket {
    pub video_key: String,
    pub put_url: String,
}

pub fn start_upload(
    signer: &impl UploadSigner,
    id: &str,
    file_name: &str,
    content_type: &str,
    size_bytes: u64,
) -> Result<UploadTicket, AppError> {
    let upload = VideoUpload::new(file_name, content_type, size_bytes).map_err(AppError::Upload)?;
    let video_key = upload.storage_key(id).map_err(AppError::Upload)?;
    let put_url = signer
        .put_url(&video_key, "video/mp4", UPLOAD_LINK_SECS)
        .map_err(AppError::Port)?;
    Ok(UploadTicket { video_key, put_url })
}

pub async fn queue_post(
    repo: &impl PostRepository,
    store: &impl UploadStore,
    now: i64,
    post: QueuedPost,
) -> Result<(), AppError> {
    validate_slot(post.slot_at, now).map_err(AppError::Queue)?;
    let existing = repo.all().await.map_err(AppError::Port)?;
    let mut queue = Queue::from_stored(existing);
    queue.add(post.clone()).map_err(AppError::Queue)?;
    // The key is valid by now, so storage is only asked about well-formed keys.
    match store
        .object_size(&post.video_key)
        .await
        .map_err(AppError::Port)?
    {
        None => return Err(AppError::VideoNotUploaded),
        Some(0) => return Err(AppError::Upload(UploadError::Empty)),
        Some(n) if n > MAX_UPLOAD_BYTES => {
            return Err(AppError::Upload(UploadError::TooLarge { size_bytes: n }))
        }
        Some(_) => {}
    }
    let slot_at = post.slot_at;
    repo.insert(&post).await.map_err(|e| match e {
        InsertError::SlotTaken => AppError::Queue(QueueError::SlotTaken { slot_at }),
        InsertError::VideoAlreadyQueued => AppError::Queue(QueueError::VideoAlreadyQueued),
        InsertError::Port(p) => AppError::Port(p),
    })
}

pub async fn upcoming(repo: &impl PostRepository, now: i64) -> Result<Vec<QueuedPost>, AppError> {
    let queue = Queue::from_stored(repo.all().await.map_err(AppError::Port)?);
    Ok(queue.upcoming(now).into_iter().cloned().collect())
}

pub async fn get_pattern(
    repo: &impl SlotPatternRepository,
) -> Result<Option<SlotPattern>, AppError> {
    repo.get().await.map_err(AppError::Port)
}

/// Validates through `SlotPattern::new`, saves, and returns the normalised pattern.
pub async fn set_pattern(
    repo: &impl SlotPatternRepository,
    utc_offset_minutes: i32,
    rules: Vec<crate::slots::SlotRule>,
) -> Result<SlotPattern, AppError> {
    let pattern = SlotPattern::new(utc_offset_minutes, rules).map_err(AppError::Slots)?;
    repo.save(&pattern).await.map_err(AppError::Port)?;
    Ok(pattern)
}

/// The next free slots after the queued posts. No saved pattern means none.
pub async fn next_slots(
    patterns: &impl SlotPatternRepository,
    posts: &impl PostRepository,
    now: i64,
    count: usize,
) -> Result<Vec<i64>, AppError> {
    let Some(pattern) = patterns.get().await.map_err(AppError::Port)? else {
        return Ok(Vec::new());
    };
    let taken: Vec<i64> = posts
        .all()
        .await
        .map_err(AppError::Port)?
        .iter()
        .map(|p| p.slot_at)
        .collect();
    Ok(next_free_slots(&pattern, &taken, now, count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MAX_UPLOAD_BYTES;
    use crate::queue::Platform;
    use crate::slots::{SlotRule, Weekday};
    use std::cell::RefCell;

    struct FakeSigner;
    impl UploadSigner for FakeSigner {
        fn put_url(&self, key: &str, ct: &str, secs: u32) -> Result<String, PortError> {
            Ok(format!("https://signed/{key}?ct={ct}&exp={secs}"))
        }
    }

    /// Reports a fixed stored size for every key, and records which keys were asked about.
    struct FakeStore {
        size: Option<u64>,
        asked: RefCell<Vec<String>>,
    }
    impl UploadStore for FakeStore {
        async fn object_size(&self, key: &str) -> Result<Option<u64>, PortError> {
            self.asked.borrow_mut().push(key.into());
            Ok(self.size)
        }
    }

    fn store_with(size: Option<u64>) -> FakeStore {
        FakeStore {
            size,
            asked: RefCell::default(),
        }
    }

    fn store() -> FakeStore {
        store_with(Some(10))
    }

    #[derive(Default)]
    struct FakeRepo {
        posts: RefCell<Vec<QueuedPost>>,
    }
    impl PostRepository for FakeRepo {
        async fn all(&self) -> Result<Vec<QueuedPost>, PortError> {
            Ok(self.posts.borrow().clone())
        }
        async fn insert(&self, post: &QueuedPost) -> Result<(), InsertError> {
            self.posts.borrow_mut().push(post.clone());
            Ok(())
        }
    }

    /// Passes the `all()` check, then loses the race at insert time.
    struct RacingRepo;
    impl PostRepository for RacingRepo {
        async fn all(&self) -> Result<Vec<QueuedPost>, PortError> {
            Ok(vec![])
        }
        async fn insert(&self, _post: &QueuedPost) -> Result<(), InsertError> {
            Err(InsertError::SlotTaken)
        }
    }

    fn post(id: &str, slot_at: i64) -> QueuedPost {
        QueuedPost {
            id: id.into(),
            video_key: format!("videos/{id}.mp4"),
            title: "Title".into(),
            caption: "Caption".into(),
            platforms: vec![Platform::Instagram],
            slot_at,
        }
    }

    #[test]
    fn start_upload_signs_a_typed_short_lived_link() {
        let t = start_upload(&FakeSigner, "abc", "reel.mp4", "video/mp4", 10).unwrap();
        assert_eq!(t.video_key, "videos/abc.mp4");
        assert_eq!(
            t.put_url,
            "https://signed/videos/abc.mp4?ct=video/mp4&exp=900"
        );
    }

    #[test]
    fn start_upload_rejects_bad_files_before_signing() {
        let e = start_upload(&FakeSigner, "abc", "reel.mov", "video/quicktime", 10);
        assert_eq!(e, Err(AppError::Upload(UploadError::NotMp4)));
    }

    #[test]
    fn queue_post_stores_a_valid_post() {
        let repo = FakeRepo::default();
        pollster::block_on(queue_post(&repo, &store(), 0, post("a", 100))).unwrap();
        assert_eq!(repo.posts.borrow().len(), 1);
    }

    #[test]
    fn queue_post_refuses_a_taken_slot_without_storing() {
        let repo = FakeRepo::default();
        repo.posts.borrow_mut().push(post("a", 100));
        let e = pollster::block_on(queue_post(&repo, &store(), 0, post("b", 100)));
        assert_eq!(
            e,
            Err(AppError::Queue(QueueError::SlotTaken { slot_at: 100 }))
        );
        assert_eq!(repo.posts.borrow().len(), 1);
    }

    #[test]
    fn queue_post_rejects_past_and_out_of_range_slots_without_storing() {
        let repo = FakeRepo::default();
        let past = pollster::block_on(queue_post(&repo, &store(), 500, post("a", 100)));
        assert_eq!(past, Err(AppError::Queue(QueueError::SlotInPast)));
        let far = pollster::block_on(queue_post(&repo, &store(), 500, post("b", 4_102_444_801)));
        assert_eq!(far, Err(AppError::Queue(QueueError::SlotOutOfRange)));
        assert!(repo.posts.borrow().is_empty());
    }

    #[test]
    fn queue_post_refuses_a_video_already_queued_without_storing() {
        let repo = FakeRepo::default();
        repo.posts.borrow_mut().push(post("a", 100));
        let mut dup = post("b", 200);
        dup.video_key = "videos/a.mp4".into();
        let e = pollster::block_on(queue_post(&repo, &store(), 0, dup));
        assert_eq!(e, Err(AppError::Queue(QueueError::VideoAlreadyQueued)));
        assert_eq!(repo.posts.borrow().len(), 1);
    }

    #[test]
    fn queue_post_is_not_blocked_by_odd_stored_rows() {
        let repo = FakeRepo::default();
        let mut old = post("old", 50);
        old.video_key = "legacy/odd key".into();
        old.title.clear();
        repo.posts.borrow_mut().push(old);
        pollster::block_on(queue_post(&repo, &store(), 0, post("a", 100))).unwrap();
        assert_eq!(repo.posts.borrow().len(), 2);
    }

    #[test]
    fn upcoming_lists_odd_stored_rows_instead_of_failing() {
        let repo = FakeRepo::default();
        let mut old = post("old", 150);
        old.platforms.clear();
        repo.posts.borrow_mut().push(old);
        assert_eq!(pollster::block_on(upcoming(&repo, 100)).unwrap().len(), 1);
    }

    /// Passes the `all()` check, then loses a video race at insert time.
    struct VideoRacingRepo;
    impl PostRepository for VideoRacingRepo {
        async fn all(&self) -> Result<Vec<QueuedPost>, PortError> {
            Ok(vec![])
        }
        async fn insert(&self, _post: &QueuedPost) -> Result<(), InsertError> {
            Err(InsertError::VideoAlreadyQueued)
        }
    }

    #[test]
    fn queue_post_maps_a_lost_video_race() {
        let e = pollster::block_on(queue_post(&VideoRacingRepo, &store(), 0, post("a", 100)));
        assert_eq!(e, Err(AppError::Queue(QueueError::VideoAlreadyQueued)));
    }

    #[test]
    fn queue_post_maps_a_lost_slot_race_to_slot_taken() {
        let e = pollster::block_on(queue_post(&RacingRepo, &store(), 0, post("a", 100)));
        assert_eq!(
            e,
            Err(AppError::Queue(QueueError::SlotTaken { slot_at: 100 }))
        );
    }

    #[test]
    fn upcoming_returns_future_posts_in_order() {
        let repo = FakeRepo::default();
        repo.posts
            .borrow_mut()
            .extend([post("late", 300), post("past", 10), post("soon", 200)]);
        let ids: Vec<String> = pollster::block_on(upcoming(&repo, 100))
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["soon", "late"]);
    }

    #[test]
    fn queue_post_refuses_a_video_missing_from_storage_without_storing() {
        let repo = FakeRepo::default();
        let e = pollster::block_on(queue_post(&repo, &store_with(None), 0, post("a", 100)));
        assert_eq!(e, Err(AppError::VideoNotUploaded));
        assert!(repo.posts.borrow().is_empty());
    }

    #[test]
    fn queue_post_refuses_an_empty_stored_video_without_storing() {
        let repo = FakeRepo::default();
        let e = pollster::block_on(queue_post(&repo, &store_with(Some(0)), 0, post("a", 100)));
        assert_eq!(e, Err(AppError::Upload(UploadError::Empty)));
        assert!(repo.posts.borrow().is_empty());
    }

    #[test]
    fn queue_post_refuses_an_oversized_stored_video_without_storing() {
        let repo = FakeRepo::default();
        let size = MAX_UPLOAD_BYTES + 1;
        let s = store_with(Some(size));
        let e = pollster::block_on(queue_post(&repo, &s, 0, post("a", 100)));
        assert_eq!(
            e,
            Err(AppError::Upload(UploadError::TooLarge { size_bytes: size }))
        );
        assert!(repo.posts.borrow().is_empty());
    }

    #[test]
    fn queue_post_accepts_a_video_at_exactly_the_size_limit() {
        let repo = FakeRepo::default();
        let s = store_with(Some(MAX_UPLOAD_BYTES));
        pollster::block_on(queue_post(&repo, &s, 0, post("a", 100))).unwrap();
        assert_eq!(repo.posts.borrow().len(), 1);
    }

    #[test]
    fn queue_post_asks_storage_about_the_posts_video_key_only_once_valid() {
        let repo = FakeRepo::default();
        let s = store();
        pollster::block_on(queue_post(&repo, &s, 0, post("a", 100))).unwrap();
        assert_eq!(*s.asked.borrow(), ["videos/a.mp4"]);
        let mut bad = post("b", 200);
        bad.video_key = "../escape".into();
        let s2 = store();
        let e = pollster::block_on(queue_post(&repo, &s2, 0, bad));
        assert_eq!(e, Err(AppError::Queue(QueueError::BadVideoKey)));
        assert!(s2.asked.borrow().is_empty());
    }

    struct BrokenStore;
    impl UploadStore for BrokenStore {
        async fn object_size(&self, _key: &str) -> Result<Option<u64>, PortError> {
            Err(PortError("r2 down".into()))
        }
    }

    #[test]
    fn queue_post_surfaces_a_storage_failure_as_a_port_error() {
        let repo = FakeRepo::default();
        let e = pollster::block_on(queue_post(&repo, &BrokenStore, 0, post("a", 100)));
        assert_eq!(e, Err(AppError::Port(PortError("r2 down".into()))));
        assert!(repo.posts.borrow().is_empty());
    }

    #[derive(Default)]
    struct FakePatterns {
        stored: RefCell<Option<SlotPattern>>,
        fail: bool,
    }
    impl SlotPatternRepository for FakePatterns {
        async fn get(&self) -> Result<Option<SlotPattern>, PortError> {
            if self.fail {
                return Err(PortError("d1 down".into()));
            }
            Ok(self.stored.borrow().clone())
        }
        async fn save(&self, p: &SlotPattern) -> Result<(), PortError> {
            if self.fail {
                return Err(PortError("d1 down".into()));
            }
            *self.stored.borrow_mut() = Some(p.clone());
            Ok(())
        }
    }

    fn rule(weekday: Weekday, h: u16) -> SlotRule {
        SlotRule {
            weekday,
            minute_of_day: h * 60,
        }
    }

    #[test]
    fn set_pattern_saves_and_returns_the_sorted_pattern() {
        let repo = FakePatterns::default();
        let rules = vec![rule(Weekday::Wed, 19), rule(Weekday::Mon, 6)];
        let saved = pollster::block_on(set_pattern(&repo, 600, rules)).unwrap();
        assert_eq!(saved.rules[0], rule(Weekday::Mon, 6));
        assert_eq!(pollster::block_on(get_pattern(&repo)), Ok(Some(saved)));
    }

    #[test]
    fn set_pattern_maps_validation_errors_and_saves_nothing() {
        let repo = FakePatterns::default();
        let e = pollster::block_on(set_pattern(&repo, 900, vec![]));
        assert_eq!(e, Err(AppError::Slots(SlotError::OffsetOutOfRange)));
        let dup = vec![rule(Weekday::Mon, 6), rule(Weekday::Mon, 6)];
        let e = pollster::block_on(set_pattern(&repo, 0, dup));
        assert_eq!(e, Err(AppError::Slots(SlotError::DuplicateRule)));
        assert!(repo.stored.borrow().is_none());
    }

    #[test]
    fn set_pattern_surfaces_a_storage_failure() {
        let repo = FakePatterns {
            fail: true,
            ..Default::default()
        };
        let e = pollster::block_on(set_pattern(&repo, 0, vec![]));
        assert_eq!(e, Err(AppError::Port(PortError("d1 down".into()))));
        let e = pollster::block_on(get_pattern(&repo));
        assert_eq!(e, Err(AppError::Port(PortError("d1 down".into()))));
    }

    #[test]
    fn next_slots_skips_slots_already_queued() {
        let patterns = FakePatterns::default();
        // 1970-01-01 is a Thursday: Thursday 00:00 UTC each week.
        pollster::block_on(set_pattern(&patterns, 0, vec![rule(Weekday::Thu, 0)])).unwrap();
        let posts = FakeRepo::default();
        posts.posts.borrow_mut().push(post("a", 604_800));
        let got = pollster::block_on(next_slots(&patterns, &posts, 0, 2)).unwrap();
        assert_eq!(got, vec![1_209_600, 1_814_400]);
    }

    #[test]
    fn next_slots_is_empty_without_a_pattern() {
        let got = pollster::block_on(next_slots(
            &FakePatterns::default(),
            &FakeRepo::default(),
            0,
            5,
        ));
        assert_eq!(got, Ok(vec![]));
    }

    #[test]
    fn next_slots_surfaces_storage_failures() {
        let broken = FakePatterns {
            fail: true,
            ..Default::default()
        };
        let e = pollster::block_on(next_slots(&broken, &FakeRepo::default(), 0, 5));
        assert_eq!(e, Err(AppError::Port(PortError("d1 down".into()))));
    }
}
