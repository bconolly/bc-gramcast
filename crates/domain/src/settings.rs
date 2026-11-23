//! The owner's publish settings (slice 6a): whether the scheduled run publishes, and the
//! privacy it uses. Stored as one row; with no row, the old `PUBLISH_ENABLED` and
//! `PUBLISH_PRIVACY` vars are the fallback. A row that cannot be read or decoded is an
//! error, never the fallback, so a bad read can never turn publishing on.

use crate::app::PortError;
use crate::publish_app::Cas;
use crate::youtube::{publish_enabled, Privacy};

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

/// Persistence port for the one settings row.
#[allow(async_fn_in_trait)]
pub trait SettingsRepository {
    /// `None` until the owner has saved. A row that does not decode is `BadRow`.
    async fn get(&self) -> Result<Option<PublishSettings>, SettingsError>;
    /// Writes `next` only if the stored `updated_at` is `expected` (`None`: no row yet).
    /// Otherwise `Conflict` and nothing is written.
    async fn update(&self, next: &PublishSettings, expected: Option<u64>)
        -> Result<Cas, PortError>;
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

pub async fn load_settings(
    repo: &impl SettingsRepository,
    enabled_var: Option<&str>,
    privacy_var: Option<&str>,
) -> Result<EffectiveSettings, SettingsError> {
    // `?`: a failed read or a bad row stops here. The vars are only for no row at all.
    Ok(match repo.get().await? {
        Some(s) => EffectiveSettings {
            enabled: s.enabled,
            privacy: s.privacy,
            updated_at: Some(s.updated_at),
        },
        None => EffectiveSettings {
            enabled: publish_enabled(enabled_var),
            privacy: Privacy::from_var(privacy_var),
            updated_at: None,
        },
    })
}

pub async fn save_settings(
    repo: &impl SettingsRepository,
    now: i64,
    update: SettingsUpdate,
) -> Result<PublishSettings, SettingsError> {
    let next = PublishSettings {
        enabled: update.enabled,
        privacy: update.privacy,
        updated_at: next_updated_at(now, update.expected_updated_at),
    };
    match repo
        .update(&next, update.expected_updated_at)
        .await
        .map_err(SettingsError::Port)?
    {
        Cas::Applied => Ok(next),
        Cas::Conflict => Err(SettingsError::Conflict),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fake store: one optional row, plus switches for the failures.
    #[derive(Default)]
    struct Fake {
        row: RefCell<Option<PublishSettings>>,
        bad_row: bool,
        down: bool,
    }

    impl SettingsRepository for Fake {
        async fn get(&self) -> Result<Option<PublishSettings>, SettingsError> {
            if self.down {
                return Err(SettingsError::Port(PortError("down".into())));
            }
            if self.bad_row {
                return Err(SettingsError::BadRow);
            }
            Ok(self.row.borrow().clone())
        }
        async fn update(
            &self,
            next: &PublishSettings,
            expected: Option<u64>,
        ) -> Result<Cas, PortError> {
            if self.down {
                return Err(PortError("down".into()));
            }
            let mut row = self.row.borrow_mut();
            if row.as_ref().map(|r| r.updated_at) != expected {
                return Ok(Cas::Conflict);
            }
            *row = Some(next.clone());
            Ok(Cas::Applied)
        }
    }

    fn row(enabled: bool, privacy: Privacy, updated_at: u64) -> Option<PublishSettings> {
        Some(PublishSettings {
            enabled,
            privacy,
            updated_at,
        })
    }

    fn update(enabled: bool, privacy: &str, expected: Option<u64>) -> SettingsUpdate {
        parse_update(enabled, privacy, expected).unwrap()
    }

    #[test]
    fn no_row_falls_back_to_the_vars() {
        let repo = Fake::default();
        let s = pollster::block_on(load_settings(&repo, Some("true"), Some("unlisted"))).unwrap();
        assert_eq!(
            (s.enabled, s.privacy, s.updated_at, s.source()),
            (true, Privacy::Unlisted, None, Source::Defaults)
        );
    }

    #[test]
    fn the_var_fallback_uses_the_old_strict_rules() {
        let repo = Fake::default();
        let s = pollster::block_on(load_settings(&repo, Some("True"), Some("PUBLIC"))).unwrap();
        assert_eq!((s.enabled, s.privacy), (false, Privacy::Private));
        let s = pollster::block_on(load_settings(&repo, None, None)).unwrap();
        assert_eq!((s.enabled, s.privacy), (false, Privacy::Private));
    }

    #[test]
    fn a_row_wins_over_the_vars() {
        let repo = Fake {
            row: RefCell::new(row(false, Privacy::Public, 50)),
            ..Default::default()
        };
        let s = pollster::block_on(load_settings(&repo, Some("true"), Some("private"))).unwrap();
        assert_eq!(
            (s.enabled, s.privacy, s.updated_at, s.source()),
            (false, Privacy::Public, Some(50), Source::Settings)
        );
    }

    #[test]
    fn a_bad_row_or_a_failed_read_is_an_error_never_the_vars() {
        let bad = Fake {
            bad_row: true,
            ..Default::default()
        };
        assert_eq!(
            pollster::block_on(load_settings(&bad, Some("true"), None)),
            Err(SettingsError::BadRow)
        );
        let down = Fake {
            down: true,
            ..Default::default()
        };
        assert_eq!(
            pollster::block_on(load_settings(&down, Some("true"), None)),
            Err(SettingsError::Port(PortError("down".into())))
        );
    }

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
    fn the_first_save_expects_no_row() {
        let repo = Fake::default();
        let saved =
            pollster::block_on(save_settings(&repo, 1000, update(true, "public", None))).unwrap();
        assert_eq!(saved.updated_at, 1000);
        assert_eq!(*repo.row.borrow(), row(true, Privacy::Public, 1000));
        // A second "first" save loses: the row exists now.
        assert_eq!(
            pollster::block_on(save_settings(&repo, 1001, update(false, "private", None))),
            Err(SettingsError::Conflict)
        );
        assert_eq!(*repo.row.borrow(), row(true, Privacy::Public, 1000));
    }

    #[test]
    fn a_stale_expected_value_conflicts_and_writes_nothing() {
        let repo = Fake {
            row: RefCell::new(row(true, Privacy::Private, 500)),
            ..Default::default()
        };
        assert_eq!(
            pollster::block_on(save_settings(
                &repo,
                900,
                update(false, "public", Some(499))
            )),
            Err(SettingsError::Conflict)
        );
        assert_eq!(
            pollster::block_on(save_settings(&repo, 900, update(false, "public", None))),
            Err(SettingsError::Conflict)
        );
        assert_eq!(*repo.row.borrow(), row(true, Privacy::Private, 500));
    }

    #[test]
    fn updated_at_strictly_increases_within_one_second() {
        let repo = Fake::default();
        let first =
            pollster::block_on(save_settings(&repo, 1000, update(true, "private", None))).unwrap();
        let second = pollster::block_on(save_settings(
            &repo,
            1000,
            update(false, "private", Some(first.updated_at)),
        ))
        .unwrap();
        assert_eq!((first.updated_at, second.updated_at), (1000, 1001));
        // A clock that went backwards still moves forward.
        let third = pollster::block_on(save_settings(
            &repo,
            10,
            update(true, "unlisted", Some(second.updated_at)),
        ))
        .unwrap();
        assert_eq!(third.updated_at, 1002);
    }

    #[test]
    fn the_new_time_is_the_clock_when_it_is_ahead_and_never_negative() {
        assert_eq!(next_updated_at(2000, Some(5)), 2000);
        assert_eq!(next_updated_at(-7, None), 0);
        assert_eq!(next_updated_at(-7, Some(3)), 4);
        assert_eq!(next_updated_at(i64::MAX, None), MAX_UPDATED_AT);
    }

    #[test]
    fn a_storage_failure_on_save_is_a_port_error() {
        let repo = Fake {
            down: true,
            ..Default::default()
        };
        assert_eq!(
            pollster::block_on(save_settings(&repo, 1, update(true, "private", None))),
            Err(SettingsError::Port(PortError("down".into())))
        );
    }

    #[test]
    fn the_source_codes_are_fixed() {
        assert_eq!(Source::Settings.code(), "settings");
        assert_eq!(Source::Defaults.code(), "defaults");
    }
}
