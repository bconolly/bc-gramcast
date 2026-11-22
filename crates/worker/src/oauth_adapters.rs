//! Cloudflare adapters for the YouTube connect flow: D1 for pending states and the
//! encrypted token, Web Crypto for randomness and AES-256-GCM, and `fetch` for
//! Google's token endpoint. (This part: settings, pending states, randomness.)
//!
//! Thin glue: every decision is in `domain::oauth`, `domain::connect` and
//! `domain::seal`. These adapters cannot run natively. They never log, and their
//! errors are fixed text: no D1 or JS error is copied in, because those can carry
//! row values, and no token, code, secret, state or URL is ever put in a message.

use crate::adapters::changes;
use crate::dto;
use domain::app::PortError;
use domain::oauth::{OAuthConfig, RandomSource, StateStore};
use serde::Deserialize;
use worker::js_sys::{self, Function, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::{D1Database, Env};

fn port(text: &str) -> PortError {
    PortError(text.to_string())
}

// ---- configuration ---------------------------------------------------------

/// Client id (a secret) and `PUBLIC_BASE_URL` (a var), checked by the domain.
pub fn oauth_config(env: &Env) -> Result<OAuthConfig, PortError> {
    let id = env
        .secret("YOUTUBE_CLIENT_ID")
        .map_err(|_| port("YOUTUBE_CLIENT_ID missing"))?
        .to_string();
    let base = env
        .var("PUBLIC_BASE_URL")
        .map_err(|_| port("PUBLIC_BASE_URL missing"))?
        .to_string();
    OAuthConfig::new(&id, &base).map_err(|_| port("youtube connect settings invalid"))
}

// ---- pending states ----------------------------------------------------------

pub struct D1StateStore {
    db: D1Database,
}

impl D1StateStore {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[derive(Deserialize)]
struct ExpiryRow {
    expires_at: f64,
}

impl StateStore for D1StateStore {
    async fn put(&self, state: &str, now: i64, expires_at: i64) -> Result<(), PortError> {
        let fail = |_| port("state write failed");
        // One batch: drop expired rows, then add this one.
        let prune = self
            .db
            .prepare("DELETE FROM oauth_state WHERE expires_at <= ?1")
            .bind(&[(now as f64).into()])
            .map_err(fail)?;
        let insert = self
            .db
            .prepare("INSERT INTO oauth_state (state, expires_at) VALUES (?1, ?2)")
            .bind(&[state.into(), (expires_at as f64).into()])
            .map_err(fail)?;
        self.db.batch(vec![prune, insert]).await.map_err(fail)?;
        Ok(())
    }

    /// Reads the row, then deletes it. Two racing callbacks can both read it, but
    /// only the one whose DELETE changes a row gets `Some`. If D1 reports no change
    /// count the answer is `None` (the state is refused), so this fails closed.
    async fn take(&self, state: &str) -> Result<Option<i64>, PortError> {
        let fail = |_| port("state read failed");
        let row = self
            .db
            .prepare("SELECT expires_at FROM oauth_state WHERE state = ?1")
            .bind(&[state.into()])
            .map_err(fail)?
            .first::<ExpiryRow>(None)
            .await
            .map_err(fail)?;
        let Some(row) = row else { return Ok(None) };
        let result = self
            .db
            .prepare("DELETE FROM oauth_state WHERE state = ?1")
            .bind(&[state.into()])
            .map_err(fail)?
            .run()
            .await
            .map_err(fail)?;
        let removed = changes(&result)?;
        Ok(dto::write_applied(removed).then_some(row.expires_at as i64))
    }
}

// ---- randomness ------------------------------------------------------------------

pub struct WebCryptoRandom;

impl RandomSource for WebCryptoRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), PortError> {
        let fail = |_| port("random source failed");
        let crypto = Reflect::get(&js_sys::global(), &JsValue::from_str("crypto")).map_err(fail)?;
        let get: Function = Reflect::get(&crypto, &JsValue::from_str("getRandomValues"))
            .map_err(fail)?
            .dyn_into()
            .map_err(|_| port("random source failed"))?;
        let array = Uint8Array::new_with_length(buf.len() as u32);
        get.call1(&crypto, &array).map_err(fail)?;
        array.copy_to(buf);
        Ok(())
    }
}
