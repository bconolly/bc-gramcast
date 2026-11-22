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

/// One request may take this long before it is given up on. Our own choice: a chunk is
/// 8 MiB, which needs a few seconds even on a slow link.
pub const REQUEST_TIMEOUT_SECS: i64 = 30;
/// Wait before asking where an upload stands after a lost reply.
pub const STATUS_CHECK_DELAY_SECS: i64 = 2;
/// No request is started once this long has passed since the claim. Our own choice.
pub const UPLOAD_DEADLINE_SECS: i64 = 6 * 60;
/// The longest a publish can run from the claim. The deadline is checked before every
/// request (start, chunk, status check, forced refresh), so the last one begins just
/// inside it; a status check also waits `STATUS_CHECK_DELAY_SECS` first, and the request
/// can run for `REQUEST_TIMEOUT_SECS`. After that the outcome is written.
pub const WORST_CASE_PUBLISH_SECS: i64 =
    UPLOAD_DEADLINE_SECS + STATUS_CHECK_DELAY_SECS + REQUEST_TIMEOUT_SECS;

// Well inside the 15 minutes after which a Publishing post is flagged (and the cron
// invocation limit), with room for the state write.
const _: () = assert!(
    WORST_CASE_PUBLISH_SECS + 2 * REQUEST_TIMEOUT_SECS < crate::publish::PUBLISH_TIMEOUT_SECS
);

/// The moment no further request may start, for a post claimed at `started_at`.
pub fn upload_deadline(started_at: i64) -> i64 {
    started_at.saturating_add(UPLOAD_DEADLINE_SECS)
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

use crate::connect::{parse_token_reply, ExchangeError, Grant, TokenReply};
use crate::publish_app::PublishError;
use crate::token::StoredToken;

// ---- how provider answers are classified ------------------------------------------
//
// Error contract (W8): `Transient` only when the provider definitely did not create the
// video; a timeout or reset after any chunk was sent, or an unparsable 2xx, is `Unknown`;
// `Auth` only for account-level problems; a post-specific refusal is `Rejected`.

/// `error.errors[].reason` values from YouTube's JSON error body that change the
/// classification, from Google's documentation except where noted. Anything not listed
/// is post-specific (`Rejected`, never retried): that includes the documented
/// `videos.insert` 400s (`invalidTitle`, `invalidDescription`, `invalidTags`,
/// `invalidCategoryId`, `invalidFilename`, `invalidPublishAt`, `invalidRecordingDetails`,
/// `invalidVideoGameRating`, `invalidVideoMetadata`, `defaultLanguageNotSet`,
/// `mediaBodyRequired`) and the 403s `forbiddenLicenseSetting`, `forbiddenPrivacySetting`.
///
/// Limits that clear with time: `uploadLimitExceeded` (400, the per-channel daily upload
/// cap) and `quotaExceeded` (403). `dailyLimitExceeded`, `rateLimitExceeded` and
/// `userRateLimitExceeded` are NOT confirmed for `videos.insert`; kept as waits.
pub const QUOTA_REASONS: [&str; 5] = [
    "uploadLimitExceeded",
    "quotaExceeded",
    "dailyLimitExceeded",
    "rateLimitExceeded",
    "userRateLimitExceeded",
];
/// Account or project wide: every post would hit it, so the run stops. Documented:
/// `youtubeSignupRequired`, `authorizationRequired` (401) and `insufficientPermissions`,
/// `forbidden` (403). `accessNotConfigured` (the API is not enabled in the Cloud project)
/// and `authError` are NOT in the YouTube documentation that was read: treated as
/// project-wide so one bad project setting halts the run instead of failing a post per run.
pub const ACCOUNT_REASONS: [&str; 6] = [
    "youtubeSignupRequired",
    "authorizationRequired",
    "insufficientPermissions",
    "forbidden",
    "accessNotConfigured",
    "authError",
];

/// What a 4xx body's reason codes mean, as far as this code acts on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiReason {
    Quota,
    Account,
    Other,
}

