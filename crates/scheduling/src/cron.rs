//! 5/6-field cron expression parsing and evaluation.
//!
//! The base syntax is the canonical `minute hour day-of-month month
//! day-of-week` form. Each field accepts a literal value, a comma-separated
//! list, a range like `9-17`, the wildcard `*`, and a `*/N` (or `a-b/N`)
//! step. The pyfly-parity layer adds, mirroring `pyfly.scheduling.cron`:
//!
//! * Spring-style **6-field** expressions with a leading seconds field
//!   (`sec min hour dom month dow`);
//! * the Quartz `?` day placeholder (treated as `*`, in any field);
//! * the `@hourly` / `@daily` / `@weekly` / `@monthly` / `@yearly` macros
//!   (plus the conventional `@midnight` / `@annually` aliases).

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
    /// The expression did not have five (or six, seconds-first)
    /// whitespace-separated fields. The message wording is kept verbatim
    /// from the Go port for log compatibility even though six fields are
    /// now also accepted.
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
        /// Which field failed: `second`, `minute`, `hour`, `dom`, `month`,
        /// `dow`, or `macro` (an unknown `@…` shorthand).
        field: &'static str,
        /// The offending field part, including the step suffix.
        part: String,
    },
    /// An `a-b` range was unparsable, out of bounds, or inverted.
    #[error("{field}: bad range {part:?}")]
    BadRange {
        /// Which field failed: `second`, `minute`, `hour`, `dom`, `month`,
        /// `dow`, or `macro` (an unknown `@…` shorthand).
        field: &'static str,
        /// The offending range, with any step suffix stripped.
        part: String,
    },
    /// A literal value was unparsable or out of bounds.
    #[error("{field}: bad value {part:?}")]
    BadValue {
        /// Which field failed: `second`, `minute`, `hour`, `dom`, `month`,
        /// `dow`, or `macro` (an unknown `@…` shorthand).
        field: &'static str,
        /// The offending value, with any step suffix stripped.
        part: String,
    },
}

/// A parsed cron expression — canonical 5-field
/// (`minute hour day-of-month month day-of-week`) or Spring 6-field with a
/// leading seconds field (`second minute hour day-of-month month
/// day-of-week`).
///
/// Each field is the sorted, deduplicated set of values the field matches.
/// Day-of-week uses `0` = Sunday through `6` = Saturday. A 5-field
/// expression parses with `second == [0]` — it fires on the whole minute,
/// exactly as before seconds support existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    /// Matching seconds, `0`–`59`. `[0]` for 5-field expressions.
    pub second: Vec<u32>,
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

/// Expands a `@macro` shorthand to its 5-field equivalent.
fn expand_macro(expr: &str) -> Option<&'static str> {
    match expr {
        "@hourly" => Some("0 * * * *"),
        "@daily" | "@midnight" => Some("0 0 * * *"),
        "@weekly" => Some("0 0 * * 0"),
        "@monthly" => Some("0 0 1 * *"),
        "@yearly" | "@annually" => Some("0 0 1 1 *"),
        _ => None,
    }
}

