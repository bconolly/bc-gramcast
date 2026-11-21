//! Cloudflare adapters for the domain ports: D1 for posts, R2 presigning for uploads.

use crate::dto;
use domain::app::{PortError, PostRepository, UploadSigner};
use domain::queue::QueuedPost;
use serde::Deserialize;
use worker::{D1Database, Env};

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

fn port_err(e: impl std::fmt::Debug) -> PortError {
    PortError(format!("{e:?}"))
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
        rows.into_iter()
            .map(|r| {
                let platforms = r
                    .platforms
                    .split(',')
                    .map(dto::platform_from_str)
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| PortError("bad platforms in row".into()))?;
                Ok(QueuedPost {
                    id: r.id,
                    video_key: r.video_key,
                    title: r.title,
                    caption: r.caption,
                    platforms,
                    slot_at: r.slot_at as i64,
                })
            })
            .collect()
    }

    async fn insert(&self, post: &QueuedPost) -> Result<(), PortError> {
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
                (post.slot_at as f64).into(),
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
