// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! firefly-validators — the canonical input-validation tier.
//!
//! Pure functions that return `Ok(())` on success or a [`ValidationError`]
//! carrying a human-readable reason on failure. Every function is
//! allocation-light on the success path and safe for concurrent use
//! (no interior mutability beyond lazily compiled regexes).
//!
//! Coverage: IBAN (ISO 13616 mod-97), BIC/SWIFT (ISO 9362), Luhn and
//! credit cards, E.164 phone numbers, ISO 4217 currency codes, RFC 5322
//! email addresses, configurable password policies, UK sort codes,
//! Australian BSBs, US SSNs, EU-style VAT numbers, and Spanish
//! DNI/NIE/NIF identifiers — plus the pyfly banking predicates: CVV,
//! PIN, monetary amounts, account numbers, interest-rate bands,
//! ISO-8601 dates/datetimes (chrono-backed), and the generic
//! national-id / tax-id format checks.
//!
//! This crate is the Rust port of the Go module
//! `github.com/fireflyframework/fireflyframework-go/validators`; error
//! message texts match the Go implementation so log lines and problem
//! details stay comparable across ports. The banking predicates ported
//! from `pyfly.validation.domain` keep pyfly's error messages instead
//! (`invalid CVV`, `interest rate out of range`, ...) so the two ports
//! emit identical reasons.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use std::sync::LazyLock;

use regex::Regex;

/// Framework version stamp.
pub const VERSION: &str = "26.6.4";

/// The canonical error returned by every validator when the input is
/// malformed.
///
/// This is the Rust analog of the Go sentinel `validators.ErrInvalid`:
/// every failure is the same error kind, wrapping a human-readable
/// reason. The [`std::fmt::Display`] rendering matches the Go port
/// (`firefly/validators: invalid: <reason>`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// The input failed validation for the embedded reason
    /// (e.g. `"iban: mod-97 mismatch"`).
    #[error("firefly/validators: invalid: {0}")]
    Invalid(String),
}

impl ValidationError {
    /// Returns the human-readable reason (e.g. `"iban: too short"`)
    /// without the `firefly/validators: invalid: ` prefix.
    pub fn reason(&self) -> &str {
        let Self::Invalid(reason) = self;
        reason
    }
}

/// Builds the canonical invalid error while keeping call sites terse —
/// the analog of the Go package's unexported `invalid(reason)` helper.
fn invalid(reason: impl Into<String>) -> ValidationError {
    ValidationError::Invalid(reason.into())
}

// ----- IBAN (ISO 13616) -----

/// Maps ISO 3166-1 country codes to their canonical IBAN length.
/// Subset covering the SEPA zone + select non-SEPA states — extend as
/// needed. Sorted by country code so [`iban_country_length`] can use a
/// binary search.
pub const IBAN_COUNTRY_LENGTHS: &[(&str, usize)] = &[
    ("AD", 24),
    ("AE", 23),
    ("AL", 28),
    ("AT", 20),
    ("AZ", 28),
    ("BA", 20),
    ("BE", 16),
    ("BG", 22),
    ("BH", 22),
    ("BR", 29),
    ("CH", 21),
    ("CR", 22),
    ("CY", 28),
    ("CZ", 24),
    ("DE", 22),
    ("DK", 18),
    ("DO", 28),
    ("EE", 20),
    ("ES", 24),
    ("FI", 18),
    ("FO", 18),
    ("FR", 27),
    ("GB", 22),
    ("GE", 22),
    ("GI", 23),
    ("GL", 18),
    ("GR", 27),
    ("GT", 28),
    ("HR", 21),
    ("HU", 28),
    ("IE", 22),
    ("IL", 23),
    ("IS", 26),
    ("IT", 27),
    ("JO", 30),
    ("KW", 30),
    ("KZ", 20),
    ("LB", 28),
    ("LC", 32),
    ("LI", 21),
    ("LT", 20),
    ("LU", 20),
    ("LV", 21),
    ("MC", 27),
    ("MD", 24),
    ("ME", 22),
    ("MK", 19),
    ("MR", 27),
    ("MT", 31),
    ("MU", 30),
    ("NL", 18),
    ("NO", 15),
    ("PK", 24),
    ("PL", 28),
    ("PS", 29),
    ("PT", 25),
    ("QA", 29),
    ("RO", 24),
    ("RS", 22),
    ("SA", 24),
    ("SC", 31),
    ("SE", 24),
    ("SI", 19),
    ("SK", 24),
    ("SM", 27),
    ("ST", 25),
    ("SV", 28),
    ("TL", 23),
    ("TN", 24),
    ("TR", 26),
    ("UA", 29),
    ("VA", 22),
    ("VG", 24),
    ("XK", 20),
];

/// Looks up the canonical IBAN length for an ISO 3166-1 country code,
/// or `None` when the country is not in [`IBAN_COUNTRY_LENGTHS`].
pub fn iban_country_length(country: &str) -> Option<usize> {
    IBAN_COUNTRY_LENGTHS
        .binary_search_by(|(code, _)| (*code).cmp(country))
        .ok()
        .map(|i| IBAN_COUNTRY_LENGTHS[i].1)
}

