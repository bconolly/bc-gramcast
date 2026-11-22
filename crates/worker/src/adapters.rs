//! Cloudflare adapters for the domain ports: D1 for posts, R2 presigning and
//! the R2 binding for uploads.

use crate::dto;
use domain::app::{
    DeleteError, InsertError, PortError, PostRepository, SlotPatternRepository, UpdateError,
    UploadSigner, UploadStore,
};
use domain::queue::QueuedPost;
use domain::slots::SlotPattern;
use serde::Deserialize;
use worker::{Bucket, D1Database, Env};

pub struct D1PostRepository {
    db: D1Database,
}

impl D1PostRepository {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[derive(Deserialize)]
struct PostRow {
    id: String,
    video_key: String,
    title: String,
    caption: String,
    platforms: String,
    slot_at: f64,
}

/// Rows changed by a write, from D1's result metadata, if it reports them.
fn changes(result: &worker::D1Result) -> Result<Option<usize>, PortError> {
    let meta = result.meta().map_err(port_err)?;
    Ok(meta.and_then(|m| m.changes))
}

fn row_to_post(r: PostRow) -> Result<QueuedPost, PortError> {
    let platforms = r
        .platforms
        .split(',')
        .map(dto::platform_from_str)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            // Row id only: never log URLs, keys or secrets.
            worker::console_error!("bad platforms in posts row {}", r.id);
            PortError("bad platforms in row".into())
        })?;
    Ok(QueuedPost {
        id: r.id,
        video_key: r.video_key,
        title: r.title,
        caption: r.caption,
        platforms,
        slot_at: r.slot_at as i64,
    })
}

fn port_err(e: impl std::fmt::Debug) -> PortError {
    PortError(format!("{e:?}"))
}

/// Builds the text the pure matcher in `dto` searches.
///
/// Found in worker 0.9.0 (src/d1/mod.rs, src/error.rs): `worker::Error::D1`
/// wraps a `D1Error` around a `js_sys::Error` whose `message` is only the
/// generic "D1_ERROR: ..." text; the SQLite detail ("UNIQUE constraint failed:
/// posts.slot_at") lives in the JS error's `cause`. `D1Error::cause()` returns
/// that cause's message, `Display` prints the cause, and `Error`'s Display uses
/// the `{:#?}` Debug form, which only shows the cause as a raw JsValue. So we
/// join the cause and the JS error message explicitly rather than rely on a
/// formatter, and fall back to Display for any other error kind.
fn d1_error_text(e: &worker::Error) -> String {
    match e {
        worker::Error::D1(d1) => {
            let js: &worker::js_sys::Error = d1.as_ref();
            format!("{} | {}", d1.cause(), String::from(js.message()))
        }
        other => other.to_string(),
    }
}

impl PostRepository for D1PostRepository {
    async fn all(&self) -> Result<Vec<QueuedPost>, PortError> {
        let rows = self
            .db
            .prepare("SELECT id, video_key, title, caption, platforms, slot_at FROM posts")
            .all()
            .await
            .map_err(port_err)?
            .results::<PostRow>()
            .map_err(port_err)?;
        rows.into_iter().map(row_to_post).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<QueuedPost>, PortError> {
        self.db
            .prepare(
                "SELECT id, video_key, title, caption, platforms, slot_at FROM posts WHERE id = ?1",
            )
            .bind(&[id.into()])
            .map_err(port_err)?
            .first::<PostRow>(None)
            .await
            .map_err(port_err)?
            .map(row_to_post)
            .transpose()
    }

    async fn insert(&self, post: &QueuedPost) -> Result<(), InsertError> {
        let platforms: Vec<&str> = post
            .platforms
            .iter()
            .map(|p| dto::platform_to_str(*p))
            .collect();
        // slot_at is UNIQUE in the schema, so a racing insert for the same slot fails here.
        self.db
            .prepare(
                "INSERT INTO posts (id, video_key, title, caption, platforms, slot_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(&[
                post.id.as_str().into(),
                post.video_key.as_str().into(),
                post.title.as_str().into(),
                post.caption.as_str().into(),
                platforms.join(",").into(),
                // D1 binds numbers as JS doubles. The domain caps slot_at at 2100,
                // far below 2^53, so the f64 is exact.
                (post.slot_at as f64).into(),
            ])
            .map_err(|e| InsertError::Port(port_err(e)))?
            .run()
            .await
            .map_err(|e| dto::classify_insert_error(&d1_error_text(&e)))?;
        Ok(())
    }

