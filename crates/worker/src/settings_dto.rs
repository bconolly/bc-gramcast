//! How the publish settings cross the wire and the `settings` table (migration 0005):
//! the JSON shapes, the SQL, and the mapping of D1's answers. No decisions here; those are
//! `domain::settings`.

use domain::publish_app::Cas;
use domain::settings::{EffectiveSettings, PublishSettings, SettingsError, Source};
use serde::{Deserialize, Deserializer, Serialize};

/// The 409 text. The web client matches it exactly (`SETTINGS_CONFLICT_MESSAGE` in
/// `web/src/lib/api.ts`); a test here reads that file and checks the two agree.
pub const CONFLICT_MESSAGE: &str = "settings changed since you loaded them";

pub const GET_SQL: &str =
    "SELECT publish_enabled, publish_privacy, updated_at FROM settings WHERE id = 1";

/// First save: inserts only if there is no row, so a row that is already there is left alone.
pub const INSERT_SQL: &str =
    "INSERT INTO settings (id, publish_enabled, publish_privacy, updated_at) \
     VALUES (1, ?1, ?2, ?3) ON CONFLICT(id) DO NOTHING";

/// Later saves: changes the row only if its `updated_at` is still the one the caller saw (?4).
pub const UPDATE_SQL: &str = "UPDATE settings SET publish_enabled = ?1, publish_privacy = ?2, \
     updated_at = ?3 WHERE id = 1 AND updated_at = ?4";

/// Decodes the row exactly as D1 returned it (any JSON). A row of the wrong shape or type
/// (a NULL, a text number, a missing column) is `BadRow`, the same as a bad value.
pub fn decode_row(value: serde_json::Value) -> Result<PublishSettings, SettingsError> {
    let row: SettingsRow = serde_json::from_value(value).map_err(|_| SettingsError::BadRow)?;
    row_to_settings(&row)
}

/// The row as D1 returns it: integers may arrive as doubles.
#[derive(Debug, Deserialize)]
pub struct SettingsRow {
    pub publish_enabled: f64,
    pub publish_privacy: String,
    pub updated_at: f64,
}

pub fn row_to_settings(row: &SettingsRow) -> Result<PublishSettings, SettingsError> {
    PublishSettings::from_row(row.publish_enabled, &row.publish_privacy, row.updated_at)
}

/// One changed row means the write applied. A count that is missing or zero is a
/// conflict: that is the safe direction, since nothing is then reported as saved.
pub fn cas_from_changes(changes: Option<usize>) -> Cas {
    match changes {
        Some(n) if n > 0 => Cas::Applied,
        _ => Cas::Conflict,
    }
}

/// The PUT body. `expected_updated_at` must be present (`null` means "no row yet");
/// a missing field is refused, as is any unknown field.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PutBody {
    pub enabled: bool,
    pub privacy: String,
    #[serde(deserialize_with = "present_option")]
    pub expected_updated_at: Option<u64>,
}

fn present_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(d)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SettingsJson {
    pub enabled: bool,
    pub privacy: &'static str,
    pub updated_at: Option<u64>,
    pub source: &'static str,
}

pub fn effective_to_json(e: &EffectiveSettings) -> SettingsJson {
    SettingsJson {
        enabled: e.enabled,
        privacy: e.privacy.status(),
        updated_at: e.updated_at,
        source: e.source().code(),
    }
}

pub fn saved_to_json(s: &PublishSettings) -> SettingsJson {
    SettingsJson {
        enabled: s.enabled,
        privacy: s.privacy.status(),
        updated_at: Some(s.updated_at),
        source: Source::Settings.code(),
    }
}

