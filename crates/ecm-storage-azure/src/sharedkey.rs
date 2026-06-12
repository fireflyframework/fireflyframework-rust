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

//! Self-contained Azure Storage **Shared Key** request signer.
//!
//! Implements the [Shared Key authorization scheme for Blob, Queue, and File
//! services](https://learn.microsoft.com/en-us/rest/api/storageservices/authorize-with-shared-key)
//! on top of the workspace `hmac`, `sha2`, and `base64` crates — no Azure SDK
//! is linked. Given the request line, the canonicalized headers, and the
//! canonicalized resource, it builds the *string-to-sign*, signs it with the
//! base64-decoded account key, and assembles the
//! `Authorization: SharedKey <account>:<signature>` header.
//!
//! Only the Blob-service shape the [`crate::BlobStore`] adapter needs is
//! implemented: the full 13-line string-to-sign, the `x-ms-*` canonical-header
//! block, and the `/<account>/<container>/<blob>` canonical resource.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// One HTTP header. Names are matched case-insensitively (lowercased) for the
/// `x-ms-*` canonical-header block.
#[derive(Debug, Clone)]
pub struct Header {
    /// Header name (any case).
    pub name: String,
    /// Header value.
    pub value: String,
}

impl Header {
    /// Builds a header from a name/value pair.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Inputs for one Shared Key signature.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// HTTP verb, uppercase (`GET`, `PUT`, `DELETE`).
    pub method: &'a str,
    /// `Content-Length`; for Blob the empty string is sent when zero (the
    /// 2015-02-21+ rule).
    pub content_length: &'a str,
    /// `Content-Type`, or empty.
    pub content_type: &'a str,
    /// All `x-ms-*` headers; lowercased, sorted, and joined into the canonical
    /// block.
    pub x_ms_headers: Vec<Header>,
    /// Canonicalized resource, e.g. `/<account>/<container>/<blob>`.
    pub canonical_resource: &'a str,
}

/// Builds the canonical-headers block: every `x-ms-*` header lowercased,
/// trimmed, sorted by name, rendered `name:value\n`.
fn canonical_headers(headers: &[Header]) -> String {
    let mut hs: Vec<(String, String)> = headers
        .iter()
        .map(|h| {
            (
                h.name.trim().to_ascii_lowercase(),
                h.value.trim().to_string(),
            )
        })
        .filter(|(n, _)| n.starts_with("x-ms-"))
        .collect();
    hs.sort_by(|a, b| a.0.cmp(&b.0));
    hs.iter().map(|(n, v)| format!("{n}:{v}\n")).collect()
}

/// Builds the Blob-service string-to-sign (the canonical request) for `req`.
/// Exposed for the KAT tests; production callers use [`sign`].
pub fn string_to_sign(req: &Request<'_>) -> String {
    // The 13 standard headers, in fixed order, then the x-ms block, then the
    // canonical resource. Unused headers are blank lines.
    format!(
        "{method}\n\
         \n\
         \n\
         {content_length}\n\
         \n\
         {content_type}\n\
         \n\
         \n\
         \n\
         \n\
         \n\
         \n\
         {canonical_headers}{canonical_resource}",
        method = req.method,
        content_length = req.content_length,
        content_type = req.content_type,
        canonical_headers = canonical_headers(&req.x_ms_headers),
        canonical_resource = req.canonical_resource,
    )
}

/// Signs `req` for storage account `account` whose base64 `account_key` is
/// supplied, returning `(authorization, signature, string_to_sign)`.
///
/// Returns `Err` only when `account_key` is not valid base64.
pub fn sign(
    req: &Request<'_>,
    account: &str,
    account_key: &str,
) -> Result<(String, String, String), String> {
    let key = B64
        .decode(account_key)
        .map_err(|e| format!("invalid base64 account key: {e}"))?;
    let sts = string_to_sign(req);
    let mut mac = HmacSha256::new_from_slice(&key).map_err(|e| format!("hmac key: {e}"))?;
    mac.update(sts.as_bytes());
    let signature = B64.encode(mac.finalize().into_bytes());
    let authorization = format!("SharedKey {account}:{signature}");
    Ok((authorization, signature, sts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_headers_lowercases_sorts_and_filters() {
        let h = canonical_headers(&[
            Header::new("X-Ms-Version", "2021-08-06"),
            Header::new("Content-Type", "ignored"),
            Header::new("x-ms-date", "Fri, 01 Jan 2021 00:00:00 GMT"),
            Header::new("x-ms-blob-type", "BlockBlob"),
        ]);
        // Only x-ms-*; sorted; \n-terminated; Content-Type dropped.
        assert_eq!(
            h,
            "x-ms-blob-type:BlockBlob\nx-ms-date:Fri, 01 Jan 2021 00:00:00 GMT\nx-ms-version:2021-08-06\n"
        );
    }

    #[test]
    fn string_to_sign_has_the_blob_shape() {
        let req = Request {
            method: "GET",
            content_length: "",
            content_type: "",
            x_ms_headers: vec![
                Header::new("x-ms-date", "Fri, 01 Jan 2021 00:00:00 GMT"),
                Header::new("x-ms-version", "2021-08-06"),
            ],
            canonical_resource: "/devstoreaccount1/my-container/doc-xyz/v1",
        };
        let sts = string_to_sign(&req);
        let want = "GET\n\n\n\n\n\n\n\n\n\n\n\n\
                    x-ms-date:Fri, 01 Jan 2021 00:00:00 GMT\n\
                    x-ms-version:2021-08-06\n\
                    /devstoreaccount1/my-container/doc-xyz/v1";
        assert_eq!(sts, want);
    }

    /// KAT against a known HMAC-SHA256 computation: signing the fixed
    /// string-to-sign with the well-known Azurite/devstore account key yields a
    /// deterministic, cross-checked base64 signature.
    #[test]
    fn sign_matches_reference_hmac() {
        // The public Azurite development account + key (safe to embed).
        let account = "devstoreaccount1";
        let key = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
        let req = Request {
            method: "PUT",
            content_length: "11",
            content_type: "application/octet-stream",
            x_ms_headers: vec![
                Header::new("x-ms-blob-type", "BlockBlob"),
                Header::new("x-ms-date", "Fri, 01 Jan 2021 00:00:00 GMT"),
                Header::new("x-ms-version", "2021-08-06"),
            ],
            canonical_resource: "/devstoreaccount1/my-container/doc-xyz/v1",
        };
        let (authz, signature, _sts) = sign(&req, account, key).unwrap();
        // Cross-checked against an independent HMAC-SHA256 reference impl.
        assert_eq!(signature, "q9xu8GAb5rvi47osTh/TeVb5oyXDp7xoadjOxcaS7TE=");
        assert_eq!(
            authz,
            "SharedKey devstoreaccount1:q9xu8GAb5rvi47osTh/TeVb5oyXDp7xoadjOxcaS7TE="
        );
    }

    #[test]
    fn sign_rejects_bad_base64_key() {
        let req = Request {
            method: "GET",
            content_length: "",
            content_type: "",
            x_ms_headers: vec![],
            canonical_resource: "/a/c/b",
        };
        let err = sign(&req, "acct", "not!base64!").unwrap_err();
        assert!(err.contains("base64"), "{err}");
    }
}
