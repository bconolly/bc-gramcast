//! The resumable upload as a state machine with no I/O. The Worker adapter does what
//! each `Step` says (read a range from R2 and PUT it, or ask where the upload stands)
//! and feeds the answer back as a `Reply`; every decision is made here.
//!
//! Safety rule (W8): the video exists at the provider only once the last byte is
//! accepted. So a session that is gone, with the final chunk never sent, is safe to
//! start over (`Transient`); once the final chunk may have reached the provider, or any
//! reply was lost, the outcome is `Unknown` unless a status check settles it. Running out
//! of budget, status checks or time follows the same rule: `Unknown` only once the final
//! chunk was sent.

use crate::publish::is_valid_external_id;
use crate::publish_app::{PublishError, PublishedRef};
use crate::youtube::{
    classify_refusal, Reply, SubrequestBudget, CHUNK_BYTES, CHUNK_SUBREQUESTS, MAX_PUBLISH_BYTES,
    STATUS_CHECK_RESERVE,
};

/// One PUT of the upload: bytes `start..=end` of `total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
    pub total: u64,
}

impl Chunk {
    pub fn byte_len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_last(&self) -> bool {
        self.end + 1 == self.total
    }

    /// The `Content-Range` header value, such as `bytes 0-8388607/20971520`.
    pub fn content_range(&self) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, self.total)
    }
}

/// The `Content-Range` of an empty PUT that asks how much the provider has.
pub fn status_content_range(total: u64) -> String {
    format!("bytes */{total}")
}

/// The chunk that starts at `offset`: `CHUNK_BYTES` long, or what is left. Every chunk
/// but the last is a multiple of 256 KiB because `CHUNK_BYTES` is.
pub fn chunk_at(offset: u64, total: u64) -> Option<Chunk> {
    if offset >= total {
        return None;
    }
    let end = offset.saturating_add(CHUNK_BYTES).min(total) - 1;
    Some(Chunk {
        start: offset,
        end,
        total,
    })
}

/// Whether the bytes read back from storage are what `chunk` expects: exactly its length.
/// A short or long read means the object changed after its size was read.
pub fn read_matches(chunk: &Chunk, read_len: u64) -> bool {
    read_len == chunk.byte_len()
}

/// How many bytes the provider says it holds, from a 308's `Range` header. No header
/// means none yet. `bytes=0-N` means `N + 1`. Anything else, or more than `total`, is
/// `None`.
pub fn confirmed_bytes(range: Option<&str>, total: u64) -> Option<u64> {
    let Some(text) = range else { return Some(0) };
    let upper = text.trim().strip_prefix("bytes=0-")?;
    if upper.is_empty() || upper.len() > 15 || !upper.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let received = upper.parse::<u64>().ok()?.checked_add(1)?;
    (received <= total).then_some(received)
}

/// What the adapter does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Read this range from R2 and PUT it with its `Content-Range`.
    Send(Chunk),
    /// An empty PUT with `status_content_range(total)`, to learn where the upload stands.
    Query,
    /// The provider accepted the whole video: read its `id` and call `finish`.
    Complete,
    Fail(PublishError),
}

#[derive(Debug)]
pub struct Upload {
    total: u64,
    in_flight: Option<Chunk>,
    sent_final: bool,
    queries: u32,
}

/// The video resource's `id`, as we are willing to store it. A success reply without a
/// usable id is `Unknown`: the video may exist.
pub fn finish(id: Option<&str>) -> Result<PublishedRef, PublishError> {
    match id {
        Some(id) if is_valid_external_id(id) => Ok(PublishedRef {
            external_id: id.to_string(),
        }),
        _ => Err(PublishError::Unknown),
    }
}

impl Upload {
    /// `total` is the object's size. Empty or above `MAX_PUBLISH_BYTES` is `Rejected`.
    pub fn new(total: u64) -> Result<Upload, PublishError> {
        if total == 0 || total > MAX_PUBLISH_BYTES {
            return Err(PublishError::Rejected);
        }
        Ok(Upload {
            total,
            in_flight: None,
            sent_final: false,
            queries: 0,
        })
    }