static IBAN_CHAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z0-9]+$").expect("hard-coded regex"));

/// Runs the ISO 13616 mod-97 check after enforcing the per-country
/// length and the `[A-Z0-9]` alphabet. Spaces and case are normalised
/// before validation.
pub fn validate_iban(s: &str) -> Result<(), ValidationError> {
    let input = s.replace(' ', "").to_uppercase();
    if input.len() < 4 {
        return Err(invalid("iban: too short"));
    }
    if !IBAN_CHAR_RE.is_match(&input) {
        return Err(invalid("iban: alphabet"));
    }
    let country = &input[..2];
    let Some(want) = iban_country_length(country) else {
        return Err(invalid(format!("iban: unknown country {country}")));
    };
    if input.len() != want {
        return Err(invalid(format!(
            "iban: length {}, want {} for {}",
            input.len(),
            want,
            country
        )));
    }
    // Move the leading "CCkk" block to the back, substitute A..Z with
    // 10..35, and reduce mod 97 in a streaming pass — equivalent to the
    // big-integer division the Go port performs with math/big.
    let bytes = input.as_bytes();
    let mut rem: u32 = 0;
    for &b in bytes[4..].iter().chain(&bytes[..4]) {
        match b {
            b'0'..=b'9' => rem = (rem * 10 + u32::from(b - b'0')) % 97,
            b'A'..=b'Z' => rem = (rem * 100 + u32::from(b - b'A') + 10) % 97,
            _ => return Err(invalid("iban: alphabet")),
        }
    }
    if rem != 1 {
        return Err(invalid("iban: mod-97 mismatch"));
    }
    Ok(())
}

// ----- BIC / SWIFT (ISO 9362) -----

static BIC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Z]{4}[A-Z]{2}[A-Z0-9]{2}([A-Z0-9]{3})?$").expect("hard-coded regex")
});

/// Accepts 8- or 11-character BICs. Spaces and case are normalised
/// before validation.
pub fn validate_bic(s: &str) -> Result<(), ValidationError> {
    let input = s.replace(' ', "").to_uppercase();
    if !BIC_RE.is_match(&input) {
        return Err(invalid("bic: format"));
    }
    Ok(())
}

// ----- Luhn / Credit Card -----

/// Returns `Ok(())` when `s` passes the Luhn check (used for credit
/// cards, IMEI, SIN, etc.). Spaces and dashes are stripped.
pub fn validate_luhn(s: &str) -> Result<(), ValidationError> {
    let clean = s.replace([' ', '-'], "");
    if clean.len() < 2 {
        return Err(invalid("luhn: too short"));
    }
    let mut sum: u32 = 0;
    let mut double = false;
    for &c in clean.as_bytes().iter().rev() {
        if !c.is_ascii_digit() {
            return Err(invalid("luhn: non-digit"));
        }
        let mut d = u32::from(c - b'0');
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }
    if sum % 10 != 0 {
        return Err(invalid("luhn: checksum"));
    }
    Ok(())
}

/// Luhn + length 12..19.
pub fn validate_credit_card(s: &str) -> Result<(), ValidationError> {
    let clean = s.replace([' ', '-'], "");
    let l = clean.len();
    if !(12..=19).contains(&l) {
        return Err(invalid("credit-card: length"));
    }
    validate_luhn(&clean)
}

// ----- E.164 phone -----

static E164_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\+[1-9][0-9]{6,14}$").expect("hard-coded regex"));

/// Accepts `+<country><subscriber>`, total 8..16 chars.
pub fn validate_phone_e164(s: &str) -> Result<(), ValidationError> {
    if !E164_RE.is_match(s) {
        return Err(invalid("phone: not E.164"));
    }
    Ok(())
}

// ----- Currency (ISO 4217) -----

static CURRENCY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z]{3}$").expect("hard-coded regex"));

/// Checks for a 3-letter uppercase code. Membership against the live
/// ISO 4217 set is left to callers — many systems accept defunct or
/// private codes (XAU, XBT) that aren't in the canonical list.
pub fn validate_currency(s: &str) -> Result<(), ValidationError> {
    if !CURRENCY_RE.is_match(s) {
        return Err(invalid("currency: format"));
    }
    Ok(())
}

// ----- Email -----

/// Accepts any RFC 5322 syntactically-valid address.
///
/// Like Go's `net/mail.ParseAddress`, both a bare addr-spec
/// (`alice@example.com`) and the angle-addr form with an optional
/// display name (`Alice <alice@example.com>`) are accepted. The local
/// part may be a dot-atom or a quoted string; the domain may be a
/// dot-atom or a `[...]` domain literal. Non-ASCII characters are
/// permitted in atoms, mirroring the Go parser's UTF-8 leniency.
pub fn validate_email(s: &str) -> Result<(), ValidationError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(invalid("email: empty address"));
    }
    // Angle-addr form: take what is inside the last `<...>` pair; the
    // display name in front is intentionally not validated further.
    let addr = if let Some(stripped) = trimmed.strip_suffix('>') {
        match stripped.rfind('<') {
            Some(i) => &stripped[i + 1..],
            None => return Err(invalid("email: unmatched angle bracket")),
        }
    } else {
        trimmed
    };
    match parse_addr_spec(addr) {
        Ok(()) => Ok(()),
        Err(reason) => Err(invalid(format!("email: {reason}"))),
    }
}

