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
//! token, secret or URL. `start` also refuses a `Sec-Fetch-Site` that is not
//! `same-origin` or `none` (302 to Settings with `connect_error=try_again`), so another site cannot start the flow for the owner.

use crate::adapters::now_secs;
use crate::connect_dto;
use crate::oauth_adapters::{
    oauth_config, AesGcmCipher, D1StateStore, D1TokenStore, GoogleExchanger, NoCipher,
    WebCryptoRandom,
};
use domain::app::PortError;
use domain::auth;
use domain::connect::{self, ConnectError};
use domain::connection;
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
pub async fn start(req: Request, ctx: Ctx) -> Result<Response> {
    // Only the owner's own navigation may start the flow, not a link on another site.
    let site = req.headers().get("Sec-Fetch-Site").ok().flatten();
    if !auth::check_navigation(site.as_deref()) {
        // Send the owner to the app with a message, not a bare error page.
        let refused = Err(ConnectError::TryAgain);
        return redirect(&connect_dto::redirect_location(refused));
    }
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

/// Builds the token store (and so needs `TOKEN_ENC_KEY`).
async fn token_store(
    env: &Env,
) -> std::result::Result<D1TokenStore<AesGcmCipher, WebCryptoRandom>, PortError> {
    let cipher = AesGcmCipher::from_env(env).await?;
    let db = env
        .d1("DB")
        .map_err(|_| PortError("d1 binding missing".into()))?;
    Ok(D1TokenStore::new(db, cipher, WebCryptoRandom))
}

/// Everything the callback needs, or the fixed reason it cannot be built.
async fn run_callback(req: &Request, env: &Env) -> std::result::Result<(), ConnectError> {
    let setup = |e: PortError| {
        console_log!("connect setup failed: {}", e.0);
        ConnectError::Server
    };
    let url = req.url().map_err(|_| ConnectError::BadState)?;
    let query = connect_dto::callback_query(
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned())),
    );
    let config = oauth_config(env).map_err(setup)?;
    let exchanger = GoogleExchanger::from_env(env, config).map_err(setup)?;
    let tokens = token_store(env).await.map_err(setup)?;
    let db = env
        .d1("DB")
        .map_err(|_| setup(PortError("d1 binding missing".into())))?;
    let states = D1StateStore::new(db);
    connect::connect_youtube_callback(&states, &exchanger, &tokens, &query.as_params(), now_secs())
        .await
}

/// `GET /api/connect/youtube/callback`: Google sends the owner back here. Always
/// answers 302 to the Settings page: `?connected=1`, or `?connect_error=<fixed code>`.
pub async fn callback(req: Request, ctx: Ctx) -> Result<Response> {
    let result = run_callback(&req, &ctx.env).await;
    if let Err(e) = result {
        console_log!("youtube connect refused: {}", e.code());
    }
    redirect(&connect_dto::redirect_location(result))
}

/// `GET /api/connect/status`: only `{youtube: {connected, needs_reconnect}}`. It reads
/// row flags and never decrypts, so it needs no `TOKEN_ENC_KEY` and still answers
/// after the key is lost.
pub async fn status(_req: Request, ctx: Ctx) -> Result<Response> {
    let tokens = D1TokenStore::new(ctx.env.d1("DB")?, NoCipher, WebCryptoRandom);
    match connection::connection_status(&tokens).await {
        Ok(s) => Response::from_json(&connect_dto::status_json(s)),
        Err(_) => {
            console_log!("connection status unavailable");
            Response::error("internal error", 500)
        }
    }
}
