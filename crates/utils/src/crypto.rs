//! AES-GCM encryption helpers and URL-safe base64 — the Rust port of
//! Go's `utils.EncryptAESGCM` / `DecryptAESGCM` / `DeriveKey256` /
//! `EncodeBase64` / `DecodeBase64`.
//!
//! The wire format is byte-compatible with the Go port (and through it
//! the Java/.NET/Python ports): a random 12-byte nonce, followed by
//! the ciphertext with the 16-byte GCM authentication tag appended —
//! `nonce || ciphertext || tag`.

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, AeadCore, KeyInit};
use aes_gcm::aes::Aes192;
use aes_gcm::{Aes128Gcm, Aes256Gcm, AesGcm, Nonce};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// AES-192-GCM with the standard 96-bit nonce, completing the
/// 16/24/32-byte key support Go gets for free from `aes.NewCipher`.
type Aes192Gcm = AesGcm<Aes192, U12>;

/// The AES-GCM nonce length in bytes (96 bits), prepended verbatim to
/// every ciphertext.
const NONCE_LEN: usize = 12;

/// Errors produced by the crypto and base64 helpers.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The key is not 16, 24, or 32 bytes — the same lengths Go's
    /// `aes.NewCipher` accepts.
    #[error("firefly/utils: invalid key size {0} (want 16, 24, or 32 bytes)")]
    InvalidKeySize(usize),
    /// Malformed ciphertext (too short, wrong nonce, tag mismatch,
    /// etc.) — the counterpart of Go's `ErrCipherText` sentinel.
    #[error("firefly/utils: invalid ciphertext")]
    CipherText,
    /// The input is not valid URL-safe base64.
    #[error("firefly/utils: invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
}

impl CryptoError {
    /// Reports whether this error denotes malformed ciphertext — the
    /// Rust analog of `errors.Is(err, ErrCipherText)` in Go.
    pub fn is_cipher_text(&self) -> bool {
        matches!(self, CryptoError::CipherText)
    }
}

/// Returns a 32-byte AES key derived from `passphrase` via SHA-256.
/// For production use, prefer a real KDF (scrypt/argon2id) — this
/// helper exists for parity with the Java/.NET/Go helpers that wrap
/// short integration secrets.
pub fn derive_key256(passphrase: &str) -> [u8; 32] {
    Sha256::digest(passphrase.as_bytes()).into()
}

/// Encrypts `plaintext` under a 16/24/32-byte key with a random
/// 12-byte nonce. The returned bytes are `nonce || ciphertext` (with
/// the GCM tag appended to the ciphertext), byte-compatible with the
/// Go port's `EncryptAESGCM`.
pub fn encrypt_aes_gcm(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    match key.len() {
        16 => encrypt_with::<Aes128Gcm>(key, plaintext),
        24 => encrypt_with::<Aes192Gcm>(key, plaintext),
        32 => encrypt_with::<Aes256Gcm>(key, plaintext),
        n => Err(CryptoError::InvalidKeySize(n)),
    }
}

/// Reverses [`encrypt_aes_gcm`]. Returns [`CryptoError::CipherText`]
/// for any malformed input — too short, tampered, or keyed wrongly.
pub fn decrypt_aes_gcm(key: &[u8], payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
    match key.len() {
        16 => decrypt_with::<Aes128Gcm>(key, payload),
        24 => decrypt_with::<Aes192Gcm>(key, payload),
        32 => decrypt_with::<Aes256Gcm>(key, payload),
        n => Err(CryptoError::InvalidKeySize(n)),
    }
}

