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

//! firefly-utils — the framework's general-purpose helper grab-bag.
//!
//! The Rust port of the Go `utils` module (Java original:
//! `firefly-common-utils`): the small set of primitives every module
//! reaches for and that don't fit into a more specific module.
//!
//! - **Try** — [`try_run`] / [`try_of`]: panic-safe function execution
//!   with the panic value surfaced as a [`TryError`].
//! - **Retry** — [`retry`] / [`retry_if`]: async exponential-backoff
//!   retry with jitter ([`RetryConfig`]) and a pluggable
//!   retryable-error predicate.
//! - **Slug** — [`slugify`]: URL-safe lower-case slug from any UTF-8
//!   string, folding accented Latin letters and dropping combining
//!   marks.
//! - **Crypto** — [`encrypt_aes_gcm`] / [`decrypt_aes_gcm`] with the
//!   cross-port `nonce || ciphertext` wire format, plus
//!   [`derive_key256`] (SHA-256 KDF) and the [`encode_base64`] /
//!   [`decode_base64`] URL-safe helpers.
//! - **Templates** — [`render_text`] and [`render_html`]
//!   (auto-escaping) over any [`serde::Serialize`] data.
//!
//! The public surface is flat, like the Go package: everything is
//! re-exported from the crate root.
//!
//! # Quick start
//!
//! ```
//! use firefly_utils::{retry, slugify, derive_key256, encrypt_aes_gcm, decrypt_aes_gcm};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let mut cfg = firefly_utils::RetryConfig::default(); // 3 attempts, 100ms→5s, ×2, ±20%
//! # cfg.initial_delay = std::time::Duration::from_millis(1);
//! let result = retry(cfg, || async { Ok::<_, std::io::Error>("order placed") }).await;
//! assert_eq!(result.unwrap(), "order placed");
//!
//! assert_eq!(slugify("Cañón del Río"), "canon-del-rio");
//!
//! let key = derive_key256("super-secret");
//! let ct = encrypt_aes_gcm(&key, b"hi").unwrap();
//! assert_eq!(decrypt_aes_gcm(&key, &ct).unwrap(), b"hi");
//! # }
//! ```

#![warn(missing_docs)]

mod crypto;
mod retry;
mod slug;
mod template;
mod tryfn;

pub use crypto::{
    decode_base64, decrypt_aes_gcm, derive_key256, encode_base64, encrypt_aes_gcm, CryptoError,
};
pub use retry::{retry, retry_if, RetryConfig};
pub use slug::slugify;
pub use template::{render_html, render_text, TemplateError};
pub use tryfn::{try_of, try_run, TryError};

/// Framework version stamp.
pub const VERSION: &str = "26.6.21";

#[cfg(test)]
mod tests {
    /// The version stamp matches the workspace CalVer release.
    #[test]
    fn version_stamp() {
        assert_eq!(super::VERSION, "26.6.21");
    }
}
