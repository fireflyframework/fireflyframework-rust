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

//! Ports of the pyfly validation contract for the generic identifier
//! predicates added at pyfly parity: national id and tax id.
//!
//! pyfly ships these without dedicated test classes, so behavior is
//! pinned by `src/pyfly/validation/domain.py`:
//! - `is_valid_national_id` (L249-254): strip spaces and dashes,
//!   upper-case, then 5..=20 `isalnum()`.
//! - `is_valid_tax_id` (L202-203): upper-case only (no separator
//!   stripping), then match `_TAX_ID_GENERIC = ^[A-Z0-9]{3,20}$`.
//!
//! These are GENERIC format checks, distinct from the nation-specific
//! `validate_dni`/`validate_nie`/`validate_nif`/`validate_ssn`/
//! `validate_vat` validators the crate also ships.

use firefly_validators::{validate_national_id, validate_tax_id};

// ----- national id (pinned by pyfly is_valid_national_id) -----

#[test]
fn national_id_length_bounds() {
    // 5..=20 alnum after normalization.
    validate_national_id("12345").expect("5 chars minimum");
    validate_national_id("12345678901234567890").expect("20 chars maximum");
    assert!(
        validate_national_id("1234").is_err(),
        "4 chars below minimum"
    );
    assert!(
        validate_national_id("123456789012345678901").is_err(),
        "21 chars above maximum"
    );
}

#[test]
fn national_id_strips_spaces_and_dashes_then_upper_cases() {
    // pyfly: value.replace(" ", "").replace("-", "").upper()
    validate_national_id("123-456-789").expect("dashes stripped");
    validate_national_id("12 34 56").expect("spaces stripped");
    validate_national_id("ab-12c").expect("lowercase upper-cased, dashes stripped");
    // Stripping happens BEFORE the length check, so a short core fails.
    assert!(
        validate_national_id("AB 12").is_err(),
        "becomes AB12 (4 chars) which is too short"
    );
}

#[test]
fn national_id_rejects_non_alnum_and_empty() {
    for bad in ["ABC!23", "12.345", "abc 1@", "", "   ", "-----"] {
        assert!(
            validate_national_id(bad).is_err(),
            "bad national id passed: {bad:?}"
        );
    }
}

#[test]
fn national_id_error_reason_matches_pyfly() {
    assert_eq!(
        validate_national_id("x").expect_err("too short").reason(),
        "invalid national id"
    );
}

// ----- tax id (pinned by pyfly is_valid_tax_id) -----

#[test]
fn tax_id_length_bounds() {
    // ^[A-Z0-9]{3,20}$ after upper-casing.
    validate_tax_id("ABC").expect("3 chars minimum");
    validate_tax_id("A1B2C3D4E5F6G7H8I9J0").expect("20 chars maximum");
    assert!(validate_tax_id("AB").is_err(), "2 chars below minimum");
    assert!(
        validate_tax_id("A1B2C3D4E5F6G7H8I9J0K").is_err(),
        "21 chars above maximum"
    );
}

#[test]
fn tax_id_upper_cases_but_does_not_strip_separators() {
    // pyfly: _TAX_ID_GENERIC.match(value.upper()) — no space/dash removal.
    validate_tax_id("us1234567z").expect("lowercase upper-cased then accepted");
    validate_tax_id("ESA12345678").expect("alnum upper passes");
    for bad in ["AB-12", "AB 12", "12.34", "AB_12", "A/B/C"] {
        assert!(
            validate_tax_id(bad).is_err(),
            "separator not stripped, should fail: {bad:?}"
        );
    }
}

#[test]
fn tax_id_rejects_empty() {
    assert!(
        validate_tax_id("").is_err(),
        "empty fails the {{3,20}} regex"
    );
}

#[test]
fn tax_id_error_reason_matches_pyfly() {
    assert_eq!(
        validate_tax_id("AB").expect_err("too short").reason(),
        "invalid tax id"
    );
}

// ----- wire format stays the crate-canonical Display -----

#[test]
fn identifier_reasons_render_with_canonical_prefix() {
    assert_eq!(
        validate_national_id("x").expect_err("bad").to_string(),
        "firefly/validators: invalid: invalid national id"
    );
    assert_eq!(
        validate_tax_id("x").expect_err("bad").to_string(),
        "firefly/validators: invalid: invalid tax id"
    );
}