/// Parses `expr` into a [`CronExpr`].
///
/// Accepted syntaxes (mirroring pyfly's `CronExpression`):
///
/// * canonical **5-field** `minute hour dom month dow`;
/// * Spring **6-field** with a leading seconds field;
/// * the Quartz `?` placeholder, treated as `*` (any field);
/// * `@hourly`, `@daily` (alias `@midnight`), `@weekly`, `@monthly`, and
///   `@yearly` (alias `@annually`) macros.
///
/// Each field accepts: a comma-separated list of values, the wildcard `*`,
/// a range like `9-17`, and a `*/N` step.
///
/// ```
/// let c = firefly_scheduling::parse_cron("*/15 * * * *").unwrap();
/// assert_eq!(c.minute, vec![0, 15, 30, 45]);
///
/// let c = firefly_scheduling::parse_cron("30 0 9 ? * *").unwrap();
/// assert_eq!(c.second, vec![30]); // Spring 6-field: seconds first
/// ```
pub fn parse_cron(expr: &str) -> Result<CronExpr, CronError> {
    let trimmed = expr.trim();
    let expanded: &str = if trimmed.starts_with('@') {
        expand_macro(trimmed).ok_or_else(|| CronError::BadValue {
            field: "macro",
            part: trimmed.to_string(),
        })?
    } else {
        trimmed
    };
    // Quartz '?' placeholder — pyfly normalizes it to '*' wholesale.
    let normalized = expanded.replace('?', "*");
    let fields: Vec<&str> = normalized.split_whitespace().collect();
    match fields.len() {
        5 => Ok(CronExpr {
            second: vec![0],
            minute: parse_field(fields[0], 0, 59, "minute")?,
            hour: parse_field(fields[1], 0, 23, "hour")?,
            day_of_month: parse_field(fields[2], 1, 31, "dom")?,
            month: parse_field(fields[3], 1, 12, "month")?,
            day_of_week: parse_field(fields[4], 0, 6, "dow")?,
        }),
        6 => Ok(CronExpr {
            second: parse_field(fields[0], 0, 59, "second")?,
            minute: parse_field(fields[1], 0, 59, "minute")?,
            hour: parse_field(fields[2], 0, 23, "hour")?,
            day_of_month: parse_field(fields[3], 1, 31, "dom")?,
            month: parse_field(fields[4], 1, 12, "month")?,
            day_of_week: parse_field(fields[5], 0, 6, "dow")?,
        }),
        got => Err(CronError::FieldCount {
            got,
            expr: expr.to_string(),
        }),
    }
}

impl CronExpr {
    /// Parses `expr` — equivalent to the free function [`parse_cron`].
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        parse_cron(expr)
    }

    /// Returns the next time strictly after `from` that matches the
    /// expression, or `None` when no match exists within the next 366 days —
    /// the unsatisfiable-expression guard.
    ///
    /// Falls forward one minute at a time, then picks the first matching
    /// second within the minute — for the default `second == [0]` this is
    /// exactly the historical whole-minute behaviour. Calendar fields are
    /// evaluated in `from`'s own timezone; pass a [`chrono_tz::Tz`]-zoned
    /// `DateTime` for IANA-zone evaluation (pyfly `zone=` parity).
    pub fn next<Tz: TimeZone>(&self, from: DateTime<Tz>) -> Option<DateTime<Tz>> {
        // A fire later within `from`'s own minute (only possible when the
        // seconds field is restricted beyond the default `[0]`).
        let floor = from.with_second(0)?.with_nanosecond(0)?;
        if self.matches(&floor) {
            if let Some(&s) = self.second.iter().find(|&&s| s > from.second()) {
                return floor.with_second(s);
            }
        }
        let mut t = floor.checked_add_signed(Duration::minutes(1))?;
        for _ in 0..MAX_SEARCH_MINUTES {
            if self.matches(&t) {
                return t.with_second(*self.second.first()?);
            }
            t = t.checked_add_signed(Duration::minutes(1))?;
        }
        None
    }

    /// Returns the latest time strictly before `before` that matches the
    /// expression, or `None` when no match exists within the previous 366
    /// days. The pyfly `CronExpression.previous_fire_time` counterpart.
    pub fn prev<Tz: TimeZone>(&self, before: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let floor = before.with_second(0)?.with_nanosecond(0)?;
        // A fire at second s of minute m is strictly before `before` iff
        // s < before.second(), or s == before.second() with sub-second slack.
        let cutoff = if before.nanosecond() > 0 {
            before.second() + 1
        } else {
            before.second()
        };
        if self.matches(&floor) {
            if let Some(&s) = self.second.iter().rev().find(|&&s| s < cutoff) {
                return floor.with_second(s);
            }
        }
        let mut t = floor.checked_sub_signed(Duration::minutes(1))?;
        for _ in 0..MAX_SEARCH_MINUTES {
            if self.matches(&t) {
                return t.with_second(*self.second.last()?);
            }
            t = t.checked_sub_signed(Duration::minutes(1))?;
        }
        None
    }

    /// Returns the next `n` fire times strictly after `after`, in ascending
    /// order — pyfly's `next_n_fire_times`. Shorter than `n` only when the
    /// expression dries up (unsatisfiable within the search horizon).
    pub fn next_n<Tz: TimeZone>(&self, n: usize, after: DateTime<Tz>) -> Vec<DateTime<Tz>> {
        let mut out = Vec::with_capacity(n);
        let mut cursor = after;
        for _ in 0..n {
            match self.next(cursor.clone()) {
                Some(t) => {
                    cursor = t.clone();
                    out.push(t);
                }
                None => break,
            }
        }
        out
    }

    /// Returns the number of seconds from `after` until the next fire time,
    /// or `None` when the expression is unsatisfiable — pyfly's
    /// `seconds_until_next` (a positive float).
    pub fn seconds_until_next<Tz: TimeZone>(&self, after: DateTime<Tz>) -> Option<f64> {
        let next = self.next(after.clone())?;
        let delta = next - after;
        Some(delta.num_nanoseconds()? as f64 / 1_000_000_000.0)
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

impl std::fmt::Display for CronExpr {
    /// Renders the expression in canonical form: a full-domain field as
    /// `*`, otherwise sorted values with consecutive runs compressed to
    /// `a-b` ranges (`*/15` round-trips as `0,15,30,45`). The seconds field
    /// is printed (Spring 6-field, seconds-first) only when it is not the
    /// 5-field default `[0]`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.second != [0] {
            write!(f, "{} ", render_field(&self.second, 0, 59))?;
        }
        write!(
            f,
            "{} {} {} {} {}",
            render_field(&self.minute, 0, 59),
            render_field(&self.hour, 0, 23),
            render_field(&self.day_of_month, 1, 31),
            render_field(&self.month, 1, 12),
            render_field(&self.day_of_week, 0, 6),
        )
    }
}

