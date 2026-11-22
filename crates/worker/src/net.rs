//! One outgoing request with a timeout. Thin glue: when to give up is
//! `domain::youtube::REQUEST_TIMEOUT_SECS`. A failure carries no detail on purpose: the
//! underlying error can name the URL, which for an upload session is a secret.

use domain::youtube::{body_within_cap, REQUEST_TIMEOUT_SECS};
use worker::js_sys::{self, Function, Reflect};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::{AbortSignal, Fetch, Request, Response};

/// The request failed or timed out. Whether the provider acted on it is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetFail;

/// `AbortSignal.timeout(ms)`. That Workers provides it is assumed, not confirmed here;
/// if it does not, every request fails closed with `NetFail` before anything is sent.
fn timeout_signal() -> Result<AbortSignal, NetFail> {
    let ctor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("AbortSignal")).map_err(|_| NetFail)?;
    let make: Function = Reflect::get(&ctor, &JsValue::from_str("timeout"))
        .map_err(|_| NetFail)?
        .dyn_into()
        .map_err(|_| NetFail)?;
    let ms = JsValue::from_f64((REQUEST_TIMEOUT_SECS * 1000) as f64);
    let signal = make.call1(&ctor, &ms).map_err(|_| NetFail)?;
    let signal: worker::web_sys::AbortSignal = signal.dyn_into().map_err(|_| NetFail)?;
    Ok(AbortSignal::from(signal))
}

/// Sends the request, giving up after `REQUEST_TIMEOUT_SECS`. Redirects are not followed
/// (the caller builds requests with manual redirect).
pub async fn send(request: Request) -> Result<Response, NetFail> {
    let signal = timeout_signal()?;
    Fetch::Request(request)
        .send_with_signal(&signal)
        .await
        .map_err(|_| NetFail)
}

/// Stops the platform downloading a body we will not read. Best effort: a failure to
/// cancel changes nothing, the response is dropped either way.
pub fn discard_body(response: Response) {
    let raw: worker::web_sys::Response = response.into();
    if let Some(body) = raw.body() {
        let _ = body.cancel();
    }
}

/// The body as text, if its declared `Content-Length` is within `MAX_RESPONSE_BYTES`
/// (`domain::youtube::body_within_cap`). A response with no `Content-Length` is read as
/// before, with no limit. An oversized one is cancelled unread, and so is a read that
/// fails: both give `None`.
pub async fn bounded_text(mut response: Response) -> Option<String> {
    let declared = response.headers().get("Content-Length").ok().flatten();
    if !body_within_cap(declared.as_deref()) {
        discard_body(response);
        return None;
    }
    response.text().await.ok()
}
