//! Publishing ports (the provider, token refresh, video links, state storage)
//! and, below them, the `publish_due` use case. The Worker only plugs adapters in.

use crate::app::PortError;
use crate::publish::{
    claim, flag_if_stuck, record_failure, record_success, select_due, FailReason, PublishState,
    ScheduledPost,
};
use crate::queue::QueuedPost;
use crate::token::{StoredToken, TokenStore};
use std::fmt;

/// Short-lived access to the video bytes (a presigned link). A secret: never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct VideoAccess {
    url: String,
}

impl VideoAccess {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for VideoAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoAccess")
            .field("url", &"[redacted]")
            .finish()
    }
}

/// What the provider calls the published video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedRef {
    pub external_id: String,
}

/// Why the provider call failed, as the adapter classifies it. No provider text
/// is carried, so nothing sensitive can travel with the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// The token was refused or could not be refreshed.
    Auth,
    RateLimited,
    /// The provider refused this video or its metadata; retrying will not help.
    Rejected,
    /// Network or provider trouble that may pass.
    Transient,
}

impl From<PublishError> for FailReason {
    fn from(e: PublishError) -> Self {
        match e {
            PublishError::Auth => FailReason::Auth,
            PublishError::RateLimited => FailReason::RateLimited,
            PublishError::Rejected => FailReason::Rejected,
            PublishError::Transient => FailReason::Transient,
        }
    }
}

/// Provider port: uploads one video as a YouTube Short.
#[allow(async_fn_in_trait)]
pub trait Publisher {
    async fn publish(
        &self,
        post: &QueuedPost,
        video: &VideoAccess,
        token: &StoredToken,
    ) -> Result<PublishedRef, PublishError>;
}

/// Port: exchanges a token for a fresh one (the provider's refresh call).
#[allow(async_fn_in_trait)]
pub trait TokenRefresher {
    async fn refresh(&self, token: &StoredToken) -> Result<StoredToken, PublishError>;
}

/// Storage port: a short-lived read link for a stored video.
#[allow(async_fn_in_trait)]
pub trait VideoLinks {
    async fn access(&self, video_key: &str) -> Result<VideoAccess, PortError>;
}

/// Result of a compare-and-set on a post's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cas {
    Applied,
    /// The stored state was no longer `expected`; nothing was written.
    Conflict,
}

/// Persistence port for publishing state.
#[allow(async_fn_in_trait)]
pub trait PublishRepository {
    async fn list(&self) -> Result<Vec<ScheduledPost>, PortError>;
    /// Writes `next` only if the post's stored state still equals `expected`,
    /// in one atomic step (an `UPDATE ... WHERE state = expected`). This is what
    /// stops two overlapping runs from publishing the same post twice.
    async fn transition(
        &self,
        id: &str,
        expected: &PublishState,
        next: &PublishState,
    ) -> Result<Cas, PortError>;
}

/// What happened to one post in a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Published {
        external_id: String,
    },
    /// Failed, and put back in the queue to try again after a delay.
    Retrying(FailReason),
    Failed(FailReason),
    /// Another run claimed the post first; we did nothing.
    AlreadyClaimed,
    /// Left in Publishing too long. Needs a person; see `flag_if_stuck`.
    FlaggedStuck,
    /// The post changed under us after the provider call, so the result was not recorded.
    StateLost,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    /// (post id, outcome), in the order handled.
    pub outcomes: Vec<(String, Outcome)>,
}

/// Publishes every due post, one at a time. A failure on one post never stops the rest.
/// Only a storage failure (`PortError`) ends the run early.
pub async fn publish_due(
    now: i64,
    repo: &impl PublishRepository,
    publisher: &impl Publisher,
    tokens: &impl TokenStore,
    refresher: &impl TokenRefresher,
    links: &impl VideoLinks,
) -> Result<PublishReport, PortError> {
    let mut report = PublishReport::default();
    let posts = repo.list().await?;

    // Stuck posts first: they go to manual review and are never retried.
    for p in &posts {
        if let Some(next) = flag_if_stuck(&p.state, now) {
            if repo.transition(&p.post.id, &p.state, &next).await? == Cas::Applied {
                report
                    .outcomes
                    .push((p.post.id.clone(), Outcome::FlaggedStuck));
            }
        }
    }

    for p in select_due(&posts, now) {
        let outcome = publish_one(now, p, repo, publisher, tokens, refresher, links).await?;
        report.outcomes.push((p.post.id.clone(), outcome));
    }
    Ok(report)
}

