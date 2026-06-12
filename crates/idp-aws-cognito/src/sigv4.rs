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

//! Self-contained AWS Signature Version 4 signer (header-based).
//!
//! A from-scratch implementation of the [AWS SigV4 signing
//! process](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv4-create-signed-request.html),
//! built on the workspace `hmac`, `sha2`, and `hex` crates — **no AWS SDK is
//! pulled in**. It is fully contained within this crate so the Cognito adapter
//! can sign its admin (`AdminCreateUser`, `AdminGetUser`, …) calls without
//! depending on the S3 adapter's signer.
//!
//! Covered: the four canonical steps — (1) canonical request, (2) string to
//! sign, (3) signing-key derivation (`AWS4` → date → region → service →
//! `aws4_request`), (4) signature + `Authorization` header. Only header-based
//! signing with a precomputed `x-amz-content-sha256` is implemented (Cognito's
//! JSON POST API hashes its body), which is exactly what the adapter needs.
//!
//! The output is validated against the official AWS SigV4 test-suite "Known
//! Answer Test" (KAT) vectors in the unit tests below, so it is byte-for-byte
//! the same as AWS's reference signer for the covered cases.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Lowercase hexadecimal SHA-256 digest of `data` — AWS's `HexEncode(Hash(..))`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// A single HTTP header destined for the canonical request. Names are matched
/// and rendered case-insensitively (lowercased), values trimmed.
#[derive(Debug, Clone)]
pub struct Header {
    /// Header name (any case; lowercased for canonicalization).
    pub name: String,
    /// Header value (trimmed for canonicalization).
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

/// The inputs needed to sign one request with SigV4.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// HTTP method, uppercase (`POST`).
    pub method: &'a str,
    /// Absolute, already-encoded request path (Cognito uses `/`).
    pub canonical_uri: &'a str,
    /// Canonical query string (sorted `key=value&...`, may be empty).
    pub canonical_query: &'a str,
    /// Request headers; must include `host` and `x-amz-date` (and normally
    /// `x-amz-content-sha256` / `x-amz-target`).
    pub headers: Vec<Header>,
    /// Hex SHA-256 of the body.
    pub payload_hash: &'a str,
}

/// The credentials and scope a signature is computed against.
#[derive(Debug, Clone)]
pub struct Credentials<'a> {
    /// AWS access key id (`AKIA…`).
    pub access_key: &'a str,
    /// AWS secret access key.
    pub secret_key: &'a str,
    /// AWS region (e.g. `us-east-1`).
    pub region: &'a str,
    /// Target service (`cognito-idp`).
    pub service: &'a str,
}

/// The result of signing: the `Authorization` header value plus the canonical
/// intermediates (exposed so callers/tests can assert them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// The full `Authorization` header value to send.
    pub authorization: String,
    /// The hex-encoded request signature.
    pub signature: String,
    /// The canonical request (step 1), as fed to the hash.
    pub canonical_request: String,
    /// The string-to-sign (step 2).
    pub string_to_sign: String,
    /// The semicolon-joined, sorted list of signed header names.
    pub signed_headers: String,
}

/// Builds the canonical request and the signed-headers list.
pub fn canonical_request(req: &Request<'_>) -> (String, String) {
    let mut headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|h| {
            (
                h.name.trim().to_ascii_lowercase(),
                h.value.trim().to_string(),
            )
        })
        .collect();
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let canonical_headers: String = headers.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        req.canonical_uri,
        req.canonical_query,
        canonical_headers,
        signed_headers,
        req.payload_hash,
    );
    (canonical, signed_headers)
}

