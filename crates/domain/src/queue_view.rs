//! What the Queue screen lists: posts still in play and recently finished ones, each
//! with where it is in publishing.

use crate::app::PortError;
use crate::publish::{PublishState, ScheduledPost};

/// Published and failed posts stay listed this long after their slot. Our own choice:
/// older history stays in storage but is not shown.
pub const RECENT_SECS: i64 = 7 * 24 * 60 * 60;

/// Read port: the posts Queued or Publishing, and the Published or Failed ones whose slot
/// is after `since`. An adapter may return a little extra; `queue_view` filters again.
#[allow(async_fn_in_trait)]
pub trait QueueViewRepository {
    async fn recent(&self, since: i64) -> Result<Vec<ScheduledPost>, PortError>;
}

/// Always while Queued or Publishing (a due post waiting for the next run is still in
/// the queue); Published and Failed only until `RECENT_SECS` after their slot.
pub fn is_listed(p: &ScheduledPost, now: i64) -> bool {
    match p.state {
        PublishState::Queued { .. } | PublishState::Publishing { .. } => true,
        PublishState::Published { .. } | PublishState::Failed { .. } => {
            p.post.slot_at > now.saturating_sub(RECENT_SECS)
        }
    }
}

/// The posts for the Queue screen, earliest slot first.
pub async fn queue_view(
    repo: &impl QueueViewRepository,
    now: i64,
) -> Result<Vec<ScheduledPost>, PortError> {
    let mut posts = repo.recent(now.saturating_sub(RECENT_SECS)).await?;
    posts.retain(|p| is_listed(p, now));
    posts.sort_by_key(|p| p.post.slot_at);
    Ok(posts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::FailReason;
    use crate::queue::{Platform, QueuedPost};

    const NOW: i64 = 10_000_000;

    fn post(id: &str, slot_at: i64, state: PublishState) -> ScheduledPost {
        ScheduledPost {
            post: QueuedPost {
                id: id.into(),
                video_key: format!("videos/{id}.mp4"),
                title: "T".into(),
                caption: "C".into(),
                platforms: vec![Platform::YouTube],
                slot_at,
            },
            state,
        }
    }

    fn published() -> PublishState {
        PublishState::Published {
            external_id: "abc".into(),
        }
    }

    fn failed() -> PublishState {
        PublishState::Failed {
            attempts: 3,
            reason: FailReason::Unknown,
        }
    }

    #[test]
    fn queued_and_publishing_posts_are_always_listed() {
        let publishing = PublishState::Publishing {
            attempt: 1,
            started_at: 5,
        };
        for slot in [0, NOW - 10 * RECENT_SECS, NOW, NOW + 5] {
            assert!(is_listed(&post("a", slot, PublishState::new_queued()), NOW));
            assert!(is_listed(&post("a", slot, publishing.clone()), NOW));
        }
    }

    #[test]
    fn finished_posts_are_listed_for_a_week_after_their_slot() {
        for state in [published(), failed()] {
            assert!(is_listed(
                &post("a", NOW - RECENT_SECS + 1, state.clone()),
                NOW
            ));
            assert!(is_listed(&post("a", NOW + 100, state.clone()), NOW));
            assert!(!is_listed(
                &post("a", NOW - RECENT_SECS, state.clone()),
                NOW
            ));
            assert!(!is_listed(&post("a", 0, state), NOW));
        }
    }

    struct Fake(Vec<ScheduledPost>, std::cell::Cell<Option<i64>>);
    impl QueueViewRepository for Fake {
        async fn recent(&self, since: i64) -> Result<Vec<ScheduledPost>, PortError> {
            self.1.set(Some(since));
            Ok(self.0.clone())
        }
    }

    #[test]
    fn the_view_is_filtered_again_and_sorted_by_slot() {
        let repo = Fake(
            vec![
                post("late", NOW + 50, PublishState::new_queued()),
                post("old", 5, published()),
                post("done", NOW - 100, published()),
                post("due", NOW - 10, PublishState::new_queued()),
            ],
            Default::default(),
        );
        let got = pollster::block_on(queue_view(&repo, NOW)).unwrap();
        let ids: Vec<&str> = got.iter().map(|p| p.post.id.as_str()).collect();
        assert_eq!(ids, ["done", "due", "late"]);
        assert_eq!(repo.1.get(), Some(NOW - RECENT_SECS));
    }

    #[test]
    fn a_storage_failure_is_passed_on() {
        struct Broken;
        impl QueueViewRepository for Broken {
            async fn recent(&self, _s: i64) -> Result<Vec<ScheduledPost>, PortError> {
                Err(PortError("d1 down".into()))
            }
        }
        assert_eq!(
            pollster::block_on(queue_view(&Broken, NOW)),
            Err(PortError("d1 down".into()))
        );
    }
}