/// Validates `local@domain` per the RFC 5322 addr-spec grammar subset
/// implemented by the other ports.
fn parse_addr_spec(addr: &str) -> Result<(), &'static str> {
    let domain = if let Some(rest) = addr.strip_prefix('"') {
        // Quoted-string local part: scan to the closing quote honouring
        // backslash escapes.
        let end = quoted_string_end(rest).ok_or("unterminated quoted string in local part")?;
        rest[end + 1..]
            .strip_prefix('@')
            .ok_or("missing '@' after quoted local part")?
    } else {
        let at = addr.rfind('@').ok_or("missing '@' or angle-addr")?;
        if !is_dot_atom(&addr[..at]) {
            return Err("malformed local part");
        }
        &addr[at + 1..]
    };
    validate_email_domain(domain)
}

/// Returns the byte index of the closing `"` in a quoted-string body
/// (the opening quote already stripped), honouring `\` escapes.
fn quoted_string_end(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => return Some(i),
            _ => {}
        }
    }
    None
}

/// Reports whether `c` is RFC 5322 atext. Non-ASCII runes are accepted,
/// matching Go's `net/mail` UTF-8 leniency.
fn is_atext(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~".contains(c) || !c.is_ascii()
}

/// Reports whether `s` is a non-empty dot-atom: atext labels separated
/// by single dots, with no leading, trailing, or doubled dot.
fn is_dot_atom(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|label| !label.is_empty() && label.chars().all(is_atext))
}

/// Validates the domain side of an addr-spec: dot-atom or `[...]`
/// domain literal.
fn validate_email_domain(domain: &str) -> Result<(), &'static str> {
    if let Some(rest) = domain.strip_prefix('[') {
        let inner = rest.strip_suffix(']').ok_or("malformed domain literal")?;
        if inner.is_empty()
            || inner
                .chars()
                .any(|c| c == '[' || c == ']' || c == '\\' || c.is_control())
        {
            return Err("malformed domain literal");
        }
        return Ok(());
    }
    if !is_dot_atom(domain) {
        return Err("malformed domain");
    }
    Ok(())
}

// ----- Password strength -----

/// The canonical Firefly password policy: 12+ chars, at least one
/// upper, one lower, one digit, one symbol — see
/// [`PasswordPolicy::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordPolicy {
    /// Minimum password length in bytes (matching the Go port's
    /// `len(s)` semantics).
    pub min_length: usize,
    /// Require at least one upper-case letter (Unicode `Lu`).
    pub require_upper: bool,
    /// Require at least one lower-case letter (Unicode `Ll`).
    pub require_lower: bool,
    /// Require at least one decimal digit (Unicode `Nd`).
    pub require_digit: bool,
    /// Require at least one punctuation or symbol character (Unicode
    /// `P` or `S`).
    pub require_symbol: bool,
}

impl Default for PasswordPolicy {
    /// The policy used by the IDP starter and the notifications
    /// email-link admin endpoint — the analog of the Go port's
    /// `DefaultPasswordPolicy()`.
    fn default() -> Self {
        Self {
            min_length: 12,
            require_upper: true,
            require_lower: true,
            require_digit: true,
            require_symbol: true,
        }
    }
}

static UPPER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Lu}").expect("hard-coded regex"));
static LOWER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Ll}").expect("hard-coded regex"));
static DIGIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Nd}").expect("hard-coded regex"));
static SYMBOL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{P}\p{S}]").expect("hard-coded regex"));

/// Evaluates `s` against `p`.
///
/// Character classes follow the same Unicode general categories as the
/// Go port (`unicode.IsUpper`/`IsLower`/`IsDigit`/`IsPunct`/`IsSymbol`);
/// the length check counts bytes, exactly like Go's `len(s)`.
pub fn validate_password(s: &str, p: PasswordPolicy) -> Result<(), ValidationError> {
    if s.len() < p.min_length {
        return Err(invalid(format!("password: length < {}", p.min_length)));
    }
    if p.require_upper && !UPPER_RE.is_match(s) {
        return Err(invalid("password: needs upper-case"));
    }
    if p.require_lower && !LOWER_RE.is_match(s) {
        return Err(invalid("password: needs lower-case"));
    }
    if p.require_digit && !DIGIT_RE.is_match(s) {
        return Err(invalid("password: needs digit"));
    }
    if p.require_symbol && !SYMBOL_RE.is_match(s) {
        return Err(invalid("password: needs symbol"));
    }
    Ok(())
}

// ----- Sort code (UK 6-digit) -----

static SORT_CODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{2}-?[0-9]{2}-?[0-9]{2}$").expect("hard-coded regex"));

/// Accepts `XXXXXX` or `XX-XX-XX`.
pub fn validate_sort_code(s: &str) -> Result<(), ValidationError> {
    if !SORT_CODE_RE.is_match(s.trim()) {
        return Err(invalid("sort-code: format"));
    }
    Ok(())
}

