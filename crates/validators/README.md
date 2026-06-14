# `firefly-validators`

> **Tier:** Foundational · **Status:** Stable

## Overview

`firefly-validators` is the **canonical input-validation tier** — pure
functions that return `Ok(())` on success or a `ValidationError`
carrying a human-readable reason on failure. Every function is
allocation-light on the success path and safe for concurrent use.

Coverage:

| Validator              | What it checks                                                              |
|------------------------|-----------------------------------------------------------------------------|
| `validate_iban`        | ISO 13616 mod-97 + per-country length (74 country codes ship in the table) |
| `validate_bic`         | ISO 9362 — 8 or 11 alnum chars                                              |
| `validate_luhn`        | Luhn checksum (credit cards, IMEI, SIN)                                     |
| `validate_credit_card` | Luhn + length 12..19                                                        |
| `validate_phone_e164`  | `+<country><subscriber>`, 8..16 chars                                       |
| `validate_currency`    | ISO 4217 — 3 uppercase letters                                              |
| `validate_email`       | RFC 5322 syntactic (addr-spec or angle-addr with display name)              |
| `validate_password`    | Configurable policy (length, upper, lower, digit, symbol)                   |
| `validate_sort_code`   | UK 6-digit sort code (`XXXXXX` or `XX-XX-XX`)                               |
| `validate_bsb`         | AU 6-digit Bank-State-Branch                                                |
| `validate_ssn`         | US Social Security Number                                                   |
| `validate_vat`         | EU-style VAT (`CC` + 2..12 alnum)                                           |
| `validate_dni`         | Spanish DNI with letter checksum                                            |
| `validate_nie`         | Spanish NIE (X/Y/Z prefix variant)                                          |
| `validate_nif`         | Spanish NIF (DNI ∪ NIE)                                                     |
| `validate_cvv`         | Card CVV/CVC — 3 or 4 digits                                                |
| `validate_pin`         | Card PIN — 4 digits by default, `_with_length` variant                     |
| `validate_amount`      | Monetary amount — finite, non-negative, ≤ 18 integer digits                |
| `validate_account_number` | Bank account number — 6..34 alphanumerics                               |
| `validate_interest_rate` | Percentage band 0..=100, `_within` variant                               |
| `validate_date`        | ISO-8601 calendar date, `_with_format` strftime variant                     |
| `validate_datetime`    | ISO-8601 datetime (`T` or space, fractional secs, offset or `Z`)            |
| `validate_national_id` | Generic national id — 5..20 alphanumerics after stripping spaces/dashes     |
| `validate_tax_id`      | Generic tax id — `^[A-Z0-9]{3,20}$` after upper-casing (no separator stripping) |

## Why pure functions?

Where annotation-driven frameworks reach for validation attributes,
Rust has no annotation runtime, so this crate stays close to the
language: plain functions returning `Result<(), ValidationError>`.
Services can:

* Call them directly from CQRS command/query `validate()` methods.
* Compose them per field and collect failures into a multi-field
  validation report.
* Match on `ValidationError::Invalid(reason)` — a single error kind
  acts as a sentinel, and its `Display` rendering
  (`firefly/validators: invalid: <reason>`) is stable across the crate.

## Public surface

```rust
pub enum ValidationError { Invalid(String) }   // Display: "firefly/validators: invalid: <reason>"
impl ValidationError { pub fn reason(&self) -> &str }

pub fn validate_iban(s: &str) -> Result<(), ValidationError>;
pub fn validate_bic(s: &str) -> Result<(), ValidationError>;
pub fn validate_luhn(s: &str) -> Result<(), ValidationError>;
pub fn validate_credit_card(s: &str) -> Result<(), ValidationError>;
pub fn validate_phone_e164(s: &str) -> Result<(), ValidationError>;
pub fn validate_currency(s: &str) -> Result<(), ValidationError>;
pub fn validate_email(s: &str) -> Result<(), ValidationError>;
pub fn validate_password(s: &str, p: PasswordPolicy) -> Result<(), ValidationError>;
pub fn validate_sort_code(s: &str) -> Result<(), ValidationError>;
pub fn validate_bsb(s: &str) -> Result<(), ValidationError>;
pub fn validate_ssn(s: &str) -> Result<(), ValidationError>;
pub fn validate_vat(s: &str) -> Result<(), ValidationError>;
pub fn validate_dni(s: &str) -> Result<(), ValidationError>;
pub fn validate_nie(s: &str) -> Result<(), ValidationError>;
pub fn validate_nif(s: &str) -> Result<(), ValidationError>;

pub struct PasswordPolicy {
    pub min_length: usize,
    pub require_upper: bool,
    pub require_lower: bool,
    pub require_digit: bool,
    pub require_symbol: bool,
}
impl Default for PasswordPolicy { /* 12+ chars, upper, lower, digit, symbol */ }

pub const IBAN_COUNTRY_LENGTHS: &[(&str, usize)];
pub fn iban_country_length(country: &str) -> Option<usize>;

// banking predicates (see below)
pub fn validate_cvv(s: &str) -> Result<(), ValidationError>;
pub fn validate_pin(s: &str) -> Result<(), ValidationError>;                       // length 4
pub fn validate_pin_with_length(s: &str, length: usize) -> Result<(), ValidationError>;
pub fn validate_amount(value: f64) -> Result<(), ValidationError>;                 // allow_zero=false, max_digits=18
pub fn validate_amount_with(value: f64, allow_zero: bool, max_digits: usize) -> Result<(), ValidationError>;
pub fn validate_account_number(s: &str) -> Result<(), ValidationError>;
pub fn validate_interest_rate(value: f64) -> Result<(), ValidationError>;          // 0.0..=100.0
pub fn validate_interest_rate_within(value: f64, min_pct: f64, max_pct: f64) -> Result<(), ValidationError>;
pub fn validate_date(s: &str) -> Result<(), ValidationError>;                      // %Y-%m-%d
pub fn validate_date_with_format(s: &str, fmt: &str) -> Result<(), ValidationError>;
pub fn validate_datetime(s: &str) -> Result<(), ValidationError>;
pub fn validate_national_id(s: &str) -> Result<(), ValidationError>;               // 5..=20 alnum after strip
pub fn validate_tax_id(s: &str) -> Result<(), ValidationError>;                    // ^[A-Z0-9]{3,20}$ after upper
```

