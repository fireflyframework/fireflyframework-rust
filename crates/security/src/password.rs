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

/// A [`PasswordEncoder`] backed by **Argon2id** — the Rust analog of Spring
/// Security's `Argon2PasswordEncoder` and the OWASP-preferred, memory-hard
/// alternative to bcrypt.
///
/// Hashes are self-describing PHC strings (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`)
/// carrying their algorithm, parameters, and a random 128-bit salt — so, exactly
/// like bcrypt's embedded cost, a hash produced under one parameter set still
/// verifies after the encoder is reconfigured, and two hashes of the same
/// password differ.
///
/// ```
/// use firefly_security::{Argon2PasswordEncoder, PasswordEncoder};
///
/// // Low parameters keep the doctest fast; production uses the OWASP default.
/// let encoder = Argon2PasswordEncoder::with_params(4096, 1, 1);
/// let hash = encoder.hash("s3cret").unwrap();
/// assert!(hash.starts_with("$argon2id$"));
/// assert!(encoder.verify("s3cret", &hash).unwrap());
/// assert!(!encoder.verify("wrong", &hash).unwrap());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Argon2PasswordEncoder {
    /// Memory cost in KiB (Argon2 `m`).
    m_cost: u32,
    /// Iteration (time) cost (Argon2 `t`).
    t_cost: u32,
    /// Degree of parallelism (Argon2 `p`).
    p_cost: u32,
}

impl Argon2PasswordEncoder {
    /// Creates an encoder at the `argon2` crate's OWASP-recommended default
    /// parameters (Argon2id, `m=19456` KiB, `t=2`, `p=1`) — matching the spirit
    /// of Spring's `Argon2PasswordEncoder` defaults.
    #[must_use]
    pub fn new() -> Self {
        let d = argon2::Params::DEFAULT_M_COST;
        Self {
            m_cost: d,
            t_cost: argon2::Params::DEFAULT_T_COST,
            p_cost: argon2::Params::DEFAULT_P_COST,
        }
    }

    /// Creates an encoder at explicit Argon2 parameters — `m_cost` (memory in
    /// KiB), `t_cost` (iterations), `p_cost` (parallelism). Tests use low values
    /// to stay fast; production should keep the [`new`](Self::new) defaults or
    /// higher.
    #[must_use]
    pub fn with_params(m_cost: u32, t_cost: u32, p_cost: u32) -> Self {
        Self {
            m_cost,
            t_cost,
            p_cost,
        }
    }

    /// Builds the configured `argon2::Argon2` hasher (Argon2id, v19).
    fn hasher(&self) -> Result<argon2::Argon2<'static>, SecurityError> {
        let params = argon2::Params::new(self.m_cost, self.t_cost, self.p_cost, None)
            .map_err(|e| SecurityError::verification(format!("argon2 params invalid: {e}")))?;
        Ok(argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            params,
        ))
    }
}

/// OWASP defaults via [`Argon2PasswordEncoder::new`].
impl Default for Argon2PasswordEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PasswordEncoder for Argon2PasswordEncoder {
    fn hash(&self, raw_password: &str) -> Result<String, SecurityError> {
        use argon2::password_hash::rand_core::OsRng;
        use argon2::password_hash::{PasswordHasher, SaltString};

        let salt = SaltString::generate(&mut OsRng);
        let hash = self
            .hasher()?
            .hash_password(raw_password.as_bytes(), &salt)
            .map_err(|e| SecurityError::verification(format!("argon2 hash failed: {e}")))?;
        Ok(hash.to_string())
    }

    fn verify(&self, raw_password: &str, hashed_password: &str) -> Result<bool, SecurityError> {
        use argon2::password_hash::{Error, PasswordHash, PasswordVerifier};

        // A malformed stored hash is a structural error; a correct-but-wrong
        // password is `Ok(false)` — the trait's contract, mirroring bcrypt.
        let parsed = PasswordHash::new(hashed_password)
            .map_err(|e| SecurityError::verification(format!("argon2 hash malformed: {e}")))?;
        match self
            .hasher()?
            .verify_password(raw_password.as_bytes(), &parsed)
        {
            Ok(()) => Ok(true),
            Err(Error::Password) => Ok(false),
            Err(e) => Err(SecurityError::verification(format!(
                "argon2 verify failed: {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Low parameters keep the tests fast; production uses the OWASP defaults.
    fn argon() -> Argon2PasswordEncoder {
        Argon2PasswordEncoder::with_params(4096, 1, 1)
    }

    #[test]
    fn argon2_round_trips_and_rejects_a_wrong_password() {
        let enc = argon();
        let hash = enc.hash("s3cret").expect("hash");
        assert!(hash.starts_with("$argon2id$"), "PHC string: {hash}");
        assert!(enc.verify("s3cret", &hash).expect("verify match"));
        assert!(!enc
            .verify("wrong", &hash)
            .expect("verify mismatch is Ok(false)"));
    }

    #[test]
    fn argon2_salts_each_hash() {
        let enc = argon();
        let a = enc.hash("same").expect("hash a");
        let b = enc.hash("same").expect("hash b");
        assert_ne!(
            a, b,
            "a random salt makes two hashes of one password differ"
        );
    }

    #[test]
    fn argon2_hash_self_describes_params_so_it_verifies_after_reconfigure() {
        // A hash produced under one parameter set still verifies under another,
        // because the PHC string carries its own m/t/p (like bcrypt's cost).
        let produced = Argon2PasswordEncoder::with_params(8192, 2, 1)
            .hash("portable")
            .expect("hash");
        let verifier = Argon2PasswordEncoder::with_params(4096, 1, 1);
        assert!(verifier.verify("portable", &produced).expect("verify"));
    }

    #[test]
    fn argon2_rejects_a_malformed_stored_hash() {
        let err = argon().verify("x", "not-a-phc-string");
        assert!(
            err.is_err(),
            "a malformed stored hash is a structural error"
        );
    }

    #[test]
    fn argon2_and_bcrypt_share_the_password_encoder_port() {
        // Both encoders are usable behind the same `dyn PasswordEncoder`.
        let encoders: Vec<Box<dyn PasswordEncoder>> = vec![
            Box::new(argon()),
            Box::new(BcryptPasswordEncoder::with_rounds(4)),
        ];
        for enc in &encoders {
            let hash = enc.hash("portable").expect("hash");
            assert!(enc.verify("portable", &hash).expect("verify"));
            assert!(!enc.verify("nope", &hash).expect("mismatch"));
        }
    }
}