/// Signs `req` with `creds` at the given `amz_date` (`YYYYMMDDTHHMMSSZ`) and
/// `date_stamp` (`YYYYMMDD`), producing the `Authorization` header and the
/// canonical intermediates. The output matches AWS's reference signer (see the
/// KAT tests).
pub fn sign(
    req: &Request<'_>,
    creds: &Credentials<'_>,
    amz_date: &str,
    date_stamp: &str,
) -> Signed {
    let (canonical, signed_headers) = canonical_request(req);
    let hashed_canonical = sha256_hex(canonical.as_bytes());

    let scope = format!(
        "{}/{}/{}/aws4_request",
        date_stamp, creds.region, creds.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date, scope, hashed_canonical
    );

    let k_date = hmac(
        format!("AWS4{}", creds.secret_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac(&k_date, creds.region.as_bytes());
    let k_service = hmac(&k_region, creds.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");

    let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        creds.access_key, scope, signed_headers, signature
    );

    Signed {
        authorization,
        signature,
        canonical_request: canonical,
        string_to_sign,
        signed_headers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Official AWS SigV4 test-suite "Known Answer Tests" (aws4_testsuite).
    //   AKID    = AKIDEXAMPLE
    //   SECRET  = wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY
    //   REGION  = us-east-1
    //   SERVICE = service
    //   DATE    = 20150830T123600Z  (datestamp 20150830)
    // -----------------------------------------------------------------------
    const KAT_AKID: &str = "AKIDEXAMPLE";
    const KAT_SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const KAT_REGION: &str = "us-east-1";
    const KAT_SERVICE: &str = "service";
    const KAT_AMZ_DATE: &str = "20150830T123600Z";
    const KAT_DATE_STAMP: &str = "20150830";

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn kat_creds() -> Credentials<'static> {
        Credentials {
            access_key: KAT_AKID,
            secret_key: KAT_SECRET,
            region: KAT_REGION,
            service: KAT_SERVICE,
        }
    }

    /// `get-vanilla` — a bare `GET /` with only `Host` + `X-Amz-Date`.
    #[test]
    fn kat_get_vanilla() {
        let req = Request {
            method: "GET",
            canonical_uri: "/",
            canonical_query: "",
            headers: vec![
                Header::new("Host", "example.amazonaws.com"),
                Header::new("X-Amz-Date", KAT_AMZ_DATE),
            ],
            payload_hash: EMPTY_SHA256,
        };
        let signed = sign(&req, &kat_creds(), KAT_AMZ_DATE, KAT_DATE_STAMP);

        let want_creq = "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(signed.canonical_request, want_creq);

        let want_sts = "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/service/aws4_request\nbb579772317eb040ac9ed261061d46c1f17a8133879d6129b6e1c25292927e63";
        assert_eq!(signed.string_to_sign, want_sts);

        let want_authz = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31";
        assert_eq!(signed.authorization, want_authz);
        assert_eq!(
            signed.signature,
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    /// `get-vanilla-query` — a `GET /` with a single query parameter.
    #[test]
    fn kat_get_vanilla_query() {
        let req = Request {
            method: "GET",
            canonical_uri: "/",
            canonical_query: "Param1=value1",
            headers: vec![
                Header::new("Host", "example.amazonaws.com"),
                Header::new("X-Amz-Date", KAT_AMZ_DATE),
            ],
            payload_hash: EMPTY_SHA256,
        };
        let signed = sign(&req, &kat_creds(), KAT_AMZ_DATE, KAT_DATE_STAMP);

        let want_authz = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=a67d582fa61cc504c4bae71f336f98b97f1ea3c7a6bfe1b6e45aec72011b9aeb";
        assert_eq!(signed.authorization, want_authz);
    }

    /// `post-header-key-sort` — header lowercasing + sorting order.
    #[test]
    fn kat_post_header_key_sort() {
        let req = Request {
            method: "POST",
            canonical_uri: "/",
            canonical_query: "",
            headers: vec![
                Header::new("Host", "example.amazonaws.com"),
                Header::new("X-Amz-Date", KAT_AMZ_DATE),
                Header::new("My-Header1", "value1"),
            ],
            payload_hash: EMPTY_SHA256,
        };
        let signed = sign(&req, &kat_creds(), KAT_AMZ_DATE, KAT_DATE_STAMP);

        assert_eq!(signed.signed_headers, "host;my-header1;x-amz-date");
        let want_authz = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;my-header1;x-amz-date, Signature=c5410059b04c1ee005303aed430f6e6645f61f4dc9e1461ec8f8916fdf18852c";
        assert_eq!(signed.authorization, want_authz);
    }

    /// The derived signing key for the suite fixtures matches AWS's example.
    #[test]
    fn signing_key_matches_aws_example() {
        let k_date = hmac(
            format!("AWS4{KAT_SECRET}").as_bytes(),
            KAT_DATE_STAMP.as_bytes(),
        );
        let k_region = hmac(&k_date, KAT_REGION.as_bytes());
        let k_service = hmac(&k_region, KAT_SERVICE.as_bytes());
        let k_signing = hmac(&k_service, b"aws4_request");
        assert_eq!(
            hex::encode(k_signing),
            "938127b5336810ddb6a5d6af445fcac9e371f9ed418ed386b022aed82901be75"
        );
    }

    #[test]
    fn sha256_hex_known_vectors() {
        assert_eq!(sha256_hex(b""), EMPTY_SHA256);
        assert_eq!(
            sha256_hex(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
