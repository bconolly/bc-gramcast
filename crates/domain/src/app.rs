//! Use cases. They talk to the outside world only through the ports below,
//! so the Worker (and later an AWS adapter) plugs in without changing them.

use crate::auth::{AccessConfig, AuthError, Claims, JwtHeader};
use crate::media::{UploadError, VideoUpload, MAX_UPLOAD_BYTES};
use crate::queue::{
    is_post_id, is_valid_video_key, validate_fields, validate_slot, Platform, Queue, QueueError,
    QueuedPost,
};
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

/// Why an update failed. `SlotTaken` is the lost-race case, as for inserts.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateError {
    NotFound,
    SlotTaken,
    Port(PortError),
}

/// Persistence port for queued posts.
#[allow(async_fn_in_trait)]
pub trait PostRepository {
    async fn all(&self) -> Result<Vec<QueuedPost>, PortError>;
    /// Must fail if the slot or video is already taken, so two requests can't race.
    async fn insert(&self, post: &QueuedPost) -> Result<(), InsertError>;
    async fn get(&self, id: &str) -> Result<Option<QueuedPost>, PortError>;
    /// Replaces the editable fields of the post with this id. Must fail with
    /// `SlotTaken` if another post holds the slot, and `NotFound` if no row matched.
    async fn update(&self, post: &QueuedPost) -> Result<(), UpdateError>;
    /// `true` if a row was removed, `false` if there was none.
    async fn delete(&self, id: &str) -> Result<bool, PortError>;
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
    /// Removes the object. Removing a missing object is not an error.
    async fn delete_object(&self, key: &str) -> Result<(), PortError>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppError {
    /// No post has this id (or the id is not a valid post id).
    NotFound,
    /// The post's slot has arrived or passed, so it is no longer queued.
    NotQueued,
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

/// The editable fields of a queued post. The id and video are never editable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostChanges {
    pub title: String,
    pub caption: String,
    pub platforms: Vec<Platform>,
    pub slot_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    /// The post is gone but its video could not be removed from storage.
    VideoLeftBehind,
}

/// Replaces the title, caption, platforms and slot of a queued post, with the
/// same rules as `queue_post`. `video_key` and `id` stay as stored.
pub async fn update_post(
    repo: &impl PostRepository,
    id: &str,
    changes: PostChanges,
    now: i64,
) -> Result<QueuedPost, AppError> {
    if !is_post_id(id) {
        return Err(AppError::NotFound);
    }
    validate_fields(&changes.title, &changes.caption, &changes.platforms)
        .map_err(AppError::Queue)?;
    validate_slot(changes.slot_at, now).map_err(AppError::Queue)?;
    let mut post = repo
        .get(id)
        .await
        .map_err(AppError::Port)?
        .ok_or(AppError::NotFound)?;
    if post.slot_at <= now {
        return Err(AppError::NotQueued);
    }
    let slot_at = changes.slot_at;
    post.title = changes.title;
    post.caption = changes.caption;
    post.platforms = changes.platforms;
    post.slot_at = slot_at;
    repo.update(&post).await.map_err(|e| match e {
        UpdateError::NotFound => AppError::NotFound,
        UpdateError::SlotTaken => AppError::Queue(QueueError::SlotTaken { slot_at }),
        UpdateError::Port(p) => AppError::Port(p),
    })?;
    Ok(post)
}

/// Removes a still-queued post's row first, then the video. If the video delete fails the owner
/// still sees the post gone: the other order could leave a row pointing at a
/// missing video that can never be queued or deleted cleanly. The leftover
/// object is reported through `VideoLeftBehind` so the caller can log a fixed line.
pub async fn delete_post(
    repo: &impl PostRepository,
    store: &impl UploadStore,
    id: &str,
    now: i64,
) -> Result<DeleteOutcome, AppError> {
    if !is_post_id(id) {
        return Err(AppError::NotFound);
    }
    let post = repo
        .get(id)
        .await
        .map_err(AppError::Port)?
        .ok_or(AppError::NotFound)?;
    if post.slot_at <= now {
        return Err(AppError::NotQueued);
    }
    if !repo.delete(id).await.map_err(AppError::Port)? {
        return Err(AppError::NotFound);
    }
    // A stored key is re-checked, so storage is only asked to delete well-formed keys.
    if !is_valid_video_key(&post.video_key) {
        return Ok(DeleteOutcome::VideoLeftBehind);
    }
    Ok(match store.delete_object(&post.video_key).await {
        Ok(()) => DeleteOutcome::Deleted,
        Err(_) => DeleteOutcome::VideoLeftBehind,
    })
}

pub async fn upcoming(repo: &impl PostRepository, now: i64) -> Result<Vec<QueuedPost>, AppError> {
    let queue = Queue::from_stored(repo.all().await.map_err(AppError::Port)?);
    Ok(queue.upcoming(now).into_iter().cloned().collect())
}

/// The saved pattern, or the default (Brisbane, no rules) before one is saved.
pub async fn get_pattern(repo: &impl SlotPatternRepository) -> Result<SlotPattern, AppError> {
    let stored = repo.get().await.map_err(AppError::Port)?;
    Ok(stored.unwrap_or_default())
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

/// Why a signature could not be checked.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// No published key has this id (after any allowed refresh).
    UnknownKey,
    /// The keys could not be fetched or used. The text is for logs, never sent out.
    Unavailable(PortError),
}

/// Crypto port: checks an RS256 signature against the published key with this id.
#[allow(async_fn_in_trait)]
pub trait SignatureVerifier {
    /// `Ok(true)` only for a valid signature.
    async fn verify(
        &self,
        kid: &str,
        signing_input: &[u8],
        signature: &[u8],
    ) -> Result<bool, VerifyError>;
}

/// Accepts a token for the owner. Order matters: the header is checked first,
/// the signature next, and the claims only for a token whose signature is good,
/// so nothing in an unsigned payload is ever trusted.
pub async fn authenticate_owner(
    verifier: &impl SignatureVerifier,
    config: &AccessConfig,
    header: JwtHeader,
    claims: Claims,
    signing_input: &[u8],
    signature: &[u8],
    now: i64,
) -> Result<(), AuthError> {
    crate::auth::check_header(&header)?;
    match verifier.verify(&header.kid, signing_input, signature).await {
        Ok(true) => {}
        Ok(false) => return Err(AuthError::BadSignature),
        Err(VerifyError::UnknownKey) => return Err(AuthError::UnknownKey),
        Err(VerifyError::Unavailable(_)) => return Err(AuthError::KeysUnavailable),
    }
    crate::auth::check_claims(&claims, config, now)
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
        deleted: RefCell<Vec<String>>,
        fail_delete: bool,
    }
    impl UploadStore for FakeStore {
        async fn object_size(&self, key: &str) -> Result<Option<u64>, PortError> {
            self.asked.borrow_mut().push(key.into());
            Ok(self.size)
        }
        async fn delete_object(&self, key: &str) -> Result<(), PortError> {
            if self.fail_delete {
                return Err(PortError("r2 down".into()));
            }
            self.deleted.borrow_mut().push(key.into());
            Ok(())
        }
    }