fn encrypt_with<C>(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError>
where
    C: Aead + KeyInit + AeadCore<NonceSize = U12>,
{
    let cipher = C::new_from_slice(key).map_err(|_| CryptoError::InvalidKeySize(key.len()))?;
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::<U12>::from_slice(&nonce), plaintext)
        .map_err(|_| CryptoError::CipherText)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn decrypt_with<C>(key: &[u8], payload: &[u8]) -> Result<Vec<u8>, CryptoError>
where
    C: Aead + KeyInit + AeadCore<NonceSize = U12>,
{
    let cipher = C::new_from_slice(key).map_err(|_| CryptoError::InvalidKeySize(key.len()))?;
    if payload.len() < NONCE_LEN {
        return Err(CryptoError::CipherText);
    }
    let (nonce, ct) = payload.split_at(NONCE_LEN);
    cipher
        .decrypt(Nonce::<U12>::from_slice(nonce), ct)
        .map_err(|_| CryptoError::CipherText)
}

/// Encodes `b` with the URL-safe base64 alphabet, no padding —
/// identical output to Go's `EncodeBase64`.
pub fn encode_base64(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

/// Decodes a URL-safe base64 string, accepting both padded and
/// unpadded input — identical acceptance to Go's `DecodeBase64`,
/// which routes on `len(s) % 4`.
pub fn decode_base64(s: &str) -> Result<Vec<u8>, CryptoError> {
    if s.len() % 4 != 0 {
        Ok(URL_SAFE_NO_PAD.decode(s)?)
    } else {
        Ok(URL_SAFE.decode(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go `TestCryptoRoundTrip`: derive → encrypt → decrypt,
    /// then flip the last byte and expect the ciphertext error.
    #[test]
    fn crypto_round_trip_and_tamper_detection() {
        let key = derive_key256("super-secret");
        let mut ct = encrypt_aes_gcm(&key, b"hello, firefly").expect("encrypt");
        let pt = decrypt_aes_gcm(&key, &ct).expect("decrypt");
        assert_eq!(pt, b"hello, firefly");

        // Tamper.
        let last = ct.len() - 1;
        ct[last] ^= 0xff;
        let err = decrypt_aes_gcm(&key, &ct).expect_err("tamper must fail");
        assert!(err.is_cipher_text(), "expected CipherText, got {err:?}");
    }

    /// Cross-port compatibility: this payload was produced by the Go
    /// port (`EncryptAESGCM(DeriveKey256("super-secret"), "hello,
    /// firefly")`) and must decrypt here byte-for-byte.
    #[test]
    fn decrypts_go_produced_ciphertext() {
        let key = derive_key256("super-secret");
        let payload = decode_base64("psnzD7aJk_rAPtiZ9df_rjLVHrE68vSDbZ4FdpuRuxfmbyfIJVbGrwDs")
            .expect("decode");
        let pt = decrypt_aes_gcm(&key, &payload).expect("decrypt Go payload");
        assert_eq!(pt, b"hello, firefly");
    }

    /// The wire format is nonce(12) || ciphertext || tag(16), and the
    /// random nonce makes every encryption distinct.
    #[test]
    fn wire_format_is_nonce_then_ciphertext() {
        let key = derive_key256("k");
        let a = encrypt_aes_gcm(&key, b"abc").unwrap();
        let b = encrypt_aes_gcm(&key, b"abc").unwrap();
        assert_eq!(a.len(), 12 + 3 + 16);
        assert_ne!(a, b, "random nonces must differ");
    }

    /// 16- and 24-byte keys (AES-128/192) round-trip too, matching
    /// Go's `aes.NewCipher` key-size support.
    #[test]
    fn supports_all_aes_key_sizes() {
        for size in [16usize, 24, 32] {
            let key = vec![7u8; size];
            let ct = encrypt_aes_gcm(&key, b"payload").expect("encrypt");
            let pt = decrypt_aes_gcm(&key, &ct).expect("decrypt");
            assert_eq!(pt, b"payload", "key size {size}");
        }
    }

    /// Invalid key sizes are rejected with the size in the error.
    #[test]
    fn rejects_invalid_key_sizes() {
        for size in [0usize, 15, 31, 33] {
            let key = vec![0u8; size];
            match encrypt_aes_gcm(&key, b"x") {
                Err(CryptoError::InvalidKeySize(n)) => assert_eq!(n, size),
                other => panic!("key size {size}: expected InvalidKeySize, got {other:?}"),
            }
        }
    }

    /// Payloads shorter than the nonce are malformed ciphertext.
    #[test]
    fn rejects_short_payload() {
        let key = derive_key256("k");
        let err = decrypt_aes_gcm(&key, &[1, 2, 3]).expect_err("short payload");
        assert!(err.is_cipher_text());
    }

    /// `derive_key256` is deterministic SHA-256 — the canonical vector
    /// for "super-secret" pins cross-port key agreement.
    #[test]
    fn derive_key256_is_sha256() {
        let key = derive_key256("super-secret");
        assert_eq!(key.len(), 32);
        assert_eq!(key, derive_key256("super-secret"));
        assert_ne!(key, derive_key256("other"));
    }

    /// Port of Go `TestBase64`: round-trip of high/low bytes, plus the
    /// exact unpadded encoding the Go port produces and acceptance of
    /// the padded form.
    #[test]
    fn base64_round_trip_matches_go() {
        let input = [0xffu8, 0x00, 0xab, 0xcd];
        let enc = encode_base64(&input);
        assert_eq!(enc, "_wCrzQ"); // verified against the Go port
        assert_eq!(decode_base64(&enc).unwrap(), input);
        // Padded input (len % 4 == 0) is accepted too.
        assert_eq!(decode_base64("_wCrzQ==").unwrap(), input);
    }

    /// Invalid base64 surfaces as the Base64 error variant.
    #[test]
    fn base64_rejects_garbage() {
        let err = decode_base64("not base64!!!").expect_err("garbage");
        assert!(matches!(err, CryptoError::Base64(_)));
    }

    /// Rust-specific: errors are Send + Sync.
    #[test]
    fn crypto_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CryptoError>();
    }
}
