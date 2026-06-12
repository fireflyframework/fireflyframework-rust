//! Ports of the pyfly validation test suite for the banking predicates
//! added at pyfly parity: CVV, PIN, amount, account number, interest
//! rate, and date/datetime.
//!
//! Sources mirrored:
//! - `tests/validation/test_domain_validators.py` (`TestAmount`, `TestPin`)
//! - `tests/validation/test_validation_hardening.py` (`TestIsValidAmount`)
//! - behavior pinned by `src/pyfly/validation/domain.py` for the
//!   predicates pyfly ships without dedicated test classes.

use firefly_validators::{
    validate_account_number, validate_amount, validate_amount_with, validate_cvv, validate_date,
    validate_date_with_format, validate_datetime, validate_interest_rate,
    validate_interest_rate_within, validate_pin, validate_pin_with_length,
};

// ----- amount (pyfly TestAmount + TestIsValidAmount) -----

#[test]
fn amount_positive() {
    // TestAmount::test_positive
    validate_amount(123.45).expect("positive amount is valid");
}

#[test]
fn amount_zero_with_flag() {
    // TestAmount::test_zero_with_flag
    assert!(validate_amount(0.0).is_err(), "zero rejected by default");
    validate_amount_with(0.0, true, 18).expect("zero accepted with allow_zero");
}

#[test]
fn amount_non_finite_is_rejected_not_crashed() {
    // TestIsValidAmount::test_non_finite_is_rejected_not_crashed
    for bad in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert!(validate_amount(bad).is_err(), "non-finite {bad} passed");
    }
}

#[test]
fn amount_normal_amounts() {
    // TestIsValidAmount::test_normal_amounts
    validate_amount(100.0).expect("100.0 is valid");
    assert!(validate_amount(-1.0).is_err(), "negative rejected");
    assert!(validate_amount(0.0).is_err(), "zero rejected by default");
    validate_amount_with(0.0, true, 18).expect("zero valid with allow_zero");
}

#[test]
fn amount_max_digits_counts_integer_part() {
    // pyfly: len(str(int(value))) <= max_digits, default 18.
    validate_amount(1e17).expect("18 integer digits fit the default");
    assert!(validate_amount(1e18).is_err(), "19 digits exceed default");
    validate_amount_with(99.9, false, 2).expect("2 integer digits within 2");
    assert!(
        validate_amount_with(123.0, false, 2).is_err(),
        "3 integer digits exceed max_digits=2"
    );
    // int(0.5) == 0 -> "0", one digit.
    validate_amount_with(0.5, false, 1).expect("fraction truncates to one digit");
}

#[test]
fn amount_reason_matches_pyfly_message() {
    assert_eq!(
        validate_amount(-1.0).expect_err("negative").reason(),
        "invalid amount"
    );
}

// ----- pin (pyfly TestPin) -----

#[test]
fn pin_default_length_is_four() {
    // TestPin::test_default
    validate_pin("1234").expect("4-digit PIN valid");
    assert!(validate_pin("12345").is_err(), "5 digits fail default");
}

#[test]
fn pin_explicit_length_and_alphabet() {
    validate_pin_with_length("123456", 6).expect("6-digit PIN with length=6");
    assert!(validate_pin_with_length("1234", 6).is_err(), "wrong length");
    assert!(validate_pin("12a4").is_err(), "letters are not digits");
    assert!(
        validate_pin_with_length("", 0).is_err(),
        "empty string is falsy in pyfly even for length=0"
    );
    assert_eq!(
        validate_pin("x").expect_err("bad pin").reason(),
        "invalid pin"
    );
}

// ----- cvv (pinned by pyfly is_valid_cvv) -----

#[test]
fn cvv_three_or_four_digits() {
    validate_cvv("123").expect("3 digits valid");
    validate_cvv("1234").expect("4 digits valid");
    for bad in ["12", "12345", "12a", "", " 123"] {
        assert!(validate_cvv(bad).is_err(), "bad CVV passed: {bad:?}");
    }
    assert_eq!(
        validate_cvv("12").expect_err("too short").reason(),
        "invalid CVV"
    );
}

// ----- account number (pinned by pyfly is_valid_account_number) -----

