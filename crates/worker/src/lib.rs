//! Cloudflare Worker: validates and maps HTTP to the domain use cases.

pub mod connect_dto;
pub mod dto;
pub mod publish_dto;
pub mod youtube_dto;

#[cfg(target_arch = "wasm32")]
mod adapters;
#[cfg(target_arch = "wasm32")]
mod connect_routes;
#[cfg(target_arch = "wasm32")]
mod net;
#[cfg(target_arch = "wasm32")]
mod oauth_adapters;
#[cfg(target_arch = "wasm32")]
mod publish_adapters;
#[cfg(target_arch = "wasm32")]
mod youtube_adapters;

#[cfg(target_arch = "wasm32")]
mod entry {
    use crate::adapters::{
        AccessVerifier, D1PostRepository, D1SlotPatternRepository, R2Signer, R2Store,
    };
    use crate::dto::{self, PatchPostBody, PatternJson, QueuePostBody, StartUploadBody};
    use domain::app;
    use domain::auth::{self, AccessConfig, AuthError};
    use worker::*;

    /// Fails closed. Returns the response that denies the request, or `None` to
    /// let it through. Building a denial can fail; `?` then stops the request
    /// instead of passing it.
    ///
    /// The Origin check comes first because it is cheap and needs no key fetch.
    /// Nothing about the token, its claims or the owner's email is ever logged:
    /// the only log line is a fixed reason code.
    async fn require_owner(req: &Request, env: &Env) -> Result<Option<Response>> {
        let header = |name: &str| req.headers().get(name).ok().flatten();
        let method = req.method().to_string();
        let origin_ok = auth::check_origin(
            &method,
            header("Origin").as_deref(),
            header("Sec-Fetch-Site").as_deref(),
            header("Host").as_deref(),
        );
        if !origin_ok {
            return Response::error("forbidden", 403).map(Some);
        }
        match authenticate(req, env).await {
            Ok(()) => Ok(None),
            Err(e) => {
                console_log!("sign-in refused: {}", dto::auth_reason_code(&e));
                let (code, msg) = dto::auth_error_status(&e);
                Response::error(msg, code).map(Some)
            }
        }
    }

    fn var_text(env: &Env, name: &str) -> Option<String> {
        env.var(name)
            .ok()
            .map(|v| v.to_string())
            .filter(|v| !v.is_empty())
    }

    /// Reads the settings and asks the domain whether local dev applies.
    fn local_dev_allowed(req: &Request, env: &Env) -> bool {
        let host = req.headers().get("Host").ok().flatten();
        auth::local_dev_bypass(
            var_text(env, "LOCAL_DEV_NO_AUTH").as_deref(),
            var_text(env, "TEAM_DOMAIN").as_deref(),
            auth::is_loopback_host(host.as_deref()),
        )
    }

    /// Reads the settings. Any missing or malformed one means sign-in is not set up.
    fn access_config(env: &Env) -> Result<AccessConfig, AuthError> {
        let team = var_text(env, "TEAM_DOMAIN").ok_or(AuthError::NotConfigured)?;
        let aud = var_text(env, "POLICY_AUD").ok_or(AuthError::NotConfigured)?;
        let owner = env
            .secret("OWNER_EMAIL")
            .map(|s| s.to_string())
            .map_err(|_| AuthError::NotConfigured)?;
        AccessConfig::new(&team, &aud, &owner).map_err(|_| AuthError::NotConfigured)
    }

    /// The Access token comes from the `Cf-Access-Jwt-Assertion` header only.
    async fn authenticate(req: &Request, env: &Env) -> Result<(), AuthError> {
        if local_dev_allowed(req, env) {
            return Ok(());
        }
        let config = access_config(env)?;
        let token = req
            .headers()
            .get("Cf-Access-Jwt-Assertion")
            .ok()
            .flatten()
            .filter(|t| !t.is_empty())
            .ok_or(AuthError::MissingToken)?;
        let parts = dto::parse_token(&token)?;
        let now = now_secs();
        let verifier = AccessVerifier::new(config.certs_url());
        app::authenticate_owner(
            &verifier,
            &config,
            parts.header,
            parts.claims,
            &parts.signing_input,
            &parts.signature,
            now,
        )
        .await
    }

    fn app_error(e: &app::AppError) -> Result<Response> {
        let (code, msg) = dto::error_status(e);
        Response::error(msg, code)
    }

    /// Rejects non-JSON POST, PUT and PATCH requests (415) before the body is read.
    fn require_json(req: &Request) -> Result<Option<Response>> {
        let header = req.headers().get("Content-Type").ok().flatten();
        if dto::is_json_content_type(header.as_deref()) {
            Ok(None)
        } else {
            Response::error("content type must be application/json", 415).map(Some)
        }
    }

    fn now_secs() -> i64 {
        (Date::now().as_millis() / 1000) as i64
    }

