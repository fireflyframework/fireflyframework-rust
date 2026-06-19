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

/// The default encoder id used by [`DelegatingPasswordEncoder`] — Spring's
/// `{bcrypt}` default.
pub const DEFAULT_PASSWORD_ENCODER_ID: &str = "bcrypt";

/// A no-op (plaintext) [`PasswordEncoder`] — Spring's `{noop}`. It stores and
/// compares passwords verbatim, so it is for **tests / local development only**;
/// never use it for real credentials.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpPasswordEncoder;

impl PasswordEncoder for NoOpPasswordEncoder {
    fn hash(&self, raw_password: &str) -> Result<String, SecurityError> {
        Ok(raw_password.to_owned())
    }

    fn verify(&self, raw_password: &str, hashed_password: &str) -> Result<bool, SecurityError> {
        Ok(raw_password == hashed_password)
    }
}

/// Parses a Spring `{id}encoded` storage string into `(id, encoded)`; `None`
/// when there is no leading `{id}` prefix (a legacy / bare hash).
fn parse_encoder_id(stored: &str) -> Option<(&str, &str)> {
    let rest = stored.strip_prefix('{')?;
    let close = rest.find('}')?;
    Some((&rest[..close], &rest[close + 1..]))
}

/// An `{id}`-prefixed multi-encoder — the Rust analog of Spring Security's
/// `DelegatingPasswordEncoder` (`PasswordEncoderFactories.createDelegating…`),
/// the **recommended** password-storage format.
///
/// * [`hash`](PasswordEncoder::hash) encodes with the configured default and
///   prefixes the result with `{id}` (e.g. `{bcrypt}$2b$…`).
/// * [`verify`](PasswordEncoder::verify) reads the `{id}` and delegates to the
///   matching encoder; an unprefixed (legacy) hash is verified by the optional
///   `unprefixed` fallback (Spring's `defaultPasswordEncoderForMatches`), or
///   rejected when none is set.
/// * [`upgrade_encoding`](Self::upgrade_encoding) reports whether a stored hash
///   should be re-encoded on next login — true when its `{id}` differs from the
///   current default, or it is unprefixed — so an application can transparently
///   migrate `{argon2}` / legacy hashes to the current default.
pub struct DelegatingPasswordEncoder {
    default_id: String,
    encoders: std::collections::HashMap<String, Box<dyn PasswordEncoder + Send + Sync>>,
    unprefixed: Option<Box<dyn PasswordEncoder + Send + Sync>>,
}

impl std::fmt::Debug for DelegatingPasswordEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegatingPasswordEncoder")
            .field("default_id", &self.default_id)
            .field("ids", &self.encoders.keys().collect::<Vec<_>>())
            .field("unprefixed", &self.unprefixed.is_some())
            .finish()
    }
}

impl DelegatingPasswordEncoder {
    /// Builds with an explicit default id and encoder map (no unprefixed
    /// fallback). The `default_id` must be a key of `encoders`.
    #[must_use]
    pub fn new(
        default_id: impl Into<String>,
        encoders: std::collections::HashMap<String, Box<dyn PasswordEncoder + Send + Sync>>,
    ) -> Self {
        Self {
            default_id: default_id.into(),
            encoders,
            unprefixed: None,
        }
    }

    /// The Spring-default factory: `{bcrypt}` (default) + `{argon2}` + `{noop}`
    /// recognised, with legacy *unprefixed* hashes verified as bcrypt — so a
    /// store of bare `$2b$…` hashes migrates seamlessly to the prefixed format.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut encoders: std::collections::HashMap<
            String,
            Box<dyn PasswordEncoder + Send + Sync>,
        > = std::collections::HashMap::new();
        encoders.insert("bcrypt".into(), Box::new(BcryptPasswordEncoder::new()));
        encoders.insert("argon2".into(), Box::new(Argon2PasswordEncoder::new()));
        encoders.insert("noop".into(), Box::new(NoOpPasswordEncoder));
        Self {
            default_id: DEFAULT_PASSWORD_ENCODER_ID.to_owned(),
            encoders,
            unprefixed: Some(Box::new(BcryptPasswordEncoder::new())),
        }
    }

    /// Sets the encoder used to verify legacy *unprefixed* hashes (Spring's
    /// `setDefaultPasswordEncoderForMatches`); `None` rejects unprefixed hashes.
    #[must_use]
    pub fn with_unprefixed(
        mut self,
        encoder: Option<Box<dyn PasswordEncoder + Send + Sync>>,
    ) -> Self {
        self.unprefixed = encoder;
        self
    }

    /// Whether `stored` should be re-encoded on next login: `true` when its
    /// `{id}` differs from the current default, or it has no `{id}` prefix
    /// (a legacy hash). Mirrors Spring's `upgradeEncoding`.
    #[must_use]
    pub fn upgrade_encoding(&self, stored: &str) -> bool {
        match parse_encoder_id(stored) {
            Some((id, _)) => id != self.default_id,
            None => true,
        }
    }
}

