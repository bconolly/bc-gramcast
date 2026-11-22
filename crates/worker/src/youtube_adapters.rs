//! Cloudflare adapters for publishing to YouTube: the R2 video lookup and the resumable
//! upload.
//!
//! Thin glue: every decision is in `domain::youtube` (what each answer means) and
//! `domain::upload` (what to send next); the wire shapes are in `youtube_dto`. These
//! adapters cannot run natively, and have not run against YouTube.
//!
//! Secrets stay out of every error and log: nothing here logs, errors carry no text,
//! and the upload session address (a secret) lives only in `SessionUri`, whose Debug is
//! redacted. No response body, header value or URL is kept: a body is read only for a
//! short checked id or error reason codes, and dropped.
//!
//! Assumed, not confirmed: the chunk PUTs and status checks carry `Authorization:
//! Bearer` like the initiation does (the address is checked to be on googleapis.com
//! first); the PUTs carry no `Content-Type`.

use crate::adapters::now_secs;
use crate::net;
use crate::youtube_dto;
use domain::app::PortError;
use domain::publish_app::{PublishError, PublishedRef, Publisher, VideoAccess, VideoLinks};
use domain::queue::QueuedPost;
use domain::token::StoredToken;
use domain::upload::{self, Chunk, Step, Upload};
use domain::youtube::{
    self, Privacy, Reply, SubrequestBudget, START_SUBREQUESTS, UPLOAD_DEADLINE_SECS,
};
use std::cell::RefCell;
use std::time::Duration;
use worker::js_sys::Uint8Array;
use worker::wasm_bindgen::JsValue;
use worker::{
    Bucket, Delay, Headers, Method, Range, Request, RequestInit, RequestRedirect, Response,
};

/// Starts a resumable upload (the brief's URL, as the wire shape).
const INITIATE_URL: &str =
    "https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status";
/// Wait before asking where an upload stands after a lost reply.
const STATUS_CHECK_DELAY: Duration = Duration::from_secs(2);

// ---- the video in R2 ----------------------------------------------------------------

/// Finds a video in the bucket: its key and size. No link is made.
pub struct R2Videos {
    bucket: Bucket,
}

impl R2Videos {
    pub fn new(bucket: Bucket) -> Self {
        Self { bucket }
    }
}

impl VideoLinks for R2Videos {
    async fn access(&self, video_key: &str) -> Result<VideoAccess, PortError> {
        // The error text can carry the key, so it is replaced with fixed text.
        let object = self
            .bucket
            .head(video_key)
            .await
            .map_err(|_| PortError("video lookup failed".into()))?
            .ok_or_else(|| PortError("video missing".into()))?;
        Ok(VideoAccess::new(video_key, object.size()))
    }
}

// ---- the upload ------------------------------------------------------------------------

/// The upload session address. A secret: Debug is redacted and there is no Display.
struct SessionUri(String);

impl std::fmt::Debug for SessionUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionUri([redacted])")
    }
}

/// Publishes through YouTube's resumable upload. One value serves one invocation: the
/// subrequest budget is shared by every publish call in it.
pub struct YouTubePublisher {
    bucket: Bucket,
    privacy: Privacy,
    budget: RefCell<SubrequestBudget>,
}

impl YouTubePublisher {
    pub fn new(bucket: Bucket, privacy: Privacy) -> Self {
        Self {
            bucket,
            privacy,
            budget: RefCell::new(SubrequestBudget::for_upload()),
        }
    }

    /// Starts the session. Any failure here is before anything exists at YouTube.
    async fn start(
        &self,
        post: &QueuedPost,
        video: &VideoAccess,
        token: &StoredToken,
    ) -> Result<SessionUri, PublishError> {
        let body = youtube_dto::initiate_body(&post.title, &post.caption, self.privacy);
        let headers = Headers::new();
        let set = |name: &str, value: &str| headers.set(name, value).ok();
        set("Authorization", &format!("Bearer {}", token.access_token))
            .and(set("Content-Type", "application/json; charset=UTF-8"))
            .and(set("X-Upload-Content-Length", &video.size().to_string()))
            .and(set("X-Upload-Content-Type", "video/mp4"))
            .ok_or(PublishError::Transient)?;
        let request = request(
            Method::Post,
            INITIATE_URL,
            headers,
            Some(JsValue::from_str(&body)),
        )
        .ok_or(PublishError::Transient)?;
        let answer = answer(net::send(request).await).await;
        let location = answer.location.filter(|l| youtube::is_session_uri(l));
        let reply = Reply {
            has_location: location.is_some(),
            ..answer.reply
        };
        youtube::classify_start(&reply, now_secs())?;
        location.map(SessionUri).ok_or(PublishError::Transient)
    }

