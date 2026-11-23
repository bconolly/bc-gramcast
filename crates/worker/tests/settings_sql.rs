//! Runs the settings SQL against real SQLite (the `sqlite3` command), after applying
//! migrations 0001 to 0005, so the compare-and-set is shown to work and not just to
//! contain the right words. D1 is SQLite, but this is not D1 itself.
//!
//! Skipped, with a note on stderr, when `sqlite3` is not installed, except when the `CI`
//! environment variable is set: then a missing `sqlite3` fails the run, so CI cannot pass
//! silently without it (the workflow installs it).

use gramcast_worker::settings_dto::{INSERT_SQL, UPDATE_SQL};
use std::io::Write;
use std::process::{Command, Stdio};

const MIGRATIONS: [&str; 5] = [
    include_str!("../../../migrations/0001_posts.sql"),
    include_str!("../../../migrations/0002_slot_pattern.sql"),
    include_str!("../../../migrations/0003_oauth.sql"),
    include_str!("../../../migrations/0004_publish_state.sql"),
    include_str!("../../../migrations/0005_settings.sql"),
];

/// What to do when `sqlite3` cannot be started: skip off CI, fail on CI.
fn on_missing_sqlite(ci: bool) -> Option<String> {
    assert!(
        !ci,
        "sqlite3 is required when CI is set, but it was not found"
    );
    eprintln!("sqlite3 not found: skipping the settings SQL test");
    None
}

/// Runs the script in a fresh in-memory database and returns what it printed.
/// `None` means sqlite3 is not available here.
fn sqlite(script: &str) -> Option<String> {
    let child = Command::new("sqlite3")
        .args([":memory:", "-bail"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return on_missing_sqlite(std::env::var_os("CI").is_some()),
    };
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "sqlite3 failed: {} {text}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(text)
}

/// Binds ?1 to ?3 (and ?4 when given) the way the Worker does: every number a double.
fn binds(enabled: &str, privacy: &str, updated_at: &str, seen: Option<&str>) -> String {
    let mut s = format!(
        ".parameter set ?1 {enabled}\n.parameter set ?2 '{privacy}'\n.parameter set ?3 {updated_at}\n"
    );
    if let Some(v) = seen {
        s.push_str(&format!(".parameter set ?4 {v}\n"));
    }
    s
}

const ROW: &str = "SELECT publish_enabled, publish_privacy, updated_at, typeof(publish_enabled), \
    typeof(updated_at) FROM settings;\n";
const CHANGES: &str = "SELECT changes();\n";

fn script(steps: &[String]) -> String {
    let mut s = MIGRATIONS.join("\n");
    s.push('\n');
    s.push_str(&steps.concat());
    s
}

#[test]
fn insert_if_absent_changes_one_row_then_zero_and_leaves_the_row_alone() {
    let first = format!(
        "{}{INSERT_SQL};\n{CHANGES}",
        binds("1.0", "public", "100.0", None)
    );
    let second = format!(
        "{}{INSERT_SQL};\n{CHANGES}",
        binds("0.0", "private", "200.0", None)
    );
    let Some(out) = sqlite(&script(&[first, second, ROW.to_string()])) else {
        return;
    };
    // First insert: 1 row. Second ("DO NOTHING"): 0 rows. The row is still the first one.
    assert_eq!(out, "1\n0\n1|public|100|integer|integer\n");
}

#[test]
fn whole_doubles_pass_the_integer_checks_and_are_stored_as_integers() {
    let insert = format!(
        "{}{INSERT_SQL};\n",
        binds("0.0", "unlisted", "1700000000.0", None)
    );
    let Some(out) = sqlite(&script(&[insert, ROW.to_string()])) else {
        return;
    };
    assert_eq!(out, "0|unlisted|1700000000|integer|integer\n");
}

#[test]
fn a_stale_expected_time_changes_nothing_and_the_current_one_changes_one_row() {
    let first = format!("{}{INSERT_SQL};\n", binds("1.0", "public", "100.0", None));
    let stale = format!(
        "{}{UPDATE_SQL};\n{CHANGES}",
        binds("0.0", "private", "300.0", Some("99.0"))
    );
    let current = format!(
        "{}{UPDATE_SQL};\n{CHANGES}",
        binds("0.0", "private", "300.0", Some("100.0"))
    );
    let Some(out) = sqlite(&script(&[
        first,
        stale,
        ROW.to_string(),
        current,
        ROW.to_string(),
    ])) else {
        return;
    };
    assert_eq!(
        out,
        "0\n1|public|100|integer|integer\n1\n0|private|300|integer|integer\n"
    );
}

#[test]
#[should_panic(expected = "sqlite3 is required when CI is set")]
fn a_missing_sqlite3_fails_the_run_on_ci() {
    on_missing_sqlite(true);
}

#[test]
fn a_missing_sqlite3_is_skipped_off_ci() {
    assert_eq!(on_missing_sqlite(false), None);
}
