//! Cloudflare Worker: validates and maps HTTP to the domain use cases.

pub mod dto;

#[cfg(target_arch = "wasm32")]
mod adapters;

#[cfg(target_arch = "wasm32")]
mod entry {
    use crate::adapters::{D1PostRepository, R2Signer};
    use crate::dto::{self, QueuePostBody, StartUploadBody};
    use domain::app;
    use worker::*;

    /// Fails closed. Real owner sign-in (Cloudflare Access JWT check) comes in the
    /// security slice. Until then the API only answers in local dev.
    fn require_owner(env: &Env) -> Option<Response> {
        let local = env
            .var("LOCAL_DEV_NO_AUTH")
            .map(|v| v.to_string() == "true")
            .unwrap_or(false);
        if local {
            None
        } else {
            Response::error("sign-in not configured yet", 503).ok()
        }
    }

    fn app_error(e: &app::AppError) -> Result<Response> {
        let (code, msg) = dto::error_status(e);
        Response::error(msg, code)
    }

    fn now_secs() -> i64 {
        (Date::now().as_millis() / 1000) as i64
    }

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        if let Some(denied) = require_owner(&env) {
            return Ok(denied);
        }
        Router::new()
            .post_async("/api/uploads", |mut req, ctx| async move {
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
                let Ok(body) = req.json::<QueuePostBody>().await else {
                    return Response::error("bad request", 400);
                };
                let id = uuid::Uuid::new_v4().simple().to_string();
                let Some(post) = dto::to_domain(id, body) else {
                    return Response::error("unknown platform", 422);
                };
                let repo = D1PostRepository::new(ctx.env.d1("DB")?);
                match app::queue_post(&repo, now_secs(), post.clone()).await {
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
            .run(req, env)
            .await
    }
}
