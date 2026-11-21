//! Use cases. They talk to the outside world only through the ports below,
//! so the Worker (and later an AWS adapter) plugs in without changing them.

use crate::media::{UploadError, VideoUpload};
use crate::queue::{Queue, QueueError, QueuedPost};

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

/// Why an insert failed. `SlotTaken` is the lost-race case: another request
/// took the slot between our check and the insert.
#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    SlotTaken,
    Port(PortError),
}

/// Persistence port for queued posts.
#[allow(async_fn_in_trait)]
pub trait PostRepository {
    async fn all(&self) -> Result<Vec<QueuedPost>, PortError>;
    /// Must fail if the slot is already taken, so two requests can't race.
    async fn insert(&self, post: &QueuedPost) -> Result<(), InsertError>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppError {
    Upload(UploadError),
    Queue(QueueError),
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

pub async fn queue_post(repo: &impl PostRepository, post: QueuedPost) -> Result<(), AppError> {
    let existing = repo.all().await.map_err(AppError::Port)?;
    let mut queue = Queue::from_posts(existing).map_err(AppError::Queue)?;
    queue.add(post.clone()).map_err(AppError::Queue)?;
    let slot_at = post.slot_at;
    repo.insert(&post).await.map_err(|e| match e {
        InsertError::SlotTaken => AppError::Queue(QueueError::SlotTaken { slot_at }),
        InsertError::Port(p) => AppError::Port(p),
    })
}

pub async fn upcoming(repo: &impl PostRepository, now: i64) -> Result<Vec<QueuedPost>, AppError> {
    let queue =
        Queue::from_posts(repo.all().await.map_err(AppError::Port)?).map_err(AppError::Queue)?;
    Ok(queue.upcoming(now).into_iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Platform;
    use std::cell::RefCell;

    struct FakeSigner;
    impl UploadSigner for FakeSigner {
        fn put_url(&self, key: &str, ct: &str, secs: u32) -> Result<String, PortError> {
            Ok(format!("https://signed/{key}?ct={ct}&exp={secs}"))
        }
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
        pollster::block_on(queue_post(&repo, post("a", 100))).unwrap();
        assert_eq!(repo.posts.borrow().len(), 1);
    }

    #[test]
    fn queue_post_refuses_a_taken_slot_without_storing() {
        let repo = FakeRepo::default();
        repo.posts.borrow_mut().push(post("a", 100));
        let e = pollster::block_on(queue_post(&repo, post("b", 100)));
        assert_eq!(
            e,
            Err(AppError::Queue(QueueError::SlotTaken { slot_at: 100 }))
        );
        assert_eq!(repo.posts.borrow().len(), 1);
    }

    #[test]
    fn queue_post_maps_a_lost_slot_race_to_slot_taken() {
        let e = pollster::block_on(queue_post(&RacingRepo, post("a", 100)));
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
}