    /// The one place every response passes through, denials and errors included,
    /// so the API security headers can't be forgotten on any path. A handler
    /// error becomes a fixed 500 with no detail.
    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        let mut response = match handle(req, env).await {
            Ok(r) => r,
            Err(_) => {
                console_log!("handler error");
                Response::error("internal error", 500)?
            }
        };
        for (name, value) in dto::API_SECURITY_HEADERS {
            response.headers_mut().set(name, value)?;
        }
        Ok(response)
    }

    async fn handle(req: Request, env: Env) -> Result<Response> {
        if let Some(denied) = require_owner(&req, &env).await? {
            return Ok(denied);
        }
        Router::new()
            .post_async("/api/uploads", start_upload_route)
            .post_async("/api/posts", queue_post_route)
            .patch_async("/api/posts/:id", patch_post_route)
            .delete_async("/api/posts/:id", delete_post_route)
            .get_async("/api/queue", queue_route)
            .get_async("/api/slots/pattern", get_pattern_route)
            .put_async("/api/slots/pattern", put_pattern_route)
            .get_async("/api/slots/next", next_slots_route)
            .get_async("/api/connect/youtube/start", crate::connect_routes::start)
            .get_async(
                "/api/connect/youtube/callback",
                crate::connect_routes::callback,
            )
            .get_async("/api/connect/status", crate::connect_routes::status)
            .run(req, env)
            .await
    }

    type Ctx = RouteContext<()>;

    async fn start_upload_route(mut req: Request, ctx: Ctx) -> Result<Response> {
        if let Some(rejected) = require_json(&req)? {
            return Ok(rejected);
        }
        let Ok(body) = req.json::<StartUploadBody>().await else {
            return Response::error("bad request", 400);
        };
        let signer = R2Signer::from_env(&ctx.env, now_secs())?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        match app::start_upload(
            &signer,
            &id,
            &body.file_name,
            &body.content_type,
            body.size_bytes,
        ) {
            Ok(t) => Response::from_json(&serde_json::json!({
                "video_key": t.video_key,
                "put_url": t.put_url,
            })),
            Err(e) => app_error(&e),
        }
    }

    async fn queue_post_route(mut req: Request, ctx: Ctx) -> Result<Response> {
        if let Some(rejected) = require_json(&req)? {
            return Ok(rejected);
        }
        let Ok(body) = req.json::<QueuePostBody>().await else {
            return Response::error("bad request", 400);
        };
        let id = uuid::Uuid::new_v4().simple().to_string();
        let Some(post) = dto::to_domain(id, body) else {
            return Response::error("unknown platform", 422);
        };
        let repo = D1PostRepository::new(ctx.env.d1("DB")?);
        let store = R2Store::new(ctx.env.bucket("VIDEOS")?);
        match app::queue_post(&repo, &store, now_secs(), post.clone()).await {
            Ok(()) => Ok(Response::from_json(&dto::to_json(&post))?.with_status(201)),
            Err(e) => app_error(&e),
        }
    }

    async fn patch_post_route(mut req: Request, ctx: Ctx) -> Result<Response> {
        if let Some(rejected) = require_json(&req)? {
            return Ok(rejected);
        }
        let Some(id) = ctx.param("id").cloned() else {
            return Response::error("post not found", 404);
        };
        let Ok(body) = req.json::<PatchPostBody>().await else {
            return Response::error("bad request", 400);
        };
        let Some(changes) = dto::to_changes(body) else {
            return Response::error("unknown platform", 422);
        };
        let repo = D1PostRepository::new(ctx.env.d1("DB")?);
        match app::update_post(&repo, &id, changes, now_secs()).await {
            Ok(post) => Response::from_json(&dto::to_json(&post)),
            Err(e) => app_error(&e),
        }
    }

    async fn delete_post_route(_req: Request, ctx: Ctx) -> Result<Response> {
        let Some(id) = ctx.param("id").cloned() else {
            return Response::error("post not found", 404);
        };
        let repo = D1PostRepository::new(ctx.env.d1("DB")?);
        let store = R2Store::new(ctx.env.bucket("VIDEOS")?);
        match app::delete_post(&repo, &store, &id, now_secs()).await {
            Ok(outcome) => {
                if outcome == app::DeleteOutcome::VideoLeftBehind {
                    // The id passed the domain's hex check; no key or URL is logged.
                    console_log!("{}", dto::video_left_behind_line(&id));
                }
                Ok(Response::empty()?.with_status(204))
            }
            Err(e) => app_error(&e),
        }
    }

    async fn queue_route(_req: Request, ctx: Ctx) -> Result<Response> {
        let repo = D1PostRepository::new(ctx.env.d1("DB")?);
        match app::upcoming(&repo, now_secs()).await {
            Ok(posts) => {
                let out: Vec<_> = posts.iter().map(dto::to_json).collect();
                Response::from_json(&out)
            }
            Err(e) => app_error(&e),
        }
    }

    async fn get_pattern_route(_req: Request, ctx: Ctx) -> Result<Response> {
        let repo = D1SlotPatternRepository::new(ctx.env.d1("DB")?);
        match app::get_pattern(&repo).await {
            Ok(p) => Response::from_json(&dto::pattern_to_json(&p)),
            Err(e) => app_error(&e),
        }
    }

    async fn put_pattern_route(mut req: Request, ctx: Ctx) -> Result<Response> {
        if let Some(rejected) = require_json(&req)? {
            return Ok(rejected);
        }
        let Ok(body) = req.json::<PatternJson>().await else {
            return Response::error("bad request", 400);
        };
        let (offset, rules) = match dto::pattern_from_json(body) {
            Ok(parts) => parts,
            Err(e) => return app_error(&app::AppError::Slots(e)),
        };
        let repo = D1SlotPatternRepository::new(ctx.env.d1("DB")?);
        match app::set_pattern(&repo, offset, rules).await {
            Ok(p) => Response::from_json(&dto::pattern_to_json(&p)),
            Err(e) => app_error(&e),
        }
    }

    async fn next_slots_route(req: Request, ctx: Ctx) -> Result<Response> {
        let url = req.url()?;
        let Some(count) = dto::parse_next_count(url.query()) else {
            return Response::error("count must be a whole number from 1 to 20", 422);
        };
        let patterns = D1SlotPatternRepository::new(ctx.env.d1("DB")?);
        let posts = D1PostRepository::new(ctx.env.d1("DB")?);
        match app::next_slots(&patterns, &posts, now_secs(), count).await {
            Ok(slots) => Response::from_json(&serde_json::json!({ "slots": slots })),
            Err(e) => app_error(&e),
        }
    }
}
