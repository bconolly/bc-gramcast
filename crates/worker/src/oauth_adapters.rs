//! Cloudflare adapters for the YouTube connect flow: D1 for pending states and the
//! encrypted token, Web Crypto for randomness and AES-256-GCM, and `fetch` for
//! Google's token endpoint.
//!
//! Thin glue: every decision is in `domain::oauth`, `domain::connect` and
//! `domain::seal`. These adapters cannot run natively. They never log, and their
//! errors are fixed text: no D1 or JS error is copied in, because those can carry
//! row values, and no token, code, secret, state or URL is ever put in a message.

use crate::adapters::{call, changes, now_secs, object, subtle};
use crate::{connect_dto, dto};
use domain::app::PortError;
use domain::connect::{CodeExchanger, TokenReply};
use domain::oauth::{self, OAuthConfig, RandomSource, StateStore};
use domain::seal::{self, SealedToken, TokenCipher, KEY_LEN, NONCE_LEN, PROVIDER_YOUTUBE};
use domain::token::{StoredToken, TokenStore};
use serde::Deserialize;
use worker::js_sys::{self, Array, Function, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::{D1Database, Env, Fetch, Headers, Method, Request, RequestInit, RequestRedirect};

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

// ---- Web Crypto: randomness and AES-256-GCM ------------------------------------

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

/// AES-256-GCM through `crypto.subtle`, with the key imported once, non-extractable.
pub struct AesGcmCipher {
    key: JsValue,
}

impl AesGcmCipher {
    /// Reads `TOKEN_ENC_KEY` (base64, 32 bytes). Missing or malformed is an error.
    pub async fn from_env(env: &Env) -> Result<Self, PortError> {
        let text = env
            .secret("TOKEN_ENC_KEY")
            .map_err(|_| port("TOKEN_ENC_KEY missing"))?
            .to_string();
        let raw: [u8; KEY_LEN] =
            seal::parse_enc_key(&text).map_err(|_| port("TOKEN_ENC_KEY invalid"))?;
        let fail = |_| port("key import failed");
        let algorithm = object(&[("name", "AES-GCM".into())]).map_err(fail)?;
        let usages = Array::of2(&"encrypt".into(), &"decrypt".into());
        let args = [
            "raw".into(),
            Uint8Array::from(&raw[..]).into(),
            algorithm.into(),
            JsValue::FALSE,
            usages.into(),
        ];
        let key = call(&subtle().map_err(fail)?, "importKey", &args)
            .await
            .map_err(fail)?;
        Ok(Self { key })
    }

    async fn run(
        &self,
        method: &str,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        data: &[u8],
    ) -> Result<Vec<u8>, PortError> {
        let fail = |_| port("cipher failed");
        let params = object(&[
            ("name", "AES-GCM".into()),
            ("iv", Uint8Array::from(&nonce[..]).into()),
            ("additionalData", Uint8Array::from(aad).into()),
            ("tagLength", 128.into()),
        ])
        .map_err(fail)?;
        let args = [
            params.into(),
            self.key.clone(),
            Uint8Array::from(data).into(),
        ];
        let out = call(&subtle().map_err(fail)?, method, &args)
            .await
            .map_err(fail)?;
        Ok(Uint8Array::new(&out).to_vec())
    }
}

impl TokenCipher for AesGcmCipher {
    async fn encrypt(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        plain: &[u8],
    ) -> Result<Vec<u8>, PortError> {
        self.run("encrypt", nonce, aad, plain).await
    }

    async fn decrypt(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        cipher: &[u8],
    ) -> Result<Vec<u8>, PortError> {
        self.run("decrypt", nonce, aad, cipher).await
    }
}

// ---- the encrypted token row -----------------------------------------------------

/// `save` writes only the token. `connect` writes it and clears the mark in the same
/// single statement, so there is no moment with a new token and a stale mark.
const SAVE_SQL: &str = "INSERT INTO oauth_tokens (provider, ciphertext, nonce, expires_at, needs_reconnect, updated_at) \
     VALUES (?1, ?2, ?3, ?4, 0, ?5) \
     ON CONFLICT(provider) DO UPDATE SET ciphertext = excluded.ciphertext, nonce = excluded.nonce, \
     expires_at = excluded.expires_at, updated_at = excluded.updated_at";
const CONNECT_SQL: &str = "INSERT INTO oauth_tokens (provider, ciphertext, nonce, expires_at, needs_reconnect, updated_at) \
     VALUES (?1, ?2, ?3, ?4, 0, ?5) \
     ON CONFLICT(provider) DO UPDATE SET ciphertext = excluded.ciphertext, nonce = excluded.nonce, \
     expires_at = excluded.expires_at, needs_reconnect = 0, updated_at = excluded.updated_at";

pub struct D1TokenStore<C, R> {
    db: D1Database,
    cipher: C,
    rng: R,
}

impl<C: TokenCipher, R: RandomSource> D1TokenStore<C, R> {
    pub fn new(db: D1Database, cipher: C, rng: R) -> Self {
        Self { db, cipher, rng }
    }

    async fn write(&self, sql: &'static str, token: &StoredToken) -> Result<(), PortError> {
        let fail = |_| port("token write failed");
        let sealed = seal::seal_token(&self.cipher, &self.rng, PROVIDER_YOUTUBE, token).await?;
        self.db
            .prepare(sql)
            .bind(&[
                PROVIDER_YOUTUBE.into(),
                sealed.ciphertext.as_str().into(),
                sealed.nonce.as_str().into(),
                (token.expires_at as f64).into(),
                (now_secs() as f64).into(),
            ])
            .map_err(fail)?
            .run()
            .await
            .map_err(fail)?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct TokenRow {
    ciphertext: String,
    nonce: String,
}

#[derive(Deserialize)]
struct FlagRow {
    needs_reconnect: f64,
}

impl<C: TokenCipher, R: RandomSource> TokenStore for D1TokenStore<C, R> {
    async fn load(&self) -> Result<Option<StoredToken>, PortError> {
        let row = self
            .db
            .prepare("SELECT ciphertext, nonce FROM oauth_tokens WHERE provider = ?1")
            .bind(&[PROVIDER_YOUTUBE.into()])
            .map_err(|_| port("token read failed"))?
            .first::<TokenRow>(None)
            .await
            .map_err(|_| port("token read failed"))?;
        let Some(row) = row else { return Ok(None) };
        let sealed = SealedToken {
            ciphertext: row.ciphertext,
            nonce: row.nonce,
        };
        seal::open_token(&self.cipher, PROVIDER_YOUTUBE, &sealed)
            .await
            .map(Some)
            .map_err(|_| port("stored token unreadable"))
    }

    async fn save(&self, token: &StoredToken) -> Result<(), PortError> {
        self.write(SAVE_SQL, token).await
    }

    async fn connect(&self, token: &StoredToken) -> Result<(), PortError> {
        self.write(CONNECT_SQL, token).await
    }

    async fn mark_needs_reconnect(&self) -> Result<(), PortError> {
        self.db
            .prepare("UPDATE oauth_tokens SET needs_reconnect = 1 WHERE provider = ?1")
            .bind(&[PROVIDER_YOUTUBE.into()])
            .map_err(|_| port("mark write failed"))?
            .run()
            .await
            .map_err(|_| port("mark write failed"))?;
        Ok(())
    }

    /// A failed read is an error, and so is a value other than 0 or 1: callers stop.
    async fn needs_reconnect(&self) -> Result<bool, PortError> {
        let row = self
            .db
            .prepare("SELECT needs_reconnect FROM oauth_tokens WHERE provider = ?1")
            .bind(&[PROVIDER_YOUTUBE.into()])
            .map_err(|_| port("mark read failed"))?
            .first::<FlagRow>(None)
            .await
            .map_err(|_| port("mark read failed"))?;
        row.map_or(Ok(false), |r| connect_dto::flag_from_row(r.needs_reconnect))
    }
}

// ---- Google's token endpoint -------------------------------------------------------

pub struct GoogleExchanger {
    config: OAuthConfig,
    client_secret: String,
}

impl GoogleExchanger {
    pub fn from_env(env: &Env, config: OAuthConfig) -> Result<Self, PortError> {
        let client_secret = env
            .secret("YOUTUBE_CLIENT_SECRET")
            .map_err(|_| port("YOUTUBE_CLIENT_SECRET missing"))?
            .to_string();
        Ok(Self {
            config,
            client_secret,
        })
    }
}

impl CodeExchanger for GoogleExchanger {
    /// Posts the form to Google. The body holds the code and the client secret and is
    /// never logged; neither is the response. Redirects are not followed.
    async fn exchange(&self, code: &str) -> Result<TokenReply, PortError> {
        let fail = |_| port("token request failed");
        let body = self.config.token_request_body(code, &self.client_secret);
        let headers = Headers::new();
        headers
            .set("Content-Type", "application/x-www-form-urlencoded")
            .map_err(fail)?;
        headers.set("Accept", "application/json").map_err(fail)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(JsValue::from_str(&body)))
            .with_redirect(RequestRedirect::Manual);
        let request = Request::new_with_init(oauth::GOOGLE_TOKEN_URL, &init).map_err(fail)?;
        let mut response = Fetch::Request(request).send().await.map_err(fail)?;
        let status = response.status_code();
        let text = response.text().await.map_err(fail)?;
        Ok(connect_dto::reply_from_body(status, &text))
    }
}
