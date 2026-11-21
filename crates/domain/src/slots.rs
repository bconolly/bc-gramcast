//! Weekly posting slots. Pure maths on Unix seconds: no clock, no chrono.

pub const MAX_RULES: usize = 14;
pub const MAX_NEXT_SLOTS: usize = 20;
const MIN_OFFSET: i32 = -720;
const MAX_OFFSET: i32 = 840;
const MINUTES_PER_DAY: u16 = 1440;

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
}
