//! 5-field cron expression parsing and evaluation.
//!
//! The syntax is the canonical `minute hour day-of-month month day-of-week`
//! form with no predefined macros (yet). Each field accepts a literal value,
//! a comma-separated list, a range like `9-17`, the wildcard `*`, and a
//! `*/N` (or `a-b/N`) step.

use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike};

/// Upper bound on the minute-by-minute search in [`CronExpr::next`]: one leap
/// year of minutes, after which the expression is declared unsatisfiable.
const MAX_SEARCH_MINUTES: u32 = 366 * 24 * 60;

/// Error returned by [`parse_cron`] when an expression is malformed.
///
/// Messages mirror the Go port (`"minute: bad value \"60\""`,
/// `"cron: want 5 fields, got 4 (\"* * * *\")"`, …) so logs read the same
/// across ports.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CronError {
    /// The expression did not have exactly five whitespace-separated fields.
    #[error("cron: want 5 fields, got {got} ({expr:?})")]
    FieldCount {
        /// Number of fields actually found.
        got: usize,
        /// The offending expression, verbatim.
        expr: String,
    },
    /// A `/N` step suffix was not a positive integer.
    #[error("{field}: bad step {part:?}")]
    BadStep {
        /// Which field failed: `minute`, `hour`, `dom`, `month`, or `dow`.
        field: &'static str,
        /// The offending field part, including the step suffix.
        part: String,
    },
    /// An `a-b` range was unparsable, out of bounds, or inverted.
    #[error("{field}: bad range {part:?}")]
    BadRange {
        /// Which field failed: `minute`, `hour`, `dom`, `month`, or `dow`.
        field: &'static str,
        /// The offending range, with any step suffix stripped.
        part: String,
    },
    /// A literal value was unparsable or out of bounds.
    #[error("{field}: bad value {part:?}")]
    BadValue {
        /// Which field failed: `minute`, `hour`, `dom`, `month`, or `dow`.
        field: &'static str,
        /// The offending value, with any step suffix stripped.
        part: String,
    },
}

/// A parsed 5-field cron expression
/// (`minute hour day-of-month month day-of-week`).
///
/// Each field is the sorted, deduplicated set of values the field matches.
/// Day-of-week uses `0` = Sunday through `6` = Saturday.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    /// Matching minutes, `0`–`59`.
    pub minute: Vec<u32>,
    /// Matching hours, `0`–`23`.
    pub hour: Vec<u32>,
    /// Matching days of the month, `1`–`31`.
    pub day_of_month: Vec<u32>,
    /// Matching months, `1`–`12`.
    pub month: Vec<u32>,
    /// Matching days of the week, `0` (Sunday) – `6` (Saturday).
    pub day_of_week: Vec<u32>,
}

/// Parses `expr` into a [`CronExpr`] using the canonical 5-field syntax.
///
/// Each field accepts: a comma-separated list of values, the wildcard `*`,
/// a range like `9-17`, and a `*/N` step. No predefined macros yet.
///
/// ```
/// let c = firefly_scheduling::parse_cron("*/15 * * * *").unwrap();
/// assert_eq!(c.minute, vec![0, 15, 30, 45]);
/// ```
pub fn parse_cron(expr: &str) -> Result<CronExpr, CronError> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(CronError::FieldCount {
            got: fields.len(),
            expr: expr.to_string(),
        });
    }
    Ok(CronExpr {
        minute: parse_field(fields[0], 0, 59, "minute")?,
        hour: parse_field(fields[1], 0, 23, "hour")?,
        day_of_month: parse_field(fields[2], 1, 31, "dom")?,
        month: parse_field(fields[3], 1, 12, "month")?,
        day_of_week: parse_field(fields[4], 0, 6, "dow")?,
    })
}

