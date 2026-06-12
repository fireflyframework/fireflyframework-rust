# `firefly-validators`

> **Tier:** Foundational · **Status:** Full · **Java original:** `firefly-common-validators` · **Go module:** `validators`

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
| `validate_cvv`         | Card CVV/CVC — 3 or 4 digits *(pyfly parity)*                               |
| `validate_pin`         | Card PIN — 4 digits by default, `_with_length` variant *(pyfly parity)*    |
| `validate_amount`      | Monetary amount — finite, non-negative, ≤ 18 integer digits *(pyfly parity)* |
| `validate_account_number` | Bank account number — 6..34 alphanumerics *(pyfly parity)*               |
| `validate_interest_rate` | Percentage band 0..=100, `_within` variant *(pyfly parity)*               |
| `validate_date`        | ISO-8601 calendar date, `_with_format` strftime variant *(pyfly parity)*    |
| `validate_datetime`    | ISO-8601 datetime (`T` or space, fractional secs, offset or `Z`) *(pyfly parity)* |

## Why pure functions?

The .NET port uses validation attributes (`[ValidIban]`); Java uses
Jakarta Bean Validation annotations; the Go port exposes plain
functions returning `error`. Rust has no annotation runtime either, so
this crate keeps the Go shape: plain functions returning
`Result<(), ValidationError>`. Services can:

* Call them directly from CQRS command/query `validate()` methods.
* Compose them per field and collect failures into a multi-field
  validation report.
* Match on `ValidationError::Invalid(reason)` — the single error kind
  is the analog of Go's `errors.Is(err, ErrInvalid)` sentinel, and its
  `Display` rendering (`firefly/validators: invalid: <reason>`) matches
  the Go port byte for byte.

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

// pyfly parity — banking predicates (see below)
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
```

## pyfly parity

The banking predicates from `pyfly.validation.domain` that the Go
lineage lacked ship as the same `Result<(), ValidationError>` functions
as the rest of the crate:

* **Error reasons are pyfly's messages verbatim** — `invalid CVV`,
  `invalid pin`, `invalid amount`, `invalid account number`,
  `interest rate out of range`, `invalid date`, `invalid datetime` —
  wrapped in the crate-canonical `Display`
  (`firefly/validators: invalid: invalid CVV`). pyfly's boolean
  `is_valid_*` and its pydantic `valid_*` factory collapse into one
  function here.
* **Keyword arguments become `_with`/`_within` variants**: the
  unsuffixed function applies pyfly's defaults (PIN length 4, amount
  `allow_zero=false`/`max_digits=18`, interest band `0..=100`, date
  format `%Y-%m-%d`).
* **`validate_amount` mirrors pyfly's hardening**: `inf`/`-inf`/`NaN`
  are rejected (never panic), negatives are rejected, zero needs
  `allow_zero`, and the truncated integer part may carry at most
  `max_digits` decimal digits (Python's `len(str(int(v))) <= max_digits`).
* **`validate_date`/`validate_datetime` parse with chrono**:
  `validate_date_with_format` takes `chrono::format::strftime`
  specifiers (the same `%Y-%m-%d`-style directives `datetime.strptime`
  uses); `validate_datetime` accepts the `datetime.fromisoformat`
  shapes — `T` or space separator, optional fractional seconds,
  `±HH:MM`/`±HHMM` offsets or trailing `Z`, minute precision, and bare
  dates. Compact "basic" forms (`20260507T120000`) and hour-only times
  are deliberately not accepted.
* **Deliberate divergences**: alphabets are ASCII-only (Python's
  `str.isdigit`/`str.isalnum` would also accept non-ASCII Unicode
  digits/letters), and the amount/interest validators take a typed
  `f64` instead of pyfly's "anything `float()` can coerce".

```rust
use firefly_validators::{validate_amount_with, validate_cvv, validate_datetime};

validate_cvv("123").unwrap();
validate_amount_with(0.0, true, 18).unwrap();          // allow_zero
validate_datetime("2026-05-07T12:00:00Z").unwrap();    // Z == +00:00
assert_eq!(
    validate_cvv("12").unwrap_err().reason(),
    "invalid CVV"                                      // pyfly's message, verbatim
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
password-policy boundary cases — plus Rust-specific checks that the
error `Display` format matches the Go port and that `ValidationError`
is `Send + Sync + Clone + Eq`.
