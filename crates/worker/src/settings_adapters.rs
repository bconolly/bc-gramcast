//! Cloudflare adapter for the publish settings: the D1 row. Thin glue; every decision is in
//! `domain::settings`, and the SQL and mapping are in `settings_dto`. It cannot run natively.
//! Its errors are fixed text: no D1 error or row value is copied in.

use crate::settings_dto::{cas_from_changes, decode_row, GET_SQL, INSERT_SQL, UPDATE_SQL};
use domain::app::PortError;
use domain::publish_app::Cas;
use domain::settings::{PublishSettings, SettingsError, SettingsRepository};
use worker::wasm_bindgen::JsValue;
use worker::D1Database;

pub struct D1SettingsRepository {
    db: D1Database,
}

impl D1SettingsRepository {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

fn port(text: &str) -> PortError {
    PortError(text.to_string())
}

impl SettingsRepository for D1SettingsRepository {
    async fn get(&self) -> Result<Option<PublishSettings>, SettingsError> {
        let fail = |_| SettingsError::Port(port("settings read failed"));
        let row = self
            .db
            .prepare(GET_SQL)
            .first::<serde_json::Value>(None)
            .await
            .map_err(fail)?;
        // Read as loose JSON first, so a row of any wrong shape is a BadRow, not a read error.
        row.map(|r| {
            decode_row(r).inspect_err(|_| {
                // Only that the row was bad: its values are never logged.
                worker::console_error!("bad settings row");
            })
        })
        .transpose()
    }

    /// No row expected: insert only if absent. A number expected: UPDATE ... WHERE
    /// updated_at = that number. Either way the answer comes from D1's changed-row count.
    /// If the count is unavailable it is treated as a conflict (the safe direction:
    /// nothing is reported as saved), so the screen reloads and shows what is stored.
    async fn update(
        &self,
        next: &PublishSettings,
        expected: Option<u64>,
    ) -> Result<Cas, PortError> {
        let fail = |_| port("settings write failed");
        // Exact: updated_at is capped at 2^53 - 1 by the domain.
        let mut params = vec![
            JsValue::from_f64(f64::from(u8::from(next.enabled))),
            JsValue::from_str(next.privacy.status()),
            JsValue::from_f64(next.updated_at as f64),
        ];
        let sql = match expected {
            None => INSERT_SQL,
            Some(seen) => {
                params.push(JsValue::from_f64(seen as f64));
                UPDATE_SQL
            }
        };
        let result = self
            .db
            .prepare(sql)
            .bind(&params)
            .map_err(fail)?
            .run()
            .await
            .map_err(fail)?;
        let changes = result.meta().ok().flatten().and_then(|m| m.changes);
        Ok(cas_from_changes(changes))
    }
}