// ----- VAT -----

static VAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z]{2}[A-Z0-9]{2,12}$").expect("hard-coded regex"));

/// Accepts EU-style VAT numbers (CC + 2..12 alnum). Real per-country
/// algebraic checks would require dedicated tables — kept out of scope,
/// matching the .NET port.
pub fn validate_vat(s: &str) -> Result<(), ValidationError> {
    let input = s.replace(' ', "").to_uppercase();
    if !VAT_RE.is_match(&input) {
        return Err(invalid("vat: format"));
    }
    Ok(())
}

// ----- Spanish DNI / NIE / NIF -----

/// The DNI checksum letter table indexed by `number % 23`.
const DNI_LETTERS: &[u8] = b"TRWAGMYFPDXBNJZSQVHLCKE";

/// Validates a Spanish DNI (8 digits + checksum letter).
pub fn validate_dni(s: &str) -> Result<(), ValidationError> {
    spanish_check(s, false)
}

/// Accepts NIE format (X/Y/Z + 7 digits + letter).
pub fn validate_nie(s: &str) -> Result<(), ValidationError> {
    spanish_check(s, true)
}

/// Accepts both DNI and NIE formats.
pub fn validate_nif(s: &str) -> Result<(), ValidationError> {
    if validate_dni(s).is_ok() {
        return Ok(());
    }
    validate_nie(s)
}

/// Shared DNI/NIE checksum routine: normalise, map the NIE prefix
/// letter to its digit, and compare against the mod-23 letter table.
fn spanish_check(s: &str, nie: bool) -> Result<(), ValidationError> {
    let mut input = s.replace(' ', "").to_uppercase().into_bytes();
    if nie {
        if input.len() != 9 {
            return Err(invalid("nie: length"));
        }
        match input[0] {
            b'X' => input[0] = b'0',
            b'Y' => input[0] = b'1',
            b'Z' => input[0] = b'2',
            _ => return Err(invalid("nie: prefix")),
        }
    } else if input.len() != 9 {
        return Err(invalid("dni: length"));
    }
    let letter = input[8];
    let mut n: u32 = 0;
    for &d in &input[..8] {
        if !d.is_ascii_digit() {
            return Err(invalid("dni: digits"));
        }
        n = n * 10 + u32::from(d - b'0');
    }
    if DNI_LETTERS[(n % 23) as usize] != letter {
        return Err(invalid("dni: checksum"));
    }
    Ok(())
}

// ----- US SSN -----

static SSN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{3}-?[0-9]{2}-?[0-9]{4}$").expect("hard-coded regex"));

/// Accepts `123-45-6789` or `123456789`.
pub fn validate_ssn(s: &str) -> Result<(), ValidationError> {
    if !SSN_RE.is_match(s.trim()) {
        return Err(invalid("ssn: format"));
    }
    Ok(())
}

// ----- AU BSB -----

static BSB_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{3}-?[0-9]{3}$").expect("hard-coded regex"));

/// Accepts a 6-digit Australian BSB with optional dash.
pub fn validate_bsb(s: &str) -> Result<(), ValidationError> {
    if !BSB_RE.is_match(s.trim()) {
        return Err(invalid("bsb: format"));
    }
    Ok(())
}

// ----- pyfly banking predicates -----
//
// Ports of `pyfly.validation.domain`'s `is_valid_*` predicates that the
// Go lineage lacked. pyfly exposes each predicate twice: a boolean
// `is_valid_x` and a pydantic factory `valid_x` raising
// `ValueError(<message>)`. The Rust port collapses both into the
// crate-canonical `Result<(), ValidationError>` shape and uses pyfly's
// factory messages verbatim as the error reason. pyfly's keyword
// arguments become explicit `_with_*` variants; the unsuffixed function
// applies pyfly's defaults.
//
// Deliberate divergence: pyfly's `str.isdigit`/`str.isalnum` accept
// non-ASCII Unicode digits and letters; banking CVVs, PINs, and account
// numbers are ASCII in every real scheme, so the Rust port restricts
// the alphabet to ASCII.

/// Generic tax-id alphabet, mirroring pyfly's
/// `_TAX_ID_GENERIC = re.compile(r"^[A-Z0-9]{3,20}$")`.
static TAX_ID_GENERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z0-9]{3,20}$").expect("hard-coded regex"));

/// Validates a card CVV/CVC: 3 or 4 ASCII digits — the port of pyfly's
/// `is_valid_cvv`. Fails with reason `invalid CVV`.
pub fn validate_cvv(s: &str) -> Result<(), ValidationError> {
    if !(3..=4).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("invalid CVV"));
    }
    Ok(())
}

/// Validates a card PIN with pyfly's default length of 4 digits — the
/// port of `is_valid_pin(value)`. Fails with reason `invalid pin`.
pub fn validate_pin(s: &str) -> Result<(), ValidationError> {
    validate_pin_with_length(s, 4)
}

