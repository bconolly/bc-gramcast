//! Weekly posting slots. Pure maths on Unix seconds: no clock, no chrono.

use crate::queue::MAX_SLOT_SECS;

pub const MAX_RULES: usize = 14;
pub const MAX_NEXT_SLOTS: usize = 20;
const MIN_OFFSET: i32 = -720;
const MAX_OFFSET: i32 = 840;
const MINUTES_PER_DAY: u16 = 1440;
const DAY_SECS: i64 = 86_400;
const LOOKAHEAD_DAYS: i64 = 52 * 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    pub const ALL: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Weekday::Mon => "mon",
            Weekday::Tue => "tue",
            Weekday::Wed => "wed",
            Weekday::Thu => "thu",
            Weekday::Fri => "fri",
            Weekday::Sat => "sat",
            Weekday::Sun => "sun",
        }
    }

    /// Lowercase three-letter names only.
    pub fn parse(s: &str) -> Option<Weekday> {
        Self::ALL.into_iter().find(|d| d.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SlotRule {
    pub weekday: Weekday,
    pub minute_of_day: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotPattern {
    pub utc_offset_minutes: i32,
    pub rules: Vec<SlotRule>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SlotError {
    OffsetOutOfRange,
    TooManyRules,
    DuplicateRule,
    /// A time of day or weekday that does not parse or is out of range.
    BadTime,
}

impl SlotPattern {
    pub fn new(utc_offset_minutes: i32, mut rules: Vec<SlotRule>) -> Result<Self, SlotError> {
        if !(MIN_OFFSET..=MAX_OFFSET).contains(&utc_offset_minutes) {
            return Err(SlotError::OffsetOutOfRange);
        }
        if rules.len() > MAX_RULES {
            return Err(SlotError::TooManyRules);
        }
        if rules.iter().any(|r| r.minute_of_day >= MINUTES_PER_DAY) {
            return Err(SlotError::BadTime);
        }
        rules.sort();
        if rules.windows(2).any(|w| w[0] == w[1]) {
            return Err(SlotError::DuplicateRule);
        }
        Ok(Self {
            utc_offset_minutes,
            rules,
        })
    }

    /// Rebuilds a pattern from a stored row. Same rules as `new`.
    pub fn from_stored(utc_offset_minutes: i32, rules: &str) -> Result<Self, SlotError> {
        let parsed = if rules.is_empty() {
            Vec::new()
        } else {
            rules
                .split(',')
                .map(parse_rule)
                .collect::<Result<Vec<_>, _>>()?
        };
        Self::new(utc_offset_minutes, parsed)
    }

    /// Rebuilds a pattern from a database row, where the offset comes back as
    /// a float. A fractional or out-of-range offset is refused, never truncated.
    pub fn from_row(utc_offset_minutes: f64, rules: &str) -> Result<Self, SlotError> {
        let in_range = utc_offset_minutes.is_finite()
            && utc_offset_minutes.fract() == 0.0
            && utc_offset_minutes >= f64::from(MIN_OFFSET)
            && utc_offset_minutes <= f64::from(MAX_OFFSET);
        if !in_range {
            return Err(SlotError::OffsetOutOfRange);
        }
        Self::from_stored(utc_offset_minutes as i32, rules)
    }

    /// Compact text for storage, e.g. `mon@06:00,wed@19:00`. Empty for none.
    pub fn rules_to_stored(&self) -> String {
        let parts: Vec<String> = self
            .rules
            .iter()
            .map(|r| format!("{}@{}", r.weekday.as_str(), format_hhmm(r.minute_of_day)))
            .collect();
        parts.join(",")
    }
}

fn parse_rule(text: &str) -> Result<SlotRule, SlotError> {
    let (day, time) = text.split_once('@').ok_or(SlotError::BadTime)?;
    Ok(SlotRule {
        weekday: Weekday::parse(day).ok_or(SlotError::BadTime)?,
        minute_of_day: parse_hhmm(time)?,
    })
}

/// Parses strictly two-digit 24h `HH:MM` into minutes since midnight.
pub fn parse_hhmm(s: &str) -> Result<u16, SlotError> {
    let b = s.as_bytes();
    let ok_shape =
        b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit());
    if !ok_shape {
        return Err(SlotError::BadTime);
    }
    let two = |i: usize| u16::from(b[i] - b'0') * 10 + u16::from(b[i + 1] - b'0');
    let (h, m) = (two(0), two(3));
    if h >= 24 || m >= 60 {
        return Err(SlotError::BadTime);
    }
    Ok(h * 60 + m)
}

pub fn format_hhmm(minute_of_day: u16) -> String {
    format!("{:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
}

/// The next free slots after `now`, as UTC Unix seconds in ascending order.
/// Skips anything in `taken`, looks at most 52 weeks ahead and never passes
/// `MAX_SLOT_SECS`. A `now` outside `0..MAX_SLOT_SECS` gives nothing, which
/// matches `validate_slot`'s bounds. Returns at most `min(count, MAX_NEXT_SLOTS)`.
pub fn next_free_slots(pattern: &SlotPattern, taken: &[i64], now: i64, count: usize) -> Vec<i64> {
    let want = count.min(MAX_NEXT_SLOTS);
    let mut out = Vec::with_capacity(want);
    if want == 0 || pattern.rules.is_empty() || !(0..MAX_SLOT_SECS).contains(&now) {
        return out;
    }
    let offset_secs = i64::from(pattern.utc_offset_minutes) * 60;
    // Whole days since the epoch, in the pattern's local time. 1970-01-01 was
    // a Thursday, so day 0 is index 3 counting from Monday.
    let first_day = now.saturating_add(offset_secs).div_euclid(DAY_SECS);
    let horizon = now
        .saturating_add(LOOKAHEAD_DAYS * DAY_SECS)
        .min(MAX_SLOT_SECS);
    for day in first_day..first_day + LOOKAHEAD_DAYS + 1 {
        let weekday = Weekday::ALL[(day + 3).rem_euclid(7) as usize];
        // Rules are sorted by time within a weekday, so output stays ascending.
        for rule in pattern.rules.iter().filter(|r| r.weekday == weekday) {
            let at = day * DAY_SECS + i64::from(rule.minute_of_day) * 60 - offset_secs;
            if at > now && at <= horizon && !taken.contains(&at) {
                out.push(at);
                if out.len() == want {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(weekday: Weekday, h: u16, m: u16) -> SlotRule {
        SlotRule {
            weekday,
            minute_of_day: h * 60 + m,
        }
    }

    #[test]
    fn new_sorts_rules_by_weekday_then_time() {
        let p = SlotPattern::new(
            600,
            vec![
                r(Weekday::Fri, 6, 0),
                r(Weekday::Wed, 19, 0),
                r(Weekday::Mon, 6, 0),
                r(Weekday::Mon, 5, 0),
            ],
        )
        .unwrap();
        assert_eq!(
            p.rules,
            vec![
                r(Weekday::Mon, 5, 0),
                r(Weekday::Mon, 6, 0),
                r(Weekday::Wed, 19, 0),
                r(Weekday::Fri, 6, 0)
            ]
        );
    }

    #[test]
    fn new_checks_offset_bounds() {
        assert!(SlotPattern::new(-720, vec![]).is_ok());
        assert!(SlotPattern::new(840, vec![]).is_ok());
        assert_eq!(
            SlotPattern::new(-721, vec![]),
            Err(SlotError::OffsetOutOfRange)
        );
        assert_eq!(
            SlotPattern::new(841, vec![]),
            Err(SlotError::OffsetOutOfRange)
        );
    }

    #[test]
    fn new_limits_rule_count_to_fourteen() {
        let ok: Vec<_> = (0..14).map(|i| r(Weekday::Mon, 0, i)).collect();
        assert!(SlotPattern::new(0, ok).is_ok());
        let many: Vec<_> = (0..15).map(|i| r(Weekday::Mon, 0, i)).collect();
        assert_eq!(SlotPattern::new(0, many), Err(SlotError::TooManyRules));
    }

    #[test]
    fn new_rejects_duplicates_and_bad_times() {
        let dup = vec![r(Weekday::Mon, 6, 0), r(Weekday::Mon, 6, 0)];
        assert_eq!(SlotPattern::new(0, dup), Err(SlotError::DuplicateRule));
        let bad = vec![SlotRule {
            weekday: Weekday::Mon,
            minute_of_day: 1440,
        }];
        assert_eq!(SlotPattern::new(0, bad), Err(SlotError::BadTime));
        assert!(SlotPattern::new(0, vec![r(Weekday::Sun, 23, 59)]).is_ok());
    }

    #[test]
    fn hhmm_is_strict() {
        assert_eq!(parse_hhmm("00:00"), Ok(0));
        assert_eq!(parse_hhmm("06:05"), Ok(365));
        assert_eq!(parse_hhmm("23:59"), Ok(1439));
        for bad in [
            "24:00",
            "12:60",
            "6:00",
            "06:0",
            "0600",
            "06:00:00",
            "+6:00",
            "06:+5",
            " 6:00",
            "06 :00",
            "",
            "ab:cd",
            "-1:00",
            "06.00",
            "١٢:٠٠",
        ] {
            assert_eq!(parse_hhmm(bad), Err(SlotError::BadTime), "{bad:?}");
        }
        assert_eq!(format_hhmm(365), "06:05");
        assert_eq!(format_hhmm(1439), "23:59");
    }

    #[test]
    fn weekday_names_round_trip() {
        for d in Weekday::ALL {
            assert_eq!(Weekday::parse(d.as_str()), Some(d));
        }
        assert_eq!(Weekday::parse("Mon"), None);
        assert_eq!(Weekday::parse("monday"), None);
        assert_eq!(Weekday::parse(""), None);
    }

    #[test]
    fn stored_text_round_trips_and_is_sorted() {
        let p = SlotPattern::new(600, vec![r(Weekday::Wed, 19, 0), r(Weekday::Mon, 6, 0)]).unwrap();
        assert_eq!(p.rules_to_stored(), "mon@06:00,wed@19:00");
        assert_eq!(SlotPattern::from_stored(600, "mon@06:00,wed@19:00"), Ok(p));
        let empty = SlotPattern::new(0, vec![]).unwrap();
        assert_eq!(empty.rules_to_stored(), "");
        assert_eq!(SlotPattern::from_stored(0, ""), Ok(empty));
    }

    #[test]
    fn bad_stored_rows_fail() {
        for bad in [
            "mon@6:00",
            "xyz@06:00",
            "mon06:00",
            "mon@06:00,",
            ",",
            "mon@06:00,mon@06:00",
            "mon@24:00",
        ] {
            assert!(SlotPattern::from_stored(0, bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            SlotPattern::from_stored(900, ""),
            Err(SlotError::OffsetOutOfRange)
        );
    }

    #[test]
    fn from_row_rejects_non_integer_offsets_instead_of_truncating() {
        let ok = SlotPattern::from_row(600.0, "mon@06:00").unwrap();
        assert_eq!(ok.utc_offset_minutes, 600);
        for bad in [1.5, -0.5, 600.000_001, f64::NAN, f64::INFINITY, 1e12] {
            assert_eq!(
                SlotPattern::from_row(bad, ""),
                Err(SlotError::OffsetOutOfRange),
                "{bad}"
            );
        }
        assert_eq!(
            SlotPattern::from_row(900.0, ""),
            Err(SlotError::OffsetOutOfRange)
        );
    }

    /// Unix seconds for a UTC date and time (days-from-civil, test-only).
    fn ts(y: i64, mo: i64, d: i64, h: i64, mi: i64) -> i64 {
        let y = if mo <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        days * 86_400 + h * 3600 + mi * 60
    }

    fn pat(offset: i32, rules: Vec<SlotRule>) -> SlotPattern {
        SlotPattern::new(offset, rules).unwrap()
    }

    fn mon_wed() -> SlotPattern {
        pat(0, vec![r(Weekday::Mon, 6, 0), r(Weekday::Wed, 19, 0)])
    }

    // 2024-01-01 was a Monday.
    #[test]
    fn lists_upcoming_slots_in_order() {
        let now = ts(2024, 1, 2, 12, 0); // Tuesday
        assert_eq!(
            next_free_slots(&mon_wed(), &[], now, 3),
            vec![
                ts(2024, 1, 3, 19, 0),
                ts(2024, 1, 8, 6, 0),
                ts(2024, 1, 10, 19, 0)
            ]
        );
    }

    #[test]
    fn crosses_the_week_boundary() {
        let now = ts(2024, 1, 7, 23, 0); // Sunday night
        assert_eq!(
            next_free_slots(&pat(0, vec![r(Weekday::Mon, 6, 0)]), &[], now, 1),
            vec![ts(2024, 1, 8, 6, 0)]
        );
    }

    #[test]
    fn a_rule_earlier_today_waits_for_next_week() {
        let now = ts(2024, 1, 1, 7, 0); // Monday 07:00, rule was 06:00
        assert_eq!(
            next_free_slots(&pat(0, vec![r(Weekday::Mon, 6, 0)]), &[], now, 1),
            vec![ts(2024, 1, 8, 6, 0)]
        );
    }

    #[test]
    fn a_rule_later_today_is_included() {
        let now = ts(2024, 1, 1, 5, 0);
        assert_eq!(
            next_free_slots(&pat(0, vec![r(Weekday::Mon, 6, 0)]), &[], now, 1),
            vec![ts(2024, 1, 1, 6, 0)]
        );
    }

    #[test]
    fn a_slot_exactly_at_now_is_excluded() {
        let now = ts(2024, 1, 1, 6, 0);
        assert_eq!(
            next_free_slots(&pat(0, vec![r(Weekday::Mon, 6, 0)]), &[], now, 1),
            vec![ts(2024, 1, 8, 6, 0)]
        );
    }

    #[test]
    fn taken_slots_are_skipped() {
        let now = ts(2024, 1, 2, 12, 0);
        let taken = [ts(2024, 1, 3, 19, 0), ts(2024, 1, 10, 19, 0), 5];
        assert_eq!(
            next_free_slots(&mon_wed(), &taken, now, 3),
            vec![
                ts(2024, 1, 8, 6, 0),
                ts(2024, 1, 15, 6, 0),
                ts(2024, 1, 17, 19, 0)
            ]
        );
    }

    #[test]
    fn positive_offset_can_move_the_slot_to_the_previous_utc_day() {
        // Brisbane (+10:00) Monday 06:00 is Sunday 20:00 UTC.
        let now = ts(2024, 1, 6, 0, 0);
        assert_eq!(
            next_free_slots(&pat(600, vec![r(Weekday::Mon, 6, 0)]), &[], now, 2),
            vec![ts(2024, 1, 7, 20, 0), ts(2024, 1, 14, 20, 0)]
        );
    }

    #[test]
    fn negative_offset_can_move_the_slot_to_the_next_utc_day() {
        // UTC-05:00 Sunday 22:00 is Monday 03:00 UTC.
        let now = ts(2024, 1, 1, 0, 0);
        assert_eq!(
            next_free_slots(&pat(-300, vec![r(Weekday::Sun, 22, 0)]), &[], now, 2),
            vec![ts(2024, 1, 1, 3, 0), ts(2024, 1, 8, 3, 0)]
        );
    }

    #[test]
    fn handles_a_leap_day() {
        // 2024-02-29 is a Thursday.
        let now = ts(2024, 2, 28, 0, 0);
        assert_eq!(
            next_free_slots(&pat(0, vec![r(Weekday::Thu, 12, 0)]), &[], now, 2),
            vec![ts(2024, 2, 29, 12, 0), ts(2024, 3, 7, 12, 0)]
        );
    }

    #[test]
    fn nothing_before_the_epoch_and_the_epoch_thursday_is_not_after_now() {
        // 1970-01-01 was a Thursday.
        let p = pat(0, vec![r(Weekday::Thu, 0, 0)]);
        assert!(next_free_slots(&p, &[], -1, 1).is_empty());
        assert_eq!(next_free_slots(&p, &[], 0, 1), vec![604_800]);
    }

    #[test]
    fn count_zero_and_no_rules_give_nothing() {
        assert!(next_free_slots(&mon_wed(), &[], 0, 0).is_empty());
        assert!(next_free_slots(&pat(0, vec![]), &[], 0, 5).is_empty());
    }

    #[test]
    fn count_is_capped_at_twenty() {
        let all: Vec<_> = Weekday::ALL.iter().map(|d| r(*d, 12, 0)).collect();
        let got = next_free_slots(&pat(0, all), &[], 0, 1000);
        assert_eq!(got.len(), MAX_NEXT_SLOTS);
        assert!(got.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn looks_ahead_at_most_52_weeks() {
        let p = pat(0, vec![r(Weekday::Thu, 0, 0)]);
        let taken: Vec<i64> = (0..60).map(|w| 604_800 * (w + 1)).collect();
        // Weeks 1..=52 are taken, so only 53 and later remain: out of reach.
        assert!(next_free_slots(&p, &taken, 0, 5).is_empty());
        let got = next_free_slots(&p, &taken[..10], 0, 3);
        assert_eq!(got, vec![604_800 * 11, 604_800 * 12, 604_800 * 13]);
    }

    fn every_day_noon() -> SlotPattern {
        pat(0, Weekday::ALL.iter().map(|d| r(*d, 12, 0)).collect())
    }

    #[test]
    fn never_passes_the_queue_ceiling() {
        // MAX_SLOT_SECS is Fri 2100-01-01 00:00 UTC, so three days before is
        // Tuesday 00:00. Fri 12:00 is past the ceiling and is left out.
        let got = next_free_slots(&every_day_noon(), &[], MAX_SLOT_SECS - 3 * 86_400, 20);
        assert_eq!(
            got,
            vec![
                MAX_SLOT_SECS - 216_000,
                MAX_SLOT_SECS - 129_600,
                MAX_SLOT_SECS - 43_200
            ]
        );
    }

    #[test]
    fn a_slot_exactly_on_the_ceiling_is_allowed() {
        let p = pat(0, vec![r(Weekday::Fri, 0, 0)]);
        assert_eq!(
            next_free_slots(&p, &[], MAX_SLOT_SECS - 86_400, 5),
            vec![MAX_SLOT_SECS]
        );
    }

    #[test]
    fn extreme_now_values_are_safe() {
        assert!(next_free_slots(&mon_wed(), &[], i64::MAX, 5).is_empty());
        let wide = pat(840, Weekday::ALL.iter().map(|d| r(*d, 0, 0)).collect());
        assert!(next_free_slots(&wide, &[], i64::MIN, 5).is_empty());
        assert!(next_free_slots(&wide, &[], i64::MIN + 1, 5).is_empty());
        assert!(next_free_slots(&wide, &[], -1, 5).is_empty());
    }

    // Expected values below were computed in Python with datetime and fixed
    // offsets, from now = 2024-03-01 00:00 UTC (1_709_251_200).
    const MARCH_FIRST: i64 = 1_709_251_200;

    #[test]
    fn extreme_positive_offset_plus_840() {
        // Saturday 08:00 at +14:00 is Friday 18:00 UTC.
        let p = pat(840, vec![r(Weekday::Sat, 8, 0)]);
        assert_eq!(
            next_free_slots(&p, &[], MARCH_FIRST, 2),
            vec![1_709_316_000, 1_709_920_800]
        );
    }

    #[test]
    fn extreme_negative_offset_minus_720() {
        // Wednesday 20:30 at -12:00 is Thursday 08:30 UTC.
        let p = pat(-720, vec![r(Weekday::Wed, 20, 30)]);
        assert_eq!(
            next_free_slots(&p, &[], MARCH_FIRST, 2),
            vec![1_709_800_200, 1_710_405_000]
        );
    }

    #[test]
    fn half_hour_offset_plus_570() {
        // Friday 19:15 at +09:30 is Friday 09:45 UTC.
        let p = pat(570, vec![r(Weekday::Fri, 19, 15)]);
        assert_eq!(
            next_free_slots(&p, &[], MARCH_FIRST, 2),
            vec![1_709_286_300, 1_709_891_100]
        );
    }
}