    async fn update(&self, post: &QueuedPost, now: i64) -> Result<(), UpdateError> {
        let platforms: Vec<&str> = post
            .platforms
            .iter()
            .map(|p| dto::platform_to_str(*p))
            .collect();
        // video_key and id are never in the SET list, so they cannot change. The
        // slot_at > now test is the "still queued" rule again. `now` is the time
        // the request started, so this guards against other writers, not against
        // the slot passing while this request runs (a gap for the publishing slice).
        let result = self
            .db
            .prepare("UPDATE posts SET title = ?1, caption = ?2, platforms = ?3, slot_at = ?4 WHERE id = ?5 AND slot_at > ?6")
            .bind(&[
                post.title.as_str().into(),
                post.caption.as_str().into(),
                platforms.join(",").into(),
                (post.slot_at as f64).into(),
                post.id.as_str().into(),
                (now as f64).into(),
            ])
            .map_err(|e| UpdateError::Port(port_err(e)))?
            .run()
            .await
            .map_err(|e| dto::classify_update_error(&d1_error_text(&e)))?;
        let changes = changes(&result).map_err(UpdateError::Port)?;
        if dto::write_applied(changes) {
            return Ok(());
        }
        let row = self.get(&post.id).await.map_err(UpdateError::Port)?;
        dto::update_after_miss(changes, row.map(|p| p.slot_at), now)
    }

    async fn delete(&self, id: &str, now: i64) -> Result<(), DeleteError> {
        let result = self
            .db
            .prepare("DELETE FROM posts WHERE id = ?1 AND slot_at > ?2")
            .bind(&[id.into(), (now as f64).into()])
            .map_err(|e| DeleteError::Port(port_err(e)))?
            .run()
            .await
            .map_err(|e| DeleteError::Port(port_err(e)))?;
        let changes = changes(&result).map_err(DeleteError::Port)?;
        if dto::write_applied(changes) {
            return Ok(());
        }
        let row = self.get(id).await.map_err(|e| {
            worker::console_error!("{}", dto::DELETE_UNCONFIRMED_LINE);
            DeleteError::Port(e)
        })?;
        dto::delete_after_miss(changes, row.map(|p| p.slot_at), now)
    }
}

pub struct D1SlotPatternRepository {
    db: D1Database,
}

impl D1SlotPatternRepository {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[derive(Deserialize)]
struct PatternRow {
    utc_offset_minutes: f64,
    rules: String,
}

impl SlotPatternRepository for D1SlotPatternRepository {
    async fn get(&self) -> Result<Option<SlotPattern>, PortError> {
        let row = self
            .db
            .prepare("SELECT utc_offset_minutes, rules FROM slot_pattern WHERE id = 1")
            .first::<PatternRow>(None)
            .await
            .map_err(port_err)?;
        row.map(|r| {
            // The row text is never logged or returned, only that it was bad.
            SlotPattern::from_row(r.utc_offset_minutes, &r.rules).map_err(|_| {
                worker::console_error!("bad slot_pattern row");
                PortError("bad slot_pattern row".into())
            })
        })
        .transpose()
    }

    async fn save(&self, p: &SlotPattern) -> Result<(), PortError> {
        self.db
            .prepare(
                "INSERT INTO slot_pattern (id, utc_offset_minutes, rules) VALUES (1, ?1, ?2) \
                 ON CONFLICT(id) DO UPDATE SET utc_offset_minutes = excluded.utc_offset_minutes, rules = excluded.rules",
            )
            .bind(&[
                f64::from(p.utc_offset_minutes).into(),
                p.rules_to_stored().into(),
            ])
            .map_err(port_err)?
            .run()
            .await
            .map_err(port_err)?;
        Ok(())
    }
}

pub struct R2Signer {
    access_key_id: String,
    secret_access_key: String,
    host: String,
    bucket: String,
    amz_date: String,
}

impl R2Signer {
    pub fn from_env(env: &Env, now_secs: i64) -> worker::Result<Self> {
        let account = env.var("R2_ACCOUNT_ID")?.to_string();
        Ok(Self {
            access_key_id: env.secret("R2_ACCESS_KEY_ID")?.to_string(),
            secret_access_key: env.secret("R2_SECRET_ACCESS_KEY")?.to_string(),
            host: format!("{account}.r2.cloudflarestorage.com"),
            bucket: env.var("R2_BUCKET")?.to_string(),
            amz_date: presign::amz_date_from_unix(now_secs),
        })
    }
}

impl UploadSigner for R2Signer {
    fn put_url(
        &self,
        key: &str,
        content_type: &str,
        expires_secs: u32,
    ) -> Result<String, PortError> {
        let creds = presign::Credentials {
            access_key_id: &self.access_key_id,
            secret_access_key: &self.secret_access_key,
        };
        let path = format!("/{}/{key}", self.bucket);
        let req = presign::PresignRequest {
            method: presign::Method::Put,
            host: &self.host,
            path: &path,
            region: "auto",
            amz_date: &self.amz_date,
            expires_secs,
            content_type: Some(content_type),
        };
        presign::presign(&creds, &req).map_err(port_err)
    }
}

/// Reads object metadata through the R2 binding (`VIDEOS`), no signing needed.
pub struct R2Store {
    bucket: Bucket,
}

impl R2Store {
    pub fn new(bucket: Bucket) -> Self {
        Self { bucket }
    }
}

impl UploadStore for R2Store {
    async fn object_size(&self, key: &str) -> Result<Option<u64>, PortError> {
        // head() returns None for a missing key. The error text can carry the key, so
        // it goes through port_err, which the API never shows to the client.
        let object = self.bucket.head(key).await.map_err(port_err)?;
        Ok(object.map(|o| o.size()))
    }