/// Validates a PIN of exactly `length` ASCII digits — the port of
/// pyfly's `is_valid_pin(value, length=...)`. The empty string is
/// always rejected (pyfly treats `""` as falsy even for `length=0`).
/// Fails with reason `invalid pin`.
pub fn validate_pin_with_length(s: &str, length: usize) -> Result<(), ValidationError> {
    if s.is_empty() || s.len() != length || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("invalid pin"));
    }
    Ok(())
}

/// Validates a monetary amount with pyfly's defaults (`allow_zero =
/// false`, `max_digits = 18`) — the port of `is_valid_amount(value)`.
/// Fails with reason `invalid amount`.
pub fn validate_amount(value: f64) -> Result<(), ValidationError> {
    validate_amount_with(value, false, 18)
}

/// Validates a monetary amount with bounded integer precision — the
/// port of pyfly's `is_valid_amount(value, allow_zero=..., max_digits=...)`.
///
/// Rules, in pyfly order: non-finite values (`inf`, `-inf`, `NaN`) are
/// rejected rather than crashing; negative amounts are rejected; zero
/// is rejected unless `allow_zero`; and the decimal digit count of the
/// truncated integer part must not exceed `max_digits` (pyfly's
/// `len(str(int(value))) <= max_digits`). Fails with reason
/// `invalid amount`.
///
/// pyfly accepts any `float()`-coercible object; the Rust port is typed
/// — parse strings to `f64` before calling.
pub fn validate_amount_with(
    value: f64,
    allow_zero: bool,
    max_digits: usize,
) -> Result<(), ValidationError> {
    if !value.is_finite() || value < 0.0 || (value == 0.0 && !allow_zero) {
        return Err(invalid("invalid amount"));
    }
    // Mirror Python's len(str(int(value))): truncate toward zero, then
    // count the exact decimal digits. `.abs()` only normalises -0.0
    // (negatives were rejected above); `{:.0}` prints the exact value
    // since truncation already removed the fraction.
    let integer_part = format!("{:.0}", value.trunc().abs());
    if integer_part.len() > max_digits {
        return Err(invalid("invalid amount"));
    }
    Ok(())
}

/// Validates a bank account number: 6..=34 ASCII alphanumerics — the
/// port of pyfly's `is_valid_account_number`. Fails with reason
/// `invalid account number`.
pub fn validate_account_number(s: &str) -> Result<(), ValidationError> {
    if !(6..=34).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(invalid("invalid account number"));
    }
    Ok(())
}

/// Validates an interest rate against pyfly's default band of
/// `0.0..=100.0` percent — the port of `is_valid_interest_rate(value)`.
/// Fails with reason `interest rate out of range`.
pub fn validate_interest_rate(value: f64) -> Result<(), ValidationError> {
    validate_interest_rate_within(value, 0.0, 100.0)
}

/// Validates a percentage (e.g. `4.25` = 4.25 %) within an allowed band
/// — the port of pyfly's `is_valid_interest_rate(value, min_pct=...,
/// max_pct=...)`. `NaN` never satisfies the band, exactly like Python's
/// chained comparison. Fails with reason `interest rate out of range`.
pub fn validate_interest_rate_within(
    value: f64,
    min_pct: f64,
    max_pct: f64,
) -> Result<(), ValidationError> {
    if !(min_pct..=max_pct).contains(&value) {
        return Err(invalid("interest rate out of range"));
    }
    Ok(())
}

/// Validates an ISO-8601 calendar date (`%Y-%m-%d`, pyfly's default
/// format) — the port of `is_valid_date(value)`. Fails with reason
/// `invalid date`.
pub fn validate_date(s: &str) -> Result<(), ValidationError> {
    validate_date_with_format(s, "%Y-%m-%d")
}

/// Validates a calendar date against an explicit strftime-style format
/// — the port of pyfly's `is_valid_date(value, fmt=...)`. The format
/// string uses [`chrono::format::strftime`] specifiers, which match the
/// `datetime.strptime` specifiers pyfly accepts (`%Y`, `%m`, `%d`,
/// `%d/%m/%Y`, ...). The whole input must match, and impossible
/// calendar dates (Feb 30) are rejected. Fails with reason
/// `invalid date`.
pub fn validate_date_with_format(s: &str, fmt: &str) -> Result<(), ValidationError> {
    if chrono::NaiveDate::parse_from_str(s, fmt).is_err() {
        return Err(invalid("invalid date"));
    }
    Ok(())
}

/// Datetime layouts with a UTC offset, mirroring what
/// `datetime.fromisoformat` accepts after pyfly's `Z` substitution.
const DATETIME_OFFSET_FORMATS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f%:z",
    "%Y-%m-%d %H:%M:%S%.f%:z",
    "%Y-%m-%dT%H:%M%:z",
    "%Y-%m-%d %H:%M%:z",
];

/// Naive datetime layouts (no offset) accepted by
/// `datetime.fromisoformat`.
const DATETIME_NAIVE_FORMATS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M",
];