impl CronExpr {
    /// Parses `expr` — equivalent to the free function [`parse_cron`].
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        parse_cron(expr)
    }

    /// Returns the next time strictly after `from` (rounded up to a whole
    /// minute) that matches the expression, or `None` when no match exists
    /// within the next 366 days — the unsatisfiable-expression guard.
    ///
    /// Falls forward one minute at a time — adequate for cron-resolution
    /// scheduling. Calendar fields are evaluated in `from`'s own timezone.
    pub fn next<Tz: TimeZone>(&self, from: DateTime<Tz>) -> Option<DateTime<Tz>> {
        // Round up to the next whole minute strictly after `from`.
        let mut t = from
            .with_second(0)?
            .with_nanosecond(0)?
            .checked_add_signed(Duration::minutes(1))?;
        for _ in 0..MAX_SEARCH_MINUTES {
            if self.matches(&t) {
                return Some(t);
            }
            t = t.checked_add_signed(Duration::minutes(1))?;
        }
        None
    }

    /// Reports whether `t` matches every field of the expression.
    ///
    /// Cron semantics: when both day-of-month and day-of-week are restricted
    /// (neither is `*`-equivalent), the rule fires when EITHER matches —
    /// Vixie cron behaviour.
    fn matches<Tz: TimeZone>(&self, t: &DateTime<Tz>) -> bool {
        if !self.minute.contains(&t.minute()) {
            return false;
        }
        if !self.hour.contains(&t.hour()) {
            return false;
        }
        if !self.month.contains(&t.month()) {
            return false;
        }
        let dom = self.day_of_month.contains(&t.day());
        let dow = self
            .day_of_week
            .contains(&t.weekday().num_days_from_sunday());
        if is_restricted(&self.day_of_month, 1, 31) && is_restricted(&self.day_of_week, 0, 6) {
            return dom || dow;
        }
        dom && dow
    }
}

impl FromStr for CronExpr {
    type Err = CronError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_cron(s)
    }
}

/// Parses one cron field into the sorted set of values it matches.
fn parse_field(field: &str, lo: u32, hi: u32, name: &'static str) -> Result<Vec<u32>, CronError> {
    let mut out: BTreeSet<u32> = BTreeSet::new();
    for raw in field.split(',') {
        let mut part = raw;
        let mut step: u32 = 1;
        if let Some(i) = raw.find('/') {
            let s: i64 = raw[i + 1..].parse().map_err(|_| CronError::BadStep {
                field: name,
                part: raw.to_string(),
            })?;
            if s <= 0 {
                return Err(CronError::BadStep {
                    field: name,
                    part: raw.to_string(),
                });
            }
            step = u32::try_from(s).unwrap_or(u32::MAX);
            part = &raw[..i];
        }
        if part == "*" {
            let mut v = lo;
            while v <= hi {
                out.insert(v);
                match v.checked_add(step) {
                    Some(n) => v = n,
                    None => break,
                }
            }
        } else if part.contains('-') {
            let (a_raw, b_raw) = part.split_once('-').unwrap_or((part, ""));
            let a: i64 = a_raw.parse().map_err(|_| CronError::BadRange {
                field: name,
                part: part.to_string(),
            })?;
            let b: i64 = b_raw.parse().map_err(|_| CronError::BadRange {
                field: name,
                part: part.to_string(),
            })?;
            if a < i64::from(lo) || b > i64::from(hi) || a > b {
                return Err(CronError::BadRange {
                    field: name,
                    part: part.to_string(),
                });
            }
            let (a, b) = (a as u32, b as u32);
            let mut v = a;
            while v <= b {
                out.insert(v);
                match v.checked_add(step) {
                    Some(n) => v = n,
                    None => break,
                }
            }
        } else {
            let v: i64 = part.parse().map_err(|_| CronError::BadValue {
                field: name,
                part: part.to_string(),
            })?;
            if v < i64::from(lo) || v > i64::from(hi) {
                return Err(CronError::BadValue {
                    field: name,
                    part: part.to_string(),
                });
            }
            out.insert(v as u32);
        }
    }
    Ok(out.into_iter().collect())
}

