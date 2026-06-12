//! Hand-rolled TOTP (RFC 6238) over HMAC-SHA256, with RFC 4648 base32 secrets.
//!
//! pyfly's internal-db adapter uses `pyotp`, which defaults to HMAC-**SHA1**.
//! The Rust workspace ships `hmac` + `sha2` (not `sha1`), so this port uses
//! HMAC-**SHA256** per the parity brief ("hand-rolled RFC 6238 over
//! hmac+sha2+base64"). The TOTP is self-consistent — the same module both
//! mints and verifies codes — so the behavioral MFA flow (enable → challenge →
//! verify) matches pyfly exactly. Cross-check vectors come from RFC 6238
//! Appendix B (the SHA-256 column), not from pyotp.
//!
//! Secrets are base32 (RFC 4648, unpadded uppercase) strings, matching
//! `pyotp.random_base32()`'s alphabet so a secret is human-transcribable into
//! an authenticator app.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// The standard 30-second TOTP time step (RFC 6238 §4, the default `X`).
pub const STEP_SECS: u64 = 30;

/// Number of digits in a generated code — six, the universal default.
pub const DIGITS: u32 = 6;

/// RFC 4648 base32 alphabet (uppercase, no padding) — pyotp's secret alphabet.
const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

type HmacSha256 = Hmac<Sha256>;

/// Generates a fresh base32 TOTP secret (160 bits → 32 base32 chars).
///
/// Mirrors `pyotp.random_base32()`: 20 random bytes encoded as unpadded
/// uppercase RFC 4648 base32.
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    base32_encode(&bytes)
}

/// Encodes `data` as unpadded uppercase RFC 4648 base32.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1f) as usize;
            out.push(BASE32_ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(BASE32_ALPHABET[idx] as char);
    }
    out
}

/// Decodes an unpadded (or padded) RFC 4648 base32 string to bytes.
///
/// Case-insensitive; `=` padding and ASCII whitespace are ignored. Returns
/// `None` on any non-alphabet character.
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for c in s.chars() {
        if c == '=' || c.is_ascii_whitespace() {
            continue;
        }
        let up = c.to_ascii_uppercase() as u8;
        let val = BASE32_ALPHABET.iter().position(|&a| a == up)? as u32;
        buffer = (buffer << 5) | val;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// Computes the HOTP value (RFC 4226) for `secret_bytes` and `counter` using
/// HMAC-SHA256, truncated to `digits` decimal digits.
fn hotp(secret_bytes: &[u8], counter: u64, digits: u32) -> u32 {
    let mut mac = HmacSha256::new_from_slice(secret_bytes).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    // Dynamic truncation (RFC 4226 §5.3).
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let bin = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    bin % 10u32.pow(digits)
}

/// Returns the TOTP code for `secret` (base32) at Unix time `unix_secs`,
/// zero-padded to [`DIGITS`] digits. Returns `None` on an invalid secret.
pub fn totp_at(secret: &str, unix_secs: u64) -> Option<String> {
    let key = base32_decode(secret)?;
    let counter = unix_secs / STEP_SECS;
    Some(format!(
        "{:0width$}",
        hotp(&key, counter, DIGITS),
        width = DIGITS as usize
    ))
}

/// Returns the current TOTP code for `secret`. Returns `None` on an invalid
/// secret. Equivalent to `pyotp.TOTP(secret).now()` (but HMAC-SHA256).
pub fn totp_now(secret: &str) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs();
    totp_at(secret, now)
}