/// Validates an ISO-8601 datetime — the port of pyfly's
/// `is_valid_datetime`, which delegates to `datetime.fromisoformat`
/// after rewriting a trailing `Z` to `+00:00`.
///
/// Accepts `2026-05-07T12:00:00`, a space instead of the `T`, optional
/// fractional seconds, optional `±HH:MM`/`±HHMM` offsets, a trailing
/// `Z`, minute precision (`2026-05-07T12:00`), and a bare date
/// (`2026-05-07`) — all of which `fromisoformat` accepts. Compact
/// "basic" forms (`20260507T120000`) and hour-only times, which Python
/// 3.11+ also tolerates, are deliberately not accepted. Fails with
/// reason `invalid datetime`.
pub fn validate_datetime(s: &str) -> Result<(), ValidationError> {
    // pyfly: a trailing "Z" is rewritten to "+00:00" before parsing.
    let normalised: String = match s.strip_suffix('Z') {
        Some(stripped) => format!("{stripped}+00:00"),
        None => s.to_owned(),
    };
    let ok = DATETIME_OFFSET_FORMATS
        .iter()
        .any(|fmt| chrono::DateTime::parse_from_str(&normalised, fmt).is_ok())
        || DATETIME_NAIVE_FORMATS
            .iter()
            .any(|fmt| chrono::NaiveDateTime::parse_from_str(&normalised, fmt).is_ok())
        || chrono::NaiveDate::parse_from_str(&normalised, "%Y-%m-%d").is_ok();
    if !ok {
        return Err(invalid("invalid datetime"));
    }
    Ok(())
}

/// Validates a generic national identifier — the port of pyfly's
/// `is_valid_national_id`. After stripping spaces and dashes and
/// upper-casing, the value must be 5..=20 ASCII alphanumerics. Fails
/// with reason `invalid national id`.
///
/// This is the GENERIC format check pyfly exposes for any country to
/// plug its own algebraic rules on top of; it is deliberately distinct
/// from the nation-specific [`validate_dni`]/[`validate_nie`]/
/// [`validate_nif`]/[`validate_ssn`] checksum validators this crate also
/// ships.
///
/// Deliberate divergence: pyfly's `str.isalnum()` also accepts non-ASCII
/// Unicode letters and digits; like the other ported banking predicates
/// the Rust port restricts the alphabet to ASCII, since real national
/// identifiers are ASCII in every scheme.
pub fn validate_national_id(s: &str) -> Result<(), ValidationError> {
    let normalised = s.replace([' ', '-'], "").to_uppercase();
    if !(5..=20).contains(&normalised.len())
        || normalised.is_empty()
        || !normalised.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(invalid("invalid national id"));
    }
    Ok(())
}