/// Reports whether a field set is "restricted" — i.e. narrower than the full
/// `lo`–`hi` domain a wildcard would expand to.
fn is_restricted(xs: &[u32], lo: u32, hi: u32) -> bool {
    xs.len() != (hi - lo + 1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    // Port of Go TestParseCron.
    #[test]
    fn parse_cron_basics() {
        let c = parse_cron("0 9 * * 1-5").unwrap();
        assert_eq!(c.minute[0], 0);
        assert_eq!(c.hour[0], 9);
        // Monday 09:00.
        let from = utc(2026, 5, 4, 8, 30, 0); // Monday
        let next = c.next(from).unwrap();
        assert_eq!(next.hour(), 9);
        assert_eq!(next.minute(), 0);
        assert_eq!(next, utc(2026, 5, 4, 9, 0, 0));
    }

    // Port of Go TestCronStepAndRange.
    #[test]
    fn cron_step_and_range() {
        let c = parse_cron("*/15 * * * *").unwrap();
        assert_eq!(c.minute.len(), 4);
        assert_eq!(c.minute, vec![0, 15, 30, 45]);
    }

    // Port of Go TestCronInvalid.
    #[test]
    fn cron_invalid() {
        assert!(parse_cron("60 * * * *").is_err());
        assert!(parse_cron("a b c d e").is_err());
    }

    #[test]
    fn cron_list_and_range_with_step() {
        let c = parse_cron("0,15,30,45 9-17/2 * * *").unwrap();
        assert_eq!(c.minute, vec![0, 15, 30, 45]);
        assert_eq!(c.hour, vec![9, 11, 13, 15, 17]);
    }

    #[test]
    fn cron_field_count_error() {
        let err = parse_cron("* * * *").unwrap_err();
        assert_eq!(
            err,
            CronError::FieldCount {
                got: 4,
                expr: "* * * *".into()
            }
        );
        assert_eq!(err.to_string(), r#"cron: want 5 fields, got 4 ("* * * *")"#);
    }

    #[test]
    fn cron_error_messages_match_go() {
        let err = parse_cron("60 * * * *").unwrap_err();
        assert_eq!(err.to_string(), r#"minute: bad value "60""#);

        let err = parse_cron("* */0 * * *").unwrap_err();
        assert_eq!(err.to_string(), r#"hour: bad step "*/0""#);

        let err = parse_cron("* * 17-9 * *").unwrap_err();
        assert_eq!(err.to_string(), r#"dom: bad range "17-9""#);

        let err = parse_cron("* * * * 0-9").unwrap_err();
        assert_eq!(err.to_string(), r#"dow: bad range "0-9""#);
    }

    // Vixie cron: when both dom and dow are restricted, EITHER matching fires.
    #[test]
    fn cron_dom_dow_either_semantics() {
        let c = parse_cron("0 0 15 * 1").unwrap();
        // From Tuesday 2026-05-12: Friday May 15 (dom) precedes Monday May 18 (dow).
        let next = c.next(utc(2026, 5, 12, 5, 0, 0)).unwrap();
        assert_eq!(next, utc(2026, 5, 15, 0, 0, 0));

        // With dow alone restricted, only Mondays fire.
        let c = parse_cron("0 0 * * 1").unwrap();
        let next = c.next(utc(2026, 5, 12, 5, 0, 0)).unwrap();
        assert_eq!(next, utc(2026, 5, 18, 0, 0, 0));
    }

    #[test]
    fn cron_weekday_skips_weekend() {
        let c = parse_cron("0 9 * * 1-5").unwrap();
        // Saturday 2026-05-02 → next weekday 09:00 is Monday May 4.
        let next = c.next(utc(2026, 5, 2, 10, 0, 0)).unwrap();
        assert_eq!(next, utc(2026, 5, 4, 9, 0, 0));
    }

    #[test]
    fn cron_next_rolls_over_year() {
        let c = parse_cron("0 0 1 1 *").unwrap();
        let next = c.next(utc(2026, 6, 12, 10, 30, 0)).unwrap();
        assert_eq!(next, utc(2027, 1, 1, 0, 0, 0));
    }

    #[test]
    fn cron_next_is_strictly_after_from() {
        let c = parse_cron("* * * * *").unwrap();
        let from = utc(2026, 5, 4, 8, 30, 0);
        assert_eq!(c.next(from).unwrap(), utc(2026, 5, 4, 8, 31, 0));
    }

    // Go returns the zero time for unsatisfiable expressions; we return None.
    #[test]
    fn cron_next_none_when_unsatisfiable() {
        let c = parse_cron("0 0 31 2 *").unwrap(); // February 31st never exists.
        assert_eq!(c.next(utc(2026, 1, 1, 0, 0, 0)), None);
    }

    #[test]
    fn cron_from_str() {
        let c: CronExpr = "30 2 * * *".parse().unwrap();
        assert_eq!(c.minute, vec![30]);
        assert_eq!(c.hour, vec![2]);
        assert!("nope".parse::<CronExpr>().is_err());
    }

    #[test]
    fn cron_literal_with_step_ignores_step() {
        // Mirrors Go: a step suffix on a literal is parsed but has no effect.
        let c = parse_cron("5/2 * * * *").unwrap();
        assert_eq!(c.minute, vec![5]);
    }

    #[test]
    fn cron_negative_value_rejected() {
        // "-5" splits into an empty lower bound → bad range, as in Go.
        let err = parse_cron("-5 * * * *").unwrap_err();
        assert!(matches!(err, CronError::BadRange { .. }));
    }
}