    /// The first step, after the session was started.
    pub fn begin(&mut self, budget: &mut SubrequestBudget) -> Step {
        self.send_from(0, budget)
    }

    /// Feeds the answer to the chunk PUT the last `Send` asked for.
    pub fn on_chunk(&mut self, reply: &Reply, now: i64, budget: &mut SubrequestBudget) -> Step {
        let Some(chunk) = self.in_flight.take() else {
            return Step::Fail(PublishError::Unknown);
        };
        let final_before = self.sent_final;
        self.sent_final |= chunk.is_last();
        match reply.status {
            Some(308) => match confirmed_bytes(reply.range.as_deref(), self.total) {
                // A 308 means more is needed, so all of it cannot be there; and it
                // cannot hold more than was sent.
                Some(n) if n < self.total && n <= chunk.end + 1 => self.send_from(n, budget),
                _ => Step::Fail(PublishError::Unknown),
            },
            Some(200 | 201) if chunk.is_last() => Step::Complete,
            // The reply was lost or the server hiccuped: find out before doing more.
            None | Some(408 | 500..=599) => self.query(budget),
            Some(404 | 410) => self.session_gone(),
            // A refusal of the final chunk (every single-chunk video, 8 MiB or less), or of
            // any chunk once the final one has been sent, settles nothing about whether
            // the video exists, so it is unknown.
            Some(400..=499) if chunk.is_last() || final_before => Step::Fail(PublishError::Unknown),
            // A non-final chunk, the final one never sent: nothing can exist yet, so the
            // refusal is classified (and may be retried) as usual.
            Some(s @ 400..=499) => Step::Fail(classify_refusal(reply, s, now)),
            Some(_) => Step::Fail(PublishError::Unknown),
        }
    }

    /// Feeds the answer to the status check the last `Query` asked for.
    pub fn on_query(&mut self, reply: &Reply, budget: &mut SubrequestBudget) -> Step {
        match reply.status {
            Some(308) => match confirmed_bytes(reply.range.as_deref(), self.total) {
                Some(n) if n < self.total => self.send_from(n, budget),
                _ => Step::Fail(PublishError::Unknown),
            },
            Some(200 | 201) => Step::Complete,
            None | Some(408 | 500..=599) => self.query(budget),
            Some(404 | 410) => self.session_gone(),
            // Whatever else, it settles nothing about whether the video exists.
            Some(_) => Step::Fail(PublishError::Unknown),
        }
    }

    /// The adapter ran out of time or could not read the next chunk. Same rule as
    /// running out of budget.
    pub fn out_of_time(&self) -> PublishError {
        self.session_gone_error()
    }

    fn send_from(&mut self, offset: u64, budget: &mut SubrequestBudget) -> Step {
        let Some(chunk) = chunk_at(offset, self.total) else {
            return Step::Fail(PublishError::Unknown);
        };
        if !budget.spend(CHUNK_SUBREQUESTS) {
            return Step::Fail(self.session_gone_error());
        }
        self.in_flight = Some(chunk);
        Step::Send(chunk)
    }

    fn query(&mut self, budget: &mut SubrequestBudget) -> Step {
        if self.queries >= STATUS_CHECK_RESERVE || !budget.spend(1) {
            return Step::Fail(self.session_gone_error());
        }
        self.queries += 1;
        Step::Query
    }

    /// The session is gone. With the final chunk never sent nothing can exist, so a
    /// fresh start is safe; otherwise the video may exist.
    fn session_gone(&self) -> Step {
        Step::Fail(self.session_gone_error())
    }

