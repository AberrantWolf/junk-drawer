//! RFC3339 timestamps, written in-house (spec Appendix B rejects chrono/time).
//! We emit second-precision UTC only ("2026-07-03T10:22:00Z"); we accept
//! fractional seconds and numeric offsets on parse and normalize to UTC.

/// Milliseconds since Unix epoch, always UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Timestamp(pub i64);

#[derive(Debug, PartialEq, Eq)]
pub enum TimeError {
    TooShort,
    BadSeparator,
    BadDigit,
    BadDate,
    BadTime,
    BadOffset,
    TrailingGarbage,
}

/// Days since 1970-01-01 for a civil date (Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11], March-based
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date for days since 1970-01-01 (Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Parse exactly `n` ASCII digits at `b[at..at+n]`.
fn digits(b: &[u8], at: usize, n: usize) -> Result<i64, TimeError> {
    let slice = b.get(at..at + n).ok_or(TimeError::TooShort)?;
    let mut v: i64 = 0;
    for &c in slice {
        if !c.is_ascii_digit() {
            return Err(TimeError::BadDigit);
        }
        v = v * 10 + (c - b'0') as i64;
    }
    Ok(v)
}

impl Timestamp {
    pub fn now() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before 1970");
        Timestamp(d.as_millis() as i64)
    }

    pub fn parse_rfc3339(s: &str) -> Result<Self, TimeError> {
        let b = s.as_bytes();
        if b.len() < 20 {
            return Err(TimeError::TooShort);
        }
        let sep_ok = b[4] == b'-'
            && b[7] == b'-'
            && (b[10] == b'T' || b[10] == b't' || b[10] == b' ')
            && b[13] == b':'
            && b[16] == b':';
        if !sep_ok {
            return Err(TimeError::BadSeparator);
        }
        let y = digits(b, 0, 4)?;
        let m = digits(b, 5, 2)? as u32;
        let d = digits(b, 8, 2)? as u32;
        let h = digits(b, 11, 2)?;
        let min = digits(b, 14, 2)?;
        let mut sec = digits(b, 17, 2)?;
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
            return Err(TimeError::BadDate);
        }
        if h > 23 || min > 59 || sec > 60 {
            return Err(TimeError::BadTime);
        }
        if sec == 60 {
            sec = 59; // leap second: clamp
        }

        let mut i = 19;
        let mut millis: i64 = 0;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
            if i == start {
                return Err(TimeError::BadDigit);
            }
            // take the first 3 fraction digits as millis, right-padding
            let frac = &b[start..(start + 3).min(i)];
            let mut v: i64 = 0;
            for &c in frac {
                v = v * 10 + (c - b'0') as i64;
            }
            for _ in frac.len()..3 {
                v *= 10;
            }
            millis = v;
        }

        let offset_secs: i64 = match b.get(i) {
            Some(&b'Z') | Some(&b'z') => {
                i += 1;
                0
            }
            Some(&sign) if sign == b'+' || sign == b'-' => {
                let oh = digits(b, i + 1, 2)?;
                if b.get(i + 3) != Some(&b':') {
                    return Err(TimeError::BadOffset);
                }
                let om = digits(b, i + 4, 2)?;
                if oh > 23 || om > 59 {
                    return Err(TimeError::BadOffset);
                }
                i += 6;
                let secs = oh * 3600 + om * 60;
                if sign == b'+' { secs } else { -secs }
            }
            _ => return Err(TimeError::BadOffset),
        };
        if i != b.len() {
            return Err(TimeError::TrailingGarbage);
        }

        let day_secs = h * 3600 + min * 60 + sec;
        let epoch_secs = days_from_civil(y, m, d) * 86_400 + day_secs - offset_secs;
        Ok(Timestamp(epoch_secs * 1000 + millis))
    }

    /// Canonical form: second precision, UTC, uppercase Z. Millis are truncated.
    pub fn to_rfc3339(&self) -> String {
        let secs = self.0.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            y,
            m,
            d,
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    pub fn days_since(&self, other: Timestamp) -> f64 {
        (self.0 - other.0) as f64 / 86_400_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero() {
        assert_eq!(Timestamp(0).to_rfc3339(), "1970-01-01T00:00:00Z");
        assert_eq!(
            Timestamp::parse_rfc3339("1970-01-01T00:00:00Z").unwrap(),
            Timestamp(0)
        );
    }

    #[test]
    fn known_value_y2k() {
        // 2000-01-01T00:00:00Z = 946_684_800 seconds after epoch (well-known constant)
        assert_eq!(
            Timestamp::parse_rfc3339("2000-01-01T00:00:00Z").unwrap(),
            Timestamp(946_684_800_000)
        );
        assert_eq!(
            Timestamp(946_684_800_000).to_rfc3339(),
            "2000-01-01T00:00:00Z"
        );
    }

    #[test]
    fn canonical_strings_round_trip() {
        for s in [
            "2026-07-03T10:22:00Z",
            "1999-12-31T23:59:59Z",
            "2024-02-29T12:00:00Z", // leap day
            "2100-01-01T00:00:00Z",
            "1969-12-31T23:59:59Z", // pre-epoch (negative millis)
        ] {
            let t = Timestamp::parse_rfc3339(s).unwrap();
            assert_eq!(t.to_rfc3339(), s, "round-trip of {s}");
        }
    }

    #[test]
    fn offsets_normalize_to_utc() {
        let utc = Timestamp::parse_rfc3339("2026-07-03T10:22:00Z").unwrap();
        assert_eq!(
            Timestamp::parse_rfc3339("2026-07-03T12:22:00+02:00").unwrap(),
            utc
        );
        assert_eq!(
            Timestamp::parse_rfc3339("2026-07-03T04:52:00-05:30").unwrap(),
            utc
        );
    }

    #[test]
    fn fractional_seconds_kept_as_millis() {
        assert_eq!(
            Timestamp::parse_rfc3339("1970-01-01T00:00:00.5Z").unwrap(),
            Timestamp(500)
        );
        // truncated beyond millis, not rounded
        assert_eq!(
            Timestamp::parse_rfc3339("1970-01-01T00:00:00.1239Z").unwrap(),
            Timestamp(123)
        );
    }

    #[test]
    fn pre_epoch_fractional_millis_floor_to_earlier_second() {
        // div_euclid, not truncating division: -500 ms is inside the second
        // BEFORE the epoch. A truncating implementation renders 1970-01-01.
        assert_eq!(Timestamp(-500).to_rfc3339(), "1969-12-31T23:59:59Z");
        assert_eq!(
            Timestamp::parse_rfc3339("1969-12-31T23:59:59.5Z").unwrap(),
            Timestamp(-500)
        );
    }

    #[test]
    fn lowercase_t_and_z_accepted() {
        assert_eq!(
            Timestamp::parse_rfc3339("1970-01-01t00:00:00z").unwrap(),
            Timestamp(0)
        );
    }

    #[test]
    fn rejects_invalid() {
        for s in [
            "2023-02-29T00:00:00Z",  // not a leap year
            "2026-13-01T00:00:00Z",  // month 13
            "2026-07-32T00:00:00Z",  // day 32
            "2026-07-03T24:00:00Z",  // hour 24
            "2026-07-03T10:22:00",   // missing offset
            "2026-07-03",            // date only
            "2026-07-03T10:22:00Zx", // trailing garbage
            "not a date",
            "",
        ] {
            assert!(Timestamp::parse_rfc3339(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn days_since_works() {
        let a = Timestamp::parse_rfc3339("2026-07-01T00:00:00Z").unwrap();
        let b = Timestamp::parse_rfc3339("2026-07-04T12:00:00Z").unwrap();
        assert!((b.days_since(a) - 3.5).abs() < 1e-9);
        assert!((a.days_since(b) + 3.5).abs() < 1e-9);
    }

    #[test]
    fn now_is_plausible() {
        let now = Timestamp::now();
        let floor = Timestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        let ceil = Timestamp::parse_rfc3339("2100-01-01T00:00:00Z").unwrap();
        assert!(now > floor && now < ceil);
    }
}
