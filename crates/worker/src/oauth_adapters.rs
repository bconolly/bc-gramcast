//! Cloudflare adapters for the YouTube connect flow: D1 for pending states and the
//! encrypted token, Web Crypto for randomness and AES-256-GCM, and `fetch` for
//! Google's token endpoint.
//!
//! Thin glue: every decision is in `domain::oauth`, `domain::connect` and
//! `domain::seal`. These adapters cannot run natively. They never log, and their
//! errors are fixed text: no D1 or JS error is copied in, because those can carry
//! row values, and no token, code, secret, state or URL is ever put in a message.

use crate::adapters::{call, now_secs, object, subtle};
use crate::connect_dto;
use crate::net;
use domain::app::PortError;
use domain::connect::{CodeExchanger, TokenReply};
use domain::oauth::{self, OAuthConfig, RandomSource, StateStore};
use domain::publish_app::{PublishError, TokenRefresher};
use domain::seal::{self, SealedToken, TokenCipher, KEY_LEN, NONCE_LEN, PROVIDER_YOUTUBE};
use domain::token::{StoredToken, TokenLoad, TokenStore};
use domain::youtube;
use serde::Deserialize;
use std::cell::RefCell;
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

    /// One statement removes the row and returns its expiry, so of two racing callbacks
    /// only one gets a row back, and the answer does not depend on D1's change count.
    async fn take(&self, state: &str) -> Result<Option<i64>, PortError> {
        let fail = |_| port("state read failed");
        let row = self
            .db
            .prepare("DELETE FROM oauth_state WHERE state = ?1 RETURNING expires_at")
            .bind(&[state.into()])
            .map_err(fail)?
            .first::<ExpiryRow>(None)
            .await
            .map_err(fail)?;
        Ok(row.map(|r| r.expires_at as i64))
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

/// `save` writes only the token, and only if the row is still the version this run
/// loaded (its nonce, which changes on every write). A lost race returns no row.
const SAVE_SQL: &str =
    "UPDATE oauth_tokens SET ciphertext = ?2, nonce = ?3, expires_at = ?4, updated_at = ?5 \
     WHERE provider = ?1 AND nonce = ?6 RETURNING nonce";
/// `connect` writes the token and clears the mark in one statement, unconditionally,
/// so there is no moment with a new token and a stale mark.
const CONNECT_SQL: &str = "INSERT INTO oauth_tokens (provider, ciphertext, nonce, expires_at, needs_reconnect, updated_at) \
     VALUES (?1, ?2, ?3, ?4, 0, ?5) \
     ON CONFLICT(provider) DO UPDATE SET ciphertext = excluded.ciphertext, nonce = excluded.nonce, \
     expires_at = excluded.expires_at, needs_reconnect = 0, updated_at = excluded.updated_at";
const MARK_SQL: &str =
    "UPDATE oauth_tokens SET needs_reconnect = 1 WHERE provider = ?1 AND nonce = ?2";

pub struct D1TokenStore<C, R> {
    db: D1Database,
    cipher: C,
    rng: R,
    /// The nonce of the row `load` last read: the version `save` and `mark` must still see.
    loaded: RefCell<Option<String>>,
}

impl<C: TokenCipher, R: RandomSource> D1TokenStore<C, R> {
    pub fn new(db: D1Database, cipher: C, rng: R) -> Self {
        Self {
            db,
            cipher,
            rng,
            loaded: RefCell::new(None),
        }
    }

    fn loaded_version(&self) -> Result<String, PortError> {
        self.loaded
            .borrow()
            .clone()
            .ok_or_else(|| port("token not loaded"))
    }

    async fn seal(&self, token: &StoredToken) -> Result<SealedToken, PortError> {
        seal::seal_token(&self.cipher, &self.rng, PROVIDER_YOUTUBE, token).await
    }
}

#[derive(Deserialize)]
struct TokenRow {
    ciphertext: String,
    nonce: String,
}

#[derive(Deserialize)]
struct NonceRow {
    nonce: String,
}

#[derive(Deserialize)]
struct FlagRow {
    needs_reconnect: f64,
}

impl<C: TokenCipher, R: RandomSource> TokenStore for D1TokenStore<C, R> {
    async fn load(&self) -> Result<TokenLoad, PortError> {
        let row = self
            .db
            .prepare("SELECT ciphertext, nonce FROM oauth_tokens WHERE provider = ?1")
            .bind(&[PROVIDER_YOUTUBE.into()])
            .map_err(|_| port("token read failed"))?
            .first::<TokenRow>(None)
            .await
            .map_err(|_| port("token read failed"))?;
        let Some(row) = row else {
            return Ok(TokenLoad::Missing);
        };
        let version = row.nonce.clone();
        let sealed = SealedToken {
            ciphertext: row.ciphertext,
            nonce: row.nonce,
        };
        // The version is remembered even when the row cannot be read, so the run's
        // needs-reconnect mark stays conditional on this exact row.
        *self.loaded.borrow_mut() = Some(version);
        Ok(
            match seal::open_token(&self.cipher, PROVIDER_YOUTUBE, &sealed).await {
                Ok(token) => TokenLoad::Token(token),
                Err(_) => TokenLoad::Unreadable,
            },
        )
    }

    async fn save(&self, token: &StoredToken) -> Result<bool, PortError> {
        let fail = |_| port("token write failed");
        let version = self.loaded_version()?;
        let sealed = self.seal(token).await?;
        let written = self
            .db
            .prepare(SAVE_SQL)
            .bind(&[
                PROVIDER_YOUTUBE.into(),
                sealed.ciphertext.as_str().into(),
                sealed.nonce.as_str().into(),
                (token.expires_at as f64).into(),
                (now_secs() as f64).into(),
                version.as_str().into(),
            ])
            .map_err(fail)?
            .first::<NonceRow>(None)
            .await
            .map_err(fail)?;
        // Applied: this run now holds the new version. Not applied: the owner
        // reconnected meanwhile, which is not an error, and their token stays. The
        // caller is told either way and stops on `false`.
        let applied = written.is_some_and(|r| r.nonce == sealed.nonce);
        if applied {
            *self.loaded.borrow_mut() = Some(sealed.nonce);
        }
        Ok(applied)
    }

    async fn connect(&self, token: &StoredToken) -> Result<(), PortError> {
        let fail = |_| port("token write failed");
        let sealed = self.seal(token).await?;
        self.db
            .prepare(CONNECT_SQL)
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

    async fn mark_needs_reconnect(&self) -> Result<(), PortError> {
        let version = self.loaded_version()?;
        self.db
            .prepare(MARK_SQL)
            .bind(&[PROVIDER_YOUTUBE.into(), version.as_str().into()])
            .map_err(|_| port("mark write failed"))?
            .run()
            .await
            .map_err(|_| port("mark write failed"))?;
        Ok(())
    }

    /// A failed read is an error, and so is a value other than 0 or 1: callers stop.
    async fn needs_reconnect(&self) -> Result<bool, PortError> {
        Ok(self.connection_mark().await?.unwrap_or(false))
    }

    /// Row presence and the flag, never the ciphertext.
    async fn connection_mark(&self) -> Result<Option<bool>, PortError> {
        let row = self
            .db
            .prepare("SELECT needs_reconnect FROM oauth_tokens WHERE provider = ?1")
            .bind(&[PROVIDER_YOUTUBE.into()])
            .map_err(|_| port("mark read failed"))?
            .first::<FlagRow>(None)
            .await
            .map_err(|_| port("mark read failed"))?;
        row.map(|r| connect_dto::flag_from_row(r.needs_reconnect))
            .transpose()
    }
}

/// For the status route, which only reads row flags and so needs no key. Any use
/// of it to encrypt or decrypt is an error.
pub struct NoCipher;

impl TokenCipher for NoCipher {
    async fn encrypt(&self, _: &[u8; NONCE_LEN], _: &[u8], _: &[u8]) -> Result<Vec<u8>, PortError> {
        Err(port("no cipher"))
    }
    async fn decrypt(&self, _: &[u8; NONCE_LEN], _: &[u8], _: &[u8]) -> Result<Vec<u8>, PortError> {
        Err(port("no cipher"))
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

/// Refreshes the access token at Google's token endpoint. The classification (what is
/// `Auth`, `RateLimited` or `Transient`) is `domain::youtube::refresh_outcome`.
#[allow(dead_code)] // wired by the scheduled handler, a later commit of this slice
pub struct GoogleRefresher {
    config: OAuthConfig,
    client_secret: String,
}

#[allow(dead_code)] // as above
impl GoogleRefresher {
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

    fn request(&self, refresh_token: &str) -> Option<Request> {
        // The body holds the refresh token and the client secret: never logged.
        let body = self
            .config
            .refresh_request_body(refresh_token, &self.client_secret);
        let headers = Headers::new();
        headers
            .set("Content-Type", "application/x-www-form-urlencoded")
            .ok()?;
        headers.set("Accept", "application/json").ok()?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(JsValue::from_str(&body)))
            .with_redirect(RequestRedirect::Manual);
        Request::new_with_init(oauth::GOOGLE_TOKEN_URL, &init).ok()
    }
}

impl TokenRefresher for GoogleRefresher {
    /// A failure to send or read is `Transient`: refreshing creates nothing at YouTube.
    /// The answer's text is parsed for its fixed fields and dropped, never logged.
    async fn refresh(&self, token: &StoredToken) -> Result<StoredToken, PublishError> {
        // Nothing to refresh with: only the owner reconnecting fixes it.
        if token.refresh_token.is_empty() {
            return Err(PublishError::Auth);
        }
        let request = self
            .request(&token.refresh_token)
            .ok_or(PublishError::Transient)?;
        let mut response = net::send(request)
            .await
            .map_err(|_| PublishError::Transient)?;
        let status = response.status_code();
        let retry_after = response.headers().get("Retry-After").ok().flatten();
        let text = response.text().await.map_err(|_| PublishError::Transient)?;
        let reply = connect_dto::reply_from_body(status, &text);
        youtube::refresh_outcome(&reply, retry_after.as_deref(), now_secs())
    }
}
