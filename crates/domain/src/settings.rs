//! The owner's publish settings (slice 6a): whether the scheduled run publishes, and the
//! privacy it uses. Stored as one row; with no row, the old `PUBLISH_ENABLED` and
//! `PUBLISH_PRIVACY` vars are the fallback. A row that cannot be read or decoded is an
//! error, never the fallback, so a bad read can never turn publishing on.

use crate::app::PortError;
use crate::youtube::Privacy;

/// Largest time we store: JavaScript's largest exact integer, since D1 hands numbers back
/// as doubles.
pub const MAX_UPDATED_AT: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishSettings {
    pub enabled: bool,
    pub privacy: Privacy,
    /// Unix seconds from the Worker's clock. Strictly increases with every save.
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Settings,
    Defaults,
}

impl Source {
    pub fn code(self) -> &'static str {
        match self {
            Source::Settings => "settings",
            Source::Defaults => "defaults",
        }
    }
}

/// What the run and the screen act on: the stored row, or the vars when there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub enabled: bool,
    pub privacy: Privacy,
    /// `None` when there is no row (the vars are in force).
    pub updated_at: Option<u64>,
}

impl EffectiveSettings {
    pub fn source(&self) -> Source {
        if self.updated_at.is_some() {
            Source::Settings
        } else {
            Source::Defaults
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SettingsError {
    /// The request's values are not allowed.
    Invalid,
    /// The stored row does not decode.
    BadRow,
    /// The row was not what the caller expected; nothing was written.
    Conflict,
    Port(PortError),
}

/// A validated PUT: what to store and the `updated_at` the caller last saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsUpdate {
    enabled: bool,
    privacy: Privacy,
    expected_updated_at: Option<u64>,
}

impl PublishSettings {
    /// Decodes the stored columns. D1 may hand integers back as doubles, so they are
    /// accepted only when whole and in range. Anything else is `BadRow`.
    pub fn from_row(enabled: f64, privacy: &str, updated_at: f64) -> Result<Self, SettingsError> {
        let enabled = match whole(enabled) {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(SettingsError::BadRow),
        };
        Ok(Self {
            enabled,
            privacy: Privacy::parse(privacy).ok_or(SettingsError::BadRow)?,
            updated_at: whole(updated_at).ok_or(SettingsError::BadRow)?,
        })
    }
}

/// A double that is a whole number from 0 to `MAX_UPDATED_AT`.
fn whole(v: f64) -> Option<u64> {
    (v.is_finite() && v >= 0.0 && v.fract() == 0.0 && v <= MAX_UPDATED_AT as f64)
        .then_some(v as u64)
}

/// Validates a PUT. The privacy must be one of the three names exactly, and the expected
/// time must leave room to go one higher.
pub fn parse_update(
    enabled: bool,
    privacy: &str,
    expected_updated_at: Option<u64>,
) -> Result<SettingsUpdate, SettingsError> {
    let privacy = Privacy::parse(privacy).ok_or(SettingsError::Invalid)?;
    if expected_updated_at.is_some_and(|t| t >= MAX_UPDATED_AT) {
        return Err(SettingsError::Invalid);
    }
    Ok(SettingsUpdate {
        enabled,
        privacy,
        expected_updated_at,
    })
}

/// The time to store: the clock, but always after the time being replaced, so two saves
/// in one second (or a clock that stepped back) still increase it.
pub fn next_updated_at(now: i64, expected: Option<u64>) -> u64 {
    let clock = u64::try_from(now).unwrap_or(0).min(MAX_UPDATED_AT);
    let after = expected.map_or(0, |t| t.saturating_add(1));
    clock.max(after).min(MAX_UPDATED_AT)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stored_row_decodes_strictly() {
        let ok = PublishSettings::from_row(1.0, "unlisted", 12.0).unwrap();
        assert_eq!(
            ok,
            PublishSettings {
                enabled: true,
                privacy: Privacy::Unlisted,
                updated_at: 12
            }
        );
        assert!(
            !PublishSettings::from_row(0.0, "private", 0.0)
                .unwrap()
                .enabled
        );
        let bad: [(f64, &str, f64); 11] = [
            (2.0, "private", 1.0),
            (-1.0, "private", 1.0),
            (0.5, "private", 1.0),
            (f64::NAN, "private", 1.0),
            (1.0, "Public", 1.0),
            (1.0, "", 1.0),
            (1.0, "private", -1.0),
            (1.0, "private", 1.5),
            (1.0, "private", f64::NAN),
            (1.0, "private", f64::INFINITY),
            (1.0, "private", 9_007_199_254_740_992.0),
        ];
        for (e, p, u) in bad {
            assert_eq!(
                PublishSettings::from_row(e, p, u),
                Err(SettingsError::BadRow),
                "{e} {p} {u}"
            );
        }
    }

    #[test]
    fn the_put_body_is_validated_here() {
        assert!(parse_update(true, "public", None).is_ok());
        assert!(parse_update(false, "private", Some(0)).is_ok());
        for bad in ["", "Public", " private", "secret", "true"] {
            assert_eq!(
                parse_update(true, bad, None),
                Err(SettingsError::Invalid),
                "{bad:?}"
            );
        }
        assert_eq!(
            parse_update(true, "private", Some(MAX_UPDATED_AT)),
            Err(SettingsError::Invalid)
        );
        assert!(parse_update(true, "private", Some(MAX_UPDATED_AT - 1)).is_ok());
    }

    #[test]
    fn the_new_time_is_the_clock_when_it_is_ahead_and_never_negative() {
        assert_eq!(next_updated_at(2000, Some(5)), 2000);
        assert_eq!(next_updated_at(-7, None), 0);
        assert_eq!(next_updated_at(-7, Some(3)), 4);
        assert_eq!(next_updated_at(i64::MAX, None), MAX_UPDATED_AT);
    }

    #[test]
    fn the_source_codes_are_fixed() {
        assert_eq!(Source::Settings.code(), "settings");
        assert_eq!(Source::Defaults.code(), "defaults");
    }
}