    fn store_with(size: Option<u64>) -> FakeStore {
        FakeStore {
            size,
            asked: RefCell::default(),
            deleted: RefCell::default(),
            fail_delete: false,
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
        async fn get(&self, id: &str) -> Result<Option<QueuedPost>, PortError> {
            Ok(self.posts.borrow().iter().find(|p| p.id == id).cloned())
        }
        async fn update(&self, post: &QueuedPost) -> Result<(), UpdateError> {
            let mut posts = self.posts.borrow_mut();
            if posts
                .iter()
                .any(|p| p.id != post.id && p.slot_at == post.slot_at)
            {
                return Err(UpdateError::SlotTaken);
            }
            match posts.iter_mut().find(|p| p.id == post.id) {
                Some(slot) => *slot = post.clone(),
                None => return Err(UpdateError::NotFound),
            }
            Ok(())
        }
        async fn delete(&self, id: &str) -> Result<bool, PortError> {
            let mut posts = self.posts.borrow_mut();
            let before = posts.len();
            posts.retain(|p| p.id != id);
            Ok(posts.len() != before)
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
        async fn get(&self, _id: &str) -> Result<Option<QueuedPost>, PortError> {
            Ok(None)
        }
        async fn update(&self, _post: &QueuedPost) -> Result<(), UpdateError> {
            Err(UpdateError::NotFound)
        }
        async fn delete(&self, _id: &str) -> Result<bool, PortError> {
            Ok(false)
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
        async fn get(&self, _id: &str) -> Result<Option<QueuedPost>, PortError> {
            Ok(None)
        }
        async fn update(&self, _post: &QueuedPost) -> Result<(), UpdateError> {
            Err(UpdateError::NotFound)
        }
        async fn delete(&self, _id: &str) -> Result<bool, PortError> {
            Ok(false)
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
        async fn delete_object(&self, _key: &str) -> Result<(), PortError> {
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
        assert_eq!(pollster::block_on(get_pattern(&repo)), Ok(saved));
    }

    #[test]
    fn get_pattern_is_the_brisbane_default_until_one_is_saved() {
        let got = pollster::block_on(get_pattern(&FakePatterns::default())).unwrap();
        assert_eq!(got, SlotPattern::default());
        assert_eq!(got.utc_offset_minutes, 600);
        assert!(got.rules.is_empty());
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

    type Call = (String, Vec<u8>, Vec<u8>);

    struct FakeVerifier {
        outcome: Result<bool, VerifyError>,
        calls: RefCell<Vec<Call>>,
    }

    impl FakeVerifier {
        fn new(outcome: Result<bool, VerifyError>) -> Self {
            Self {
                outcome,
                calls: RefCell::new(vec![]),
            }
        }
    }

    impl SignatureVerifier for FakeVerifier {
        async fn verify(&self, kid: &str, input: &[u8], sig: &[u8]) -> Result<bool, VerifyError> {
            self.calls
                .borrow_mut()
                .push((kid.into(), input.to_vec(), sig.to_vec()));
            match &self.outcome {
                Ok(b) => Ok(*b),
                Err(VerifyError::UnknownKey) => Err(VerifyError::UnknownKey),
                Err(VerifyError::Unavailable(p)) => {
                    Err(VerifyError::Unavailable(PortError(p.0.clone())))
                }
            }
        }
    }

    const NOW: i64 = 1_800_000_000;

    fn access() -> AccessConfig {
        AccessConfig::new("t.cloudflareaccess.com", "aud1", "owner@example.com").unwrap()
    }

    fn good_header() -> JwtHeader {
        JwtHeader {
            alg: "RS256".into(),
            kid: "key1".into(),
        }
    }

    fn good_claims() -> Claims {
        Claims {
            iss: "https://t.cloudflareaccess.com".into(),
            aud: vec!["aud1".into()],
            exp: NOW + 60,
            nbf: None,
            email: Some("owner@example.com".into()),
        }
    }

    fn auth(v: &FakeVerifier, header: JwtHeader, claims: Claims) -> Result<(), AuthError> {
        pollster::block_on(authenticate_owner(
            v,
            &access(),
            header,
            claims,
            b"h.p",
            b"sig",
            NOW,
        ))
    }

    #[test]
    fn authenticate_owner_accepts_a_good_token_and_passes_the_exact_bytes() {
        let v = FakeVerifier::new(Ok(true));
        assert_eq!(auth(&v, good_header(), good_claims()), Ok(()));
        assert_eq!(
            *v.calls.borrow(),
            vec![("key1".to_string(), b"h.p".to_vec(), b"sig".to_vec())]
        );
    }

    #[test]
    fn authenticate_owner_refuses_a_bad_header_without_asking_the_verifier() {
        let v = FakeVerifier::new(Ok(true));
        let mut h = good_header();
        h.alg = "none".into();
        assert_eq!(auth(&v, h, good_claims()), Err(AuthError::UnsupportedAlg));
        let mut h = good_header();
        h.kid = "bad kid".into();
        assert_eq!(auth(&v, h, good_claims()), Err(AuthError::BadKeyId));
        assert!(v.calls.borrow().is_empty());
    }

    #[test]
    fn authenticate_owner_maps_signature_outcomes() {
        let v = FakeVerifier::new(Ok(false));
        assert_eq!(
            auth(&v, good_header(), good_claims()),
            Err(AuthError::BadSignature)
        );
        let v = FakeVerifier::new(Err(VerifyError::UnknownKey));
        assert_eq!(
            auth(&v, good_header(), good_claims()),
            Err(AuthError::UnknownKey)
        );
    }

    #[test]
    fn authenticate_owner_turns_a_port_failure_into_keys_unavailable() {
        let v = FakeVerifier::new(Err(VerifyError::Unavailable(PortError("boom".into()))));
        assert_eq!(
            auth(&v, good_header(), good_claims()),
            Err(AuthError::KeysUnavailable)
        );
    }

    #[test]
    fn authenticate_owner_checks_the_signature_before_the_claims() {
        // Every claim is wrong too, but the bad signature is what is reported.
        let mut c = good_claims();
        c.iss = "x".into();
        c.exp = 0;
        let v = FakeVerifier::new(Ok(false));
        assert_eq!(
            auth(&v, good_header(), c.clone()),
            Err(AuthError::BadSignature)
        );
        // An unavailable key service also wins over bad claims.
        let v = FakeVerifier::new(Err(VerifyError::Unavailable(PortError("x".into()))));
        assert_eq!(auth(&v, good_header(), c), Err(AuthError::KeysUnavailable));
    }

    #[test]
    fn authenticate_owner_checks_claims_once_the_signature_is_good() {
        let v = FakeVerifier::new(Ok(true));
        let mut c = good_claims();
        c.email = Some("someone@else.com".into());
        assert_eq!(auth(&v, good_header(), c), Err(AuthError::NotOwner));
        let mut c = good_claims();
        c.exp = NOW - 3600;
        assert_eq!(auth(&v, good_header(), c), Err(AuthError::Expired));
    }

    const ID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn changes(slot_at: i64) -> PostChanges {
        PostChanges {
            title: "New title".into(),
            caption: "New caption".into(),
            platforms: vec![Platform::YouTube],
            slot_at,
        }
    }

    fn repo_with(ids: &[(&str, i64)]) -> FakeRepo {
        let repo = FakeRepo::default();
        repo.posts
            .borrow_mut()
            .extend(ids.iter().map(|(id, slot)| post(id, *slot)));
        repo
    }

    #[test]
    fn delete_post_removes_the_row_then_the_video() {
        let repo = repo_with(&[(ID_A, 100)]);
        let s = store();
        let out = pollster::block_on(delete_post(&repo, &s, ID_A, 0)).unwrap();
        assert_eq!(out, DeleteOutcome::Deleted);
        assert!(repo.posts.borrow().is_empty());
        assert_eq!(*s.deleted.borrow(), [format!("videos/{ID_A}.mp4")]);
    }

    #[test]
    fn delete_post_of_a_missing_or_malformed_id_is_not_found() {
        let repo = repo_with(&[(ID_A, 100)]);
        let s = store();
        for id in [ID_B, "", "A/../x", "a'; DROP TABLE posts;--"] {
            let e = pollster::block_on(delete_post(&repo, &s, id, 0));
            assert_eq!(e, Err(AppError::NotFound), "{id}");
        }
        assert_eq!(repo.posts.borrow().len(), 1);
        assert!(s.deleted.borrow().is_empty());
    }

    #[test]
    fn delete_post_still_succeeds_when_the_video_delete_fails() {
        let repo = repo_with(&[(ID_A, 100)]);
        let mut s = store();
        s.fail_delete = true;
        let out = pollster::block_on(delete_post(&repo, &s, ID_A, 0)).unwrap();
        assert_eq!(out, DeleteOutcome::VideoLeftBehind);
        assert!(repo.posts.borrow().is_empty());
    }

    #[test]
    fn update_post_changes_the_editable_fields_only() {
        let repo = repo_with(&[(ID_A, 100)]);
        let updated = pollster::block_on(update_post(&repo, ID_A, changes(200), 0)).unwrap();
        assert_eq!(updated.id, ID_A);
        assert_eq!(updated.video_key, format!("videos/{ID_A}.mp4"));
        assert_eq!(updated.title, "New title");
        assert_eq!(updated.platforms, vec![Platform::YouTube]);
        assert_eq!(updated.slot_at, 200);
        assert_eq!(repo.posts.borrow()[0], updated);
    }

    #[test]
    fn update_post_may_keep_its_own_slot() {
        let repo = repo_with(&[(ID_A, 100)]);
        pollster::block_on(update_post(&repo, ID_A, changes(100), 0)).unwrap();
    }

    #[test]
    fn update_post_refuses_a_slot_taken_by_another_post() {
        let repo = repo_with(&[(ID_A, 100), (ID_B, 200)]);
        let e = pollster::block_on(update_post(&repo, ID_A, changes(200), 0));
        assert_eq!(
            e,
            Err(AppError::Queue(QueueError::SlotTaken { slot_at: 200 }))
        );
        assert_eq!(repo.posts.borrow()[0].slot_at, 100);
    }

    #[test]
    fn update_post_of_a_missing_id_is_not_found() {
        let repo = repo_with(&[(ID_A, 100)]);
        let e = pollster::block_on(update_post(&repo, ID_B, changes(200), 0));
        assert_eq!(e, Err(AppError::NotFound));
        let e = pollster::block_on(update_post(&repo, "no/pe", changes(200), 0));
        assert_eq!(e, Err(AppError::NotFound));
    }

    #[test]
    fn update_post_uses_the_queue_validation_rules() {
        let repo = repo_with(&[(ID_A, 100)]);
        let run = |c: PostChanges| pollster::block_on(update_post(&repo, ID_A, c, 50));
        let mut c = changes(200);
        c.title = "  ".into();
        assert_eq!(run(c), Err(AppError::Queue(QueueError::EmptyTitle)));
        let mut c = changes(200);
        c.platforms.clear();
        assert_eq!(run(c), Err(AppError::Queue(QueueError::NoPlatforms)));
        let mut c = changes(200);
        c.platforms = vec![Platform::YouTube, Platform::YouTube];
        assert_eq!(run(c), Err(AppError::Queue(QueueError::DuplicatePlatform)));
        let mut c = changes(200);
        c.caption = "x".repeat(2_201);
        assert_eq!(run(c), Err(AppError::Queue(QueueError::TooLong)));
        assert_eq!(
            run(changes(10)),
            Err(AppError::Queue(QueueError::SlotInPast))
        );
        assert_eq!(
            run(changes(4_102_444_801)),
            Err(AppError::Queue(QueueError::SlotOutOfRange))
        );
        assert_eq!(repo.posts.borrow()[0].title, "Title");
    }

    #[test]
    fn update_post_maps_a_lost_slot_race() {
        struct Racing;
        impl PostRepository for Racing {
            async fn all(&self) -> Result<Vec<QueuedPost>, PortError> {
                Ok(vec![])
            }
            async fn insert(&self, _p: &QueuedPost) -> Result<(), InsertError> {
                Ok(())
            }
            async fn get(&self, id: &str) -> Result<Option<QueuedPost>, PortError> {
                Ok(Some(post(id, 100)))
            }
            async fn update(&self, _p: &QueuedPost) -> Result<(), UpdateError> {
                Err(UpdateError::SlotTaken)
            }
            async fn delete(&self, _id: &str) -> Result<bool, PortError> {
                Ok(true)
            }
        }
        let e = pollster::block_on(update_post(&Racing, ID_A, changes(300), 0));
        assert_eq!(
            e,
            Err(AppError::Queue(QueueError::SlotTaken { slot_at: 300 }))
        );
    }

    #[test]
    fn delete_post_refuses_a_post_that_is_no_longer_queued() {
        let repo = repo_with(&[(ID_A, 100)]);
        let s = store();
        for now in [100, 101] {
            let e = pollster::block_on(delete_post(&repo, &s, ID_A, now));
            assert_eq!(e, Err(AppError::NotQueued), "now={now}");
        }
        assert_eq!(repo.posts.borrow().len(), 1);
        assert!(s.deleted.borrow().is_empty());
    }

    #[test]
    fn update_post_refuses_a_post_that_is_no_longer_queued() {
        let repo = repo_with(&[(ID_A, 100)]);
        for now in [100, 101] {
            let e = pollster::block_on(update_post(&repo, ID_A, changes(500), now));
            assert_eq!(e, Err(AppError::NotQueued), "now={now}");
        }
        assert_eq!(repo.posts.borrow()[0].title, "Title");
    }

    #[test]
    fn delete_post_does_not_ask_storage_to_delete_an_odd_video_key() {
        let repo = repo_with(&[(ID_A, 100)]);
        repo.posts.borrow_mut()[0].video_key = "legacy/../odd key".into();
        let s = store();
        let out = pollster::block_on(delete_post(&repo, &s, ID_A, 0)).unwrap();
        assert_eq!(out, DeleteOutcome::VideoLeftBehind);
        assert!(repo.posts.borrow().is_empty());
        assert!(s.deleted.borrow().is_empty());
    }
}