/// Renders one field set canonically: `*` for the full domain, else comma
/// list with consecutive runs of three or more compressed to `a-b`.
fn render_field(xs: &[u32], lo: u32, hi: u32) -> String {
    if xs.is_empty() || xs.len() == (hi - lo + 1) as usize {
        return "*".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < xs.len() {
        let start = xs[i];
        let mut end = start;
        while i + 1 < xs.len() && xs[i + 1] == end + 1 {
            i += 1;
            end = xs[i];
        }
        if end > start + 1 {
            parts.push(format!("{start}-{end}"));
        } else if end == start + 1 {
            parts.push(start.to_string());
            parts.push(end.to_string());
        } else {
            parts.push(start.to_string());
        }
        i += 1;
    }
    parts.join(",")
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

    // ---- pyfly parity: ports of tests/scheduling/test_cron.py ----

    // Port of pyfly test_valid_expression_creates_instance.
    #[test]
    fn valid_expression_creates_instance() {
        let c = parse_cron("*/5 * * * *").unwrap();
        assert_eq!(c.minute, vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]);
    }

    // Port of pyfly test_invalid_expression_raises_value_error.
    #[test]
    fn invalid_expression_is_an_error() {
        assert!(parse_cron("not a cron").is_err());
    }

    // Port of pyfly test_next_fire_time_returns_correct_time.
    #[test]
    fn next_fire_time_returns_correct_time() {
        let c = parse_cron("30 14 * * *").unwrap(); // Every day at 14:30
        let base = utc(2026, 1, 15, 10, 0, 0);
        assert_eq!(c.next(base).unwrap(), utc(2026, 1, 15, 14, 30, 0));
    }

    // Port of pyfly test_next_fire_time_without_base_defaults_to_now.
    #[test]
    fn next_fire_time_from_now_is_within_60_seconds() {
        let c = parse_cron("* * * * *").unwrap(); // Every minute
        let now = Utc::now();
        let next = c.next(now).unwrap();
        let delta = (next - now).num_milliseconds() as f64 / 1000.0;
        assert!(delta > 0.0 && delta <= 60.0, "delta: {delta}");
    }

    // Port of pyfly test_previous_fire_time_works.
    #[test]
    fn previous_fire_time_works() {
        let c = parse_cron("0 12 * * *").unwrap(); // Every day at noon
        let base = utc(2026, 1, 15, 14, 0, 0);
        assert_eq!(c.prev(base).unwrap(), utc(2026, 1, 15, 12, 0, 0));
    }

    // Port of pyfly test_next_n_fire_times_returns_correct_count_and_ascending.
    #[test]
    fn next_n_fire_times_returns_correct_count_and_ascending() {
        let c = parse_cron("0 * * * *").unwrap(); // Every hour
        let base = utc(2026, 1, 15, 10, 0, 0);
        let times = c.next_n(5, base);
        assert_eq!(times.len(), 5);
        for w in times.windows(2) {
            assert!(w[1] > w[0]);
        }
        assert_eq!(times[0], utc(2026, 1, 15, 11, 0, 0));
        assert_eq!(times[4], utc(2026, 1, 15, 15, 0, 0));
    }

    // Port of pyfly test_seconds_until_next_returns_positive_float.
    #[test]
    fn seconds_until_next_returns_positive_float() {
        let c = parse_cron("* * * * *").unwrap();
        let base = utc(2026, 1, 15, 10, 0, 0);
        let seconds = c.seconds_until_next(base).unwrap();
        assert!(seconds > 0.0);
    }

    // Port of pyfly test_every_minute_cron_fires_within_60_seconds.
    #[test]
    fn every_minute_cron_fires_within_60_seconds() {
        let c = parse_cron("* * * * *").unwrap();
        let base = utc(2026, 1, 15, 10, 0, 30);
        let seconds = c.seconds_until_next(base).unwrap();
        assert!(seconds > 0.0 && seconds <= 60.0, "seconds: {seconds}");
    }

    // Port of pyfly test_midnight_cron_fires_at_midnight.
    #[test]
    fn midnight_cron_fires_at_midnight() {
        let c = parse_cron("0 0 * * *").unwrap();
        let base = utc(2026, 1, 15, 23, 0, 0);
        assert_eq!(c.next(base).unwrap(), utc(2026, 1, 16, 0, 0, 0));
    }

    // ---- pyfly parity: ports of test_wave_scheduling_fixes.py ----

    // Port of pyfly test_six_field_spring_cron_accepted (audit #185).
    #[test]
    fn six_field_spring_cron_accepted() {
        // noon every day, '?' day-of-month
        let c = parse_cron("0 0 12 ? * *").unwrap();
        assert!(c.next(Utc::now()).is_some());
        assert_eq!(c.second, vec![0]);
        assert_eq!(c.hour, vec![12]);
    }

    // Port of pyfly test_five_field_cron_still_works.
    #[test]
    fn five_field_cron_still_works() {
        let c = parse_cron("*/5 * * * *").unwrap();
        assert!(c.next(Utc::now()).is_some());
    }

    // ---- pyfly parity: ports of test_cron_timezone.py ----

    // Port of pyfly test_cron_without_zone_is_utc.
    #[test]
    fn cron_without_zone_is_utc() {
        let c = parse_cron("0 0 * * *").unwrap(); // daily midnight
        let after = utc(2026, 6, 7, 15, 0, 0);
        let next = c.next(after).unwrap();
        assert_eq!((next.hour(), next.minute()), (0, 0)); // UTC midnight
    }

    // Port of pyfly test_cron_zone_fires_at_zone_midnight.
    #[test]
    fn cron_zone_fires_at_zone_midnight() {
        let ny: chrono_tz::Tz = "America/New_York".parse().unwrap();
        let c = parse_cron("0 0 * * *").unwrap();
        let after = ny.with_ymd_and_hms(2026, 6, 7, 15, 0, 0).unwrap(); // 3pm in New York
        let next = c.next(after).unwrap();
        assert_eq!((next.hour(), next.minute()), (0, 0)); // midnight in New York
        assert_eq!(next.timezone(), ny);
    }

    // Port of pyfly test_zone_changes_the_utc_instant.
    #[test]
    fn zone_changes_the_utc_instant() {
        // The same "midnight" cron resolves to different UTC instants per zone.
        let ny: chrono_tz::Tz = "America/New_York".parse().unwrap();
        let c = parse_cron("0 0 * * *").unwrap();
        let utc_next = c.next(utc(2026, 6, 7, 15, 0, 0)).unwrap();
        let ny_next = c
            .next(ny.with_ymd_and_hms(2026, 6, 7, 15, 0, 0).unwrap())
            .unwrap();
        assert_ne!(utc_next, ny_next.with_timezone(&Utc));
    }

    // ---- pyfly parity: 6-field seconds, '?', and macros ----

    #[test]
    fn six_field_seconds_evaluation() {
        let c = parse_cron("*/30 * * * * *").unwrap();
        assert_eq!(c.second, vec![0, 30]);
        // Within the same minute when a later second matches.
        assert_eq!(
            c.next(utc(2026, 1, 15, 10, 0, 15)).unwrap(),
            utc(2026, 1, 15, 10, 0, 30)
        );
        // Strictly after: exactly on a fire time rolls to the next one.
        assert_eq!(
            c.next(utc(2026, 1, 15, 10, 0, 30)).unwrap(),
            utc(2026, 1, 15, 10, 1, 0)
        );
    }

    #[test]
    fn six_field_next_picks_first_second_of_next_minute() {
        let c = parse_cron("15 * * * * *").unwrap();
        assert_eq!(
            c.next(utc(2026, 1, 15, 10, 0, 20)).unwrap(),
            utc(2026, 1, 15, 10, 1, 15)
        );
    }

    #[test]
    fn six_field_prev_with_seconds() {
        let c = parse_cron("*/30 * * * * *").unwrap();
        assert_eq!(
            c.prev(utc(2026, 1, 15, 10, 0, 45)).unwrap(),
            utc(2026, 1, 15, 10, 0, 30)
        );
        // Strictly before: exactly on a fire time yields the previous one.
        assert_eq!(
            c.prev(utc(2026, 1, 15, 10, 0, 30)).unwrap(),
            utc(2026, 1, 15, 10, 0, 0)
        );
        assert_eq!(
            c.prev(utc(2026, 1, 15, 10, 0, 0)).unwrap(),
            utc(2026, 1, 15, 9, 59, 30)
        );
    }

    #[test]
    fn question_mark_is_wildcard_in_both_forms() {
        assert_eq!(
            parse_cron("0 0 12 ? * *").unwrap(),
            parse_cron("0 0 12 * * *").unwrap()
        );
        assert_eq!(
            parse_cron("0 12 ? * ?").unwrap(),
            parse_cron("0 12 * * *").unwrap()
        );
    }

    #[test]
    fn six_field_second_errors_are_labeled() {
        let err = parse_cron("60 * * * * *").unwrap_err();
        assert_eq!(err.to_string(), r#"second: bad value "60""#);
    }

    #[test]
    fn seven_fields_rejected() {
        let err = parse_cron("* * * * * * *").unwrap_err();
        assert!(matches!(err, CronError::FieldCount { got: 7, .. }));
    }

    #[test]
    fn macros_expand_to_canonical_expressions() {
        for (m, expansion) in [
            ("@hourly", "0 * * * *"),
            ("@daily", "0 0 * * *"),
            ("@midnight", "0 0 * * *"),
            ("@weekly", "0 0 * * 0"),
            ("@monthly", "0 0 1 * *"),
            ("@yearly", "0 0 1 1 *"),
            ("@annually", "0 0 1 1 *"),
        ] {
            assert_eq!(
                parse_cron(m).unwrap(),
                parse_cron(expansion).unwrap(),
                "macro {m}"
            );
        }
        // @daily fires at the next midnight.
        let c = parse_cron("@daily").unwrap();
        assert_eq!(
            c.next(utc(2026, 1, 15, 23, 0, 0)).unwrap(),
            utc(2026, 1, 16, 0, 0, 0)
        );
    }

    #[test]
    fn unknown_macro_rejected() {
        let err = parse_cron("@fortnightly").unwrap_err();
        assert_eq!(err.to_string(), r#"macro: bad value "@fortnightly""#);
    }

    #[test]
    fn display_renders_canonical_form() {
        assert_eq!(
            parse_cron("0 9 * * 1-5").unwrap().to_string(),
            "0 9 * * 1-5"
        );
        assert_eq!(
            parse_cron("*/15 * * * *").unwrap().to_string(),
            "0,15,30,45 * * * *"
        );
        // 6-field renders seconds-first; 5-field omits the [0] default.
        assert_eq!(
            parse_cron("30 0 9 * * *").unwrap().to_string(),
            "30 0 9 * * *"
        );
        assert_eq!(parse_cron("@daily").unwrap().to_string(), "0 0 * * *");
        // Round-trip: the canonical rendering reparses to the same expression.
        let c = parse_cron("5,6,10-12 9-17/2 1 6 *").unwrap();
        assert_eq!(parse_cron(&c.to_string()).unwrap(), c);
    }
}
