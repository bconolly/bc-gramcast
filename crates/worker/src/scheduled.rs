//! The Cron Trigger's run: publishes due posts to YouTube. Thin glue. Every decision is
//! in `domain::publish_app` (what to publish, how to record it), `domain::youtube` and
//! `domain::upload` (what each answer means); this builds the real adapters and logs
//! only the fixed lines of `publish_log`.
//!
//! Publishing happens ONLY here, with the Worker's own clock (W2): no HTTP route calls
//! it, and no time comes from a request or from the cron event.
//!
//! Off unless the owner's saved settings say so (the Publishing card). With no saved row
//! the old `PUBLISH_ENABLED` and `PUBLISH_PRIVACY` vars apply. A settings read that fails
//! stops the run (`domain::settings::run_gate`). One post per run
//! (`POSTS_PER_RUN`). Limits (see `domain::youtube`): the free plan's 50 subrequests per
//! invocation, 10 ms CPU (network waiting does not count; whether copying bytes between
//! R2 and fetch does is unconfirmed) and 15 minutes for a cron invocation.

use crate::adapters::now_secs;
use crate::oauth_adapters::{
    oauth_config, AesGcmCipher, D1TokenStore, GoogleRefresher, WebCryptoRandom,
};
use crate::publish_adapters::D1PublishRepository;
use crate::publish_log::{failure_line, run_lines, RunFailure, DISABLED_LINE};
use crate::settings_adapters::D1SettingsRepository;
use crate::youtube_adapters::{R2Videos, YouTubePublisher};
use domain::publish_app::{publish_due, PublishReport, POSTS_PER_RUN};
use domain::settings::{load_settings, run_gate, RunGate};
use domain::youtube::Privacy;
use worker::{console_log, Env};

fn var_text(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string())
}

/// Whatever went wrong building an adapter is dropped: the log says only "setup".
fn setup<E>(_: E) -> RunFailure {
    RunFailure::Setup
}

/// Builds every adapter and runs.
async fn build_and_run(env: &Env, privacy: Privacy) -> Result<PublishReport, RunFailure> {
    let bucket = env.bucket("VIDEOS").map_err(setup)?;
    let config = oauth_config(env).map_err(setup)?;
    let refresher = GoogleRefresher::from_env(env, config).map_err(setup)?;
    let cipher = AesGcmCipher::from_env(env).await.map_err(setup)?;
    let tokens = D1TokenStore::new(env.d1("DB").map_err(setup)?, cipher, WebCryptoRandom);
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

/// Reads the settings first, on every run. The error behind a failed read is dropped.
async fn read_gate(env: &Env) -> RunGate {
    let read = match env.d1("DB") {
        Ok(db) => {
            let repo = D1SettingsRepository::new(db);
            let enabled = var_text(env, "PUBLISH_ENABLED");
            let privacy = var_text(env, "PUBLISH_PRIVACY");
            load_settings(&repo, enabled.as_deref(), privacy.as_deref()).await
        }
        Err(_) => Err(domain::settings::SettingsError::BadRow),
    };
    run_gate(&read)
}

pub async fn run(env: &Env) {
    let privacy = match read_gate(env).await {
        RunGate::Stop => {
            console_log!("{}", failure_line(RunFailure::Settings));
            return;
        }
        RunGate::Disabled => {
            console_log!("{}", DISABLED_LINE);
            return;
        }
        RunGate::Publish(privacy) => privacy,
    };
    match build_and_run(env, privacy).await {
        Ok(report) => {
            for line in run_lines(&report) {
                console_log!("{}", line);
            }
        }
        Err(failure) => console_log!("{}", failure_line(failure)),
    }
}
