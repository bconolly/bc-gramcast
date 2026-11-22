//! YouTube publishing rules that need no I/O: the limits a run must stay inside, the
//! privacy setting, and (in later sections) how each provider answer is classified.
//! The Worker adapters only send requests and hand the answers here.
//!
//! What is assumed about Cloudflare, not confirmed (see the README):
//! - Free plan: 50 subrequests per invocation. R2 and D1 calls through bindings are
//!   assumed to count, the safe reading, so every one is budgeted below.
//! - Free plan: 10 ms CPU per invocation. Waiting on the network does not count;
//!   whether copying bytes between R2 and `fetch` does is UNCONFIRMED.
//! - A cron invocation may run up to 15 minutes, which is `PUBLISH_TIMEOUT_SECS`.

/// The upload chunk size, in bytes. YouTube's resumable protocol wants every chunk
/// except the last to be a multiple of 256 KiB; 8 MiB is 32 of those.
pub const CHUNK_BYTES: u64 = 8 * 1024 * 1024;
pub const CHUNK_GRANULARITY: u64 = 256 * 1024;

/// Subrequests allowed per invocation on the free plan.
pub const SUBREQUEST_LIMIT: u32 = 50;
/// Reserved for everything but the upload itself, counted as the worst case of one run:
/// D1: list 1, needs-reconnect 1, token load 1, claim 1, final write 1, stuck flags up to
/// 3, requeue after an auth failure 1, release 1, mark 1 (11); token refresh: the
/// refresh call and its save, twice if the provider refuses the token once (4); plus 3
/// of slack. A guess to be tuned from real runs, kept on the high side on purpose.
pub const RESERVED_SUBREQUESTS: u32 = 18;
/// What the upload may spend in one invocation, shared by a retry after a refresh.
pub const UPLOAD_SUBREQUESTS: u32 = SUBREQUEST_LIMIT - RESERVED_SUBREQUESTS;
/// Reading the object's size from R2 (1) and starting the upload session (1).
pub const START_SUBREQUESTS: u32 = 2;
/// Each chunk is one R2 range read and one PUT.
pub const CHUNK_SUBREQUESTS: u32 = 2;
/// Status checks (an empty PUT asking where the upload stands) kept back for lost replies.
pub const STATUS_CHECK_RESERVE: u32 = 8;

/// The most chunks one upload may need within the budget.
pub const MAX_CHUNKS: u32 =
    (UPLOAD_SUBREQUESTS - START_SUBREQUESTS - STATUS_CHECK_RESERVE) / CHUNK_SUBREQUESTS;
/// The largest video a run can upload inside the subrequest limit. Videos above it are
/// refused when they are uploaded, not discovered at publish time.
pub const MAX_PUBLISH_BYTES: u64 = MAX_CHUNKS as u64 * CHUNK_BYTES;

const _: () = {
    assert!(CHUNK_BYTES.is_multiple_of(CHUNK_GRANULARITY));
    assert!(
        RESERVED_SUBREQUESTS
            + START_SUBREQUESTS
            + MAX_CHUNKS * CHUNK_SUBREQUESTS
            + STATUS_CHECK_RESERVE
            <= SUBREQUEST_LIMIT
    );
};

/// Counts subrequests against the invocation's allowance, so an upload stops before the
/// platform would stop it midway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubrequestBudget {
    left: u32,
}

impl SubrequestBudget {
    pub fn for_upload() -> Self {
        Self {
            left: UPLOAD_SUBREQUESTS,
        }
    }

    /// Takes `n` from the budget. `false` (and nothing taken) if that would overspend.
    pub fn spend(&mut self, n: u32) -> bool {
        match self.left.checked_sub(n) {
            Some(rest) => {
                self.left = rest;
                true
            }
            None => false,
        }
    }

    pub fn left(&self) -> u32 {
        self.left
    }
}

/// Who can see the video. The default is private, so a first real test never goes public.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    Private,
    Unlisted,
    Public,
}

impl Privacy {
    /// From the `PUBLISH_PRIVACY` var: exactly `private`, `unlisted` or `public`.
    /// Unset or anything else is private.
    pub fn from_var(value: Option<&str>) -> Privacy {
        match value {
            Some("unlisted") => Privacy::Unlisted,
            Some("public") => Privacy::Public,
            _ => Privacy::Private,
        }
    }

    /// The value YouTube's `privacyStatus` takes.
    pub fn status(self) -> &'static str {
        match self {
            Privacy::Private => "private",
            Privacy::Unlisted => "unlisted",
            Privacy::Public => "public",
        }
    }
}

/// Publishing runs only when `PUBLISH_ENABLED` is exactly `true`.
pub fn publish_enabled(value: Option<&str>) -> bool {
    value == Some("true")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_case_run_stays_inside_the_subrequest_limit() {
        let worst = RESERVED_SUBREQUESTS
            + START_SUBREQUESTS
            + MAX_CHUNKS * CHUNK_SUBREQUESTS
            + STATUS_CHECK_RESERVE;
        assert!(worst <= SUBREQUEST_LIMIT, "{worst}");
        // One more chunk would not fit.
        assert!(worst + CHUNK_SUBREQUESTS > SUBREQUEST_LIMIT, "{worst}");
    }

    #[test]
    fn the_largest_publishable_video_is_whole_chunks_that_fit() {
        assert_eq!(MAX_CHUNKS, 11);
        assert_eq!(MAX_PUBLISH_BYTES, 88 * 1024 * 1024);
        assert_eq!(MAX_PUBLISH_BYTES % CHUNK_GRANULARITY, 0);
    }

    #[test]
    fn the_budget_refuses_an_overspend_and_takes_nothing() {
        let mut b = SubrequestBudget::for_upload();
        assert_eq!(b.left(), UPLOAD_SUBREQUESTS);
        assert!(b.spend(UPLOAD_SUBREQUESTS - 1));
        assert!(!b.spend(2));
        assert_eq!(b.left(), 1);
        assert!(b.spend(1));
        assert!(!b.spend(1));
        assert!(b.spend(0));
    }

    #[test]
    fn privacy_defaults_to_private_unless_exactly_named() {
        assert_eq!(Privacy::from_var(None), Privacy::Private);
        for bad in [
            "", "Public", "PUBLIC", " public", "public ", "pub", "1", "true",
        ] {
            assert_eq!(Privacy::from_var(Some(bad)), Privacy::Private, "{bad:?}");
        }
        assert_eq!(Privacy::from_var(Some("private")), Privacy::Private);
        assert_eq!(Privacy::from_var(Some("unlisted")), Privacy::Unlisted);
        assert_eq!(Privacy::from_var(Some("public")), Privacy::Public);
        assert_eq!(Privacy::Unlisted.status(), "unlisted");
    }

    #[test]
    fn publishing_is_on_only_for_exactly_true() {
        assert!(publish_enabled(Some("true")));
        for off in [
            None,
            Some(""),
            Some("false"),
            Some("True"),
            Some("TRUE"),
            Some("1"),
            Some(" true"),
            Some("true "),
        ] {
            assert!(!publish_enabled(off), "{off:?}");
        }
    }

    #[test]
    fn uploads_are_capped_at_what_a_run_can_send() {
        assert_eq!(crate::media::MAX_UPLOAD_BYTES, MAX_PUBLISH_BYTES);
    }
}
