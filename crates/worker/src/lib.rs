//! Cloudflare Worker: validates and maps HTTP to the domain use cases.

pub mod dto;

#[cfg(target_arch = "wasm32")]
mod adapters;

#[cfg(target_arch = "wasm32")]
mod entry {
    use crate::adapters::{D1PostRepository, D1SlotPatternRepository, R2Signer, R2Store};
    use crate::dto::{self, PatternJson, QueuePostBody, StartUploadBody};
    use domain::app;
    use worker::*;

    /// Fails closed. Real owner sign-in (Cloudflare Access JWT check) comes in the
    /// security slice. Until then the API only answers in local dev, and then
    /// only for loopback Host headers, so DNS rebinding can't reach it.
    /// Returns the response that denies the request, or `None` to let it through.
    /// Building a denial can fail; `?` then stops the request instead of passing it.
    fn require_owner(req: &Request, env: &Env) -> Result<Option<Response>> {
        let local = env
            .var("LOCAL_DEV_NO_AUTH")
            .map(|v| v.to_string() == "true")
            .unwrap_or(false);
        if !local {
            return Response::error("sign-in not configured yet", 503).map(Some);
        }
        let host = req.headers().get("Host").ok().flatten();
        if dto::is_local_host(host.as_deref()) {
            Ok(None)
        } else {
            Response::error("forbidden", 403).map(Some)
        }
    }

    fn app_error(e: &app::AppError) -> Result<Response> {
        let (code, msg) = dto::error_status(e);
        Response::error(msg, code)
    }

    /// Rejects non-JSON POSTs (415) before the body is read.
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

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        if let Some(denied) = require_owner(&req, &env)? {
            return Ok(denied);
        }
        Router::new()
            .post_async("/api/uploads", |mut req, ctx| async move {
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
            })
            .post_async("/api/posts", |mut req, ctx| async move {
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
            })
            .get_async("/api/queue", |_req, ctx| async move {
                let repo = D1PostRepository::new(ctx.env.d1("DB")?);
                match app::upcoming(&repo, now_secs()).await {
                    Ok(posts) => {
                        let out: Vec<_> = posts.iter().map(dto::to_json).collect();
                        Response::from_json(&out)
                    }
                    Err(e) => app_error(&e),
                }
            })
            .get_async("/api/slots/pattern", |_req, ctx| async move {
                let repo = D1SlotPatternRepository::new(ctx.env.d1("DB")?);
                match app::get_pattern(&repo).await {
                    Ok(Some(p)) => Response::from_json(&dto::pattern_to_json(&p)),
                    Ok(None) => Response::from_json(&dto::default_pattern_json()),
                    Err(e) => app_error(&e),
                }
            })
            .put_async("/api/slots/pattern", |mut req, ctx| async move {
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
            })
            .get_async("/api/slots/next", |req, ctx| async move {
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
            })
            .run(req, env)
            .await
    }
}
