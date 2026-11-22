//! Publishing ports (the provider, token refresh, video links, state storage)
//! and, below them, the `publish_due` use case. The Worker only plugs adapters in.

use crate::app::PortError;
use crate::publish::{
    claim, flag_if_stuck, is_valid_external_id, record_failure, record_success, select_due,
    FailReason, PublishState, ScheduledPost,
};
use crate::queue::QueuedPost;
use crate::token::{StoredToken, TokenLoad, TokenStore};
use crate::youtube::MAX_PUBLISH_BYTES;
use std::fmt;

/// Where a stored video is and how big it is, which is all a publisher needs to read it
/// from storage itself. No link is ever handed out. The key is hidden from `Debug`
/// anyway, so a stray `{:?}` shows nothing about storage.
// Do NOT derive or implement Serialize (or Display) for this type.
#[derive(Clone, PartialEq, Eq)]
pub struct VideoAccess {
    key: String,
    size: u64,
}

impl VideoAccess {
    pub fn new(key: impl Into<String>, size: u64) -> Self {
        Self {
            key: key.into(),
            size,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Size in bytes, as storage reports it.
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl fmt::Debug for VideoAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoAccess")
            .field("key", &"[redacted]")
            .field("size", &self.size)
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
    /// `retry_at` (Unix seconds) is the provider's hint, if it gave one.
    RateLimited { retry_at: Option<i64> },
    /// The provider refused this video or its metadata; retrying will not help.
    Rejected,
    /// The provider definitely did not create the video, so retrying is safe.
    /// If there is any doubt, an adapter must return `Unknown` instead.
    Transient,
    /// The outcome is unknown: the provider may have accepted the upload (a timeout
    /// after sending, a dropped response). Never retried; goes to manual review.
    Unknown,
}

impl From<PublishError> for FailReason {
    fn from(e: PublishError) -> Self {
        match e {
            PublishError::Auth => FailReason::Auth,
            PublishError::RateLimited { .. } => FailReason::RateLimited,
            PublishError::Unknown => FailReason::Unknown,
            PublishError::Rejected => FailReason::Rejected,
            PublishError::Transient => FailReason::Transient,
        }
    }
}

/// Provider port: uploads one video as a YouTube Short.
///
/// Error contract: `Transient` means the provider definitely did NOT create the
/// video. Any failure where it might have (timeout after the body was sent, a
/// lost response, an unparsable success) must be `Unknown`.
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
/// Return `Auth` only when the provider says the refresh token itself is no longer
/// valid: it marks the account as needing a reconnect until the owner signs in again.
#[allow(async_fn_in_trait)]
pub trait TokenRefresher {
    async fn refresh(&self, token: &StoredToken) -> Result<StoredToken, PublishError>;
}

/// Storage port: finds a stored video (its key and size, read from storage).
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
    /// What a run needs, as of `now` (Unix seconds from the run's clock): the Queued
    /// posts that are due (slot reached and any retry delay over) and every Publishing
    /// post (to flag the stuck ones). Not Published or Failed posts: history stays out.
    /// The use case still checks `is_due`, so an adapter may return a little extra.
    async fn list(&self, now: i64) -> Result<Vec<ScheduledPost>, PortError>;
    /// Writes `next` only if the post's stored state still equals `expected`,
    /// in one atomic step (an `UPDATE ... WHERE state = expected`). This is what
    /// stops two overlapping runs from publishing the same post twice.
    ///
    /// The comparison must cover the WHOLE state: the variant and every field
    /// (attempts, not_before, started_at, ...), not just the status column. A
    /// post that does not exist returns `Cas::Conflict`, not an error.
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
    /// Put back in the queue to try again later (a transient failure, a rate
    /// limit, or `Auth`, which did not use up an attempt).
    Retrying(FailReason),
    Failed(FailReason),
    /// Another run claimed the post first; we did nothing.
    AlreadyClaimed,
    /// The claim was undone because the token could not be refreshed. The post is
    /// back exactly as it was: no attempt used, nothing sent to the provider this turn.
    Released,
    /// Left in Publishing too long. Needs a person; see `flag_if_stuck`.
    FlaggedStuck,
    /// The final state write did not land (the post changed under us, or storage
    /// failed). `external_id` is the provider's id if the upload succeeded, so the
    /// caller can log it. The post stays in Publishing (even for a failure we knew about)
    /// and is flagged for manual review once `PUBLISH_TIMEOUT_SECS` passes.
    StateLost {
        external_id: Option<String>,
    },
}

/// Why a run stopped before working through every due post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// No usable token: none stored, or the provider refused it or the refresh.
    /// The owner must reconnect the account.
    NeedsReconnect,
    /// The token could not be refreshed for a reason that may pass.
    TokenUnavailable,
    /// The provider rate-limited us; the rest wait for the next run.
    RateLimited,
    /// Storage failed.
    Storage,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    /// (post id, outcome), in the order handled.
    pub outcomes: Vec<(String, Outcome)>,
    pub halted: Option<Halt>,
}

/// Publishes due posts one at a time, reading `clock` (Unix seconds) afresh for every
/// time-dependent step. Posts that were not reached stay Queued and unclaimed. A failure
/// on one post does not stop the next, except `Auth` and rate limits, which would hit
/// every post and so end the run (`PublishReport::halted`).
/// Only a failure to list posts is an `Err`; later storage errors end the run as `Halt::Storage`.
pub async fn publish_due(
    clock: &impl Fn() -> i64,
    repo: &impl PublishRepository,
    publisher: &impl Publisher,
    tokens: &impl TokenStore,
    refresher: &impl TokenRefresher,
    links: &impl VideoLinks,
) -> Result<PublishReport, PortError> {
    let run = Run {
        clock,
        repo,
        publisher,
        tokens,
        refresher,
        links,
    };
    let posts = repo.list(clock()).await?;
    let mut report = PublishReport::default();
    if run.flag_stuck(&posts, &mut report).await.is_err() {
        report.halted = Some(Halt::Storage);
        return Ok(report);
    }
    let due = select_due(&posts, clock());
    if due.is_empty() {
        return Ok(report);
    }
    // One token check for the run: if it fails, nothing is claimed.
    let mut token = match run.load_fresh_token().await {
        Ok(t) => t,
        Err(e) => {
            report.halted = Some(token_halt(e));
            return Ok(report);
        }
    };
    for p in due {
        // Before the claim: a token problem is not an upload outcome, so it must
        // never fail a post that was not uploaded. Nothing is claimed.
        if let Err(e) = run.refresh_if_needed(&mut token).await {
            report.halted = Some(token_halt(e));
            break;
        }
        match run.publish_one(p, &mut token).await {
            Err(_) => {
                report.halted = Some(Halt::Storage);
                break;
            }
            Ok(step) => {
                report.outcomes.push((p.post.id.clone(), step.outcome));
                if step.halt.is_some() {
                    report.halted = step.halt;
                    break;
                }
            }
        }
    }
    Ok(report)
}