/// Quota wins over account if both appear, since waiting is the lesser disruption.
pub fn api_reason<'a>(reasons: impl IntoIterator<Item = &'a str>) -> ApiReason {
    let reasons: Vec<&str> = reasons.into_iter().collect();
    if reasons.iter().any(|r| QUOTA_REASONS.contains(r)) {
        ApiReason::Quota
    } else if reasons.iter().any(|r| ACCOUNT_REASONS.contains(r)) {
        ApiReason::Account
    } else {
        ApiReason::Other
    }
}

/// Longest upload session address we will use.
pub const MAX_SESSION_URI_LEN: usize = 2048;

/// Whether the `Location` of a started session is an address we will send the video
/// and the access token to: https, on Google's own domain, no user info, no port, no
/// odd characters. Defence in depth: the answer comes from Google, but a bearer token
/// must never follow a redirect-like value to another host.
pub fn is_session_uri(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("https://") else {
        return false;
    };
    if uri.len() > MAX_SESSION_URI_LEN || !uri.bytes().all(|b| b.is_ascii_graphic() && b != b'\\') {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.to_ascii_lowercase();
    !host.contains(['@', ':']) && (host == "googleapis.com" || host.ends_with(".googleapis.com"))
}

/// An absolute retry time from a `Retry-After` header, as delta-seconds added to the
/// Worker's `now`. An HTTP-date, a negative or a non-numeric value gives `None` (the
/// domain then waits its default); dates are not parsed here.
pub fn retry_at(retry_after: Option<&str>, now: i64) -> Option<i64> {
    let text = retry_after?.trim();
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(now.saturating_add(text.parse::<i64>().ok()?))
}

/// What the Worker saw of one response, boiled down to what decides the next step. Built
/// from the status, the `Range`, `Retry-After` and `Location` headers and the error
/// reasons in the body. `status: None` means no response came (a network error, a reset
/// or a timeout). It never holds a body, a URL or a header value we keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: Option<u16>,
    pub retry_after: Option<String>,
    /// The `Range` header of a 308, such as `bytes=0-524287`.
    pub range: Option<String>,
    pub has_location: bool,
    pub reason: ApiReason,
}

impl Reply {
    pub fn lost() -> Self {
        Self::with(None)
    }

    pub fn status(code: u16) -> Self {
        Self::with(Some(code))
    }

    fn with(status: Option<u16>) -> Self {
        Self {
            status,
            retry_after: None,
            range: None,
            has_location: false,
            reason: ApiReason::Other,
        }
    }
}

/// An explicit refusal (a 4xx that is not a missing session): which error it is.
pub fn classify_refusal(reply: &Reply, status: u16, now: i64) -> PublishError {
    match status {
        401 => PublishError::Auth,
        // The per-channel daily upload cap is a 400, separate from the API quota.
        400 if reply.reason == ApiReason::Quota => PublishError::RateLimited { retry_at: None },
        429 => PublishError::RateLimited {
            retry_at: retry_at(reply.retry_after.as_deref(), now),
        },
        403 => match reply.reason {
            // Daily quota resets at midnight Pacific; the domain waits an hour.
            ApiReason::Quota => PublishError::RateLimited { retry_at: None },
            ApiReason::Account => PublishError::Auth,
            ApiReason::Other => PublishError::Rejected,
        },
        _ => PublishError::Rejected,
    }
}

/// The answer to starting the upload session. Nothing exists at the provider until the
/// last byte is accepted, so every failure here is safe to retry (`Transient`), except
/// a refusal, which is classified as such.
pub fn classify_start(reply: &Reply, now: i64) -> Result<(), PublishError> {
    match reply.status {
        Some(200) if reply.has_location => Ok(()),
        Some(200) | None => Err(PublishError::Transient),
        Some(408 | 500..=599) => Err(PublishError::Transient),
        Some(s @ 400..=499) => Err(classify_refusal(reply, s, now)),
        Some(_) => Err(PublishError::Transient),
    }
}