## Banking predicates

A second family of predicates covers card, money, and date fields, all
returning the same `Result<(), ValidationError>` as the rest of the
crate:

* **Concise error reasons** — `invalid CVV`, `invalid pin`,
  `invalid amount`, `invalid account number`,
  `interest rate out of range`, `invalid date`, `invalid datetime` —
  each wrapped in the crate-canonical `Display`
  (`firefly/validators: invalid: invalid CVV`). One function per check;
  there is no separate boolean predicate.
* **`_with`/`_within` variants accept overrides**: the unsuffixed
  function applies sensible defaults (PIN length 4, amount
  `allow_zero=false`/`max_digits=18`, interest band `0..=100`, date
  format `%Y-%m-%d`).
* **`validate_amount` is hardened**: `inf`/`-inf`/`NaN` are rejected
  (never panic), negatives are rejected, zero needs `allow_zero`, and
  the truncated integer part may carry at most `max_digits` decimal
  digits.
* **`validate_date`/`validate_datetime` parse with chrono**:
  `validate_date_with_format` takes `chrono::format::strftime`
  specifiers (the familiar `%Y-%m-%d`-style directives);
  `validate_datetime` accepts the common ISO-8601 shapes — `T` or space
  separator, optional fractional seconds, `±HH:MM`/`±HHMM` offsets or
  trailing `Z`, minute precision, and bare dates. Compact "basic" forms
  (`20260507T120000`) and hour-only times are deliberately not accepted.
* **`validate_national_id` / `validate_tax_id` are GENERIC format
  checks** — distinct from the nation-specific `validate_dni` / `nie` /
  `nif` / `ssn` / `vat` validators above. `validate_national_id` strips
  spaces and dashes, upper-cases, then requires 5..=20 alphanumerics;
  `validate_tax_id` only upper-cases (no separator stripping) and
  matches `^[A-Z0-9]{3,20}$`, so any space/dash/punctuation in a tax id
  is rejected. Reasons are `invalid national id` / `invalid tax id`.
* **ASCII-only alphabets**: digit and alphanumeric checks accept only
  ASCII characters (not non-ASCII Unicode digits/letters — this applies
  to `validate_national_id` too), and the amount/interest validators
  take a typed `f64`.

```rust
use firefly_validators::{validate_amount_with, validate_cvv, validate_datetime};

validate_cvv("123").unwrap();
validate_amount_with(0.0, true, 18).unwrap();          // allow_zero
validate_datetime("2026-05-07T12:00:00Z").unwrap();    // Z == +00:00
assert_eq!(
    validate_cvv("12").unwrap_err().reason(),
    "invalid CVV"
);
```

## Quick start

```rust
use firefly_validators::{
    validate_email, validate_iban, validate_password, PasswordPolicy, ValidationError,
};

struct RegisterCmd {
    email: String,
    password: String,
    iban: String,
}

impl RegisterCmd {
    fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let checks = [
            validate_email(&self.email),
            validate_password(&self.password, PasswordPolicy::default()),
            validate_iban(&self.iban),
        ];
        let errs: Vec<ValidationError> = checks.into_iter().filter_map(Result::err).collect();
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }
}

fn main() {
    let cmd = RegisterCmd {
        email: "alice@example.com".into(),
        password: "Hello-World-1!".into(),
        iban: "ES91 2100 0418 4502 0005 1332".into(),
    };
    assert!(cmd.validate().is_ok());

    let err = validate_iban("GB82WEST12345698765431").unwrap_err();
    assert_eq!(err.reason(), "iban: mod-97 mismatch");
}
```

## Testing

```bash
cargo test -p firefly-validators
```

Suite includes mod-97 round-trips on canonical IBAN test vectors
(GB, DE, FR, ES), Luhn against a known-good and known-bad pair,
DNI checksum letter table (including the Y/Z NIE prefixes), and
password-policy boundary cases — plus checks that the error `Display`
format is stable and that `ValidationError` is
`Send + Sync + Clone + Eq`.
