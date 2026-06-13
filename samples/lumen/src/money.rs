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

//! The [`Money`] value object — the heart of Lumen's domain model (book
//! chapter 6, "Domain-Driven Design").
//!
//! A value object is defined entirely by its attributes (it has no identity)
//! and is **immutable**: every arithmetic operation returns a *new* `Money`
//! rather than mutating in place. Amounts are stored as integer **minor
//! units** (cents) so the math is exact — no binary floating-point drift in
//! money arithmetic, the classic correctness bug a value object is built to
//! prevent.
//!
//! ```
//! use firefly_sample_lumen::money::Money;
//!
//! let balance = Money::cents(1_000);            // €10.00
//! let after = balance.add(Money::cents(250));   // €12.50
//! assert_eq!(after.cents_value(), 1_250);
//! assert_eq!(after.to_string(), "12.50");
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

/// An exact monetary amount, expressed in integer **minor units** (cents).
///
/// `Money` is the textbook value object: immutable, compared by value, and
/// closed under the operations a wallet needs (`add` / `subtract`). It
/// serialises on the wire as the plain integer cent count, so a balance of
/// €10.00 is the JSON number `1000` — the contract the read model and event
/// payloads share.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money {
    /// The amount in minor units (cents). Kept private so the only way to a
    /// `Money` is through the validating constructors.
    cents: i64,
}

/// The typed error a `Money` operation can fail with — a non-positive amount
/// where the domain requires a strictly positive one, or an arithmetic
/// underflow (subtracting more than is held).
///
/// `Display` + `std::error::Error` are hand-written rather than derived with
/// `thiserror`, so Lumen keeps its one-Firefly-dependency promise (only
/// `firefly` + `axum` + `serde`) — the book makes a point of this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoneyError {
    /// An amount was expected to be strictly positive (`> 0`) but was not.
    NonPositive,
    /// A subtraction would drop the balance below zero.
    Overdraw,
}

impl fmt::Display for MoneyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoneyError::NonPositive => f.write_str("amount must be positive"),
            MoneyError::Overdraw => f.write_str("amount exceeds balance"),
        }
    }
}

impl std::error::Error for MoneyError {}

impl Money {
    /// A zero amount — the opening balance of a brand-new wallet.
    pub const ZERO: Money = Money { cents: 0 };

    /// Builds a `Money` from a raw minor-unit (cent) count. Any value is
    /// accepted, including zero and (for internal folding) negatives; the
    /// *domain* rules about positivity live on the wallet commands, not here.
    pub const fn cents(cents: i64) -> Self {
        Money { cents }
    }

    /// Builds a `Money` from a whole-currency unit count (e.g. `from_units(10)`
    /// is €10.00).
    pub const fn from_units(units: i64) -> Self {
        Money { cents: units * 100 }
    }

    /// The amount in minor units (cents) — the wire representation.
    pub const fn cents_value(self) -> i64 {
        self.cents
    }

    /// Whether this amount is strictly positive (`> 0`) — the predicate the
    /// deposit / withdraw commands require.
    pub const fn is_positive(self) -> bool {
        self.cents > 0
    }

    /// Whether this amount is zero.
    pub const fn is_zero(self) -> bool {
        self.cents == 0
    }

    /// Returns a new `Money` that is `self + other` (immutable addition).
    #[must_use]
    pub const fn add(self, other: Money) -> Money {
        Money {
            cents: self.cents + other.cents,
        }
    }

    /// Returns `self - other`, or [`MoneyError::Overdraw`] if that would go
    /// below zero — the invariant that protects a wallet from overdrawing.
    pub fn subtract(self, other: Money) -> Result<Money, MoneyError> {
        if other.cents > self.cents {
            return Err(MoneyError::Overdraw);
        }
        Ok(Money {
            cents: self.cents - other.cents,
        })
    }

    /// Validates that this amount is strictly positive, returning it
    /// unchanged on success — the guard every mutating wallet command runs
    /// before raising an event.
    pub fn require_positive(self) -> Result<Money, MoneyError> {
        if self.is_positive() {
            Ok(self)
        } else {
            Err(MoneyError::NonPositive)
        }
    }
}

impl fmt::Display for Money {
    /// Renders the amount as a fixed two-decimal major-unit string
    /// (`1250` cents → `"12.50"`), the human-readable form used in logs and
    /// the banner.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.cents < 0 { "-" } else { "" };
        let abs = self.cents.abs();
        write!(f, "{sign}{}.{:02}", abs / 100, abs % 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_and_units_constructors_agree() {
        assert_eq!(Money::from_units(10), Money::cents(1_000));
        assert_eq!(Money::ZERO, Money::cents(0));
        assert!(Money::ZERO.is_zero());
    }

    #[test]
    fn add_is_immutable_and_exact() {
        let a = Money::cents(1_000);
        let b = a.add(Money::cents(250));
        assert_eq!(a.cents_value(), 1_000, "operand is unchanged");
        assert_eq!(b.cents_value(), 1_250);
    }

    #[test]
    fn subtract_guards_against_overdraw() {
        let a = Money::cents(100);
        assert_eq!(a.subtract(Money::cents(40)).unwrap(), Money::cents(60));
        assert_eq!(
            a.subtract(Money::cents(101)).unwrap_err(),
            MoneyError::Overdraw
        );
    }

    #[test]
    fn require_positive_rejects_zero_and_negative() {
        assert!(Money::cents(1).require_positive().is_ok());
        assert_eq!(
            Money::ZERO.require_positive().unwrap_err(),
            MoneyError::NonPositive
        );
        assert_eq!(
            Money::cents(-5).require_positive().unwrap_err(),
            MoneyError::NonPositive
        );
    }

    #[test]
    fn display_is_two_decimal_major_units() {
        assert_eq!(Money::cents(1_250).to_string(), "12.50");
        assert_eq!(Money::cents(5).to_string(), "0.05");
        assert_eq!(Money::cents(-1_250).to_string(), "-12.50");
    }

    #[test]
    fn serialises_as_a_bare_cent_integer() {
        assert_eq!(serde_json::to_string(&Money::cents(1_250)).unwrap(), "1250");
        let back: Money = serde_json::from_str("1250").unwrap();
        assert_eq!(back, Money::cents(1_250));
    }
}
