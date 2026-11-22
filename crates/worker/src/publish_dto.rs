//! How a post's publishing state is stored in the `posts` table (migration 0004),
//! and the SQL that moves it. No decisions here; those are `domain::publish`.
//!
//! One column per state field. Fields that do not belong to a state are NULL, and
//! the counters that do not belong to it are 0, so a stored state has exactly one
//! shape and the compare-and-set can compare every column.

use domain::app::PortError;
use domain::publish::{is_valid_external_id, FailReason, PublishState};
use serde::Deserialize;

/// The columns of one `PublishState`, in the order the SQL below binds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateColumns {
    pub state: &'static str,
    pub attempts: i64,
    pub not_before: i64,
    pub attempt: Option<i64>,
    pub started_at: Option<i64>,
    pub external_id: Option<String>,
    pub fail_reason: Option<&'static str>,
}

pub fn columns_of(state: &PublishState) -> StateColumns {
    let base = StateColumns {
        state: "queued",
        attempts: 0,
        not_before: 0,
        attempt: None,
        started_at: None,
        external_id: None,
        fail_reason: None,
    };
    match state {
        PublishState::Queued {
            attempts,
            not_before,
        } => StateColumns {
            attempts: i64::from(*attempts),
            not_before: *not_before,
            ..base
        },
        PublishState::Publishing {
            attempt,
            started_at,
        } => StateColumns {
            state: "publishing",
            attempt: Some(i64::from(*attempt)),
            started_at: Some(*started_at),
            ..base
        },
        PublishState::Published { external_id } => StateColumns {
            state: "published",
            external_id: Some(external_id.clone()),
            ..base
        },
        PublishState::Failed { attempts, reason } => StateColumns {
            state: "failed",
            attempts: i64::from(*attempts),
            fail_reason: Some(reason.code()),
            ..base
        },
    }
}

/// Due Queued posts for YouTube plus every Publishing post, as of `?1` (the run's clock).
/// Published and Failed posts are history and stay out. A post for Instagram only is
/// never due here, so it cannot crowd the others out.
pub const LIST_SQL: &str = "SELECT id, video_key, title, caption, platforms, slot_at, \
     state, attempts, not_before, attempt, started_at, external_id, fail_reason FROM posts \
     WHERE state = 'publishing' \
     OR (state = 'queued' AND slot_at <= ?1 AND not_before <= ?1 AND platforms != 'instagram') \
     ORDER BY slot_at";

/// The compare-and-set: ONE statement that writes `next` (?2 to ?8) only if the row
/// still holds every column of `expected` (?9 to ?15). `IS` compares NULLs too. A row
/// that is missing or changed returns nothing, which the adapter reports as a conflict.
pub const TRANSITION_SQL: &str = "UPDATE posts SET state = ?2, attempts = ?3, not_before = ?4, \
     attempt = ?5, started_at = ?6, external_id = ?7, fail_reason = ?8 \
     WHERE id = ?1 AND state = ?9 AND attempts IS ?10 AND not_before IS ?11 AND attempt IS ?12 \
     AND started_at IS ?13 AND external_id IS ?14 AND fail_reason IS ?15 RETURNING id";

/// One value to bind. The adapter turns these into D1 values (numbers as doubles, which
/// are exact here: the domain caps times far below 2^53).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    Int(i64),
    Text(String),
    Null,
}

fn columns_params(c: StateColumns) -> [Param; 7] {
    let text = |t: Option<String>| t.map_or(Param::Null, Param::Text);
    let int = |n: Option<i64>| n.map_or(Param::Null, Param::Int);
    [
        Param::Text(c.state.to_string()),
        Param::Int(c.attempts),
        Param::Int(c.not_before),
        int(c.attempt),
        int(c.started_at),
        text(c.external_id),
        text(c.fail_reason.map(str::to_string)),
    ]
}

/// The 15 values for `TRANSITION_SQL`, in placeholder order.
pub fn transition_params(id: &str, expected: &PublishState, next: &PublishState) -> Vec<Param> {
    let mut v = vec![Param::Text(id.to_string())];
    v.extend(columns_params(columns_of(next)));
    v.extend(columns_params(columns_of(expected)));
    v
}

