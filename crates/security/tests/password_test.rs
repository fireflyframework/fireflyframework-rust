//! Port of pyfly's `tests/security/test_password.py`
//! (`TestBcryptPasswordEncoder`) — the standalone `PasswordEncoder` /
//! `BcryptPasswordEncoder` primitive. Uses `rounds=4` throughout so the
//! suite stays fast (bcrypt cost is exponential).

use firefly_security::{BcryptPasswordEncoder, PasswordEncoder, DEFAULT_ROUNDS};

#[test]
fn hash_produces_bcrypt_output() {
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    let hashed = encoder.hash("my-secret-password").unwrap();
    // The `bcrypt` crate emits the `$2b$` identifier, like pyfly.
    assert!(hashed.starts_with("$2b$"), "hash was: {hashed}");
}

#[test]
fn verify_correct_password() {
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    let hashed = encoder.hash("correct-password").unwrap();
    assert!(encoder.verify("correct-password", &hashed).unwrap());
}

#[test]
fn verify_wrong_password() {
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    let hashed = encoder.hash("correct-password").unwrap();
    assert!(!encoder.verify("wrong-password", &hashed).unwrap());
}

#[test]
fn different_passwords_different_hashes() {
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    let hash1 = encoder.hash("same-password").unwrap();
    let hash2 = encoder.hash("same-password").unwrap();
    // Distinct per-hash salts make the two encodings differ.
    assert_ne!(hash1, hash2);
}

#[test]
fn custom_rounds() {
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    let hashed = encoder.hash("test").unwrap();
    assert!(hashed.contains("$04$"), "hash was: {hashed}");
    assert!(encoder.verify("test", &hashed).unwrap());
}

#[test]
fn empty_password_hashes() {
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    let hashed = encoder.hash("").unwrap();
    assert!(hashed.starts_with("$2b$"), "hash was: {hashed}");
    assert!(encoder.verify("", &hashed).unwrap());
    assert!(!encoder.verify("non-empty", &hashed).unwrap());
}

#[test]
fn default_uses_default_rounds() {
    let encoder = BcryptPasswordEncoder::default();
    assert_eq!(encoder.rounds(), DEFAULT_ROUNDS);
    assert_eq!(DEFAULT_ROUNDS, 12);
    assert_eq!(BcryptPasswordEncoder::new().rounds(), 12);
}

#[test]
fn used_via_trait_object() {
    // The encoder is reusable through the port (e.g. injected into a
    // custom user store), matching pyfly's protocol-conformance test.
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    let dyn_encoder: &dyn PasswordEncoder = &encoder;
    let hashed = dyn_encoder.hash("via-trait").unwrap();
    assert!(dyn_encoder.verify("via-trait", &hashed).unwrap());
}

#[test]
fn verify_rejects_malformed_hash() {
    // A correct-but-mismatching password is `Ok(false)`; a structurally
    // invalid stored hash is an error (Rust surfaces it as a value
    // rather than pyfly's exception).
    let encoder = BcryptPasswordEncoder::with_rounds(4);
    assert!(encoder.verify("anything", "not-a-bcrypt-hash").is_err());
}

#[test]
fn cross_round_hash_still_verifies() {
    // A hash carries its own cost, so it verifies regardless of the
    // encoder's currently-configured rounds.
    let writer = BcryptPasswordEncoder::with_rounds(4);
    let hashed = writer.hash("portable").unwrap();
    let reader = BcryptPasswordEncoder::with_rounds(6);
    assert!(reader.verify("portable", &hashed).unwrap());
}
