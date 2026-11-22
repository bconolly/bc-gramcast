//! The YouTube connect routes. Every one runs behind `require_owner` (the Access
//! sign-in check in `lib.rs`), which fails closed. No route returns a token.
//!
//! The two connect routes are GET navigations, so the Origin check (which guards
//! only POST, PUT, PATCH and DELETE) does not apply to them, and the callback's
//! request comes from Google, a different site, by design. What protects the callback
//! is the single-use state: 256 random bits that only this owner's `start` call
//! created, valid for ten minutes, removed on first use. A forged, replayed or
//! expired callback is refused before anything is sent to Google. There is no PKCE
//! (support for this flow was not confirmed). Neither route logs a state, code,
//! token, secret or URL.

use crate::adapters::now_secs;
use crate::oauth_adapters::{oauth_config, D1StateStore, WebCryptoRandom};
use domain::oauth;
use worker::*;

type Ctx = RouteContext<()>;

/// A 302 built by hand: `Response.redirect()` has immutable headers, and every
/// response must still get the security headers in `fetch`.
fn redirect(location: &str) -> Result<Response> {
    let mut response = Response::empty()?.with_status(302);
    response.headers_mut().set("Location", location)?;
    Ok(response)
}

/// `GET /api/connect/youtube/start`: creates a state and sends the owner to Google.
pub async fn start(_req: Request, ctx: Ctx) -> Result<Response> {
    let config = match oauth_config(&ctx.env) {
        Ok(c) => c,
        Err(e) => {
            console_log!("youtube connect not configured: {}", e.0);
            return Response::error("youtube connect is not configured", 503);
        }
    };
    let states = D1StateStore::new(ctx.env.d1("DB")?);
    match oauth::issue_state(&WebCryptoRandom, &states, now_secs()).await {
        Ok(state) => redirect(&config.authorization_url(&state)),
        Err(e) => {
            console_log!("could not start youtube connect: {}", e.0);
            Response::error("internal error", 500)
        }
    }
}