    /// `Unknown` once the final chunk may have landed, else `Transient`.
    fn session_gone_error(&self) -> PublishError {
        if self.sent_final {
            PublishError::Unknown
        } else {
            PublishError::Transient
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::youtube::{ApiReason, START_SUBREQUESTS, UPLOAD_SUBREQUESTS};

    const MIB: u64 = 1024 * 1024;

    fn budget() -> SubrequestBudget {
        let mut b = SubrequestBudget::for_upload();
        assert!(b.spend(START_SUBREQUESTS));
        b
    }

    fn with_range(status: u16, range: Option<&str>) -> Reply {
        Reply {
            range: range.map(str::to_string),
            ..Reply::status(status)
        }
    }

    fn ok308(upto: u64) -> Reply {
        with_range(308, Some(&format!("bytes=0-{}", upto - 1)))
    }

    /// Runs the machine against a script of replies, recording what it asked for.
    fn drive(total: u64, script: Vec<Reply>) -> (Vec<String>, Step) {
        let mut up = Upload::new(total).unwrap();
        let mut b = budget();
        let mut replies = script.into_iter();
        let mut log = vec![];
        let mut step = up.begin(&mut b);
        loop {
            match step {
                Step::Send(c) => {
                    log.push(c.content_range());
                    step = up.on_chunk(&replies.next().expect("script ran out"), 0, &mut b);
                }
                Step::Query => {
                    log.push(status_content_range(total));
                    step = up.on_query(&replies.next().expect("script ran out"), &mut b);
                }
                done => return (log, done),
            }
        }
    }

    #[test]
    fn chunks_are_8_mib_and_the_last_takes_what_is_left() {
        let c = chunk_at(0, 20 * MIB).unwrap();
        assert_eq!(
            (c.start, c.end, c.byte_len(), c.is_last()),
            (0, 8 * MIB - 1, 8 * MIB, false)
        );
        assert_eq!(c.content_range(), "bytes 0-8388607/20971520");
        let last = chunk_at(16 * MIB, 20 * MIB).unwrap();
        assert_eq!((last.byte_len(), last.is_last()), (4 * MIB, true));
        assert_eq!(last.content_range(), "bytes 16777216-20971519/20971520");
        let tiny = chunk_at(0, 1).unwrap();
        assert_eq!((tiny.start, tiny.end, tiny.is_last()), (0, 0, true));
        assert_eq!(chunk_at(5, 5), None);
        assert_eq!(chunk_at(0, 0), None);
        assert_eq!(status_content_range(2_000_000), "bytes */2000000");
        // Every chunk but the last is a multiple of 256 KiB, even from an odd offset.
        let from_odd = chunk_at(262_144 + 7, 100 * MIB).unwrap();
        assert_eq!(from_odd.byte_len() % (256 * 1024), 0);
    }

    #[test]
    fn a_read_must_be_exactly_the_chunk() {
        let c = chunk_at(8 * MIB, 20 * MIB).unwrap();
        assert!(read_matches(&c, 8 * MIB));
        assert!(!read_matches(&c, 8 * MIB - 1));
        assert!(!read_matches(&c, 8 * MIB + 1));
        assert!(!read_matches(&c, 0));
    }

    #[test]
    fn the_range_header_says_how_much_the_provider_holds() {
        assert_eq!(confirmed_bytes(None, 100), Some(0));
        assert_eq!(confirmed_bytes(Some("bytes=0-0"), 100), Some(1));
        assert_eq!(confirmed_bytes(Some("bytes=0-99"), 100), Some(100));
        assert_eq!(confirmed_bytes(Some(" bytes=0-49 "), 100), Some(50));
        for bad in [
            "",
            "bytes=1-5",
            "bytes=0-",
            "bytes=0--1",
            "bytes=0-1,5-9",
            "0-5",
            "bytes=0-100",
            "bytes=0-x",
            "bytes=0-99999999999999999999",
        ] {
            assert_eq!(confirmed_bytes(Some(bad), 100), None, "{bad:?}");
        }
    }

    #[test]
    fn an_empty_or_oversized_video_is_rejected_before_anything_is_sent() {
        assert_eq!(Upload::new(0).unwrap_err(), PublishError::Rejected);
        assert_eq!(
            Upload::new(MAX_PUBLISH_BYTES + 1).unwrap_err(),
            PublishError::Rejected
        );
        assert!(Upload::new(1).is_ok());
        assert!(Upload::new(MAX_PUBLISH_BYTES).is_ok());
    }

    #[test]
    fn a_small_video_is_one_chunk() {
        let (log, end) = drive(2_000_000, vec![Reply::status(200)]);
        assert_eq!(log, ["bytes 0-1999999/2000000"]);
        assert_eq!(end, Step::Complete);
    }

    #[test]
    fn a_large_video_goes_chunk_by_chunk_on_308s() {
        let (log, end) = drive(
            20 * MIB,
            vec![ok308(8 * MIB), ok308(16 * MIB), Reply::status(201)],
        );
        assert_eq!(
            log,
            [
                "bytes 0-8388607/20971520",
                "bytes 8388608-16777215/20971520",
                "bytes 16777216-20971519/20971520"
            ]
        );
        assert_eq!(end, Step::Complete);
    }

    #[test]
    fn a_308_resumes_from_what_the_provider_holds() {
        // It kept only 256 KiB of the first chunk, then nothing at all.
        let (log, end) = drive(
            10 * MIB,
            vec![
                ok308(256 * 1024),
                with_range(308, None),
                ok308(8 * MIB),
                Reply::status(200),
            ],
        );
        assert_eq!(
            log,
            [
                "bytes 0-8388607/10485760",
                "bytes 262144-8650751/10485760",
                "bytes 0-8388607/10485760",
                "bytes 8388608-10485759/10485760"
            ]
        );
        assert_eq!(end, Step::Complete);
    }

    #[test]
    fn a_lost_reply_is_settled_by_a_status_check_before_anything_else() {
        let (log, end) = drive(
            20 * MIB,
            vec![
                ok308(8 * MIB),
                Reply::lost(),
                ok308(8 * MIB + 262_144),
                ok308(16 * MIB + 262_144),
                Reply::status(200),
            ],
        );
        assert_eq!(log[1], "bytes 8388608-16777215/20971520");
        assert_eq!(log[2], "bytes */20971520");
        assert_eq!(log[3], "bytes 8650752-17039359/20971520");
        assert_eq!(end, Step::Complete);
        // A 5xx or a 408 on a chunk is handled the same way.
        for status in [500, 503, 408] {
            let (log, end) = drive(
                10 * MIB,
                vec![with_range(status, None), ok308(8 * MIB), Reply::status(200)],
            );
            assert_eq!(log[1], "bytes */10485760", "{status}");
            assert_eq!(end, Step::Complete);
        }
    }

    #[test]
    fn a_lost_final_reply_that_the_status_check_shows_complete_is_a_success() {
        let (log, end) = drive(1_000, vec![Reply::lost(), Reply::status(200)]);
        assert_eq!(log, ["bytes 0-999/1000", "bytes */1000"]);
        assert_eq!(end, Step::Complete);
    }

    #[test]
    fn a_session_that_is_gone_is_safe_to_restart_only_if_the_final_chunk_was_never_sent() {
        // First chunk of two lost, then the session is gone: no final chunk, so Transient.
        let (_, end) = drive(20 * MIB, vec![Reply::lost(), Reply::status(404)]);
        assert_eq!(end, Step::Fail(PublishError::Transient));
        let (_, end) = drive(20 * MIB, vec![Reply::status(404)]);
        assert_eq!(end, Step::Fail(PublishError::Transient));
        let (_, end) = drive(20 * MIB, vec![ok308(8 * MIB), Reply::status(410)]);
        assert_eq!(end, Step::Fail(PublishError::Transient));
        // The final chunk went out and then the session is gone: the video may exist.
        let (_, end) = drive(1_000, vec![Reply::lost(), Reply::status(404)]);
        assert_eq!(end, Step::Fail(PublishError::Unknown));
        let (_, end) = drive(1_000, vec![Reply::status(404)]);
        assert_eq!(end, Step::Fail(PublishError::Unknown));
    }

    #[test]
    fn a_status_check_that_keeps_failing_ends_unknown_once_the_budget_is_spent() {
        let mut script = vec![Reply::lost()];
        script.extend(std::iter::repeat_with(Reply::lost).take(100));
        let mut up = Upload::new(1_000).unwrap();
        let mut b = budget();
        let mut step = up.begin(&mut b);
        let mut queries = 0;
        for reply in script {
            step = match step {
                Step::Send(_) => up.on_chunk(&reply, 0, &mut b),
                Step::Query => {
                    queries += 1;
                    up.on_query(&reply, &mut b)
                }
                _ => break,
            };
        }
        assert_eq!(step, Step::Fail(PublishError::Unknown));
        assert_eq!(
            b.left(),
            UPLOAD_SUBREQUESTS - START_SUBREQUESTS - CHUNK_SUBREQUESTS - queries
        );
        assert!(queries >= 6, "{queries}");
    }

    #[test]
    fn an_explicit_refusal_of_a_chunk_is_classified_not_unknown() {
        let r = |status, reason| Reply {
            reason,
            ..Reply::status(status)
        };
        let end = |reply: Reply| drive(20 * MIB, vec![reply]).1;
        assert_eq!(
            end(r(401, ApiReason::Other)),
            Step::Fail(PublishError::Auth)
        );
        assert_eq!(
            end(r(403, ApiReason::Quota)),
            Step::Fail(PublishError::RateLimited { retry_at: None })
        );
        assert_eq!(
            end(r(403, ApiReason::Other)),
            Step::Fail(PublishError::Rejected)
        );
        assert_eq!(
            end(r(400, ApiReason::Other)),
            Step::Fail(PublishError::Rejected)
        );
        assert_eq!(
            end(Reply {
                retry_after: Some("60".into()),
                ..r(429, ApiReason::Other)
            }),
            Step::Fail(PublishError::RateLimited { retry_at: Some(60) })
        );
    }

    fn refused(status: u16, reason: ApiReason) -> Reply {
        Reply {
            reason,
            retry_after: (status == 429).then(|| "60".to_string()),
            ..Reply::status(status)
        }
    }

    #[test]
    fn a_refusal_of_the_final_chunk_is_unknown() {
        // A single-chunk video (8 MiB or less) sends its final chunk first: a refusal of
        // it may still have let the video through, so it is not classified.
        for (status, reason) in [
            (401, ApiReason::Other),
            (429, ApiReason::Other),
            (400, ApiReason::Other),
            (403, ApiReason::Quota),
        ] {
            let (_, end) = drive(MIB, vec![refused(status, reason)]);
            assert_eq!(end, Step::Fail(PublishError::Unknown), "{status}");
        }
        // The same for the last chunk of a bigger video.
        let (_, end) = drive(
            20 * MIB,
            vec![
                ok308(8 * MIB),
                ok308(16 * MIB),
                refused(401, ApiReason::Other),
            ],
        );
        assert_eq!(end, Step::Fail(PublishError::Unknown));
    }

    #[test]
    fn a_refusal_of_a_non_final_chunk_is_classified_as_before() {
        // Nothing can exist before the final chunk, so these are retryable per the usual rules.
        let end = |reply| drive(20 * MIB, vec![ok308(8 * MIB), reply]).1;
        assert_eq!(
            end(refused(401, ApiReason::Other)),
            Step::Fail(PublishError::Auth)
        );
        assert_eq!(
            end(refused(429, ApiReason::Other)),
            Step::Fail(PublishError::RateLimited { retry_at: Some(60) })
        );
        assert_eq!(
            end(refused(400, ApiReason::Other)),
            Step::Fail(PublishError::Rejected)
        );
        assert_eq!(
            end(refused(403, ApiReason::Quota)),
            Step::Fail(PublishError::RateLimited { retry_at: None })
        );
        // The first chunk of a big video, and one after a lost reply settled by a check.
        let (_, first) = drive(20 * MIB, vec![refused(401, ApiReason::Other)]);
        assert_eq!(first, Step::Fail(PublishError::Auth));
        let (_, after_lost) = drive(
            20 * MIB,
            vec![
                Reply::lost(),
                ok308(8 * MIB),
                refused(401, ApiReason::Other),
            ],
        );
        assert_eq!(after_lost, Step::Fail(PublishError::Auth));
    }

    #[test]
    fn a_refusal_once_the_final_chunk_was_sent_is_unknown_on_every_path() {
        // The final chunk's reply is lost; the status check is refused.
        for status in [401, 403, 429, 400] {
            let (_, end) = drive(MIB, vec![Reply::lost(), refused(status, ApiReason::Other)]);
            assert_eq!(end, Step::Fail(PublishError::Unknown), "{status}");
        }
        // The final chunk went out, the status check asked for a resend of an earlier
        // part, and that (non-final) chunk is refused: the final was sent before, so Unknown.
        let (_, end) = drive(
            10 * MIB,
            vec![
                ok308(8 * MIB),
                Reply::lost(),
                ok308(8 * MIB),
                refused(401, ApiReason::Other),
            ],
        );
        assert_eq!(end, Step::Fail(PublishError::Unknown));
    }

    #[test]
    fn a_non_final_resend_refused_after_the_final_chunk_went_out_is_unknown() {
        // The final chunk's reply is lost; the status check says only 4 MiB arrived, so
        // a non-final chunk is resent from there, and it is refused. The final chunk was
        // sent earlier, so the video may exist: Unknown, not Auth.
        let (log, end) = drive(
            20 * MIB,
            vec![
                ok308(8 * MIB),
                ok308(16 * MIB),
                Reply::lost(),
                with_range(308, Some("bytes=0-4194303")),
                refused(401, ApiReason::Other),
            ],
        );
        assert_eq!(log[3], "bytes */20971520");
        assert_eq!(log[4], "bytes 4194304-12582911/20971520");
        assert_eq!(end, Step::Fail(PublishError::Unknown));
    }

    #[test]
    fn answers_that_make_no_sense_are_unknown() {
        // A 200 for a chunk that is not the last; a 3xx or 2xx we do not know.
        for reply in [
            Reply::status(200),
            Reply::status(201),
            Reply::status(204),
            Reply::status(302),
        ] {
            let (_, end) = drive(20 * MIB, vec![reply]);
            assert_eq!(end, Step::Fail(PublishError::Unknown));
        }
        // A 308 whose Range is malformed, claims more than was sent, or says all of it arrived.
        for range in ["bytes=5-9", "bytes=0-99999999999", "junk"] {
            let (_, end) = drive(20 * MIB, vec![with_range(308, Some(range))]);
            assert_eq!(end, Step::Fail(PublishError::Unknown), "{range}");
        }
        let (_, end) = drive(20 * MIB, vec![with_range(308, Some("bytes=0-20971519"))]);
        assert_eq!(end, Step::Fail(PublishError::Unknown));
        // Same for the answer to a status check.
        let (_, end) = drive(
            1_000,
            vec![Reply::lost(), with_range(308, Some("bytes=0-999"))],
        );
        assert_eq!(end, Step::Fail(PublishError::Unknown));
        let (_, end) = drive(1_000, vec![Reply::lost(), Reply::status(401)]);
        assert_eq!(end, Step::Fail(PublishError::Unknown));
    }

    #[test]
    fn the_biggest_video_fits_the_budget_with_the_status_reserve_untouched() {
        let mut up = Upload::new(MAX_PUBLISH_BYTES).unwrap();
        let mut b = budget();
        let mut step = up.begin(&mut b);
        let mut sent = 0;
        while let Step::Send(c) = step {
            sent += 1;
            let reply = if c.is_last() {
                Reply::status(200)
            } else {
                ok308(c.end + 1)
            };
            step = up.on_chunk(&reply, 0, &mut b);
        }
        assert_eq!(step, Step::Complete);
        assert_eq!(sent, crate::youtube::MAX_CHUNKS);
        assert_eq!(b.left(), crate::youtube::STATUS_CHECK_RESERVE);
    }

    #[test]
    fn a_video_that_cannot_finish_inside_the_budget_stops_with_the_right_error() {
        // Nothing sent yet: safe to retry.
        let mut up = Upload::new(MIB).unwrap();
        let mut none = SubrequestBudget::for_upload();
        assert!(none.spend(UPLOAD_SUBREQUESTS));
        assert_eq!(up.begin(&mut none), Step::Fail(PublishError::Transient));
        // A non-final chunk went out, then the budget ran out: nothing can exist before
        // the final chunk, so this is safe to retry too.
        let mut up = Upload::new(20 * MIB).unwrap();
        let mut one = SubrequestBudget::for_upload();
        assert!(one.spend(UPLOAD_SUBREQUESTS - CHUNK_SUBREQUESTS));
        let Step::Send(_) = up.begin(&mut one) else {
            panic!()
        };
        assert_eq!(
            up.on_chunk(&ok308(8 * MIB), 0, &mut one),
            Step::Fail(PublishError::Transient)
        );
        // The final chunk went out and the budget ran out before it was settled.
        let mut up = Upload::new(MIB).unwrap();
        let mut b = budget();
        let Step::Send(_) = up.begin(&mut b) else {
            panic!()
        };
        assert!(b.spend(b.left()));
        assert_eq!(
            up.on_chunk(&Reply::lost(), 0, &mut b),
            Step::Fail(PublishError::Unknown)
        );
    }

    #[test]
    fn lost_replies_on_non_final_chunks_of_the_biggest_video_end_transient() {
        let mut up = Upload::new(MAX_PUBLISH_BYTES).unwrap();
        let mut b = budget();
        let mut step = up.begin(&mut b);
        let mut lost = 0;
        let mut held = 0;
        // Every chunk reply is lost and each status check shows the chunk arrived, so
        // the upload creeps on until the status checks (or the budget) run out.
        while !matches!(step, Step::Fail(_) | Step::Complete) {
            step = match step {
                Step::Send(c) => {
                    lost += 1;
                    held = c.end + 1;
                    up.on_chunk(&Reply::lost(), 0, &mut b)
                }
                _ => up.on_query(&ok308(held), &mut b),
            };
        }
        assert!(lost >= 3, "{lost}");
        assert_eq!(step, Step::Fail(PublishError::Transient));
    }

    #[test]
    fn running_out_of_time_follows_the_same_rule() {
        let mut up = Upload::new(20 * MIB).unwrap();
        let mut b = budget();
        let Step::Send(_) = up.begin(&mut b) else {
            panic!()
        };
        assert_eq!(up.out_of_time(), PublishError::Transient);
        up.on_chunk(&ok308(8 * MIB), 0, &mut b);
        assert_eq!(up.out_of_time(), PublishError::Transient);
        let mut up = Upload::new(1_000).unwrap();
        let mut b = budget();
        up.begin(&mut b);
        up.on_chunk(&Reply::lost(), 0, &mut b);
        assert_eq!(up.out_of_time(), PublishError::Unknown);
    }

    #[test]
    fn finishing_takes_only_a_well_formed_id() {
        assert_eq!(
            finish(Some("dQw4w9WgXcQ")).unwrap().external_id,
            "dQw4w9WgXcQ"
        );
        for bad in [
            None,
            Some(""),
            Some("a b"),
            Some("<x>"),
            Some(&"a".repeat(33)[..]),
        ] {
            assert_eq!(finish(bad), Err(PublishError::Unknown), "{bad:?}");
        }
    }
}
