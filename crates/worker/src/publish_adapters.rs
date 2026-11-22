//! Cloudflare adapters for publishing: the D1 state store.
//!
//! Thin glue: every decision is in `domain::publish` and `domain::publish_app`; the
//! column mapping and SQL are in `publish_dto`. These adapters cannot run natively.
//! Their errors are fixed text: no D1 error or row value is copied in.

use crate::publish_dto::{self, Param};
use crate::publish_log;
use domain::app::PortError;
use domain::publish::ScheduledPost;
use domain::publish_app::{Cas, PublishRepository};
use domain::queue_view::QueueViewRepository;
use serde::Deserialize;
use worker::wasm_bindgen::JsValue;
use worker::D1Database;

fn port(text: &str) -> PortError {
    PortError(text.to_string())
}

fn js(p: &Param) -> JsValue {
    match p {
        // Exact: the domain caps times far below 2^53, and counters are u32.
        Param::Int(n) => JsValue::from_f64(*n as f64),
        Param::Text(t) => JsValue::from_str(t),
        Param::Null => JsValue::NULL,
    }
}

pub struct D1PublishRepository {
    db: D1Database,
}

impl D1PublishRepository {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

impl D1PublishRepository {
    /// Runs one of the two read statements, which bind a single time value.
    async fn read(&self, sql: &str, at: i64) -> Result<Vec<ScheduledPost>, PortError> {
        let fail = |_| port("publish list failed");
        let rows = self
            .db
            .prepare(sql)
            .bind(&[JsValue::from_f64(at as f64)])
            .map_err(fail)?
            .all()
            .await
            .map_err(fail)?
            .results::<publish_dto::ListRow>()
            .map_err(fail)?;
        // One bad row is left out and logged by id, so it cannot stop every run.
        let (posts, skipped) = publish_dto::map_rows(rows);
        for id in skipped {
            worker::console_error!("{}", publish_log::skipped_row_line(&id));
        }
        Ok(posts)
    }
}

impl QueueViewRepository for D1PublishRepository {
    async fn recent(&self, since: i64) -> Result<Vec<ScheduledPost>, PortError> {
        self.read(publish_dto::RECENT_SQL, since).await
    }
}

impl PublishRepository for D1PublishRepository {
    async fn list(&self, now: i64) -> Result<Vec<ScheduledPost>, PortError> {
        self.read(publish_dto::LIST_SQL, now).await
    }

    /// ONE `UPDATE ... WHERE id AND every state column = expected RETURNING id`. Only a
    /// returned row counts as applied; a missing or changed row returns none, so it is
    /// a conflict. The answer never depends on D1's change count.
    async fn transition(
        &self,
        id: &str,
        expected: &domain::publish::PublishState,
        next: &domain::publish::PublishState,
    ) -> Result<Cas, PortError> {
        let fail = |_| port("publish state write failed");
        let params: Vec<JsValue> = publish_dto::transition_params(id, expected, next)
            .iter()
            .map(js)
            .collect();
        let row = self
            .db
            .prepare(publish_dto::TRANSITION_SQL)
            .bind(&params)
            .map_err(fail)?
            .first::<IdRow>(None)
            .await
            .map_err(fail)?;
        Ok(match row {
            Some(r) if r.id == id => Cas::Applied,
            _ => Cas::Conflict,
        })
    }
}