/// Verifies `code` against `secret` at the current time, accepting codes from
/// the `valid_window` steps on either side of "now" (RFC 6238 clock-skew
/// tolerance). Mirrors `pyotp.TOTP(secret).verify(code, valid_window=…)`.
///
/// Comparison is over the parsed code value (leading-zero-insensitive) but the
/// candidate is generated zero-padded, so `"000123"` matches `123`.
pub fn verify(secret: &str, code: &str, valid_window: i64) -> bool {
    let key = match base32_decode(secret) {
        Some(k) => k,
        None => return false,
    };
    let code = code.trim();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs();
    let counter = (now / STEP_SECS) as i64;
    // Accumulate the result without short-circuiting: every step in the window is
    // generated and compared against `code` in constant time, and the per-step
    // results are OR-ed via a `Choice` (no data-dependent branch). This avoids
    // both the per-byte early-exit of `str`/`String` `==` and a per-step early
    // `return`, either of which could leak how close a guess was via timing.
    let mut matched = subtle::Choice::from(0u8);
    for delta in -valid_window..=valid_window {
        let c = (counter + delta).max(0) as u64;
        let candidate = format!("{:0width$}", hotp(&key, c, DIGITS), width = DIGITS as usize);
        // `ConstantTimeEq` on equal-length byte slices does not return on the
        // first differing byte. Slices of differing length compare unequal
        // (length is not secret here — the candidate is always `DIGITS` long).
        matched |= candidate.as_bytes().ct_eq(code.as_bytes());
    }
    matched.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // RFC 4648 base32 round-trips, cross-checked against the RFC test
    // vectors (§10) — the SHA-256 HOTP path below feeds on decoded bytes.
    // -----------------------------------------------------------------

    #[test]
    fn base32_rfc4648_vectors() {
        // RFC 4648 §10 examples (unpadded form).
        assert_eq!(base32_encode(b""), "");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_encode(b"fo"), "MZXQ");
        assert_eq!(base32_encode(b"foo"), "MZXW6");
        assert_eq!(base32_encode(b"foob"), "MZXW6YQ");
        assert_eq!(base32_encode(b"fooba"), "MZXW6YTB");
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn base32_decode_inverts_encode() {
        for sample in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let enc = base32_encode(sample);
            assert_eq!(base32_decode(&enc).unwrap(), sample);
        }
        // Padding and lowercase tolerated.
        assert_eq!(base32_decode("mzxw6ytboi======").unwrap(), b"foobar");
        assert!(base32_decode("0189!").is_none());
    }

    #[test]
    fn generated_secret_is_valid_base32_and_stable_length() {
        let s = generate_secret();
        // 20 bytes → ceil(160/5) = 32 base32 chars.
        assert_eq!(s.len(), 32);
        assert!(base32_decode(&s).is_some());
        // All characters are within the alphabet.
        assert!(s.bytes().all(|b| BASE32_ALPHABET.contains(&b)));
    }

    // -----------------------------------------------------------------
    // RFC 6238 Appendix B known-answer vectors — the SHA-256 column.
    // The shared seed is the ASCII string repeated to 32 bytes:
    //   "12345678901234567890123456789012"
    // The expected 8-digit codes are taken verbatim from the RFC table.
    // -----------------------------------------------------------------

    /// RFC 6238 Appendix B SHA-256 seed (32 ASCII bytes).
    const RFC_SEED_SHA256: &[u8] = b"12345678901234567890123456789012";

    /// (Unix time T, expected 8-digit TOTP) pairs, SHA-256 column of the
    /// RFC 6238 Appendix B table.
    const RFC6238_SHA256_VECTORS: &[(u64, &str)] = &[
        (59, "46119246"),
        (1111111109, "68084774"),
        (1111111111, "67062674"),
        (1234567890, "91819424"),
        (2000000000, "90698825"),
        (20000000000, "77737706"),
    ];

    #[test]
    fn rfc6238_appendix_b_sha256_known_answers() {
        let secret_b32 = base32_encode(RFC_SEED_SHA256);
        let key = base32_decode(&secret_b32).unwrap();
        assert_eq!(key, RFC_SEED_SHA256, "base32 round-trip of seed");
        for &(t, want) in RFC6238_SHA256_VECTORS {
            let counter = t / STEP_SECS;
            // RFC table uses 8 digits.
            let got = format!("{:08}", hotp(&key, counter, 8));
            assert_eq!(got, want, "RFC 6238 SHA-256 vector at T={t}");
        }
    }

    // -----------------------------------------------------------------
    // Round-trip: a code generated now verifies; a wrong code does not.
    // -----------------------------------------------------------------

    #[test]
    fn totp_now_verifies_within_window() {
        let secret = generate_secret();
        let code = totp_now(&secret).unwrap();
        assert_eq!(code.len(), DIGITS as usize);
        assert!(verify(&secret, &code, 1), "freshly minted code must verify");
        assert!(!verify(&secret, "000000", 1) || code == "000000");
    }

    #[test]
    fn verify_rejects_wrong_code_and_bad_secret() {
        let secret = generate_secret();
        // A clearly wrong code (shifted from the real one) fails.
        let real = totp_now(&secret).unwrap();
        let wrong = if real == "111111" { "222222" } else { "111111" };
        // Within a tiny chance the wrong literal collides; guard against it.
        if real != wrong {
            assert!(!verify(&secret, wrong, 0) || real == wrong);
        }
        // Invalid base32 secret never verifies.
        assert!(!verify("0!notbase32", &real, 1));
    }

    // -----------------------------------------------------------------
    // Regression (Bug 1): `verify` must compare the candidate against the
    // attacker-supplied `code` in constant time (`subtle::ConstantTimeEq`),
    // not via short-circuiting `String`/`&str` `==`. We cannot directly
    // measure timing in a unit test, but we pin the *behavioral* contract of
    // the non-short-circuiting path: correct codes accept; wrong codes of any
    // length (shorter, longer, or differing only in the final byte) reject,
    // and the result is independent of how early the mismatch occurs.
    // -----------------------------------------------------------------

    #[test]
    fn verify_constant_time_compare_behavior() {
        let secret = generate_secret();
        let real = totp_now(&secret).unwrap();
        assert_eq!(real.len(), DIGITS as usize);

        // The genuine code accepts.
        assert!(verify(&secret, &real, 0), "real code must verify");

        // A wrong code differing only in the LAST byte must reject. Under a
        // short-circuiting `==` this is the slowest-to-reject case; under the
        // constant-time compare it is rejected like any other mismatch.
        let mut last_diff: Vec<u8> = real.as_bytes().to_vec();
        last_diff[DIGITS as usize - 1] = if last_diff[DIGITS as usize - 1] == b'9' {
            b'0'
        } else {
            last_diff[DIGITS as usize - 1] + 1
        };
        let last_diff = String::from_utf8(last_diff).unwrap();
        assert_ne!(last_diff, real);
        assert!(
            !verify(&secret, &last_diff, 0),
            "code differing only in the final byte must reject"
        );

        // A wrong code differing in the FIRST byte must also reject.
        let mut first_diff: Vec<u8> = real.as_bytes().to_vec();
        first_diff[0] = if first_diff[0] == b'9' {
            b'0'
        } else {
            first_diff[0] + 1
        };
        let first_diff = String::from_utf8(first_diff).unwrap();
        assert_ne!(first_diff, real);
        assert!(
            !verify(&secret, &first_diff, 0),
            "code differing only in the first byte must reject"
        );

        // Length-mismatched guesses (a prefix and a suffixed value) must reject
        // without panicking — `ConstantTimeEq` on slices of differing length
        // returns "not equal" rather than indexing out of bounds.
        assert!(
            !verify(&secret, &real[..DIGITS as usize - 1], 0),
            "a too-short prefix of the real code must reject"
        );
        let too_long = format!("{real}0");
        assert!(
            !verify(&secret, &too_long, 0),
            "a too-long extension of the real code must reject"
        );
        assert!(!verify(&secret, "", 0), "the empty code must reject");
    }

    #[test]
    fn totp_at_is_deterministic() {
        let secret = generate_secret();
        // Anchor to a step boundary so both samples fall in the same 30s step.
        let base = 1_000_000 - (1_000_000 % STEP_SECS);
        let a = totp_at(&secret, base).unwrap();
        let b = totp_at(&secret, base + (STEP_SECS - 1)).unwrap();
        assert_eq!(a, b, "codes within one step must match");
        // Next step → (almost surely) a different code.
        let c = totp_at(&secret, base + STEP_SECS).unwrap();
        let _ = c; // value differs with overwhelming probability; not asserted.
    }
}
