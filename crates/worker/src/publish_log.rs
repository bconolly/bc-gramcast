//! The only text a scheduled publish run may log (W9): fixed words, the `Halt` and
//! outcome codes, `FailReason::code()`, post ids that pass the domain's id check, and
//! an external id that passes the domain's shape check. Never a `PortError` text, a
//! title, a key, a URL or anything read from a provider.

use domain::publish::is_valid_external_id;
use domain::publish_app::{Outcome, PublishReport};
use domain::queue::is_post_id;

pub const DISABLED_LINE: &str = "publish disabled";

/// Why a run could not even start. Fixed words only: the error behind it is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailure {
    Setup,
    List,
}

pub fn failure_line(f: RunFailure) -> &'static str {
    match f {
        RunFailure::Setup => "publish run failed: setup",
        RunFailure::List => "publish run failed: list",
    }
}

/// A row of the list that could not be read and was left out. `id` comes from
/// `publish_dto::map_rows`: a well-formed post id or the word `invalid`.
pub fn skipped_row_line(id: &str) -> String {
    format!("publish list: skipped an unreadable row post={id}")
}

/// One line for the run, one per post handled, one for a halt.
pub fn run_lines(report: &PublishReport) -> Vec<String> {
    let mut lines = vec![format!("publish run posts={}", report.outcomes.len())];
    for (id, outcome) in &report.outcomes {
        // An id that is not a post id (it should never happen) is not echoed.
        let id = if is_post_id(id) {
            id.as_str()
        } else {
            "invalid"
        };
        let mut line = format!("publish post={id} outcome={}", outcome.code());
        if let Some(reason) = outcome.reason() {
            line.push_str(&format!(" reason={}", reason.code()));
        }
        let external = match outcome {
            Outcome::Published { external_id } => Some(external_id),
            Outcome::StateLost { external_id } => external_id.as_ref(),
            _ => None,
        };
        if let Some(ext) = external.filter(|e| is_valid_external_id(e)) {
            line.push_str(&format!(" external_id={ext}"));
        }
        lines.push(line);
    }
    if let Some(halt) = report.halted {
        lines.push(format!("publish halted={}", halt.code()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::publish::FailReason;
    use domain::publish_app::Halt;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn report(outcomes: Vec<(&str, Outcome)>, halted: Option<Halt>) -> PublishReport {
        PublishReport {
            outcomes: outcomes
                .into_iter()
                .map(|(i, o)| (i.to_string(), o))
                .collect(),
            halted,
        }
    }

    #[test]
    fn a_skipped_row_logs_a_fixed_line_with_its_id() {
        assert_eq!(
            skipped_row_line(ID),
            format!("publish list: skipped an unreadable row post={ID}")
        );
    }

    #[test]
    fn an_empty_run_logs_one_line() {
        assert_eq!(
            run_lines(&PublishReport::default()),
            ["publish run posts=0"]
        );
    }

    #[test]
    fn a_published_post_logs_its_id_and_the_validated_external_id() {
        let lines = run_lines(&report(
            vec![(
                ID,
                Outcome::Published {
                    external_id: "dQw4w9WgXcQ".into(),
                },
            )],
            None,
        ));
        assert_eq!(
            lines,
            [
                "publish run posts=1".to_string(),
                format!("publish post={ID} outcome=published external_id=dQw4w9WgXcQ")
            ]
        );
    }

    #[test]
    fn failures_and_retries_log_the_reason_code() {
        let lines = run_lines(&report(
            vec![
                (ID, Outcome::Retrying(FailReason::RateLimited)),
                (ID, Outcome::Failed(FailReason::Unknown)),
                (ID, Outcome::FlaggedStuck),
            ],
            Some(Halt::RateLimited),
        ));
        assert_eq!(
            lines[1],
            format!("publish post={ID} outcome=retrying reason=rate_limited")
        );
        assert_eq!(
            lines[2],
            format!("publish post={ID} outcome=failed reason=unknown")
        );
        assert_eq!(lines[3], format!("publish post={ID} outcome=flagged_stuck"));
        assert_eq!(lines[4], "publish halted=rate_limited");
    }

    #[test]
    fn a_lost_state_write_still_reports_the_external_id_if_it_is_well_formed() {
        let ok = run_lines(&report(
            vec![(
                ID,
                Outcome::StateLost {
                    external_id: Some("abc_-123".into()),
                },
            )],
            Some(Halt::Storage),
        ));
        assert_eq!(
            ok[1],
            format!("publish post={ID} outcome=state_lost external_id=abc_-123")
        );
        assert_eq!(ok[2], "publish halted=storage");
    }

    #[test]
    fn nothing_unchecked_reaches_a_log_line() {
        let evil = "https://x/?sig=SECRET token=ya29";
        let lines = run_lines(&report(
            vec![
                (
                    evil,
                    Outcome::Published {
                        external_id: evil.into(),
                    },
                ),
                (
                    ID,
                    Outcome::StateLost {
                        external_id: Some(evil.into()),
                    },
                ),
            ],
            None,
        ));
        let all = lines.join("\n");
        assert!(
            !all.contains("SECRET") && !all.contains("ya29") && !all.contains("https"),
            "{all}"
        );
        assert_eq!(lines[1], "publish post=invalid outcome=published");
        assert_eq!(lines[2], format!("publish post={ID} outcome=state_lost"));
        assert!(!is_valid_external_id(evil));
    }

    /// Publishing ships switched off, private, and on a five minute cron.
    #[test]
    fn wrangler_toml_ships_publishing_off_private_and_on_a_cron() {
        let toml = include_str!("../../../wrangler.toml");
        let lines: Vec<&str> = toml.lines().map(str::trim).collect();
        assert!(
            lines.contains(&"PUBLISH_ENABLED = \"false\""),
            "PUBLISH_ENABLED default"
        );
        assert!(
            lines.contains(&"PUBLISH_PRIVACY = \"private\""),
            "PUBLISH_PRIVACY default"
        );
        assert!(lines.contains(&"[triggers]"));
        assert!(lines.contains(&"crons = [\"*/5 * * * *\"]"));
        assert!(!toml.contains("PUBLISH_ENABLED = \"true\""));
    }

    #[test]
    fn the_fixed_lines_are_fixed() {
        assert_eq!(DISABLED_LINE, "publish disabled");
        assert_eq!(failure_line(RunFailure::Setup), "publish run failed: setup");
        assert_eq!(failure_line(RunFailure::List), "publish run failed: list");
    }
}