/// A token problem halts the run: `Auth` needs the owner, anything else may pass.
fn token_halt(e: PublishError) -> Halt {
    match e {
        PublishError::Auth => Halt::NeedsReconnect,
        _ => Halt::TokenUnavailable,
    }
}

/// Provider answers that would hit every post, so the run stops. Derived from the
/// error itself, so it holds even if the post's own state write is lost.
fn error_halt(e: PublishError) -> Option<Halt> {
    match e {
        PublishError::Auth => Some(Halt::NeedsReconnect),
        PublishError::RateLimited { .. } => Some(Halt::RateLimited),
        _ => None,
    }
}

/// Result of the one retry after a provider `Auth`.
enum Retried {
    Done(Result<PublishedRef, PublishError>),
    /// The forced refresh failed for a reason other than `Auth`.
    TokenTrouble,
}

struct Step {
    outcome: Outcome,
    halt: Option<Halt>,
}

impl Step {
    fn of(outcome: Outcome) -> Self {
        Step {
            outcome,
            halt: None,
        }
    }
}

/// The ports and clock for one run.
struct Run<'a, C, R, P, T, F, L> {
    clock: &'a C,
    repo: &'a R,
    publisher: &'a P,
    tokens: &'a T,
    refresher: &'a F,
    links: &'a L,
}