/// Claims one post, calls the provider, and records the result.
async fn publish_one(
    now: i64,
    p: &ScheduledPost,
    repo: &impl PublishRepository,
    publisher: &impl Publisher,
    tokens: &impl TokenStore,
    refresher: &impl TokenRefresher,
    links: &impl VideoLinks,
) -> Result<Outcome, PortError> {
    let id = &p.post.id;
    let Ok(claimed) = claim(&p.state, now) else {
        return Ok(Outcome::AlreadyClaimed);
    };
    if repo.transition(id, &p.state, &claimed).await? == Cas::Conflict {
        return Ok(Outcome::AlreadyClaimed);
    }

    let result = upload(now, &p.post, publisher, tokens, refresher, links).await;
    // If the writes below fail after a successful upload, the post stays in
    // Publishing and is flagged for manual review later rather than re-posted.
    let (next, outcome) = match result {
        Ok(published) => (
            record_success(&claimed, &published.external_id),
            Outcome::Published {
                external_id: published.external_id,
            },
        ),
        Err(e) => {
            let reason = FailReason::from(e);
            let next = record_failure(&claimed, reason, now);
            let outcome = if matches!(next, Ok(PublishState::Queued { .. })) {
                Outcome::Retrying(reason)
            } else {
                Outcome::Failed(reason)
            };
            (next, outcome)
        }
    };
    let Ok(next) = next else {
        return Ok(Outcome::StateLost);
    };
    match repo.transition(id, &claimed, &next).await? {
        Cas::Applied => Ok(outcome),
        Cas::Conflict => Ok(Outcome::StateLost),
    }
}