#[test]
fn account_number_alnum_6_to_34() {
    validate_account_number("123456").expect("6 chars minimum");
    validate_account_number("GB29NWBK60161331926819").expect("IBAN-shaped is alnum");
    validate_account_number("abc123").expect("lowercase letters are alphanumeric");
    let max = "A1".repeat(17); // 34 chars
    validate_account_number(&max).expect("34 chars maximum");
    let too_long = format!("{max}A"); // 35 chars
    for bad in ["12345", too_long.as_str(), "ACC-12345", "ACC 1234", ""] {
        assert!(
            validate_account_number(bad).is_err(),
            "bad account number passed: {bad:?}"
        );
    }
    assert_eq!(
        validate_account_number("x")
            .expect_err("too short")
            .reason(),
        "invalid account number"
    );
}

// ----- interest rate (pinned by pyfly is_valid_interest_rate) -----

#[test]
fn interest_rate_default_band_is_0_to_100() {
    validate_interest_rate(4.25).expect("4.25 % valid");
    validate_interest_rate(0.0).expect("lower bound inclusive");
    validate_interest_rate(100.0).expect("upper bound inclusive");
    assert!(validate_interest_rate(-0.01).is_err(), "below band");
    assert!(validate_interest_rate(100.01).is_err(), "above band");
}

#[test]
fn interest_rate_custom_band_and_non_finite() {
    validate_interest_rate_within(7.5, 5.0, 10.0).expect("inside custom band");
    assert!(
        validate_interest_rate_within(4.9, 5.0, 10.0).is_err(),
        "below custom band"
    );
    assert!(
        validate_interest_rate(f64::NAN).is_err(),
        "NaN never satisfies the chained comparison"
    );
    assert!(validate_interest_rate(f64::INFINITY).is_err(), "inf above");
    assert_eq!(
        validate_interest_rate(-1.0).expect_err("below").reason(),
        "interest rate out of range"
    );
}

// ----- date (pinned by pyfly is_valid_date) -----

#[test]
fn date_default_format_is_iso_calendar_date() {
    validate_date("2026-05-07").expect("ISO date valid");
    for bad in [
        "2026-13-01",
        "2026-02-30",
        "not-a-date",
        "",
        "2026-05-07T00:00:00",
    ] {
        assert!(validate_date(bad).is_err(), "bad date passed: {bad:?}");
    }
    assert_eq!(
        validate_date("nope").expect_err("not a date").reason(),
        "invalid date"
    );
}

#[test]
fn date_custom_strftime_format() {
    validate_date_with_format("07/05/2026", "%d/%m/%Y").expect("custom format valid");
    assert!(
        validate_date_with_format("2026-05-07", "%d/%m/%Y").is_err(),
        "ISO input fails a slash format"
    );
}

// ----- datetime (pinned by pyfly is_valid_datetime) -----

#[test]
fn datetime_accepts_fromisoformat_shapes() {
    for good in [
        "2026-05-07T12:00:00",        // docstring example
        "2026-05-07T12:00:00+00:00",  // docstring example
        "2026-05-07T12:00:00Z",       // docstring example (Z rewrite)
        "2026-05-07 12:00:00",        // space separator
        "2026-05-07T12:00:00.123456", // fractional seconds
        "2026-05-07T12:00:00.5+02:00",
        "2026-05-07T12:00:00+0000", // fromisoformat also accepts the colonless offset
        "2026-05-07T12:00",         // minute precision
        "2026-05-07",               // bare date, like datetime.fromisoformat
    ] {
        validate_datetime(good).unwrap_or_else(|e| panic!("{good:?} should be valid: {e}"));
    }
}

#[test]
fn datetime_rejects_malformed_inputs() {
    for bad in [
        "",
        "nope",
        "2026-05-07T25:00:00",  // impossible hour
        "2026-02-30T12:00:00",  // impossible date
        "2026-05-07T12:00:00X", // bogus suffix
        "12:00:00",             // time without a date
    ] {
        assert!(
            validate_datetime(bad).is_err(),
            "bad datetime passed: {bad:?}"
        );
    }
    assert_eq!(
        validate_datetime("nope")
            .expect_err("not a datetime")
            .reason(),
        "invalid datetime"
    );
}

// ----- wire format stays the crate-canonical Display -----

#[test]
fn reasons_render_with_the_canonical_prefix() {
    assert_eq!(
        validate_cvv("12").expect_err("short cvv").to_string(),
        "firefly/validators: invalid: invalid CVV"
    );
}