    async fn delete_object(&self, key: &str) -> Result<(), PortError> {
        // R2 delete succeeds for a missing key. The error text can carry the key,
        // so it goes through port_err, which is never shown or logged.
        self.bucket.delete(key).await.map_err(port_err)
    }
}

// ---- Cloudflare Access signature check -----------------------------------
//
// Thin glue: the key-cache policy is `domain::keycache`, the key-set parsing is
// `dto::parse_jwks`, and the token checks are `domain::auth`. This only fetches
// the key set and calls Web Crypto (`crypto.subtle`) through js_sys, which the
// `worker` crate re-exports, so no dependency or web-sys feature was added.
// It cannot run natively. It logs fixed reason text and counts only, never tokens, keys or URLs.

use domain::app::{SignatureVerifier, VerifyError};
use domain::keycache::{self, KeyCache, KeyLookup};
use std::cell::RefCell;
use std::time::Duration;
use worker::js_sys::{self, Array, Function, Object, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Delay, Fetch, Method, Request, RequestInit, RequestRedirect};

thread_local! {
    /// Imported public keys, kept for the life of the isolate.
    static KEYS: RefCell<KeyCache<JsValue>> = const { RefCell::new(KeyCache::new_const()) };
}

fn now_secs() -> i64 {
    (worker::Date::now().as_millis() / 1000) as i64
}

/// If the request that is fetching the keys is dropped (the client went away),
/// this marks the fetch as failed so the cache is not left "fetching" until the
/// timeout. That the runtime drops the future when the client disconnects is
/// assumed, not verified; the 30 s fetch timeout in `KeyCache` is the backstop.
/// It cannot be unit tested natively (it needs the wasm runtime); the logic it
/// calls, `KeyCache::fetch_failed`, is tested in the domain.
struct FetchGuard {
    started: i64,
    finished: bool,
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        if !self.finished {
            KEYS.with(|c| c.borrow_mut().fetch_failed(self.started, now_secs()));
        }
    }
}

pub struct AccessVerifier {
    certs_url: String,
}

impl AccessVerifier {
    /// `certs_url` must come from `AccessConfig::certs_url`, never from a token.
    pub fn new(certs_url: String) -> Self {
        Self { certs_url }
    }

    /// Asks the cache and sleeps while another request is fetching. All the
    /// decisions are the domain's (`KeyCache`).
    async fn key_for(&self, kid: &str) -> Result<JsValue, VerifyError> {
        for _ in 0..keycache::WAIT_POLLS {
            let now = now_secs();
            let lookup = KEYS.with(|c| c.borrow_mut().lookup(kid, now));
            match lookup {
                KeyLookup::Use(key) => return Ok(key),
                KeyLookup::Fetch => return self.fetch_and_find(kid, now).await,
                KeyLookup::Wait => {
                    Delay::from(Duration::from_millis(keycache::WAIT_STEP_MS)).await;
                }
                other => return Err(failure(&other)),
            }
        }
        Err(unavailable("keys still loading"))
    }

    /// This request does the fetch (begun at `started`).
    async fn fetch_and_find(&self, kid: &str, started: i64) -> Result<JsValue, VerifyError> {
        let mut guard = FetchGuard {
            started,
            finished: false,
        };
        let keys = match self.refresh().await {
            Ok(keys) => Some(keys),
            Err(err) => {
                worker::console_log!("{}", dto::key_refresh_failed_line(&err.0));
                None
            }
        };
        guard.finished = true;
        KEYS.with(|c| c.borrow_mut().fetched(kid, started, keys, now_secs()))
    }