/// Fixed text for every status; nothing from storage is ever shown.
pub fn settings_error_status(e: &SettingsError) -> (u16, &'static str) {
    match e {
        SettingsError::Invalid => (400, "bad request"),
        SettingsError::Conflict => (409, CONFLICT_MESSAGE),
        SettingsError::BadRow | SettingsError::Port(_) => (500, "internal error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::app::PortError;
    use domain::youtube::Privacy;

    fn body(json: &str) -> Result<PutBody, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn a_row_maps_through_the_domain_decoder() {
        let row = SettingsRow {
            publish_enabled: 1.0,
            publish_privacy: "unlisted".into(),
            updated_at: 77.0,
        };
        assert_eq!(
            row_to_settings(&row),
            Ok(PublishSettings {
                enabled: true,
                privacy: Privacy::Unlisted,
                updated_at: 77
            })
        );
        let bad = SettingsRow {
            publish_enabled: 7.0,
            ..row
        };
        assert_eq!(row_to_settings(&bad), Err(SettingsError::BadRow));
    }

    #[test]
    fn every_row_that_fails_to_decode_is_a_bad_row() {
        let ok =
            serde_json::json!({"publish_enabled": 1, "publish_privacy": "public", "updated_at": 5});
        assert_eq!(decode_row(ok).unwrap().updated_at, 5);
        let floats = serde_json::json!({"publish_enabled": 0.0, "publish_privacy": "private", "updated_at": 5.0});
        assert!(decode_row(floats).is_ok());
        for bad in [
            serde_json::json!({"publish_enabled": null, "publish_privacy": "private", "updated_at": 5}),
            serde_json::json!({"publish_enabled": "1", "publish_privacy": "private", "updated_at": 5}),
            serde_json::json!({"publish_enabled": 1, "publish_privacy": 7, "updated_at": 5}),
            serde_json::json!({"publish_enabled": 1, "publish_privacy": "private"}),
            serde_json::json!({"publish_enabled": 1, "publish_privacy": "x", "updated_at": 5}),
            serde_json::json!({"publish_enabled": 1, "publish_privacy": "private", "updated_at": "5"}),
            serde_json::json!(null),
            serde_json::json!([]),
        ] {
            assert_eq!(decode_row(bad.clone()), Err(SettingsError::BadRow), "{bad}");
        }
    }

    /// The web client recognises a conflict by this exact text (`SETTINGS_CONFLICT_MESSAGE`
    /// in `web/src/lib/api.ts`). If either side changes, this fails.
    #[test]
    fn the_conflict_text_is_the_one_the_web_client_looks_for() {
        let api = include_str!("../../../web/src/lib/api.ts");
        let wanted = format!("SETTINGS_CONFLICT_MESSAGE = \"{CONFLICT_MESSAGE}\"");
        assert!(api.contains(&wanted), "{wanted}");
        assert_eq!(
            settings_error_status(&SettingsError::Conflict),
            (409, CONFLICT_MESSAGE)
        );
    }

    #[test]
    fn a_row_decodes_from_d1_style_json_with_float_integers() {
        let row: SettingsRow = serde_json::from_str(
            r#"{"publish_enabled":0.0,"publish_privacy":"private","updated_at":5.0}"#,
        )
        .unwrap();
        assert!(row_to_settings(&row).is_ok());
    }

    #[test]
    fn only_a_positive_change_count_is_applied() {
        assert_eq!(cas_from_changes(Some(1)), Cas::Applied);
        assert_eq!(cas_from_changes(Some(0)), Cas::Conflict);
        // Unavailable: the safe direction is a conflict.
        assert_eq!(cas_from_changes(None), Cas::Conflict);
    }

    #[test]
    fn the_sql_binds_what_the_adapter_binds() {
        assert!(INSERT_SQL.contains("ON CONFLICT(id) DO NOTHING"));
        assert!(INSERT_SQL.contains("?3") && !INSERT_SQL.contains("?4"));
        assert!(UPDATE_SQL.contains("WHERE id = 1 AND updated_at = ?4"));
        assert!(GET_SQL.contains("WHERE id = 1"));
    }

    #[test]
    fn the_put_body_needs_all_three_fields() {
        assert_eq!(
            body(r#"{"enabled":true,"privacy":"public","expected_updated_at":12}"#).unwrap(),
            PutBody {
                enabled: true,
                privacy: "public".into(),
                expected_updated_at: Some(12)
            }
        );
        assert_eq!(
            body(r#"{"enabled":false,"privacy":"private","expected_updated_at":null}"#)
                .unwrap()
                .expected_updated_at,
            None
        );
        for bad in [
            r#"{"enabled":true,"privacy":"public"}"#,
            r#"{"privacy":"public","expected_updated_at":1}"#,
            r#"{"enabled":true,"expected_updated_at":1}"#,
        ] {
            assert!(body(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_put_body_refuses_wrong_types_and_unknown_fields() {
        for bad in [
            r#"{"enabled":"true","privacy":"public","expected_updated_at":1}"#,
            r#"{"enabled":1,"privacy":"public","expected_updated_at":1}"#,
            r#"{"enabled":true,"privacy":1,"expected_updated_at":1}"#,
            r#"{"enabled":true,"privacy":"public","expected_updated_at":-1}"#,
            r#"{"enabled":true,"privacy":"public","expected_updated_at":1.5}"#,
            r#"{"enabled":true,"privacy":"public","expected_updated_at":"1"}"#,
            r#"{"enabled":true,"privacy":"public","expected_updated_at":1,"updated_at":2}"#,
            r#"{"enabled":true,"privacy":"public","expected_updated_at":1,"source":"x"}"#,
        ] {
            assert!(body(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_json_shape_is_fixed() {
        let none = EffectiveSettings {
            enabled: false,
            privacy: Privacy::Private,
            updated_at: None,
        };
        assert_eq!(
            serde_json::to_string(&effective_to_json(&none)).unwrap(),
            r#"{"enabled":false,"privacy":"private","updated_at":null,"source":"defaults"}"#
        );
        let saved = PublishSettings {
            enabled: true,
            privacy: Privacy::Public,
            updated_at: 9,
        };
        assert_eq!(
            serde_json::to_string(&saved_to_json(&saved)).unwrap(),
            r#"{"enabled":true,"privacy":"public","updated_at":9,"source":"settings"}"#
        );
    }

    #[test]
    fn every_error_has_a_status_and_fixed_text() {
        assert_eq!(
            settings_error_status(&SettingsError::Invalid),
            (400, "bad request")
        );
        assert_eq!(
            settings_error_status(&SettingsError::Conflict),
            (409, "settings changed since you loaded them")
        );
        for e in [
            SettingsError::BadRow,
            SettingsError::Port(PortError("secret detail".into())),
        ] {
            assert_eq!(settings_error_status(&e), (500, "internal error"));
        }
    }
}
