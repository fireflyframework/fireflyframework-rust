//! Standalone, reusable password-encoder primitive — the Rust port of
//! pyfly's `pyfly.security.password` (`PasswordEncoder` protocol +
//! `BcryptPasswordEncoder`).
//!
//! Where bcrypt hashing previously lived buried inside the
//! `firefly-idp-internal-db` adapter, this module surfaces it as a
//! framework primitive any application can use independently of an IdP —
//! a worker, a custom user store, a credential-rotation job.
//!
//! ```
//! use firefly_security::{BcryptPasswordEncoder, PasswordEncoder};
//!
//! // Low work factor keeps the doctest fast; production uses the
//! // default (12).
//! let encoder = BcryptPasswordEncoder::with_rounds(4);
//! let hash = encoder.hash("s3cret").unwrap();
//! assert!(encoder.verify("s3cret", &hash).unwrap());
//! assert!(!encoder.verify("wrong", &hash).unwrap());
//! ```

use crate::SecurityError;

/// Default bcrypt work factor — matches pyfly's `BcryptPasswordEncoder`
/// default of `rounds=12` (and the `bcrypt` crate's `DEFAULT_COST`).
pub const DEFAULT_ROUNDS: u32 = 12;

/// Port for password hashing and verification — the Rust analog of
/// pyfly's `PasswordEncoder` protocol.
///
/// Both methods return a [`Result`] (rather than pyfly's bare `str` /
/// `bool`) because Rust bcrypt surfaces structural errors (a malformed
/// stored hash, an invalid cost) as values rather than exceptions.
pub trait PasswordEncoder {
    /// Hashes a raw password, returning the encoded hash string
    /// (`$2b$<cost>$…`).
    ///
    /// # Errors
    /// Returns a [`SecurityError`] if the underlying hash routine fails
    /// (e.g. an out-of-range cost).
    fn hash(&self, raw_password: &str) -> Result<String, SecurityError>;

    /// Verifies a raw password against a previously produced hash.
    ///
    /// # Errors
    /// Returns a [`SecurityError`] only when `hashed_password` is not a
    /// well-formed bcrypt hash. A correct-but-mismatching password
    /// returns `Ok(false)`, not an error.
    fn verify(&self, raw_password: &str, hashed_password: &str) -> Result<bool, SecurityError>;
}

/// A [`PasswordEncoder`] backed by bcrypt — the Rust port of pyfly's
/// `BcryptPasswordEncoder`.
///
/// Hashes carry their own cost and 128-bit salt, so a hash produced at
/// one cost still verifies after the encoder's configured cost changes,
/// and two hashes of the same password differ. Wire-compatible with the
/// `firefly-idp-internal-db` adapter and the Go/Java/.NET ports, which
/// all use the same `$2b$` bcrypt format.
#[derive(Debug, Clone, Copy)]
pub struct BcryptPasswordEncoder {
    rounds: u32,
}

impl BcryptPasswordEncoder {
    /// Creates an encoder at the [`DEFAULT_ROUNDS`] work factor (12),
    /// matching pyfly's `BcryptPasswordEncoder()` default.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rounds: DEFAULT_ROUNDS,
        }
    }

    /// Creates an encoder at an explicit bcrypt work factor — pyfly's
    /// `BcryptPasswordEncoder(rounds=…)`. Tests use a low value (e.g.
    /// `4`) to stay fast.
    #[must_use]
    pub fn with_rounds(rounds: u32) -> Self {
        Self { rounds }
    }

    /// Returns the configured work factor.
    #[must_use]
    pub fn rounds(&self) -> u32 {
        self.rounds
    }
}

/// pyfly default: [`DEFAULT_ROUNDS`].
impl Default for BcryptPasswordEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PasswordEncoder for BcryptPasswordEncoder {
    fn hash(&self, raw_password: &str) -> Result<String, SecurityError> {
        bcrypt::hash(raw_password, self.rounds)
            .map_err(|e| SecurityError::verification(format!("bcrypt hash failed: {e}")))
    }

    fn verify(&self, raw_password: &str, hashed_password: &str) -> Result<bool, SecurityError> {
        bcrypt::verify(raw_password, hashed_password)
            .map_err(|e| SecurityError::verification(format!("bcrypt verify failed: {e}")))
    }
}