    async fn refresh(&self) -> Result<Vec<(String, JsValue)>, PortError> {
        let mut init = RequestInit::new();
        // Workers does not implement redirect mode "error" (constructing such
        // a Request throws), so use "manual": a redirect is then returned as
        // a 3xx response, never followed, and refused by the status check.
        init.with_method(Method::Get)
            .with_redirect(RequestRedirect::Manual);
        let request =
            Request::new_with_init(&self.certs_url, &init).map_err(|_| port("bad key request"))?;
        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|_| port("key fetch failed"))?;
        if !dto::jwks_status_ok(response.status_code()) {
            return Err(port("key fetch status"));
        }
        // Refuse an oversized body before reading it. Without the header, the
        // size is checked again by `parse_jwks`.
        let declared = response.headers().get("Content-Length").ok().flatten();
        if !dto::jwks_length_ok(declared.as_deref()) {
            return Err(port("key body too large"));
        }
        let body = response.text().await.map_err(|_| port("key body"))?;
        let jwks = dto::parse_jwks(&body).map_err(|_| port("key set not usable"))?;
        let parsed = jwks.len();
        let mut imported = Vec::new();
        for jwk in jwks {
            if let Ok(key) = import_key(&jwk).await {
                imported.push((jwk.kid, key));
            }
        }
        if imported.is_empty() {
            worker::console_log!("{}", dto::key_import_counts_line(parsed, 0));
            return Err(port("no key imported"));
        }
        Ok(imported)
    }
}

fn failure(lookup: &KeyLookup<JsValue>) -> VerifyError {
    keycache::lookup_failure(lookup).unwrap_or_else(|| unavailable("keys unavailable"))
}

impl SignatureVerifier for AccessVerifier {
    async fn verify(
        &self,
        kid: &str,
        signing_input: &[u8],
        signature: &[u8],
    ) -> Result<bool, VerifyError> {
        let key = self.key_for(kid).await?;
        let subtle = subtle().map_err(|_| unavailable("no web crypto"))?;
        let args = [
            JsValue::from_str("RSASSA-PKCS1-v1_5"),
            key,
            Uint8Array::from(signature).into(),
            Uint8Array::from(signing_input).into(),
        ];
        let result = call(&subtle, "verify", &args)
            .await
            .map_err(|_| unavailable("verify failed"))?;
        Ok(result.as_bool() == Some(true))
    }
}

fn port(text: &str) -> PortError {
    PortError(text.to_string())
}

fn unavailable(text: &str) -> VerifyError {
    VerifyError::Unavailable(port(text))
}

fn object(fields: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let o = Object::new();
    for (k, v) in fields {
        Reflect::set(&o, &JsValue::from_str(k), v)?;
    }
    Ok(o)
}

fn subtle() -> Result<JsValue, JsValue> {
    let crypto = Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))?;
    Reflect::get(&crypto, &JsValue::from_str("subtle"))
}

/// Calls `crypto.subtle[method](...args)` and awaits the promise.
async fn call(subtle: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f: Function = Reflect::get(subtle, &JsValue::from_str(method))?.dyn_into()?;
    let list = Array::new();
    for a in args {
        list.push(a);
    }
    let promise: js_sys::Promise = f.apply(subtle, &list)?.dyn_into()?;
    JsFuture::from(promise).await
}

/// `importKey("jwk", jwk, {name: RSASSA-PKCS1-v1_5, hash: SHA-256}, false, ["verify"])`.
/// Checked against Node's Web Crypto; see the slice notes in the README.
async fn import_key(jwk: &dto::Jwk) -> Result<JsValue, JsValue> {
    let key_data = object(&[
        ("kty", "RSA".into()),
        ("n", jwk.n.as_str().into()),
        ("e", jwk.e.as_str().into()),
        ("alg", "RS256".into()),
        ("ext", JsValue::TRUE),
    ])?;
    let algorithm = object(&[
        ("name", "RSASSA-PKCS1-v1_5".into()),
        ("hash", "SHA-256".into()),
    ])?;
    let usages = Array::of1(&JsValue::from_str("verify"));
    let args = [
        "jwk".into(),
        key_data.into(),
        algorithm.into(),
        JsValue::FALSE,
        usages.into(),
    ];
    call(&subtle()?, "importKey", &args).await
}