    /// One PUT to the session: a chunk, or (no bytes) a status check. A request that
    /// fails or times out is a lost reply, which the domain settles.
    async fn put(
        &self,
        session: &SessionUri,
        token: &StoredToken,
        content_range: &str,
        bytes: Option<Vec<u8>>,
    ) -> (Reply, Option<String>) {
        let headers = Headers::new();
        let ok = headers
            .set("Authorization", &format!("Bearer {}", token.access_token))
            .and(headers.set("Content-Range", content_range))
            .is_ok();
        let body = bytes.map(|b| JsValue::from(Uint8Array::from(&b[..])));
        let Some(req) = ok
            .then(|| request(Method::Put, &session.0, headers, body))
            .flatten()
        else {
            return (Reply::lost(), None);
        };
        let answer = answer(net::send(req).await).await;
        (answer.reply, answer.body)
    }

    /// Reads one chunk's bytes from R2 by range. Fails if the object is missing or
    /// not the size the chunk expects (it changed after the size was read).
    async fn read_chunk(&self, key: &str, chunk: &Chunk) -> Option<Vec<u8>> {
        let object = self
            .bucket
            .get(key)
            .range(Range::OffsetWithLength {
                offset: chunk.start,
                length: chunk.byte_len(),
            })
            .execute()
            .await
            .ok()??;
        let bytes = object.body()?.bytes().await.ok()?;
        (bytes.len() as u64 == chunk.byte_len()).then_some(bytes)
    }
}

/// What came back, reduced to what the domain decides on.
struct Answer {
    reply: Reply,
    body: Option<String>,
    location: Option<String>,
}

fn request(method: Method, url: &str, headers: Headers, body: Option<JsValue>) -> Option<Request> {
    let mut init = RequestInit::new();
    init.with_method(method)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Manual);
    if let Some(b) = body {
        init.with_body(Some(b));
    }
    Request::new_with_init(url, &init).ok()
}

/// Turns a response into a `Reply`. The body is read only for a 200 or 201 (the video's
/// id) and a 4xx (error reason codes), then handed on as text for the caller to take the
/// id from; error text is reduced to reason codes here and dropped.
async fn answer(sent: Result<Response, net::NetFail>) -> Answer {
    let Ok(mut response) = sent else {
        return Answer {
            reply: Reply::lost(),
            body: None,
            location: None,
        };
    };
    let status = response.status_code();
    let header = |name: &str| response.headers().get(name).ok().flatten();
    let (range, retry_after, location) =
        (header("Range"), header("Retry-After"), header("Location"));
    let text = if matches!(status, 200 | 201 | 400..=499) {
        response.text().await.ok()
    } else {
        None
    };
    let client_error = (400..500).contains(&status);
    let reason = match (&text, client_error) {
        (Some(t), true) => {
            let codes = youtube_dto::error_reasons(t);
            youtube::api_reason(codes.iter().map(String::as_str))
        }
        _ => youtube::ApiReason::Other,
    };
    Answer {
        reply: Reply {
            status: Some(status),
            retry_after,
            range,
            has_location: false,
            reason,
        },
        body: text.filter(|_| !client_error),
        location,
    }
}

impl Publisher for YouTubePublisher {
    async fn publish(
        &self,
        post: &QueuedPost,
        video: &VideoAccess,
        token: &StoredToken,
    ) -> Result<PublishedRef, PublishError> {
        // The size lookup that made `video` and the start request: nothing sent yet,
        // so running out here is safe to retry.
        if !self.budget.borrow_mut().spend(START_SUBREQUESTS) {
            return Err(PublishError::Transient);
        }
        let started = now_secs();
        let session = self.start(post, video, token).await?;
        let mut up = Upload::new(video.size())?;
        let mut last_body: Option<String> = None;
        let mut step = up.begin(&mut self.budget.borrow_mut());
        loop {
            match step {
                Step::Send(chunk) => {
                    if now_secs().saturating_sub(started) >= UPLOAD_DEADLINE_SECS {
                        return Err(up.out_of_time());
                    }
                    let Some(bytes) = self.read_chunk(video.key(), &chunk).await else {
                        // Not sent: only a final chunk already out makes the outcome unknown.
                        return Err(up.out_of_time());
                    };
                    let (reply, body) = self
                        .put(&session, token, &chunk.content_range(), Some(bytes))
                        .await;
                    last_body = body;
                    step = up.on_chunk(&reply, now_secs(), &mut self.budget.borrow_mut());
                }
                Step::Query => {
                    Delay::from(STATUS_CHECK_DELAY).await;
                    let range = upload::status_content_range(video.size());
                    let (reply, body) = self.put(&session, token, &range, None).await;
                    last_body = body;
                    step = up.on_query(&reply, &mut self.budget.borrow_mut());
                }
                Step::Complete => {
                    let id = last_body.as_deref().and_then(youtube_dto::video_id);
                    return upload::finish(id.as_deref());
                }
                Step::Fail(e) => return Err(e),
            }
        }
    }
}
