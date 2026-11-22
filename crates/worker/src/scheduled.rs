//! The Cron Trigger's run: publishes due posts to YouTube. Thin glue. Every decision is
//! in `domain::publish_app` (what to publish, how to record it), `domain::youtube` and
//! `domain::upload` (what each answer means); this builds the real adapters and logs
//! only the fixed lines of `publish_log`.
//!
//! Publishing happens ONLY here, with the Worker's own clock (W2): no HTTP route calls
//! it, and no time comes from a request or from the cron event.
//!
//! Off unless `PUBLISH_ENABLED` is exactly `true`. One post per run
//! (`POSTS_PER_RUN`). Limits (see `domain::youtube`): the free plan's 50 subrequests per
//! invocation, 10 ms CPU (network waiting does not count; whether copying bytes between
//! R2 and fetch does is unconfirmed) and 15 minutes for a cron invocation.

use crate::adapters::now_secs;
use crate::oauth_adapters::{
    oauth_config, AesGcmCipher, D1TokenStore, GoogleRefresher, WebCryptoRandom,
};
use crate::publish_adapters::D1PublishRepository;
use crate::publish_log::{failure_line, run_lines, RunFailure, DISABLED_LINE};
use crate::youtube_adapters::{R2Videos, YouTubePublisher};
use domain::publish_app::{publish_due, PublishReport, POSTS_PER_RUN};
use domain::youtube::{publish_enabled, Privacy};
use worker::{console_log, Env};

fn var_text(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string())
}

/// Whatever went wrong building an adapter is dropped: the log says only "setup".
fn setup<E>(_: E) -> RunFailure {
    RunFailure::Setup
}

/// Builds every adapter and runs.
async fn build_and_run(env: &Env) -> Result<PublishReport, RunFailure> {
    let bucket = env.bucket("VIDEOS").map_err(setup)?;
    let config = oauth_config(env).map_err(setup)?;
    let refresher = GoogleRefresher::from_env(env, config).map_err(setup)?;
    let cipher = AesGcmCipher::from_env(env).await.map_err(setup)?;
    let tokens = D1TokenStore::new(env.d1("DB").map_err(setup)?, cipher, WebCryptoRandom);
    let privacy = Privacy::from_var(var_text(env, "PUBLISH_PRIVACY").as_deref());
    let repo = D1PublishRepository::new(env.d1("DB").map_err(setup)?);
    let links = R2Videos::new(bucket.clone());
    let publisher = YouTubePublisher::new(bucket, privacy);
    publish_due(
        &now_secs,
        &repo,
        &publisher,
        &tokens,
        &refresher,
        &links,
        POSTS_PER_RUN,
    )
    .await
    .map_err(|_| RunFailure::List)
}

pub async fn run(env: &Env) {
    if !publish_enabled(var_text(env, "PUBLISH_ENABLED").as_deref()) {
        console_log!("{}", DISABLED_LINE);
        return;
    }
    match build_and_run(env).await {
        Ok(report) => {
            for line in run_lines(&report) {
                console_log!("{}", line);
            }
        }
        Err(failure) => console_log!("{}", failure_line(failure)),
    }
}