/// A `posts` row's state columns as D1 returns them (numbers arrive as doubles).
#[derive(Debug, Deserialize)]
pub struct RawState {
    pub state: String,
    pub attempts: f64,
    pub not_before: f64,
    pub attempt: Option<f64>,
    pub started_at: Option<f64>,
    pub external_id: Option<String>,
    pub fail_reason: Option<String>,
}

/// Largest integer a JavaScript number (as D1 returns it) holds exactly.
const MAX_EXACT: f64 = 9_007_199_254_740_991.0;

fn bad_row() -> PortError {
    // Fixed text: a row value is never copied into an error.
    PortError("bad publish state in row".into())
}

fn whole(n: f64) -> Result<i64, PortError> {
    if n.is_finite() && n.fract() == 0.0 && (0.0..=MAX_EXACT).contains(&n) {
        Ok(n as i64)
    } else {
        Err(bad_row())
    }
}

fn counter(n: f64) -> Result<u32, PortError> {
    u32::try_from(whole(n)?).map_err(|_| bad_row())
}

/// Reads a row back into a state, refusing any shape `columns_of` never writes
/// (a field that does not belong, a missing one, a bad id or reason code).
pub fn state_from_row(row: &RawState) -> Result<PublishState, PortError> {
    let attempts = counter(row.attempts)?;
    let not_before = whole(row.not_before)?;
    let attempt = row.attempt.map(counter).transpose()?;
    let started_at = row.started_at.map(whole).transpose()?;
    let id = row.external_id.as_deref();
    let reason = row.fail_reason.as_deref();
    let state = match (row.state.as_str(), attempt, started_at, id, reason) {
        ("queued", None, None, None, None) => PublishState::Queued {
            attempts,
            not_before,
        },
        ("publishing", Some(attempt), Some(started_at), None, None) if attempt >= 1 => {
            PublishState::Publishing {
                attempt,
                started_at,
            }
        }
        ("published", None, None, Some(id), None) if is_valid_external_id(id) => {
            PublishState::Published {
                external_id: id.to_string(),
            }
        }
        ("failed", None, None, None, Some(code)) => PublishState::Failed {
            attempts,
            reason: FailReason::from_code(code).ok_or_else(bad_row)?,
        },
        _ => return Err(bad_row()),
    };
    // The counters that do not belong to a state are 0, exactly as `columns_of` writes them.
    let expected = columns_of(&state);
    if expected.attempts != i64::from(attempts) || expected.not_before != not_before {
        return Err(bad_row());
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_states() -> Vec<PublishState> {
        let mut v = vec![
            PublishState::new_queued(),
            PublishState::Queued {
                attempts: 2,
                not_before: 1_700_000_000,
            },
            PublishState::Publishing {
                attempt: 3,
                started_at: 1_700_000_100,
            },
            PublishState::Published {
                external_id: "dQw4w9WgXcQ".into(),
            },
        ];
        v.extend(FailReason::ALL.map(|reason| PublishState::Failed {
            attempts: 3,
            reason,
        }));
        v
    }

    fn raw(c: &StateColumns) -> RawState {
        RawState {
            state: c.state.into(),
            attempts: c.attempts as f64,
            not_before: c.not_before as f64,
            attempt: c.attempt.map(|n| n as f64),
            started_at: c.started_at.map(|n| n as f64),
            external_id: c.external_id.clone(),
            fail_reason: c.fail_reason.map(str::to_string),
        }
    }

    fn placeholders(sql: &str) -> usize {
        let mut max = 0;
        let mut rest = sql;
        while let Some(i) = rest.find('?') {
            let digits: String = rest[i + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            max = max.max(digits.parse().unwrap());
            rest = &rest[i + 1..];
        }
        max
    }

    #[test]
    fn the_transition_binds_one_value_per_placeholder_in_order() {
        let expected = PublishState::Queued {
            attempts: 1,
            not_before: 50,
        };
        let next = PublishState::Publishing {
            attempt: 2,
            started_at: 60,
        };
        let p = transition_params("post1", &expected, &next);
        assert_eq!(p.len(), placeholders(TRANSITION_SQL));
        assert_eq!(p[0], Param::Text("post1".into()));
        // ?2 to ?8 are the new state, ?9 to ?15 the expected one.
        assert_eq!(p[1], Param::Text("publishing".into()));
        assert_eq!(p[4], Param::Int(2));
        assert_eq!(p[8], Param::Text("queued".into()));
        assert_eq!(p[9], Param::Int(1));
        assert_eq!(p[10], Param::Int(50));
        assert_eq!(
            &p[11..],
            [Param::Null, Param::Null, Param::Null, Param::Null]
        );
    }

    #[test]
    fn the_list_binds_only_the_run_time() {
        assert_eq!(placeholders(LIST_SQL), 1);
    }

    #[test]
    fn every_state_round_trips_through_its_columns() {
        for s in all_states() {
            assert_eq!(state_from_row(&raw(&columns_of(&s))), Ok(s));
        }
    }

    #[test]
    fn a_state_keeps_only_its_own_fields() {
        let c = columns_of(&PublishState::Queued {
            attempts: 2,
            not_before: 9,
        });
        assert_eq!(
            c,
            StateColumns {
                state: "queued",
                attempts: 2,
                not_before: 9,
                attempt: None,
                started_at: None,
                external_id: None,
                fail_reason: None
            }
        );
        let c = columns_of(&PublishState::Publishing {
            attempt: 2,
            started_at: 7,
        });
        assert_eq!(
            (c.state, c.attempts, c.not_before, c.attempt, c.started_at),
            ("publishing", 0, 0, Some(2), Some(7))
        );
        let c = columns_of(&PublishState::Published {
            external_id: "abc".into(),
        });
        assert_eq!(
            (c.state, c.external_id.as_deref(), c.attempt),
            ("published", Some("abc"), None)
        );
        let c = columns_of(&PublishState::Failed {
            attempts: 3,
            reason: FailReason::Unknown,
        });
        assert_eq!(
            (c.state, c.attempts, c.fail_reason, c.external_id),
            ("failed", 3, Some("unknown"), None)
        );
    }

    #[test]
    fn a_row_that_does_not_make_sense_is_refused() {
        let good = raw(&columns_of(&PublishState::new_queued()));
        let publishing = raw(&columns_of(&PublishState::Publishing {
            attempt: 1,
            started_at: 5,
        }));
        let published = raw(&columns_of(&PublishState::Published {
            external_id: "abc".into(),
        }));
        let failed = raw(&columns_of(&PublishState::Failed {
            attempts: 1,
            reason: FailReason::Auth,
        }));
        let bad: Vec<RawState> = vec![
            RawState {
                state: "gone".into(),
                ..clone_raw(&good)
            },
            RawState {
                attempts: -1.0,
                ..clone_raw(&good)
            },
            RawState {
                attempts: 1.5,
                ..clone_raw(&good)
            },
            RawState {
                not_before: f64::NAN,
                ..clone_raw(&good)
            },
            RawState {
                attempts: 5e9,
                ..clone_raw(&good)
            },
            RawState {
                attempt: Some(1.0),
                ..clone_raw(&good)
            },
            RawState {
                attempt: None,
                ..clone_raw(&publishing)
            },
            RawState {
                attempt: Some(0.0),
                ..clone_raw(&publishing)
            },
            RawState {
                started_at: None,
                ..clone_raw(&publishing)
            },
            RawState {
                external_id: None,
                ..clone_raw(&published)
            },
            RawState {
                external_id: Some("a b".into()),
                ..clone_raw(&published)
            },
            RawState {
                external_id: Some(String::new()),
                ..clone_raw(&published)
            },
            RawState {
                fail_reason: None,
                ..clone_raw(&failed)
            },
            RawState {
                fail_reason: Some("because".into()),
                ..clone_raw(&failed)
            },
            RawState {
                fail_reason: Some("auth".into()),
                ..clone_raw(&good)
            },
            RawState {
                external_id: Some("abc".into()),
                ..clone_raw(&failed)
            },
        ];
        for (i, row) in bad.iter().enumerate() {
            assert!(state_from_row(row).is_err(), "case {i}: {row:?}");
        }
    }

    fn clone_raw(r: &RawState) -> RawState {
        RawState {
            state: r.state.clone(),
            attempts: r.attempts,
            not_before: r.not_before,
            attempt: r.attempt,
            started_at: r.started_at,
            external_id: r.external_id.clone(),
            fail_reason: r.fail_reason.clone(),
        }
    }

    #[test]
    fn the_row_error_carries_no_row_text() {
        let mut r = raw(&columns_of(&PublishState::new_queued()));
        r.external_id = Some("SECRET-LOOKING-VALUE".into());
        let e = state_from_row(&r).unwrap_err();
        assert!(!e.0.contains("SECRET"), "{e:?}");
    }
}