impl<C, R, P, T, F, L> Run<'_, C, R, P, T, F, L>
where
    C: Fn() -> i64,
    R: PublishRepository,
    P: Publisher,
    T: TokenStore,
    F: TokenRefresher,
    L: VideoLinks,
{
    /// Stuck posts go to manual review and are never retried.
    async fn flag_stuck(
        &self,
        posts: &[ScheduledPost],
        report: &mut PublishReport,
    ) -> Result<(), PortError> {
        for p in posts {
            let Some(next) = flag_if_stuck(&p.state, (self.clock)()) else {
                continue;
            };
            if self.repo.transition(&p.post.id, &p.state, &next).await? == Cas::Applied {
                report
                    .outcomes
                    .push((p.post.id.clone(), Outcome::FlaggedStuck));
            }
        }
        Ok(())
    }

    async fn load_fresh_token(&self) -> Result<StoredToken, PublishError> {
        // The provider kept refusing the stored token: nothing runs until the owner
        // reconnects (which saves a new token and clears the mark).
        if self
            .tokens
            .needs_reconnect()
            .await
            .map_err(|_| PublishError::Transient)?
        {
            return Err(PublishError::Auth);
        }
        let mut token = match self
            .tokens
            .load()
            .await
            .map_err(|_| PublishError::Transient)?
        {
            TokenLoad::Token(t) => t,
            TokenLoad::Missing => return Err(PublishError::Auth),
            TokenLoad::Unreadable => {
                // The row exists but cannot be decrypted (a lost or changed key): only
                // the owner reconnecting fixes it. The mark applies only if the row is
                // still the version just read; a failed mark is ignored, the run halts anyway.
                let _ = self.tokens.mark_needs_reconnect().await;
                return Err(PublishError::Auth);
            }
        };
        self.refresh_if_needed(&mut token).await?;
        Ok(token)
    }

    async fn refresh_if_needed(&self, token: &mut StoredToken) -> Result<(), PublishError> {
        if !token.needs_refresh((self.clock)()) {
            return Ok(());
        }
        self.refresh(token).await
    }

    /// Refreshes whether or not the token looks expired.
    async fn refresh(&self, token: &mut StoredToken) -> Result<(), PublishError> {
        let mut fresh = match self.refresher.refresh(token).await {
            Ok(t) => t,
            Err(PublishError::Auth) => {
                // The refresh token itself is refused (revoked or expired): only the
                // owner reconnecting fixes it. A failed mark is ignored; the run halts anyway.
                let _ = self.tokens.mark_needs_reconnect().await;
                return Err(PublishError::Auth);
            }
            Err(e) => return Err(e),
        };
        // Providers often leave the refresh token out of a refresh response.
        if fresh.refresh_token.is_empty() {
            fresh.refresh_token = token.refresh_token.clone();
        }
        // A refresh that cannot be saved is not safe to carry on with: the
        // provider may have rotated the refresh token. A save that did not apply
        // means the owner reconnected since this run loaded: the token held here is
        // stale, so the run stops rather than carry on with it.
        let applied = self
            .tokens
            .save(&fresh)
            .await
            .map_err(|_| PublishError::Transient)?;
        if !applied {
            return Err(PublishError::Transient);
        }
        *token = fresh;
        Ok(())
    }

    /// Claims one post, calls the provider, and records the result. `Err` only
    /// when the claim itself could not be stored (nothing was sent to the provider).
    async fn publish_one(
        &self,
        p: &ScheduledPost,
        token: &mut StoredToken,
    ) -> Result<Step, PortError> {
        let id = &p.post.id;
        let claimed = match claim(&p.state, (self.clock)()) {
            Ok(c) => c,
            Err(_) => return Ok(Step::of(Outcome::AlreadyClaimed)),
        };
        if self.repo.transition(id, &p.state, &claimed).await? == Cas::Conflict {
            return Ok(Step::of(Outcome::AlreadyClaimed));
        }

        let mut result = self.upload(&p.post, token).await;
        if result == Err(PublishError::Auth) {
            match self.retry_after_refresh(&p.post, token).await {
                Retried::Done(r) => result = r,
                Retried::TokenTrouble => return self.release(id, &claimed, &p.state).await,
            }
        }
        let now = (self.clock)();
        let external_id = result.as_ref().ok().map(|r| r.external_id.clone());
        let halt = result.as_ref().err().copied().and_then(error_halt);
        let (next, outcome) = match result {
            Ok(published) => (
                record_success(&claimed, &published.external_id),
                Outcome::Published {
                    external_id: published.external_id,
                },
            ),
            Err(e) => {
                let reason = FailReason::from(e);
                let retry_at = match e {
                    PublishError::RateLimited { retry_at } => retry_at,
                    _ => None,
                };
                let next = record_failure(&claimed, reason, now, retry_at);
                let outcome = if matches!(next, Ok(PublishState::Queued { .. })) {
                    Outcome::Retrying(reason)
                } else {
                    Outcome::Failed(reason)
                };
                (next, outcome)
            }
        };
        let lost = Step {
            outcome: Outcome::StateLost { external_id },
            halt,
        };
        // `claimed` is always Publishing, so the transitions above cannot fail;
        // if that ever changed, report the loss rather than panic.
        let Ok(next) = next else { return Ok(lost) };
        // If the write fails after a successful upload the post stays in Publishing
        // and is flagged for manual review later, never re-posted.
        match self.repo.transition(id, &claimed, &next).await {
            Ok(Cas::Applied) => Ok(Step { outcome, halt }),
            Ok(Cas::Conflict) => Ok(lost),
            Err(_) => Ok(Step {
                halt: Some(Halt::Storage),
                ..lost
            }),
        }
    }

    /// The provider refused the token. That is safe to retry (nothing was created), so
    /// force one refresh and try once more. If the provider still refuses, mark the
    /// token so later runs stop at once; a failed mark is ignored because the post is
    /// requeued and the run halts either way. A refresh that fails for any reason but
    /// `Auth` is a token problem, not a verdict on this post (see `release`).
    async fn retry_after_refresh(&self, post: &QueuedPost, token: &mut StoredToken) -> Retried {
        let result = match self.refresh(token).await {
            Ok(()) => self.upload(post, token).await,
            Err(PublishError::Auth) => Err(PublishError::Auth),
            Err(_) => return Retried::TokenTrouble,
        };
        if result == Err(PublishError::Auth) {
            let _ = self.tokens.mark_needs_reconnect().await;
        }
        Retried::Done(result)
    }

    /// Undoes a claim: writes the pre-claim state back, so no attempt is used and the
    /// post is exactly as it was. The run halts with `TokenUnavailable`, with no
    /// reconnect mark, because the owner has nothing to do.
    async fn release(
        &self,
        id: &str,
        claimed: &PublishState,
        before: &PublishState,
    ) -> Result<Step, PortError> {
        let halt = Some(Halt::TokenUnavailable);
        let lost = |halt| Step {
            outcome: Outcome::StateLost { external_id: None },
            halt,
        };
        Ok(match self.repo.transition(id, claimed, before).await {
            Ok(Cas::Applied) => Step {
                outcome: Outcome::Released,
                halt,
            },
            Ok(Cas::Conflict) => lost(halt),
            Err(_) => lost(Some(Halt::Storage)),
        })
    }

    /// Between "claimed" and "provider answered": token, video link, upload.
    async fn upload(
        &self,
        post: &QueuedPost,
        token: &StoredToken,
    ) -> Result<PublishedRef, PublishError> {
        let video = self
            .links
            .access(&post.video_key)
            .await
            .map_err(|_| PublishError::Transient)?;
        // Too big for one run's subrequest allowance (or empty): refuse it here, before
        // anything is sent. Uploads are capped at the same size, so this is a backstop
        // for an object that was replaced after it was queued.
        if video.size() == 0 || video.size() > MAX_PUBLISH_BYTES {
            return Err(PublishError::Rejected);
        }
        let published = self.publisher.publish(post, &video, token).await?;
        // The upload succeeded but the id is not one we will store or trust: the video
        // may exist, so this is an unknown outcome, never a retry.
        if !is_valid_external_id(&published.external_id) {
            return Err(PublishError::Unknown);
        }
        Ok(published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_access_debug_hides_the_key() {
        let v = VideoAccess::new("videos/SECRET-KEY.mp4", 1234);
        let out = format!("{v:?} {v:#?}");
        assert!(!out.contains("SECRET") && !out.contains("videos/"), "{out}");
        assert!(out.contains("[redacted]") && out.contains("1234"));
        assert_eq!((v.key(), v.size()), ("videos/SECRET-KEY.mp4", 1234));
    }

    #[test]
    fn provider_errors_map_to_fixed_reason_codes() {
        assert_eq!(FailReason::from(PublishError::Auth), FailReason::Auth);
        assert_eq!(
            FailReason::from(PublishError::RateLimited { retry_at: None }),
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

    use crate::publish::{MAX_ATTEMPTS, PUBLISH_TIMEOUT_SECS};
    use crate::queue::Platform;
    use crate::token::fakes::FakeTokenStore;
    use crate::youtube::MAX_PUBLISH_BYTES;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

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
        /// Simulates another writer changing the post while the provider call ran.
        steal_final: Cell<bool>,
        /// Storage fails on a write from Publishing (a final write or a stuck flag), once
        /// this many of them have gone through. `None`: never fails.
        fail_final: Cell<Option<u32>>,
        /// The `now` each `list` call was given.
        listed_at: RefCell<Vec<i64>>,
    }
    impl FakeRepo {
        fn with(posts: Vec<ScheduledPost>) -> Self {
            Self {
                posts: RefCell::new(posts),
                ..Self::default()
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
        async fn list(&self, now: i64) -> Result<Vec<ScheduledPost>, PortError> {
            self.listed_at.borrow_mut().push(now);
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
            if matches!(expected, PublishState::Publishing { .. }) {
                match self.fail_final.get() {
                    Some(0) => return Err(PortError("db down".into())),
                    Some(n) => self.fail_final.set(Some(n - 1)),
                    None => {}
                }
                if self.steal_final.get() {
                    p.state = PublishState::new_queued();
                }
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
        /// (access token, video url, Debug of both) as the provider call received them.
        seen: RefCell<Vec<(String, String, String)>>,
        /// Moves the shared clock forward by `.1` seconds on every call.
        advance: Option<(Rc<Cell<i64>>, i64)>,
    }
    impl FakePublisher {
        fn returning(r: Vec<Result<PublishedRef, PublishError>>) -> Self {
            Self {
                results: RefCell::new(r.into()),
                ..Self::default()
            }
        }
    }
    impl Publisher for FakePublisher {
        async fn publish(
            &self,
            post: &QueuedPost,
            video: &VideoAccess,
            token: &StoredToken,
        ) -> Result<PublishedRef, PublishError> {
            self.calls.borrow_mut().push(post.id.clone());
            self.seen.borrow_mut().push((
                token.access_token.clone(),
                video.key().to_string(),
                format!("{token:?} {video:?}"),
            ));
            if let Some((clock, secs)) = &self.advance {
                clock.set(clock.get() + secs);
            }
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
            Ok(VideoAccess::new(format!("{key}?SECRET"), 1_000_000))
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
            &|| NOW,
            repo,
            publisher,
            tokens,
            refresher,
            &FakeLinks,
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
    fn the_run_lists_posts_as_of_its_own_clock() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let publisher = FakePublisher::returning(vec![ok_ref("yt1")]);
        run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(*repo.listed_at.borrow(), [NOW]);
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
    fn auth_twice_requeues_the_post_without_an_attempt_and_marks_the_token() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let tokens = fresh_store();
        let refresher = refresher_ok();
        let publisher =
            FakePublisher::returning(vec![Err(PublishError::Auth), Err(PublishError::Auth)]);
        let report = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(report.outcomes[0].1, Outcome::Retrying(FailReason::Auth));
        assert_eq!(
            repo.state("a"),
            PublishState::Queued {
                attempts: 0,
                not_before: NOW
            }
        );
        assert_eq!(refresher.calls.get(), 1);
        assert!(tokens.reconnect.get());
    }

    #[test]
    fn auth_once_forces_a_refresh_and_the_retry_uses_the_new_token() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let tokens = fresh_store();
        let publisher = FakePublisher::returning(vec![Err(PublishError::Auth), ok_ref("yt1")]);
        let report = run(&repo, &publisher, &tokens, &refresher_ok());
        assert_eq!(
            report.outcomes[0].1,
            Outcome::Published {
                external_id: "yt1".into()
            }
        );
        let seen = publisher.seen.borrow();
        assert_eq!(seen[0].0, "ya29.SECRET");
        assert_eq!(seen[1].0, "ya29.NEW-SECRET");
        assert!(!tokens.reconnect.get());
        assert_eq!(report.halted, None);
    }

    #[test]
    fn auth_then_a_refresh_the_provider_refuses_marks_the_token() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let tokens = fresh_store();
        let refresher = FakeRefresher {
            result: Err(PublishError::Auth),
            calls: Cell::new(0),
        };
        let publisher = FakePublisher::returning(vec![Err(PublishError::Auth)]);
        let report = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(report.halted, Some(Halt::NeedsReconnect));
        assert!(tokens.reconnect.get());
        assert_eq!(publisher.calls.borrow().len(), 1);
    }

    #[test]
    fn auth_then_a_refresh_that_hit_trouble_releases_the_post_and_halts() {
        for err in [
            PublishError::Transient,
            PublishError::Unknown,
            PublishError::Rejected,
            PublishError::RateLimited { retry_at: None },
        ] {
            let repo = two_due();
            let tokens = fresh_store();
            let refresher = FakeRefresher {
                result: Err(err),
                calls: Cell::new(0),
            };
            let publisher = FakePublisher::returning(vec![Err(PublishError::Auth)]);
            let report = run(&repo, &publisher, &tokens, &refresher);
            assert_eq!(report.outcomes, [("a".to_string(), Outcome::Released)]);
            assert_eq!(report.halted, Some(Halt::TokenUnavailable), "{err:?}");
            // Exactly as before the claim: no attempt used, nothing failed.
            assert_eq!(repo.state("a"), PublishState::new_queued());
            assert_eq!(repo.state("b"), PublishState::new_queued());
            assert!(!tokens.reconnect.get());
        }
    }

    #[test]
    fn a_refresh_that_keeps_failing_never_burns_attempts_or_claims_the_next_post() {
        let repo = two_due();
        let tokens = FakeTokenStore::default();
        // Valid for all three runs; only the provider's Auth forces a refresh.
        *tokens.token.borrow_mut() = Some(token(NOW + 1_000_000, "ya29.SECRET"));
        let refresher = FakeRefresher {
            result: Err(PublishError::Transient),
            calls: Cell::new(0),
        };
        let publisher = FakePublisher::returning(vec![Err(PublishError::Auth); 3]);
        let now = Cell::new(NOW);
        for _ in 0..3 {
            let report = pollster::block_on(publish_due(
                &|| now.get(),
                &repo,
                &publisher,
                &tokens,
                &refresher,
                &FakeLinks,
            ))
            .unwrap();
            assert_eq!(report.halted, Some(Halt::TokenUnavailable));
            now.set(now.get() + 10_000);
        }
        assert_eq!(repo.state("a"), PublishState::new_queued());
        assert_eq!(repo.state("b"), PublishState::new_queued());
        assert_eq!(*publisher.calls.borrow(), ["a", "a", "a"]);
    }

    /// The provider is called at most twice in total (the call, and one retry after a
    /// forced refresh), and a second run does not call it at all.
    #[test]
    fn a_token_the_provider_keeps_refusing_stops_all_later_runs() {
        let repo = two_due();
        let tokens = fresh_store();
        let refresher = refresher_ok();
        let publisher = FakePublisher::returning(vec![Err(PublishError::Auth); 4]);
        let first = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(first.halted, Some(Halt::NeedsReconnect));
        assert_eq!(publisher.calls.borrow().len(), 2);
        let second = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(second.halted, Some(Halt::NeedsReconnect));
        assert!(second.outcomes.is_empty());
        assert_eq!(publisher.calls.borrow().len(), 2);
        assert_eq!(repo.state("b"), PublishState::new_queued());
        // A new owner connect clears the mark.
        pollster::block_on(tokens.connect(&token(NOW + 3600, "ya29.RECONNECTED"))).unwrap();
        assert!(!tokens.reconnect.get());
    }

    #[test]
    fn a_requeue_write_conflict_after_auth_still_halts() {
        let repo = two_due();
        repo.steal_final.set(true);
        let publisher =
            FakePublisher::returning(vec![Err(PublishError::Auth), Err(PublishError::Auth)]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.halted, Some(Halt::NeedsReconnect));
        assert_eq!(report.outcomes.len(), 1);
    }

    #[test]
    fn an_expiring_token_is_refreshed_saved_and_used() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW + 10, "ya29.OLD"));
        let refresher = refresher_ok();
        let publisher = FakePublisher::returning(vec![ok_ref("y")]);
        run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(refresher.calls.get(), 1);
        let saved = tokens.token.borrow().clone().unwrap();
        assert_eq!(saved.access_token, "ya29.NEW-SECRET");
        // The provider call used the NEW token, not the stored one.
        assert_eq!(publisher.seen.borrow()[0].0, "ya29.NEW-SECRET");
    }

    #[test]
    fn a_fresh_token_is_not_refreshed() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let refresher = refresher_ok();
        run(
            &repo,
            &FakePublisher::returning(vec![ok_ref("y")]),
            &fresh_store(),
            &refresher,
        );
        assert_eq!(refresher.calls.get(), 0);
    }

    #[test]
    fn a_rate_limit_requeues_with_backoff() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let publisher =
            FakePublisher::returning(vec![Err(PublishError::RateLimited { retry_at: None })]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            report.outcomes[0].1,
            Outcome::Retrying(FailReason::RateLimited)
        );
        assert_eq!(
            repo.state("a"),
            PublishState::Queued {
                attempts: 0,
                not_before: NOW + 3600
            }
        );
    }

    #[test]
    fn an_unknown_outcome_is_failed_for_review_and_never_requeued() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let publisher = FakePublisher::returning(vec![Err(PublishError::Unknown)]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.outcomes[0].1, Outcome::Failed(FailReason::Unknown));
        assert_eq!(
            repo.state("a"),
            PublishState::Failed {
                attempts: 1,
                reason: FailReason::Unknown
            }
        );
        assert!(run(&repo, &publisher, &fresh_store(), &refresher_ok())
            .outcomes
            .is_empty());
    }

    #[test]
    fn a_rejection_fails_the_post_at_once() {
        let repo = FakeRepo::with(vec![scheduled("a", NOW - 1)]);
        let publisher = FakePublisher::returning(vec![Err(PublishError::Rejected)]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.outcomes[0].1, Outcome::Failed(FailReason::Rejected));
    }

    #[test]
    fn transient_failures_retry_then_fail_after_the_last_attempt() {
        let repo = FakeRepo::with(vec![scheduled("a", 0)]);
        let tokens = fresh_store();
        let publisher =
            FakePublisher::returning(vec![Err(PublishError::Transient); MAX_ATTEMPTS as usize]);
        let now = Cell::new(NOW);
        let mut outcomes = Vec::new();
        for _ in 0..MAX_ATTEMPTS {
            let r = pollster::block_on(publish_due(
                &|| now.get(),
                &repo,
                &publisher,
                &tokens,
                &refresher_ok(),
                &FakeLinks,
            ))
            .unwrap();
            outcomes.push(r.outcomes[0].1.clone());
            now.set(now.get() + 10_000); // past any backoff
        }
        assert_eq!(
            outcomes,
            [
                Outcome::Retrying(FailReason::Transient),
                Outcome::Retrying(FailReason::Transient),
                Outcome::Failed(FailReason::Transient)
            ]
        );
        assert_eq!(
            repo.state("a"),
            PublishState::Failed {
                attempts: MAX_ATTEMPTS,
                reason: FailReason::Transient
            }
        );
    }

    #[test]
    fn a_retry_is_not_attempted_before_its_delay_is_over() {
        let repo = FakeRepo::with(vec![scheduled("a", 0)]);
        let tokens = fresh_store();
        let publisher = FakePublisher::returning(vec![Err(PublishError::Transient)]);
        run(&repo, &publisher, &tokens, &refresher_ok());
        let again = run(&repo, &publisher, &tokens, &refresher_ok());
        assert!(again.outcomes.is_empty());
    }

    #[test]
    fn a_post_stuck_in_publishing_is_flagged_and_never_retried() {
        let mut stuck = scheduled("stuck", 0);
        stuck.state = PublishState::Publishing {
            attempt: 1,
            started_at: NOW - PUBLISH_TIMEOUT_SECS,
        };
        let mut recent = scheduled("recent", 0);
        recent.state = PublishState::Publishing {
            attempt: 1,
            started_at: NOW - 1,
        };
        let repo = FakeRepo::with(vec![stuck, recent]);
        let publisher = FakePublisher::default();
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            report.outcomes,
            [("stuck".to_string(), Outcome::FlaggedStuck)]
        );
        assert_eq!(
            repo.state("stuck"),
            PublishState::Failed {
                attempts: 1,
                reason: FailReason::StuckPublishing
            }
        );
        assert!(matches!(
            repo.state("recent"),
            PublishState::Publishing { .. }
        ));
        assert!(publisher.calls.borrow().is_empty());
    }

    #[test]
    fn one_failure_does_not_stop_the_next_post() {
        let repo = FakeRepo::with(vec![scheduled("a", 1), scheduled("b", 2)]);
        let publisher = FakePublisher::returning(vec![Err(PublishError::Rejected), ok_ref("yt-b")]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.outcomes.len(), 2);
        assert_eq!(*publisher.calls.borrow(), ["a", "b"]);
    }

    fn two_due() -> FakeRepo {
        FakeRepo::with(vec![scheduled("a", 1), scheduled("b", 2)])
    }

    #[test]
    fn a_missing_token_stops_the_run_before_any_post_is_claimed() {
        let repo = two_due();
        let publisher = FakePublisher::default();
        let report = run(
            &repo,
            &publisher,
            &FakeTokenStore::default(),
            &refresher_ok(),
        );
        assert_eq!(report.halted, Some(Halt::NeedsReconnect));
        assert!(report.outcomes.is_empty());
        assert_eq!(repo.state("a"), PublishState::new_queued());
        assert_eq!(repo.state("b"), PublishState::new_queued());
        assert!(publisher.calls.borrow().is_empty());
    }

    #[test]
    fn a_refused_refresh_stops_the_run_and_leaves_posts_queued() {
        let repo = two_due();
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW, "old"));
        let refresher = FakeRefresher {
            result: Err(PublishError::Auth),
            calls: Cell::new(0),
        };
        let publisher = FakePublisher::default();
        let report = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(report.halted, Some(Halt::NeedsReconnect));
        assert_eq!(repo.state("a"), PublishState::new_queued());
        assert!(publisher.calls.borrow().is_empty());
    }

    #[test]
    fn a_refresh_that_hit_trouble_halts_without_asking_for_a_reconnect() {
        let repo = two_due();
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW, "old"));
        let refresher = FakeRefresher {
            result: Err(PublishError::Transient),
            calls: Cell::new(0),
        };
        let report = run(&repo, &FakePublisher::default(), &tokens, &refresher);
        assert_eq!(report.halted, Some(Halt::TokenUnavailable));
        assert_eq!(repo.state("a"), PublishState::new_queued());
    }

    #[test]
    fn a_run_with_nothing_due_does_not_touch_the_token() {
        let repo = FakeRepo::with(vec![scheduled("later", NOW + 1)]);
        let report = run(
            &repo,
            &FakePublisher::default(),
            &FakeTokenStore::default(),
            &refresher_ok(),
        );
        assert_eq!(report, PublishReport::default());
    }

    #[test]
    fn auth_from_the_provider_stops_the_run_and_requeues_only_that_post() {
        let repo = two_due();
        let publisher =
            FakePublisher::returning(vec![Err(PublishError::Auth), Err(PublishError::Auth)]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.halted, Some(Halt::NeedsReconnect));
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(
            repo.state("a"),
            PublishState::Queued {
                attempts: 0,
                not_before: NOW
            }
        );
        assert_eq!(repo.state("b"), PublishState::new_queued());
        assert_eq!(publisher.calls.borrow().len(), 2);
    }

    #[test]
    fn a_rate_limit_stops_the_run_and_honours_retry_at() {
        let repo = two_due();
        let publisher = FakePublisher::returning(vec![Err(PublishError::RateLimited {
            retry_at: Some(NOW + 500),
        })]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.halted, Some(Halt::RateLimited));
        assert_eq!(
            repo.state("a"),
            PublishState::Queued {
                attempts: 0,
                not_before: NOW + 500
            }
        );
        assert_eq!(repo.state("b"), PublishState::new_queued());
        assert_eq!(publisher.calls.borrow().len(), 1);
    }

    #[test]
    fn time_is_read_fresh_for_each_post() {
        let clock = Rc::new(Cell::new(NOW));
        let repo = two_due();
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW + 400, "ya29.OLD"));
        let refresher = refresher_ok();
        let publisher = FakePublisher {
            results: RefCell::new(
                vec![Err(PublishError::Transient), Err(PublishError::Transient)].into(),
            ),
            advance: Some((clock.clone(), 5_000)),
            ..FakePublisher::default()
        };
        let c = clock.clone();
        pollster::block_on(publish_due(
            &move || c.get(),
            &repo,
            &publisher,
            &tokens,
            &refresher,
            &FakeLinks,
        ))
        .unwrap();
        // Backoff is measured from the moment each post failed, not from the start of the run.
        let not_before = |id| match repo.state(id) {
            PublishState::Queued { not_before, .. } => not_before,
            other => panic!("{other:?}"),
        };
        assert_eq!(not_before("a"), NOW + 5_000 + 60);
        assert_eq!(not_before("b"), NOW + 10_000 + 60);
        // The token was fine for the first post and had expired by the second.
        assert_eq!(refresher.calls.get(), 1);
    }

    #[test]
    fn a_refresh_without_a_refresh_token_keeps_the_stored_one() {
        let repo = FakeRepo::with(vec![scheduled("a", 1)]);
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW, "ya29.OLD"));
        let mut renewed = token(NOW + 7200, "ya29.NEW");
        renewed.refresh_token = String::new();
        let refresher = FakeRefresher {
            result: Ok(renewed),
            calls: Cell::new(0),
        };
        run(
            &repo,
            &FakePublisher::returning(vec![ok_ref("y")]),
            &tokens,
            &refresher,
        );
        let saved = tokens.token.borrow().clone().unwrap();
        assert_eq!(saved.access_token, "ya29.NEW");
        assert_eq!(saved.refresh_token, "1//SECRET-REFRESH");
    }

    #[test]
    fn a_refresh_trouble_mid_run_halts_and_never_fails_or_claims_the_next_post() {
        let clock = Rc::new(Cell::new(NOW));
        let repo = two_due();
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW + 400, "ya29.OLD"));
        // Fine at run start; expired by the time post b is reached.
        let refresher = FakeRefresher {
            result: Err(PublishError::Unknown),
            calls: Cell::new(0),
        };
        let publisher = FakePublisher {
            results: RefCell::new(vec![ok_ref("yt1")].into()),
            advance: Some((clock.clone(), 5_000)),
            ..FakePublisher::default()
        };
        let c = clock.clone();
        let report = pollster::block_on(publish_due(
            &move || c.get(),
            &repo,
            &publisher,
            &tokens,
            &refresher,
            &FakeLinks,
        ))
        .unwrap();
        assert_eq!(report.halted, Some(Halt::TokenUnavailable));
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(repo.state("b"), PublishState::new_queued());
        assert_eq!(publisher.calls.borrow().len(), 1);
    }

    #[test]
    fn a_rate_limit_halts_even_when_its_requeue_write_conflicts() {
        let repo = two_due();
        repo.steal_final.set(true);
        let publisher =
            FakePublisher::returning(vec![Err(PublishError::RateLimited { retry_at: None })]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            report.outcomes[0].1,
            Outcome::StateLost { external_id: None }
        );
        assert_eq!(report.halted, Some(Halt::RateLimited));
        assert_eq!(publisher.calls.borrow().len(), 1);
    }

    #[test]
    fn a_post_left_in_publishing_keeps_the_claim_time_of_its_own_turn() {
        let clock = Rc::new(Cell::new(NOW));
        let repo = two_due();
        // Post a's final write goes through; post b's fails and leaves it in Publishing.
        repo.fail_final.set(Some(1));
        let publisher = FakePublisher {
            results: RefCell::new(vec![ok_ref("yt1"), ok_ref("yt2")].into()),
            advance: Some((clock.clone(), 5_000)),
            ..FakePublisher::default()
        };
        let c = clock.clone();
        let report = pollster::block_on(publish_due(
            &move || c.get(),
            &repo,
            &publisher,
            &fresh_store(),
            &refresher_ok(),
            &FakeLinks,
        ))
        .unwrap();
        assert_eq!(report.halted, Some(Halt::Storage));
        assert_eq!(
            repo.state("b"),
            PublishState::Publishing {
                attempt: 1,
                started_at: NOW + 5_000
            }
        );
    }

    #[test]
    fn a_storage_error_while_flagging_a_stuck_post_halts_before_anything_is_claimed() {
        let mut stuck = scheduled("stuck", 0);
        stuck.state = PublishState::Publishing {
            attempt: 1,
            started_at: NOW - PUBLISH_TIMEOUT_SECS,
        };
        let repo = FakeRepo::with(vec![stuck, scheduled("due", 1)]);
        repo.fail_final.set(Some(0));
        let publisher = FakePublisher::default();
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(report.halted, Some(Halt::Storage));
        assert!(report.outcomes.is_empty());
        assert_eq!(repo.state("due"), PublishState::new_queued());
        assert!(publisher.calls.borrow().is_empty());
    }

    #[test]
    fn a_refresh_the_provider_refuses_marks_the_token_so_the_next_run_stops_early() {
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW, "old"));
        let refresher = FakeRefresher {
            result: Err(PublishError::Auth),
            calls: Cell::new(0),
        };
        let repo = two_due();
        let publisher = FakePublisher::default();
        let first = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(first.halted, Some(Halt::NeedsReconnect));
        assert!(tokens.reconnect.get());
        let second = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(second.halted, Some(Halt::NeedsReconnect));
        assert_eq!(refresher.calls.get(), 1);
    }

    #[test]
    fn a_refresh_that_merely_hit_trouble_does_not_mark_the_token() {
        let tokens = FakeTokenStore::default();
        *tokens.token.borrow_mut() = Some(token(NOW, "old"));
        let refresher = FakeRefresher {
            result: Err(PublishError::Transient),
            calls: Cell::new(0),
        };
        run(&two_due(), &FakePublisher::default(), &tokens, &refresher);
        assert!(!tokens.reconnect.get());
    }

    #[test]
    fn a_refresh_that_is_rate_limited_or_unknown_at_run_start_halts_without_claiming() {
        for err in [
            PublishError::RateLimited { retry_at: None },
            PublishError::Unknown,
            PublishError::Rejected,
        ] {
            let repo = two_due();
            let tokens = FakeTokenStore::default();
            *tokens.token.borrow_mut() = Some(token(NOW, "old"));
            let refresher = FakeRefresher {
                result: Err(err),
                calls: Cell::new(0),
            };
            let publisher = FakePublisher::default();
            let report = run(&repo, &publisher, &tokens, &refresher);
            assert_eq!(report.halted, Some(Halt::TokenUnavailable), "{err:?}");
            assert_eq!(repo.state("a"), PublishState::new_queued());
            assert!(publisher.calls.borrow().is_empty());
        }
    }

    /// A known outcome (here a rejection) whose final write is lost leaves the post in
    /// Publishing. It is then flagged stuck once the timeout passes: manual review, not a
    /// retry, which is the safe side because the real outcome was not recorded.
    #[test]
    fn a_post_whose_failure_was_not_recorded_is_later_flagged_stuck() {
        let repo = FakeRepo::with(vec![scheduled("a", 1)]);
        repo.fail_final.set(Some(0));
        let publisher = FakePublisher::returning(vec![Err(PublishError::Rejected)]);
        let first = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            first.outcomes[0].1,
            Outcome::StateLost { external_id: None }
        );
        assert!(matches!(repo.state("a"), PublishState::Publishing { .. }));
        repo.fail_final.set(None);
        let later = pollster::block_on(publish_due(
            &|| NOW + PUBLISH_TIMEOUT_SECS,
            &repo,
            &publisher,
            &fresh_store(),
            &refresher_ok(),
            &FakeLinks,
        ))
        .unwrap();
        assert_eq!(later.outcomes, [("a".to_string(), Outcome::FlaggedStuck)]);
        assert_eq!(publisher.calls.borrow().len(), 1);
    }

    #[test]
    fn a_lost_final_write_still_reports_the_external_id() {
        let repo = FakeRepo::with(vec![scheduled("a", 1)]);
        repo.steal_final.set(true);
        let publisher = FakePublisher::returning(vec![ok_ref("yt9")]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            report.outcomes[0].1,
            Outcome::StateLost {
                external_id: Some("yt9".into())
            }
        );
        assert_eq!(report.halted, None);
    }

    #[test]
    fn a_storage_error_on_the_final_write_still_reports_the_external_id() {
        let repo = two_due();
        repo.fail_final.set(Some(0));
        let publisher = FakePublisher::returning(vec![ok_ref("yt9")]);
        let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
        assert_eq!(
            report.outcomes,
            [(
                "a".to_string(),
                Outcome::StateLost {
                    external_id: Some("yt9".into())
                }
            )]
        );
        assert_eq!(report.halted, Some(Halt::Storage));
        assert_eq!(publisher.calls.borrow().len(), 1);
    }

    #[test]
    fn secrets_reach_the_provider_but_never_a_debug_string() {
        let repo = two_due();
        let tokens = fresh_store();
        let publisher = FakePublisher::returning(vec![Err(PublishError::Transient), ok_ref("yt1")]);
        let report = run(&repo, &publisher, &tokens, &refresher_ok());
        // The fake did receive real secrets...
        let seen = publisher.seen.borrow();
        assert!(seen[0].0.contains("SECRET") && seen[0].1.contains("SECRET"));
        // ...yet nothing that prints them (what the provider call was given, the
        // report, the stored states, the token store) shows one.
        let dump = format!(
            "{report:?} {:?} {:?} {:?}",
            seen.iter().map(|s| &s.2).collect::<Vec<_>>(),
            repo.posts.borrow(),
            tokens.token.borrow(),
        );
        assert!(!dump.contains("SECRET") && !dump.contains("ya29"), "{dump}");
    }

    /// A token store whose `save` answers as told: `Some(applied)` or an error (`None`).
    struct SaveAnswers(FakeTokenStore, Option<bool>);
    impl TokenStore for SaveAnswers {
        async fn load(&self) -> Result<TokenLoad, PortError> {
            self.0.load().await
        }
        async fn save(&self, _t: &StoredToken) -> Result<bool, PortError> {
            self.1.ok_or_else(|| PortError("kv down".into()))
        }
        async fn connect(&self, t: &StoredToken) -> Result<(), PortError> {
            self.0.connect(t).await
        }
        async fn mark_needs_reconnect(&self) -> Result<(), PortError> {
            self.0.mark_needs_reconnect().await
        }
        async fn needs_reconnect(&self) -> Result<bool, PortError> {
            self.0.needs_reconnect().await
        }
        async fn connection_mark(&self) -> Result<Option<bool>, PortError> {
            self.0.connection_mark().await
        }
    }

    fn run_with_save(answer: Option<bool>) -> (PublishReport, FakeRepo, FakePublisher) {
        let inner = FakeTokenStore::default();
        *inner.token.borrow_mut() = Some(token(NOW, "ya29.OLD"));
        let repo = two_due();
        let publisher = FakePublisher::default();
        let report = pollster::block_on(publish_due(
            &|| NOW,
            &repo,
            &publisher,
            &SaveAnswers(inner, answer),
            &refresher_ok(),
            &FakeLinks,
        ))
        .unwrap();
        (report, repo, publisher)
    }

    #[test]
    fn a_failing_token_save_stops_the_run_before_the_provider_is_called() {
        let (report, repo, publisher) = run_with_save(None);
        assert_eq!(report.halted, Some(Halt::TokenUnavailable));
        assert_eq!(repo.state("a"), PublishState::new_queued());
        assert!(publisher.calls.borrow().is_empty());
    }

    #[test]
    fn a_refresh_save_that_did_not_apply_stops_the_run_without_using_the_new_token() {
        // The owner reconnected between load and save: the refreshed token is stale.
        let (report, repo, publisher) = run_with_save(Some(false));
        assert_eq!(report.halted, Some(Halt::TokenUnavailable));
        assert!(report.outcomes.is_empty());
        assert_eq!(repo.state("a"), PublishState::new_queued());
        assert!(
            publisher.calls.borrow().is_empty(),
            "no upload with the stale token"
        );
    }

    #[test]
    fn a_refresh_save_that_did_not_apply_mid_run_releases_the_claimed_post() {
        let inner = FakeTokenStore::default();
        *inner.token.borrow_mut() = Some(token(NOW + 3600, "ya29.OLD"));
        let repo = two_due();
        let publisher = FakePublisher::returning(vec![Err(PublishError::Auth)]);
        let report = pollster::block_on(publish_due(
            &|| NOW,
            &repo,
            &publisher,
            &SaveAnswers(inner, Some(false)),
            &refresher_ok(),
            &FakeLinks,
        ))
        .unwrap();
        assert_eq!(report.halted, Some(Halt::TokenUnavailable));
        assert_eq!(report.outcomes[0].1, Outcome::Released);
        assert_eq!(repo.state("a"), PublishState::new_queued());
        assert_eq!(
            publisher.calls.borrow().len(),
            1,
            "no retry with the stale token"
        );
    }

    #[test]
    fn an_unreadable_token_marks_the_row_and_halts_for_a_reconnect() {
        let tokens = fresh_store();
        tokens.unreadable.set(true);
        let repo = two_due();
        let publisher = FakePublisher::default();
        let refresher = refresher_ok();
        let report = run(&repo, &publisher, &tokens, &refresher);
        assert_eq!(report.halted, Some(Halt::NeedsReconnect));
        assert!(report.outcomes.is_empty());
        assert!(
            tokens.reconnect.get(),
            "the row is marked, so later runs stop early"
        );
        assert_eq!(repo.state("a"), PublishState::new_queued());
        assert!(publisher.calls.borrow().is_empty());
        assert_eq!(refresher.calls.get(), 0);
    }

    #[test]
    fn an_unreadable_token_is_not_marked_if_the_owner_reconnected_since_the_load() {
        /// Reconnects right after the load, as the owner could between two steps of a run.
        struct ReconnectsAfterLoad(FakeTokenStore);
        impl TokenStore for ReconnectsAfterLoad {
            async fn load(&self) -> Result<TokenLoad, PortError> {
                let loaded = self.0.load().await;
                self.0.connect(&token(NOW + 3600, "ya29.NEW")).await?;
                loaded
            }
            async fn save(&self, t: &StoredToken) -> Result<bool, PortError> {
                self.0.save(t).await
            }
            async fn connect(&self, t: &StoredToken) -> Result<(), PortError> {
                self.0.connect(t).await
            }
            async fn mark_needs_reconnect(&self) -> Result<(), PortError> {
                self.0.mark_needs_reconnect().await
            }
            async fn needs_reconnect(&self) -> Result<bool, PortError> {
                self.0.needs_reconnect().await
            }
            async fn connection_mark(&self) -> Result<Option<bool>, PortError> {
                self.0.connection_mark().await
            }
        }
        let inner = fresh_store();
        inner.unreadable.set(true);
        let store = ReconnectsAfterLoad(inner);
        let repo = two_due();
        let report = pollster::block_on(publish_due(
            &|| NOW,
            &repo,
            &FakePublisher::default(),
            &store,
            &refresher_ok(),
            &FakeLinks,
        ))
        .unwrap();
        assert_eq!(report.halted, Some(Halt::NeedsReconnect));
        assert!(
            !store.0.reconnect.get(),
            "the new connection must stay unmarked"
        );
    }

    #[test]
    fn a_video_link_failure_is_retried_without_calling_the_provider() {
        struct NoLinks;
        impl VideoLinks for NoLinks {
            async fn access(&self, _k: &str) -> Result<VideoAccess, PortError> {
                Err(PortError("r2 down".into()))
            }
        }
        let repo = FakeRepo::with(vec![scheduled("a", 1)]);
        let publisher = FakePublisher::default();
        let report = pollster::block_on(publish_due(
            &|| NOW,
            &repo,
            &publisher,
            &fresh_store(),
            &refresher_ok(),
            &NoLinks,
        ))
        .unwrap();
        assert_eq!(
            report.outcomes[0].1,
            Outcome::Retrying(FailReason::Transient)
        );
        assert_eq!(
            repo.state("a"),
            PublishState::Queued {
                attempts: 1,
                not_before: NOW + 60
            }
        );
        assert!(publisher.calls.borrow().is_empty());
    }

    #[test]
    fn a_video_too_big_or_empty_to_publish_is_rejected_without_calling_the_provider() {
        struct Sized(u64);
        impl VideoLinks for Sized {
            async fn access(&self, k: &str) -> Result<VideoAccess, PortError> {
                Ok(VideoAccess::new(k, self.0))
            }
        }
        for (size, rejected) in [
            (0, true),
            (1, false),
            (MAX_PUBLISH_BYTES, false),
            (MAX_PUBLISH_BYTES + 1, true),
            (u64::MAX, true),
        ] {
            let repo = FakeRepo::with(vec![scheduled("a", 1)]);
            let publisher = FakePublisher::returning(vec![ok_ref("yt1")]);
            let report = pollster::block_on(publish_due(
                &|| NOW,
                &repo,
                &publisher,
                &fresh_store(),
                &refresher_ok(),
                &Sized(size),
            ))
            .unwrap();
            assert_eq!(
                publisher.calls.borrow().len(),
                usize::from(!rejected),
                "{size}"
            );
            if rejected {
                assert_eq!(report.outcomes[0].1, Outcome::Failed(FailReason::Rejected));
            }
        }
    }

    #[test]
    fn an_external_id_of_the_wrong_shape_is_treated_as_unknown() {
        for bad in ["", "has space", "<script>", &"a".repeat(33)] {
            let repo = FakeRepo::with(vec![scheduled("a", 1)]);
            let publisher = FakePublisher::returning(vec![ok_ref(bad)]);
            let report = run(&repo, &publisher, &fresh_store(), &refresher_ok());
            assert_eq!(report.outcomes[0].1, Outcome::Failed(FailReason::Unknown));
            assert_eq!(
                repo.state("a"),
                PublishState::Failed {
                    attempts: 1,
                    reason: FailReason::Unknown
                }
            );
        }
    }
}
