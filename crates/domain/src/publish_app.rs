//! Publishing ports (the provider, token refresh, video links, state storage)
//! and, below them, the `publish_due` use case. The Worker only plugs adapters in.

use crate::app::PortError;
use crate::publish::{FailReason, PublishState, ScheduledPost};
use crate::queue::QueuedPost;
use crate::token::StoredToken;
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
}