/// Validates a generic tax identifier — the port of pyfly's
/// `is_valid_tax_id`. After upper-casing (but with NO space/dash
/// stripping, matching pyfly's `_TAX_ID_GENERIC.match(value.upper())`),
/// the value must match `^[A-Z0-9]{3,20}$`. Fails with reason
/// `invalid tax id`.
///
/// This is the GENERIC format check pyfly exposes — distinct from the
/// EU-style [`validate_vat`] this crate also ships. Because no
/// normalisation strips separators, any space, dash, or other
/// non-`[A-Z0-9]` character causes a rejection.
pub fn validate_tax_id(s: &str) -> Result<(), ValidationError> {
    if !TAX_ID_GENERIC_RE.is_match(&s.to_uppercase()) {
        return Err(invalid("invalid tax id"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- ports of the Go test suite -----

    #[test]
    fn iban_good_and_bad() {
        let good = [
            "GB82 WEST 1234 5698 7654 32",
            "DE89370400440532013000",
            "FR1420041010050500013M02606",
            "ES9121000418450200051332",
        ];
        for s in good {
            assert!(validate_iban(s).is_ok(), "good IBAN failed ({s})");
        }
        let bad = [
            "GB82WEST12345698765431", // wrong checksum
            "AA001234567890",         // unknown country
            "DE89-3704-not-numeric",
            "",
        ];
        for s in bad {
            assert!(validate_iban(s).is_err(), "bad IBAN passed: {s:?}");
        }
    }

    #[test]
    fn bic_good_and_bad() {
        for s in ["DEUTDEFF", "deutdeff500"] {
            assert!(validate_bic(s).is_ok(), "BIC {s:?} should pass");
        }
        for s in ["DEUT", "DEUT@@FF", "1234567X"] {
            assert!(validate_bic(s).is_err(), "bad BIC passed: {s:?}");
        }
    }

    #[test]
    fn luhn_and_card() {
        validate_luhn("4539 1488 0343 6467").expect("known-good Luhn");
        assert!(
            validate_luhn("4539 1488 0343 6468").is_err(),
            "bad checksum should fail"
        );
        validate_credit_card("4539-1488-0343-6467").expect("known-good card");
        assert!(validate_credit_card("0").is_err(), "short card should fail");
    }

    #[test]
    fn e164() {
        validate_phone_e164("+34911234567").expect("valid E.164");
        assert!(
            validate_phone_e164("0034911234567").is_err(),
            "missing + should fail"
        );
    }

    #[test]
    fn currency_and_email() {
        validate_currency("EUR").expect("EUR is valid");
        assert!(validate_currency("eur").is_err(), "lowercase should fail");
        validate_email("a@b.co").expect("a@b.co is valid");
        assert!(validate_email("not-an-email").is_err(), "bad email passed");
    }

    #[test]
    fn password_policy() {
        let p = PasswordPolicy::default();
        validate_password("Hello-World-1!", p).expect("canonical strong password");
        for bad in [
            "short",
            "alllower-lower-1!",
            "ALLUPPER-UPPER-1!",
            "NoDigit-NoDigit!",
        ] {
            let err = validate_password(bad, p);
            assert!(
                matches!(err, Err(ValidationError::Invalid(_))),
                "password {bad:?} should fail"
            );
        }
    }

    #[test]
    fn sort_bsb_ssn_vat() {
        validate_sort_code("12-34-56").expect("valid sort code");
        validate_bsb("062-001").expect("valid BSB");
        validate_ssn("123-45-6789").expect("valid SSN");
        validate_vat("ES X1234567L").expect("valid VAT");
    }

    #[test]
    fn spanish_ids() {
        validate_dni("12345678Z").expect("valid DNI");
        validate_nie("X1234567L").expect("valid NIE");
        validate_nif("12345678Z").expect("valid NIF");
        assert!(validate_dni("12345678A").is_err(), "bad DNI letter passed");
    }

    #[test]
    fn error_is_invalid_and_names_validator() {
        let err = validate_iban("nope").expect_err("nope is not an IBAN");
        let ValidationError::Invalid(_) = &err;
        assert!(
            err.to_string().contains("iban"),
            "error should name validator: {err}"
        );
    }

    // ----- Rust-specific additions -----

    #[test]
    fn error_display_matches_go_wire_format() {
        let err = validate_iban("").expect_err("empty IBAN");
        assert_eq!(
            err.to_string(),
            "firefly/validators: invalid: iban: too short"
        );
        assert_eq!(err.reason(), "iban: too short");
        let err = validate_iban("nope").expect_err("NO is a known country, wrong length");
        assert_eq!(
            err.to_string(),
            "firefly/validators: invalid: iban: length 4, want 15 for NO"
        );
        let err = validate_iban("AA001234567890").expect_err("unknown country");
        assert_eq!(err.reason(), "iban: unknown country AA");
        let err = validate_iban("GB82WEST12345698765431").expect_err("checksum");
        assert_eq!(err.reason(), "iban: mod-97 mismatch");
    }

    #[test]
    fn validation_error_is_send_sync_clone_eq() {
        fn assert_bounds<T: Send + Sync + Clone + PartialEq + std::error::Error>() {}
        assert_bounds::<ValidationError>();
        let e = invalid("x");
        assert_eq!(e.clone(), e);
    }

    #[test]
    fn iban_table_is_sorted_and_lookup_works() {
        assert!(
            IBAN_COUNTRY_LENGTHS.windows(2).all(|w| w[0].0 < w[1].0),
            "table must stay sorted for binary search"
        );
        assert_eq!(iban_country_length("ES"), Some(24));
        assert_eq!(iban_country_length("DE"), Some(22));
        assert_eq!(iban_country_length("ZZ"), None);
    }

    #[test]
    fn iban_normalises_case_and_spaces() {
        validate_iban("gb82 west 1234 5698 7654 32").expect("lowercase + spaces");
    }

    #[test]
    fn bic_normalises_and_rejects_odd_lengths() {
        validate_bic("deut deff").expect("spaces stripped");
        assert!(validate_bic("DEUTDEFF5").is_err(), "9 chars invalid");
        assert!(validate_bic("DEUTDEFF50").is_err(), "10 chars invalid");
    }

    #[test]
    fn credit_card_length_boundaries() {
        // 10 zeros pass Luhn but are too short for a card; 20 too long.
        validate_luhn("0000000000").expect("zeros pass Luhn");
        assert!(validate_credit_card("0000000000").is_err(), "len 10");
        validate_credit_card("000000000000").expect("len 12 ok");
        validate_credit_card("0000000000000000000").expect("len 19 ok");
        assert!(
            validate_credit_card("00000000000000000000").is_err(),
            "len 20"
        );
        assert!(validate_luhn("4539a48803436467").is_err(), "non-digit");
    }

    #[test]
    fn e164_boundaries() {
        validate_phone_e164("+1234567").expect("8 chars minimum");
        assert!(validate_phone_e164("+123456").is_err(), "too short");
        validate_phone_e164("+123456789012345").expect("16 chars maximum");
        assert!(
            validate_phone_e164("+1234567890123456").is_err(),
            "too long"
        );
        assert!(
            validate_phone_e164("+0123456789").is_err(),
            "leading zero country"
        );
    }

    #[test]
    fn email_forms() {
        validate_email("Alice Example <alice@example.com>").expect("angle-addr form");
        validate_email("\"quoted local\"@example.com").expect("quoted local part");
        validate_email("a@b").expect("dotless domain, like net/mail");
        validate_email("a@[127.0.0.1]").expect("domain literal");
        for bad in ["a@", "@b.co", "a b@c.d", "a@@b.co", "a@b..co", "", "a@[..."] {
            assert!(validate_email(bad).is_err(), "bad email passed: {bad:?}");
        }
    }

    #[test]
    fn password_unicode_classes_and_byte_length() {
        let p = PasswordPolicy::default();
        // U+2022 BULLET is Unicode Po — counts as a symbol, like Go's IsPunct.
        validate_password("Hello•World•12", p).expect("unicode punctuation is a symbol");
        // Length is measured in bytes, matching Go's len(s).
        let policy = PasswordPolicy {
            min_length: 4,
            require_upper: false,
            require_lower: false,
            require_digit: false,
            require_symbol: false,
        };
        validate_password("ñé", policy).expect("4 bytes, 2 chars");
        assert!(validate_password("abc", policy).is_err(), "3 bytes < 4");
        let err = validate_password("x", p).expect_err("too short");
        assert_eq!(err.reason(), "password: length < 12");
        assert_eq!(
            validate_password("nodigit-nodigit!A", p)
                .expect_err("digit")
                .reason(),
            "password: needs digit"
        );
    }

    #[test]
    fn sort_code_ssn_bsb_variants() {
        validate_sort_code("123456").expect("plain sort code");
        validate_sort_code("12-3456").expect("partial dashes allowed by pattern");
        assert!(validate_sort_code("1-23456").is_err());
        validate_ssn("123456789").expect("plain SSN");
        validate_ssn("123-456789").expect("partial dashes allowed by pattern");
        assert!(validate_ssn("12-345-6789").is_err());
        validate_bsb("062001").expect("plain BSB");
        assert!(validate_bsb("06-2001").is_err());
        validate_sort_code(" 12-34-56 ").expect("surrounding whitespace trimmed");
    }

    #[test]
    fn vat_normalisation_and_bounds() {
        validate_vat("esx1234567l").expect("lowercase normalised");
        assert!(validate_vat("E1").is_err(), "missing country prefix");
        assert!(validate_vat("ES1").is_err(), "body too short");
        assert!(validate_vat("ESABCDEFGHIJKL1").is_err(), "body too long");
    }

    #[test]
    fn spanish_id_prefixes_and_normalisation() {
        validate_nie("Y1234567X").expect("Y prefix maps to 1");
        validate_nie("Z1234567R").expect("Z prefix maps to 2");
        validate_nif("X1234567L").expect("NIF accepts NIE");
        validate_dni("12345678 z").expect("spaces stripped, case folded");
        assert!(validate_nie("A1234567L").is_err(), "bad NIE prefix");
        assert!(validate_nie("X123456L").is_err(), "bad NIE length");
        assert!(validate_dni("1234567Z").is_err(), "bad DNI length");
        assert!(validate_dni("1234567aZ").is_err(), "non-digit body");
    }

    #[test]
    fn national_id_format_and_normalisation() {
        // pyfly: strip spaces and dashes, upper-case, 5..=20 alnum.
        validate_national_id("12345").expect("5 chars minimum");
        validate_national_id("ABC123XYZ").expect("mixed alnum");
        validate_national_id("12345678901234567890").expect("20 chars maximum");
        validate_national_id("123 456-789").expect("separators stripped before length check");
        validate_national_id("ab12c").expect("lowercase upper-cased then accepted");
        for bad in [
            "1234",                  // 4 chars after strip is too short
            "123456789012345678901", // 21 chars too long
            "ABC!23",                // punctuation is not alnum
            "AB 12",                 // becomes "AB12" -> 4 chars, too short
            "",                      // empty
            "    ",                  // all spaces -> empty -> too short
        ] {
            assert!(
                validate_national_id(bad).is_err(),
                "bad national id passed: {bad:?}"
            );
        }
        assert_eq!(
            validate_national_id("x").expect_err("too short").reason(),
            "invalid national id"
        );
    }

    #[test]
    fn tax_id_format_no_separator_stripping() {
        // pyfly: only upper-cases, no space/dash stripping; ^[A-Z0-9]{3,20}$.
        validate_tax_id("ABC").expect("3 chars minimum");
        validate_tax_id("us1234567z").expect("lowercase upper-cased then accepted");
        validate_tax_id("A1B2C3D4E5F6G7H8I9J0").expect("20 chars maximum");
        for bad in [
            "AB",                    // 2 chars too short
            "A1B2C3D4E5F6G7H8I9J0K", // 21 chars too long
            "AB-12",                 // dash not stripped -> fails alphabet
            "AB 12",                 // space not stripped -> fails alphabet
            "AB_12",                 // underscore not in alphabet
            "",                      // empty
        ] {
            assert!(validate_tax_id(bad).is_err(), "bad tax id passed: {bad:?}");
        }
        assert_eq!(
            validate_tax_id("AB").expect_err("too short").reason(),
            "invalid tax id"
        );
    }

    #[test]
    fn national_id_and_tax_id_render_canonical_prefix() {
        assert_eq!(
            validate_national_id("x").expect_err("bad").to_string(),
            "firefly/validators: invalid: invalid national id"
        );
        assert_eq!(
            validate_tax_id("x").expect_err("bad").to_string(),
            "firefly/validators: invalid: invalid tax id"
        );
    }

    #[test]
    fn version_stamp() {
        assert_eq!(VERSION, "26.6.4");
    }
}
