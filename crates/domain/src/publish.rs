//! Publishing lifecycle: the state a post moves through once its slot arrives.
//!
//! Pure functions only. Storage, the provider and the clock are the caller's
//! business (see `publish_app`), so every rule here tests without I/O.
//!
//! Atomicity: these functions compute the *next* state. The repository port
//! applies it as a compare-and-set against the state it was computed from, so
//! two runs that both read `Queued` cannot both claim the post.

use crate::queue::{Platform, QueuedPost};

/// Attempts allowed in total (the first try counts) before a retryable failure becomes `Failed`.
pub const MAX_ATTEMPTS: u32 = 3;
/// First retry waits this long; each further attempt doubles it.
pub const BACKOFF_BASE_SECS: i64 = 60;
/// How long a post may sit in `Publishing` before it is flagged for manual review.
/// Our own choice, not a provider limit.
pub const PUBLISH_TIMEOUT_SECS: i64 = 15 * 60;

/// Short, fixed codes. Never raw provider text, which could carry tokens or URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    Auth,
    RateLimited,
    Rejected,
    Transient,
    StuckPublishing,
}

impl FailReason {
    pub fn code(self) -> &'static str {
        match self {
            FailReason::Auth => "auth",
            FailReason::RateLimited => "rate_limited",
            FailReason::Rejected => "rejected",
            FailReason::Transient => "transient",
            FailReason::StuckPublishing => "stuck_publishing",
        }
    }

    /// Only failures that say "try again later" are retried. An auth failure
    /// needs the owner to reconnect, and a rejection will not change.
    pub fn is_retryable(self) -> bool {
        matches!(self, FailReason::RateLimited | FailReason::Transient)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishState {
    /// Waiting for its slot, or for `not_before` after a retryable failure.
    Queued {
        attempts: u32,
        not_before: i64,
    },
    Publishing {
        attempt: u32,
        started_at: i64,
    },
    Published {
        external_id: String,
    },
    Failed {
        attempts: u32,
        reason: FailReason,
    },
}

impl PublishState {
    pub fn new_queued() -> Self {
        PublishState::Queued {
            attempts: 0,
            not_before: 0,
        }
    }
}

/// A queued post together with where it is in the publishing lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledPost {
    pub post: QueuedPost,
    pub state: PublishState,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StateError {
    /// The transition does not apply to the state the post is in.
    WrongState,
}

/// Seconds to wait after failed attempt number `attempt` (1-based): 60, 120, 240...
pub fn backoff_secs(attempt: u32) -> i64 {
    let doublings = attempt.saturating_sub(1).min(20);
    BACKOFF_BASE_SECS.saturating_mul(1_i64 << doublings)
}

/// Due: slot reached, still queued, any retry delay over, and bound for YouTube.
pub fn is_due(p: &ScheduledPost, now: i64) -> bool {
    match p.state {
        PublishState::Queued { not_before, .. } => {
            p.post.slot_at <= now
                && not_before <= now
                && p.post.platforms.contains(&Platform::YouTube)
        }
        _ => false,
    }
}

/// Due posts, earliest slot first.
pub fn select_due(posts: &[ScheduledPost], now: i64) -> Vec<&ScheduledPost> {
    let mut due: Vec<&ScheduledPost> = posts.iter().filter(|p| is_due(p, now)).collect();
    due.sort_by_key(|p| p.post.slot_at);
    due
}

/// Queued -> Publishing. Counts the attempt now, so a crash mid-publish still shows it was tried.
pub fn claim(state: &PublishState, now: i64) -> Result<PublishState, StateError> {
    match state {
        PublishState::Queued { attempts, .. } => Ok(PublishState::Publishing {
            attempt: attempts + 1,
            started_at: now,
        }),
        _ => Err(StateError::WrongState),
    }
}

/// Publishing -> Published.
pub fn record_success(state: &PublishState, external_id: &str) -> Result<PublishState, StateError> {
    match state {
        PublishState::Publishing { .. } => Ok(PublishState::Published {
            external_id: external_id.to_string(),
        }),
        _ => Err(StateError::WrongState),
    }
}

/// Publishing -> Queued (retry after backoff) or Failed (not retryable, or out of attempts).
pub fn record_failure(
    state: &PublishState,
    reason: FailReason,
    now: i64,
) -> Result<PublishState, StateError> {
    let PublishState::Publishing { attempt, .. } = state else {
        return Err(StateError::WrongState);
    };
    if reason.is_retryable() && *attempt < MAX_ATTEMPTS {
        Ok(PublishState::Queued {
            attempts: *attempt,
            not_before: now.saturating_add(backoff_secs(*attempt)),
        })
    } else {
        Ok(PublishState::Failed {
            attempts: *attempt,
            reason,
        })
    }
}

/// A post stuck in `Publishing` past the timeout goes to `Failed(StuckPublishing)`,
/// which is the manual-review state. It is deliberately NOT put back in the queue:
/// we cannot tell whether the provider already accepted the upload, so an automatic
/// retry could post the video twice. A person checks the channel, then decides.
pub fn flag_if_stuck(state: &PublishState, now: i64) -> Option<PublishState> {
    match state {
        PublishState::Publishing {
            attempt,
            started_at,
        } if now.saturating_sub(*started_at) >= PUBLISH_TIMEOUT_SECS => {
            Some(PublishState::Failed {
                attempts: *attempt,
                reason: FailReason::StuckPublishing,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(id: &str, slot_at: i64, state: PublishState, platforms: Vec<Platform>) -> ScheduledPost {
        ScheduledPost {
            post: QueuedPost {
                id: id.into(),
                video_key: format!("videos/{id}.mp4"),
                title: "T".into(),
                caption: "C".into(),
                platforms,
                slot_at,
            },
            state,
        }
    }

    fn yt(id: &str, slot_at: i64) -> ScheduledPost {
        sp(
            id,
            slot_at,
            PublishState::new_queued(),
            vec![Platform::YouTube],
        )
    }

    fn publishing(attempt: u32, started_at: i64) -> PublishState {
        PublishState::Publishing {
            attempt,
            started_at,
        }
    }

    #[test]
    fn due_means_slot_reached_and_still_queued() {
        assert!(!is_due(&yt("a", 101), 100));
        assert!(is_due(&yt("a", 100), 100));
        let mut p = yt("a", 50);
        p.state = publishing(1, 60);
        assert!(!is_due(&p, 100));
        p.state = PublishState::Published {
            external_id: "x".into(),
        };
        assert!(!is_due(&p, 100));
    }

    #[test]
    fn due_skips_posts_not_bound_for_youtube() {
        let p = sp(
            "a",
            1,
            PublishState::new_queued(),
            vec![Platform::Instagram],
        );
        assert!(!is_due(&p, 100));
    }

    #[test]
    fn due_waits_out_the_retry_delay() {
        let p = sp(
            "a",
            1,
            PublishState::Queued {
                attempts: 1,
                not_before: 160,
            },
            vec![Platform::YouTube],
        );
        assert!(!is_due(&p, 159));
        assert!(is_due(&p, 160));
    }

    #[test]
    fn select_due_is_earliest_first() {
        let posts = [yt("late", 90), yt("early", 10), yt("future", 500)];
        let ids: Vec<&str> = select_due(&posts, 100)
            .iter()
            .map(|p| p.post.id.as_str())
            .collect();
        assert_eq!(ids, ["early", "late"]);
    }

    #[test]
    fn claim_moves_queued_to_publishing_and_counts_the_attempt() {
        let s = PublishState::Queued {
            attempts: 1,
            not_before: 0,
        };
        assert_eq!(claim(&s, 700), Ok(publishing(2, 700)));
    }

    #[test]
    fn claim_only_works_once() {
        let first = claim(&PublishState::new_queued(), 1).unwrap();
        assert_eq!(claim(&first, 2), Err(StateError::WrongState));
        let done = PublishState::Published {
            external_id: "x".into(),
        };
        assert_eq!(claim(&done, 2), Err(StateError::WrongState));
    }

    #[test]
    fn success_keeps_the_external_id() {
        assert_eq!(
            record_success(&publishing(1, 5), "abc"),
            Ok(PublishState::Published {
                external_id: "abc".into()
            })
        );
        assert_eq!(
            record_success(&PublishState::new_queued(), "abc"),
            Err(StateError::WrongState)
        );
    }

    #[test]
    fn backoff_doubles_per_attempt() {
        assert_eq!(backoff_secs(1), 60);
        assert_eq!(backoff_secs(2), 120);
        assert_eq!(backoff_secs(3), 240);
        assert!(backoff_secs(u32::MAX) > 0);
    }

    #[test]
    fn a_retryable_failure_requeues_with_backoff() {
        assert_eq!(
            record_failure(&publishing(1, 5), FailReason::Transient, 1000),
            Ok(PublishState::Queued {
                attempts: 1,
                not_before: 1060
            })
        );
        assert_eq!(
            record_failure(&publishing(2, 5), FailReason::RateLimited, 1000),
            Ok(PublishState::Queued {
                attempts: 2,
                not_before: 1120
            })
        );
    }

    #[test]
    fn the_last_attempt_fails_for_good() {
        assert_eq!(
            record_failure(&publishing(MAX_ATTEMPTS, 5), FailReason::Transient, 1000),
            Ok(PublishState::Failed {
                attempts: 3,
                reason: FailReason::Transient
            })
        );
    }

    #[test]
    fn auth_and_rejection_are_never_retried() {
        for r in [FailReason::Auth, FailReason::Rejected] {
            assert_eq!(
                record_failure(&publishing(1, 5), r, 1000),
                Ok(PublishState::Failed {
                    attempts: 1,
                    reason: r
                })
            );
        }
    }

    #[test]
    fn failure_only_applies_to_a_publishing_post() {
        assert_eq!(
            record_failure(&PublishState::new_queued(), FailReason::Transient, 1),
            Err(StateError::WrongState)
        );
    }

    #[test]
    fn a_stuck_post_goes_to_manual_review_not_back_in_the_queue() {
        let s = publishing(2, 1000);
        assert_eq!(flag_if_stuck(&s, 1000 + PUBLISH_TIMEOUT_SECS - 1), None);
        assert_eq!(
            flag_if_stuck(&s, 1000 + PUBLISH_TIMEOUT_SECS),
            Some(PublishState::Failed {
                attempts: 2,
                reason: FailReason::StuckPublishing
            })
        );
        assert_eq!(flag_if_stuck(&PublishState::new_queued(), i64::MAX), None);
    }

    #[test]
    fn reason_codes_are_fixed_strings() {
        assert_eq!(FailReason::Auth.code(), "auth");
        assert_eq!(FailReason::RateLimited.code(), "rate_limited");
        assert_eq!(FailReason::Rejected.code(), "rejected");
        assert_eq!(FailReason::Transient.code(), "transient");
        assert_eq!(FailReason::StuckPublishing.code(), "stuck_publishing");
    }
}