/// The token endpoint's answer to a refresh, as a token (with an empty refresh token:
/// the caller keeps the stored one) or a classified failure. `invalid_grant` is `Auth`;
/// a granted scope that lacks upload is `Auth`; 429 is `RateLimited`; everything else,
/// including `invalid_client`, 401, 5xx and an unusable reply, is `Transient`.
pub fn refresh_outcome(
    reply: &TokenReply,
    retry_after: Option<&str>,
    now: i64,
) -> Result<StoredToken, PublishError> {
    match parse_token_reply(reply, Grant::Refresh, now) {
        Ok(token) => Ok(token),
        Err(ExchangeError::InvalidGrant) => Err(PublishError::Auth),
        // A success whose scope lacks upload: the grant itself is wrong.
        Err(ExchangeError::Rejected) if (200..300).contains(&reply.status) => {
            Err(PublishError::Auth)
        }
        Err(ExchangeError::Unavailable) if reply.status == 429 => Err(PublishError::RateLimited {
            retry_at: retry_at(retry_after, now),
        }),
        Err(_) => Err(PublishError::Transient),
    }
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
    fn the_worst_case_publish_is_computed_from_its_parts() {
        assert_eq!(
            (
                REQUEST_TIMEOUT_SECS,
                STATUS_CHECK_DELAY_SECS,
                UPLOAD_DEADLINE_SECS
            ),
            (30, 2, 360)
        );
        assert_eq!(WORST_CASE_PUBLISH_SECS, 392);
        assert_eq!(upload_deadline(1_000), 1_360);
        assert_eq!(upload_deadline(i64::MAX), i64::MAX);
    }

    #[test]
    fn uploads_are_capped_at_what_a_run_can_send() {
        assert_eq!(crate::media::MAX_UPLOAD_BYTES, MAX_PUBLISH_BYTES);
    }

    fn reply(status: Option<u16>) -> Reply {
        Reply {
            status,
            ..Reply::lost()
        }
    }

    #[test]
    fn reasons_are_matched_exactly_with_quota_first() {
        for quota in [
            "quotaExceeded",
            "uploadLimitExceeded",
            "dailyLimitExceeded",
            "rateLimitExceeded",
            "userRateLimitExceeded",
        ] {
            assert_eq!(api_reason([quota]), ApiReason::Quota, "{quota}");
        }
        for account in [
            "insufficientPermissions",
            "youtubeSignupRequired",
            "authorizationRequired",
            "forbidden",
            "accessNotConfigured",
            "authError",
        ] {
            assert_eq!(api_reason([account]), ApiReason::Account, "{account}");
        }
        assert_eq!(
            api_reason(["youtubeSignupRequired", "quotaExceeded"]),
            ApiReason::Quota
        );
        for other in [
            vec![],
            vec!["invalidTitle"],
            vec!["forbiddenPrivacySetting"],
            vec!["QUOTAEXCEEDED"],
            vec!["quotaExceeded "],
        ] {
            assert_eq!(api_reason(other.clone()), ApiReason::Other, "{other:?}");
        }
    }

    #[test]
    fn only_googles_own_https_addresses_are_session_uris() {
        for ok in [
            "https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&upload_id=AB-c_9",
            "https://www.googleapis.com/x",
            "https://WWW.GoogleAPIs.com/x",
            "https://upload.googleapis.com/x",
            "https://googleapis.com",
        ] {
            assert!(is_session_uri(ok), "{ok}");
        }
        let long = format!(
            "https://www.googleapis.com/{}",
            "a".repeat(MAX_SESSION_URI_LEN)
        );
        for bad in [
            "",
            "http://www.googleapis.com/x",
            "//www.googleapis.com/x",
            "https://www.googleapis.com.evil.com/x",
            "https://evilgoogleapis.com/x",
            "https://evil.com/?www.googleapis.com",
            "https://evil.com/#.googleapis.com",
            "https://user@www.googleapis.com/x",
            "https://www.googleapis.com@evil.com/x",
            "https://www.googleapis.com:8443/x",
            "https://evil.com\\@www.googleapis.com/x",
            "https://www.googleapis.com/a b",
            "https://www.googleapis.com/a\nb",
            "https://www.googleapis.com/é",
            &long,
        ] {
            assert!(!is_session_uri(bad), "{bad}");
        }
    }

    #[test]
    fn retry_after_becomes_an_absolute_time_from_the_given_clock() {
        assert_eq!(retry_at(Some("120"), 1_000), Some(1_120));
        assert_eq!(retry_at(Some(" 30 "), 1_000), Some(1_030));
        assert_eq!(retry_at(Some("0"), 1_000), Some(1_000));
        for bad in [
            None,
            Some(""),
            Some("-5"),
            Some("+5"),
            Some("1.5"),
            Some("abc"),
            Some("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some("1234567890"),
        ] {
            assert_eq!(retry_at(bad, 1_000), None, "{bad:?}");
        }
        assert_eq!(retry_at(Some("99"), i64::MAX), Some(i64::MAX));
    }

    #[test]
    fn a_session_is_started_only_by_a_200_with_a_location() {
        let ok = Reply {
            has_location: true,
            ..reply(Some(200))
        };
        assert_eq!(classify_start(&ok, 0), Ok(()));
        assert_eq!(
            classify_start(&reply(Some(200)), 0),
            Err(PublishError::Transient)
        );
    }

    #[test]
    fn failures_before_a_session_exists_are_transient() {
        for status in [
            None,
            Some(408),
            Some(500),
            Some(502),
            Some(503),
            Some(599),
            Some(302),
            Some(204),
        ] {
            assert_eq!(
                classify_start(&reply(status), 0),
                Err(PublishError::Transient),
                "{status:?}"
            );
        }
    }

    #[test]
    fn refusals_are_classified_by_status_and_reason() {
        let with = |status, reason| Reply {
            reason,
            retry_after: Some("90".into()),
            ..reply(Some(status))
        };
        let start = |r: &Reply| classify_start(r, 1_000);
        assert_eq!(start(&with(401, ApiReason::Other)), Err(PublishError::Auth));
        assert_eq!(
            start(&with(403, ApiReason::Account)),
            Err(PublishError::Auth)
        );
        assert_eq!(
            start(&with(403, ApiReason::Other)),
            Err(PublishError::Rejected)
        );
        // Quota carries no retry time even when a header is present.
        assert_eq!(
            start(&with(403, ApiReason::Quota)),
            Err(PublishError::RateLimited { retry_at: None })
        );
        assert_eq!(
            start(&with(429, ApiReason::Other)),
            Err(PublishError::RateLimited {
                retry_at: Some(1_090)
            })
        );
        for status in [400, 404, 405, 409, 410, 413, 422] {
            assert_eq!(
                start(&with(status, ApiReason::Other)),
                Err(PublishError::Rejected),
                "{status}"
            );
        }
    }

    #[test]
    fn the_documented_post_specific_reasons_are_rejected_and_limits_wait() {
        let refuse = |status, reasons: &[&str]| {
            let reply = Reply {
                reason: api_reason(reasons.iter().copied()),
                ..reply(Some(status))
            };
            classify_start(&reply, 1_000)
        };
        for reason in [
            "invalidTitle",
            "invalidDescription",
            "invalidTags",
            "invalidCategoryId",
            "invalidFilename",
            "invalidPublishAt",
            "invalidRecordingDetails",
            "invalidVideoGameRating",
            "invalidVideoMetadata",
            "defaultLanguageNotSet",
            "mediaBodyRequired",
        ] {
            assert_eq!(
                refuse(400, &[reason]),
                Err(PublishError::Rejected),
                "{reason}"
            );
        }
        for reason in ["forbiddenLicenseSetting", "forbiddenPrivacySetting"] {
            assert_eq!(
                refuse(403, &[reason]),
                Err(PublishError::Rejected),
                "{reason}"
            );
        }
        // The per-channel daily upload cap arrives as a 400 and is a wait, not a failure.
        assert_eq!(
            refuse(400, &["uploadLimitExceeded"]),
            Err(PublishError::RateLimited { retry_at: None })
        );
        assert_eq!(
            refuse(403, &["quotaExceeded"]),
            Err(PublishError::RateLimited { retry_at: None })
        );
        // Account or project wide: the run stops instead of failing a post per run.
        for (status, reason) in [
            (401, "youtubeSignupRequired"),
            (401, "authorizationRequired"),
            (403, "insufficientPermissions"),
            (403, "forbidden"),
            (403, "accessNotConfigured"),
            (403, "authError"),
        ] {
            assert_eq!(
                refuse(status, &[reason]),
                Err(PublishError::Auth),
                "{reason}"
            );
        }
        // An account reason on a 400 is not documented: the cautious, final answer.
        assert_eq!(
            refuse(400, &["youtubeSignupRequired"]),
            Err(PublishError::Rejected)
        );
        assert_eq!(refuse(400, &["somethingNew"]), Err(PublishError::Rejected));
    }

    fn token_reply(status: u16, error: Option<&str>) -> TokenReply {
        TokenReply {
            status,
            access_token: (status == 200).then(|| "ya29.NEW".to_string()),
            refresh_token: None,
            expires_in: (status == 200).then_some(3599),
            token_type: Some("Bearer".into()),
            scope: Some(crate::oauth::YOUTUBE_UPLOAD_SCOPE.into()),
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn a_good_refresh_reply_is_a_token_with_no_refresh_token_and_a_computed_expiry() {
        let t = refresh_outcome(&token_reply(200, None), None, 1_000).unwrap();
        assert_eq!(t.access_token, "ya29.NEW");
        assert_eq!(t.refresh_token, "");
        assert_eq!(t.expires_at, 1_000 + 3599);
    }

    #[test]
    fn a_refresh_is_classified_by_what_the_provider_said() {
        let out = |r: &TokenReply, ra: Option<&str>| refresh_outcome(r, ra, 1_000).unwrap_err();
        assert_eq!(
            out(&token_reply(400, Some("invalid_grant")), None),
            PublishError::Auth
        );
        assert_eq!(
            out(&token_reply(429, None), Some("30")),
            PublishError::RateLimited {
                retry_at: Some(1_030)
            }
        );
        assert_eq!(
            out(&token_reply(429, None), None),
            PublishError::RateLimited { retry_at: None }
        );
        for (status, error) in [
            (401, Some("invalid_client")),
            (400, Some("invalid_client")),
            (401, None),
            (400, None),
            (500, None),
            (503, None),
        ] {
            assert_eq!(
                out(&token_reply(status, error), None),
                PublishError::Transient,
                "{status}"
            );
        }
    }

    #[test]
    fn an_unusable_or_wrongly_scoped_refresh_success_is_classified_not_trusted() {
        let mut no_token = token_reply(200, None);
        no_token.access_token = None;
        assert_eq!(
            refresh_outcome(&no_token, None, 0),
            Err(PublishError::Transient)
        );
        let mut no_expiry = token_reply(200, None);
        no_expiry.expires_in = None;
        assert_eq!(
            refresh_outcome(&no_expiry, None, 0),
            Err(PublishError::Transient)
        );
        let mut not_bearer = token_reply(200, None);
        not_bearer.token_type = Some("mac".into());
        assert_eq!(
            refresh_outcome(&not_bearer, None, 0),
            Err(PublishError::Transient)
        );
        // The grant lost the upload scope: only the owner can fix that.
        let mut wrong_scope = token_reply(200, None);
        wrong_scope.scope = Some("https://www.googleapis.com/auth/youtube.readonly".into());
        assert_eq!(
            refresh_outcome(&wrong_scope, None, 0),
            Err(PublishError::Auth)
        );
    }
}