/// Everything between "claimed" and "provider answered": token, video link, upload.
async fn upload(
    now: i64,
    post: &QueuedPost,
    publisher: &impl Publisher,
    tokens: &impl TokenStore,
    refresher: &impl TokenRefresher,
    links: &impl VideoLinks,
) -> Result<PublishedRef, PublishError> {
    let stored = tokens
        .load()
        .await
        .map_err(|_| PublishError::Transient)?
        .ok_or(PublishError::Auth)?;
    let token = if stored.needs_refresh(now) {
        let fresh = refresher.refresh(&stored).await?;
        // A refresh that cannot be saved is not safe to carry on with: the
        // provider may have rotated the refresh token.
        tokens
            .save(&fresh)
            .await
            .map_err(|_| PublishError::Transient)?;
        fresh
    } else {
        stored
    };
    let video = links
        .access(&post.video_key)
        .await
        .map_err(|_| PublishError::Transient)?;
    publisher.publish(post, &video, &token).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_access_debug_hides_the_link() {
        let v = VideoAccess::new("https://r2.example/videos/a.mp4?X-Sig=SECRET");
        let out = format!("{v:?}");
        assert!(
            !out.contains("SECRET") && !out.contains("r2.example"),
            "{out}"
        );
        assert!(out.contains("[redacted]"));
        assert_eq!(v.url(), "https://r2.example/videos/a.mp4?X-Sig=SECRET");
    }

    #[test]
    fn provider_errors_map_to_fixed_reason_codes() {
        assert_eq!(FailReason::from(PublishError::Auth), FailReason::Auth);
        assert_eq!(
            FailReason::from(PublishError::RateLimited),
            FailReason::RateLimited
        );
        assert_eq!(
            FailReason::from(PublishError::Rejected),
            FailReason::Rejected
        );
        assert_eq!(
            FailReason::from(PublishError::Transient),
            FailReason::Transient
        );
    }

    use crate::queue::Platform;
    use crate::token::fakes::FakeTokenStore;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    const NOW: i64 = 10_000;

    fn scheduled(id: &str, slot_at: i64) -> ScheduledPost {
        ScheduledPost {
            post: QueuedPost {
                id: id.into(),
                video_key: format!("videos/{id}.mp4"),
                title: "T".into(),
                caption: "C".into(),
                platforms: vec![Platform::YouTube],
                slot_at,
            },
            state: PublishState::new_queued(),
        }
    }

    #[derive(Default)]
    struct FakeRepo {
        posts: RefCell<Vec<ScheduledPost>>,
        /// Simulates another run winning the claim between our read and our write.
        steal_claim: Cell<bool>,
    }
    impl FakeRepo {
        fn with(posts: Vec<ScheduledPost>) -> Self {
            Self {
                posts: RefCell::new(posts),
                steal_claim: Cell::new(false),
            }
        }
        fn state(&self, id: &str) -> PublishState {
            let posts = self.posts.borrow();
            posts
                .iter()
                .find(|p| p.post.id == id)
                .unwrap()
                .state
                .clone()
        }
    }
    impl PublishRepository for FakeRepo {
        async fn list(&self) -> Result<Vec<ScheduledPost>, PortError> {
            Ok(self.posts.borrow().clone())
        }
        async fn transition(
            &self,
            id: &str,
            expected: &PublishState,
            next: &PublishState,
        ) -> Result<Cas, PortError> {
            let mut posts = self.posts.borrow_mut();
            let p = posts.iter_mut().find(|p| p.post.id == id).unwrap();
            if self.steal_claim.replace(false) {
                p.state = PublishState::Publishing {
                    attempt: 1,
                    started_at: NOW,
                };
            }
            if &p.state != expected {
                return Ok(Cas::Conflict);
            }
            p.state = next.clone();
            Ok(Cas::Applied)
        }
    }

    #[derive(Default)]
    struct FakePublisher {
        results: RefCell<VecDeque<Result<PublishedRef, PublishError>>>,
        calls: RefCell<Vec<String>>,
    }
    impl FakePublisher {
        fn returning(r: Vec<Result<PublishedRef, PublishError>>) -> Self {
            Self {
                results: RefCell::new(r.into()),
                calls: RefCell::default(),
            }
        }
    }
    impl Publisher for FakePublisher {
        async fn publish(
            &self,
            post: &QueuedPost,
            _video: &VideoAccess,
            _token: &StoredToken,
        ) -> Result<PublishedRef, PublishError> {
            self.calls.borrow_mut().push(post.id.clone());
            self.results
                .borrow_mut()
                .pop_front()
                .expect("unexpected publish")
        }
    }

    struct FakeRefresher {
        result: Result<StoredToken, PublishError>,
        calls: Cell<u32>,
    }
    impl TokenRefresher for FakeRefresher {
        async fn refresh(&self, _t: &StoredToken) -> Result<StoredToken, PublishError> {
            self.calls.set(self.calls.get() + 1);
            self.result.clone()
        }
    }

    struct FakeLinks;
    impl VideoLinks for FakeLinks {
        async fn access(&self, key: &str) -> Result<VideoAccess, PortError> {
            Ok(VideoAccess::new(format!("https://signed/{key}?sig=SECRET")))
        }
    }

    fn token(expires_at: i64, access: &str) -> StoredToken {
        StoredToken {
            access_token: access.into(),
            refresh_token: "1//SECRET-REFRESH".into(),
            expires_at,
        }
    }

    fn fresh_store() -> FakeTokenStore {
        let s = FakeTokenStore::default();
        *s.token.borrow_mut() = Some(token(NOW + 3600, "ya29.SECRET"));
        s
    }

    fn ok_ref(id: &str) -> Result<PublishedRef, PublishError> {
        Ok(PublishedRef {
            external_id: id.into(),
        })
    }

    fn refresher_ok() -> FakeRefresher {
        FakeRefresher {
            result: Ok(token(NOW + 7200, "ya29.NEW-SECRET")),
            calls: Cell::new(0),
        }
    }

    fn run(
        repo: &FakeRepo,
        publisher: &FakePublisher,
        tokens: &FakeTokenStore,
        refresher: &FakeRefresher,
    ) -> PublishReport {
        pollster::block_on(publish_due(
            NOW, repo, publisher, tokens, refresher, &FakeLinks,
        ))
        .unwrap()
    }

    #[test]
    fn publishes_a_due_post_and_records_the_external_id() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1), scheduled("later", NOW + 1)]);
        let publisher = FakePublisher::returning(vec![ok_ref("yt123")]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            report.outcomes,
            [(
                "a".to_string(),
                Outcome::Published {
                    external_id: "yt123".into()
                }
            )]
        );
        assert_eq!(
            repo.state("a"),
            PublishState::Published {
                external_id: "yt123".into()
            }
        );
        assert_eq!(repo.state("later"), PublishState::new_queued());
    }

    #[test]
    fn a_second_run_does_not_publish_the_post_again() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let publisher = FakePublisher::returning(vec![ok_ref("yt1")]);
        let tokens = fresh_store();
        run(&repo, &publisher, &tokens, &refresher_ok());
        let second = run(&repo, &publisher, &tokens, &refresher_ok());
        assert!(second.outcomes.is_empty());
        assert_eq!(publisher.calls.borrow().len(), 1);
    }

    #[test]
    fn losing_the_claim_race_means_no_publish() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        repo.steal_claim.set(true);
        let publisher = FakePublisher::default();
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            report.outcomes,
            [("a".to_string(), Outcome::AlreadyClaimed)]
        );
        assert!(publisher.calls.borrow().is_empty());
    }

    #[test]
    fn an_auth_failure_fails_the_post_without_retry() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let publisher = FakePublisher::returning(vec![Err(PublishError::Auth)]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.outcomes[0].1, Outcome::Failed(FailReason::Auth));
        assert_eq!(
            repo.state("a"),
            PublishState::Failed {
                attempts: 1,
                reason: FailReason::Auth
            }
        );
    }

    #[test]
    fn a_missing_token_is_an_auth_failure_and_the_provider_is_not_called() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let publisher = FakePublisher::default();
        let report = run(
            &repo,
            &publisher,
            &FakeTokenStore::default(),
            &refresher_ok(),
        );
        assert_eq!(report.outcomes[0].1, Outcome::Failed(FailReason::Auth));
        assert!(publisher.calls.borrow().is_empty());
    }
}