impl PasswordEncoder for DelegatingPasswordEncoder {
    fn hash(&self, raw_password: &str) -> Result<String, SecurityError> {
        let encoder = self.encoders.get(&self.default_id).ok_or_else(|| {
            SecurityError::verification(format!(
                "DelegatingPasswordEncoder: no encoder for default id {{{}}}",
                self.default_id
            ))
        })?;
        Ok(format!(
            "{{{}}}{}",
            self.default_id,
            encoder.hash(raw_password)?
        ))
    }

    fn verify(&self, raw_password: &str, hashed_password: &str) -> Result<bool, SecurityError> {
        match parse_encoder_id(hashed_password) {
            Some((id, encoded)) => {
                let encoder = self.encoders.get(id).ok_or_else(|| {
                    SecurityError::verification(format!(
                        "DelegatingPasswordEncoder: no encoder for id {{{id}}}"
                    ))
                })?;
                encoder.verify(raw_password, encoded)
            }
            None => match &self.unprefixed {
                Some(encoder) => encoder.verify(raw_password, hashed_password),
                None => Err(SecurityError::verification(
                    "DelegatingPasswordEncoder: stored hash has no {id} prefix",
                )),
            },
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

    fn fast_delegating() -> DelegatingPasswordEncoder {
        let mut encoders: std::collections::HashMap<
            String,
            Box<dyn PasswordEncoder + Send + Sync>,
        > = std::collections::HashMap::new();
        encoders.insert(
            "bcrypt".into(),
            Box::new(BcryptPasswordEncoder::with_rounds(4)),
        );
        encoders.insert(
            "argon2".into(),
            Box::new(Argon2PasswordEncoder::with_params(4096, 1, 1)),
        );
        encoders.insert("noop".into(), Box::new(NoOpPasswordEncoder));
        DelegatingPasswordEncoder::new("bcrypt", encoders)
            .with_unprefixed(Some(Box::new(BcryptPasswordEncoder::with_rounds(4))))
    }

    #[test]
    fn delegating_prefixes_default_and_round_trips() {
        let enc = fast_delegating();
        let h = enc.hash("s3cret").unwrap();
        assert!(h.starts_with("{bcrypt}$2b$"), "{h}");
        assert!(enc.verify("s3cret", &h).unwrap());
        assert!(!enc.verify("wrong", &h).unwrap());
        // A default-encoded hash needs no upgrade.
        assert!(!enc.upgrade_encoding(&h));
    }

    #[test]
    fn delegating_verifies_argon2_and_flags_it_for_upgrade() {
        let enc = fast_delegating();
        let ah = format!("{{argon2}}{}", argon().hash("s3cret").unwrap());
        assert!(enc.verify("s3cret", &ah).unwrap());
        // {argon2} != default {bcrypt} -> re-encode on next login.
        assert!(enc.upgrade_encoding(&ah));
    }

    #[test]
    fn delegating_verifies_legacy_unprefixed_via_fallback_and_upgrades() {
        let enc = fast_delegating();
        let bare = BcryptPasswordEncoder::with_rounds(4)
            .hash("s3cret")
            .unwrap();
        assert!(!bare.starts_with('{'));
        assert!(enc.verify("s3cret", &bare).unwrap());
        assert!(enc.upgrade_encoding(&bare));
    }

    #[test]
    fn delegating_noop_and_unknown_id() {
        let enc = fast_delegating();
        assert!(enc.verify("plain", "{noop}plain").unwrap());
        assert!(!enc.verify("plain", "{noop}other").unwrap());
        // Unknown {id} is a structural error.
        assert!(enc.verify("x", "{pbkdf2}whatever").is_err());
        // No fallback -> unprefixed rejected.
        let strict = DelegatingPasswordEncoder::new("noop", {
            let mut m: std::collections::HashMap<String, Box<dyn PasswordEncoder + Send + Sync>> =
                std::collections::HashMap::new();
            m.insert("noop".into(), Box::new(NoOpPasswordEncoder));
            m
        });
        assert!(strict.verify("x", "bare-no-prefix").is_err());
    }

    #[test]
    fn with_defaults_upgrade_logic() {
        let enc = DelegatingPasswordEncoder::with_defaults();
        assert!(enc.upgrade_encoding("{argon2}$argon2id$v=19$m=19456,t=2,p=1$abc$def"));
        assert!(enc.upgrade_encoding("$2b$12$bareLegacyHashValue000000000000000000000000"));
        assert!(!enc.upgrade_encoding("{bcrypt}$2b$12$x"));
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
