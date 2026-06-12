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
