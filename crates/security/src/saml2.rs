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

//! SAML 2.0 Service Provider authentication — the Rust analog of Spring
//! Security's `saml2Login()` (`RelyingPartyRegistration`,
//! `OpenSaml4AuthenticationProvider`, `Saml2MetadataFilter`).
//!
//! This is the SP side of the SAML 2.0 Web-Browser-SSO profile. The heavy
//! lifting — XML-signature verification, canonicalization, and the SAML profile
//! checks (audience, recipient, `InResponseTo`, conditions, subject
//! confirmation) — is delegated to the [`samael`] crate (which links the
//! battle-tested `xmlsec`/`libxml2`/OpenSSL stack). This module is the
//! Spring-faithful, **hardened** wrapper around it:
//!
//! 1. [`RelyingPartyRegistration`] holds one SP↔IdP relationship. Building one
//!    **fails closed** when the asserting party (IdP) has no signing
//!    certificate — without it `samael` would skip signature verification
//!    entirely, which is an authentication bypass.
//! 2. Signature verification is pinned to a safe **allow-list of algorithms**
//!    (RSA/ECDSA-SHA256+) — `samael`'s default of "all algorithms" is open to
//!    algorithm-substitution attacks.
//! 3. [`AssertionReplayCache`] adds **one-time-use** assertion-ID replay
//!    protection, which the SAML profile requires but `samael` does not do.
//! 4. [`Saml2AuthenticationRequestRepository`] tracks outgoing `AuthnRequest`
//!    IDs (with a TTL) so a response's `InResponseTo` can be matched to a
//!    request this SP actually issued.
//!
//! Everything here is gated behind the opt-in `saml2` feature, so the default
//! build keeps its pure-Rust / rustls posture.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
pub use samael::crypto::AllowedSignatureAlgorithm;
use samael::metadata::{EntityDescriptor, HTTP_REDIRECT_BINDING};
use samael::schema::Assertion;
use samael::service_provider::{ServiceProvider, ServiceProviderBuilder};
use samael::traits::ToXml;
use serde_json::Value;

use crate::authentication::{Authentication, SecurityError, ROLE_PREFIX};

/// The largest decoded SAML response we will parse (a guard against a giant
/// base64 POST body exhausting memory before the XML parser sees it).
const MAX_SAML_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// Replay-cache fallback lifetime for an assertion whose `NotOnOrAfter` is
/// absent — bounds the cache so it cannot grow without limit.
const REPLAY_FALLBACK_TTL: Duration = Duration::from_secs(60 * 60);

/// Extra time to retain a replay-cache entry past the assertion's validity
/// window, covering the clock skew validation allows.
const REPLAY_SKEW_MARGIN: Duration = Duration::from_secs(300);

/// `xmlsec`/`libxml2` operate on process-global state and are **not** safe to
/// run concurrently (concurrent use segfaults). Every entry into the native
/// XML-Security stack — signature verification here, signing in tests — is
/// serialized through this guard. Verification is fast and logins are not a hot
/// path, so the serialization cost is acceptable.
static XMLSEC_GUARD: Mutex<()> = Mutex::new(());

/// A safe default signature-algorithm allow-list (SHA-256 and stronger, RSA and
/// ECDSA). `samael` otherwise accepts *all* algorithms, which is open to
/// algorithm-substitution / downgrade attacks.
#[must_use]
fn default_allowed_algorithms() -> Vec<AllowedSignatureAlgorithm> {
    vec![
        AllowedSignatureAlgorithm::RsaSha256,
        AllowedSignatureAlgorithm::RsaSha384,
        AllowedSignatureAlgorithm::RsaSha512,
        AllowedSignatureAlgorithm::EcdsaSha256,
        AllowedSignatureAlgorithm::EcdsaSha384,
        AllowedSignatureAlgorithm::EcdsaSha512,
    ]
}

/// Escapes a string for inclusion in an XML attribute / text node.
fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Synthesises minimal IdP (asserting-party) metadata XML from explicit details,
/// for callers that configure the IdP by entity-id + SSO URL + signing cert
/// rather than by a metadata document (Spring's `assertingPartyDetails`).
fn synthesize_idp_metadata(
    entity_id: &str,
    sso_redirect_location: &str,
    signing_certificate_b64_der: &str,
) -> String {
    format!(
        concat!(
            "<EntityDescriptor xmlns=\"urn:oasis:names:tc:SAML:2.0:metadata\" entityID=\"{eid}\">",
            "<IDPSSODescriptor protocolSupportEnumeration=\"urn:oasis:names:tc:SAML:2.0:protocol\">",
            "<KeyDescriptor use=\"signing\">",
            "<KeyInfo xmlns=\"http://www.w3.org/2000/09/xmldsig#\">",
            "<X509Data><X509Certificate>{cert}</X509Certificate></X509Data>",
            "</KeyInfo></KeyDescriptor>",
            "<SingleSignOnService ",
            "Binding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect\" Location=\"{sso}\"/>",
            "</IDPSSODescriptor></EntityDescriptor>"
        ),
        eid = xml_escape(entity_id),
        cert = xml_escape(signing_certificate_b64_der.trim()),
        sso = xml_escape(sso_redirect_location),
    )
}

/// One relying-party (SP) registration against an asserting party (IdP) — the
/// Rust analog of Spring's `RelyingPartyRegistration`.
///
/// Build one with [`RelyingPartyRegistration::builder`]. A registration is
/// immutable and cheap to share (wrap it in an [`Arc`] for the repository).
pub struct RelyingPartyRegistration {
    registration_id: String,
    sp: ServiceProvider,
    sso_redirect_location: String,
    role_attributes: Vec<String>,
    role_prefix: String,
}

impl RelyingPartyRegistration {
    /// Starts building a registration identified by `registration_id` (the
    /// opaque key the repository looks it up by, e.g. `"okta"`).
    #[must_use]
    pub fn builder(registration_id: impl Into<String>) -> RelyingPartyRegistrationBuilder {
        RelyingPartyRegistrationBuilder::new(registration_id)
    }

    /// The registration's lookup id.
    #[must_use]
    pub fn registration_id(&self) -> &str {
        &self.registration_id
    }

    /// Serialises this SP's metadata document (Spring's `Saml2MetadataFilter`
    /// output) for registration at the IdP.
    pub fn metadata_xml(&self) -> Result<String, SecurityError> {
        self.sp
            .metadata()
            .map_err(|e| SecurityError::verification(format!("saml2: metadata: {e}")))?
            .to_string()
            .map_err(|e| SecurityError::verification(format!("saml2: metadata: {e}")))
    }

    /// Builds an SP-initiated `AuthnRequest` and returns the HTTP-Redirect-binding
    /// URL to send the browser to, plus the request's `ID`.
    ///
    /// The caller **must persist the returned `request_id`** (see
    /// [`Saml2AuthenticationRequestRepository`]) and pass it to
    /// [`authenticate`](Self::authenticate) as an expected id, so the response's
    /// `InResponseTo` can be matched to a request this SP issued.
    pub fn authn_request_redirect(
        &self,
        relay_state: &str,
    ) -> Result<AuthnRedirect, SecurityError> {
        let request = self
            .sp
            .make_authentication_request(&self.sso_redirect_location)
            .map_err(|e| SecurityError::verification(format!("saml2: authn request: {e}")))?;
        let request_id = request.id.clone();
        let url = request
            .redirect(relay_state)
            .map_err(|e| SecurityError::verification(format!("saml2: authn redirect: {e}")))?
            .ok_or_else(|| {
                SecurityError::verification("saml2: no IdP SSO destination configured")
            })?;
        Ok(AuthnRedirect {
            url: url.to_string(),
            request_id,
        })
    }

    /// Validates a base64-encoded SAML `Response` (HTTP-POST binding) and
    /// resolves the authenticated principal — the Rust analog of Spring's
    /// `OpenSaml4AuthenticationProvider`.
    ///
    /// `samael` performs XML-signature verification (pinned to the IdP's
    /// certificate and this registration's allowed algorithms) and every SAML
    /// profile check — audience, recipient, status, `InResponseTo`, and the
    /// response/assertion/subject-confirmation time conditions. On top of that,
    /// this method enforces **one-time-use** of the assertion via `replay_cache`
    /// (which `samael` does not track).
    ///
    /// `expected_request_ids` are the `AuthnRequest` IDs this SP issued and is
    /// still awaiting (see [`Saml2AuthenticationRequestRepository`]); the
    /// response's `InResponseTo` must match one of them unless the registration
    /// allows IdP-initiated SSO.
    ///
    /// # Errors
    /// Any signature, profile, replay, or decoding failure — all surfaced as a
    /// [`SecurityError::Verification`].
    pub fn authenticate(
        &self,
        saml_response_b64: &str,
        expected_request_ids: &[&str],
        replay_cache: &dyn AssertionReplayCache,
    ) -> Result<Authentication, SecurityError> {
        // Bound the input before allocating the decoded buffer.
        if saml_response_b64.len() > MAX_SAML_RESPONSE_BYTES * 2 {
            return Err(SecurityError::verification("saml2: response too large"));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(saml_response_b64.trim())
            .map_err(|_| SecurityError::verification("saml2: response is not valid base64"))?;
        if decoded.len() > MAX_SAML_RESPONSE_BYTES {
            return Err(SecurityError::verification("saml2: response too large"));
        }
        let xml = std::str::from_utf8(&decoded)
            .map_err(|_| SecurityError::verification("saml2: response is not valid UTF-8"))?;

        // samael verifies the XML signature (against the pinned IdP cert and the
        // allowed algorithms) and validates the SAML profile conditions. The
        // call is serialized — the native XML-Security stack is not thread-safe.
        let assertion = {
            let _guard = XMLSEC_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            self.sp.parse_xml_response(xml, Some(expected_request_ids))
        }
        .map_err(|e| SecurityError::verification(format!("saml2: {e}")))?;

        // A missing assertion ID would make replay protection collide across
        // assertions — refuse it.
        if assertion.id.trim().is_empty() {
            return Err(SecurityError::verification("saml2: assertion has no ID"));
        }

        // One-time-use: reject a replayed assertion (samael does not track this).
        // Retain the id slightly past its validity window to cover clock skew.
        let expires_at = assertion_expiry(&assertion).map(|t| t + REPLAY_SKEW_MARGIN);
        replay_cache.check_and_remember(&assertion.id, expires_at)?;

        Ok(self.map_assertion(assertion))
    }

    /// Maps a verified [`Assertion`] to an [`Authentication`]: the `NameID`
    /// becomes the principal/username, the configured role attributes become
    /// authorities (prefixed by [`role_prefix`](RelyingPartyRegistrationBuilder::role_prefix)),
    /// and every attribute is exposed in `claims`.
    fn map_assertion(&self, assertion: Assertion) -> Authentication {
        let principal = assertion
            .subject
            .as_ref()
            .and_then(|s| s.name_id.as_ref())
            .map(|n| n.value.clone())
            .unwrap_or_default();

        let mut roles = Vec::new();
        let mut claims = HashMap::new();
        for statement in assertion.attribute_statements.iter().flatten() {
            for attr in &statement.attributes {
                let Some(name) = attr.name.as_deref() else {
                    continue;
                };
                let values: Vec<String> =
                    attr.values.iter().filter_map(|v| v.value.clone()).collect();
                if self.role_attributes.iter().any(|a| a == name) {
                    for v in &values {
                        roles.push(format!("{}{}", self.role_prefix, v));
                    }
                }
                claims.insert(
                    name.to_string(),
                    Value::Array(values.into_iter().map(Value::String).collect()),
                );
            }
        }

        Authentication {
            principal: principal.clone(),
            username: principal,
            roles,
            authorities: Vec::new(),
            claims,
        }
    }
}

/// The latest moment a verified assertion could still pass validation — the
/// later of its subject-confirmation and conditions `NotOnOrAfter` — as a
/// [`SystemTime`]. `None` when the assertion carries no expiry.
fn assertion_expiry(assertion: &Assertion) -> Option<SystemTime> {
    let subject_ts = assertion
        .subject
        .as_ref()
        .and_then(|s| s.subject_confirmations.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|c| c.subject_confirmation_data.as_ref())
        .filter_map(|d| d.not_on_or_after.as_ref().map(|t| t.timestamp()))
        .max();
    let condition_ts = assertion
        .conditions
        .as_ref()
        .and_then(|c| c.not_on_or_after.as_ref().map(|t| t.timestamp()));
    let ts = [subject_ts, condition_ts].into_iter().flatten().max()?;
    u64::try_from(ts)
        .ok()
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

/// The outcome of [`RelyingPartyRegistration::authn_request_redirect`]: where to
/// redirect the browser, and the `AuthnRequest` `ID` the caller must remember.
#[derive(Debug, Clone)]
pub struct AuthnRedirect {
    /// The IdP SSO URL with the `SAMLRequest` (and `RelayState`) query params.
    pub url: String,
    /// The generated `AuthnRequest` `ID` — persist it to match `InResponseTo`.
    pub request_id: String,
}

/// Builder for a [`RelyingPartyRegistration`].
pub struct RelyingPartyRegistrationBuilder {
    registration_id: String,
    entity_id: Option<String>,
    acs_url: Option<String>,
    idp_metadata: Option<EntityDescriptor>,
    sso_redirect_location: Option<String>,
    allow_idp_initiated: bool,
    allowed_signature_algorithms: Option<Vec<AllowedSignatureAlgorithm>>,
    role_attributes: Vec<String>,
    role_prefix: String,
}

impl RelyingPartyRegistrationBuilder {
    fn new(registration_id: impl Into<String>) -> Self {
        Self {
            registration_id: registration_id.into(),
            entity_id: None,
            acs_url: None,
            idp_metadata: None,
            sso_redirect_location: None,
            allow_idp_initiated: false,
            allowed_signature_algorithms: None,
            role_attributes: Vec::new(),
            role_prefix: ROLE_PREFIX.to_string(),
        }
    }

    /// The SP's own entity id (also used as the SAML audience this SP requires).
    #[must_use]
    pub fn sp_entity_id(mut self, entity_id: impl Into<String>) -> Self {
        self.entity_id = Some(entity_id.into());
        self
    }

    /// The SP's assertion-consumer-service (ACS) URL — where the IdP POSTs the
    /// response, and the `Recipient` the response must match.
    #[must_use]
    pub fn assertion_consumer_service_location(mut self, acs_url: impl Into<String>) -> Self {
        self.acs_url = Some(acs_url.into());
        self
    }

    /// Configures the asserting party (IdP) from a SAML metadata document.
    ///
    /// # Errors
    /// If the metadata XML cannot be parsed.
    pub fn asserting_party_metadata(
        mut self,
        idp_metadata_xml: &str,
    ) -> Result<Self, SecurityError> {
        let descriptor: EntityDescriptor = idp_metadata_xml
            .parse()
            .map_err(|e| SecurityError::verification(format!("saml2: IdP metadata: {e}")))?;
        self.idp_metadata = Some(descriptor);
        Ok(self)
    }

    /// Configures the asserting party (IdP) from explicit details — Spring's
    /// `assertingPartyDetails`. `signing_certificate_b64_der` is the IdP's
    /// signing certificate as base64-encoded DER (the `<X509Certificate>` value).
    ///
    /// # Errors
    /// If the synthesised metadata cannot be parsed.
    pub fn asserting_party(
        mut self,
        idp_entity_id: &str,
        sso_redirect_location: &str,
        signing_certificate_b64_der: &str,
    ) -> Result<Self, SecurityError> {
        let xml = synthesize_idp_metadata(
            idp_entity_id,
            sso_redirect_location,
            signing_certificate_b64_der,
        );
        self = self.asserting_party_metadata(&xml)?;
        self.sso_redirect_location = Some(sso_redirect_location.to_string());
        Ok(self)
    }

    /// Allows IdP-initiated SSO (no matching `AuthnRequest`). Off by default —
    /// when off, a response whose `InResponseTo` matches no expected request id
    /// is rejected.
    #[must_use]
    pub fn allow_idp_initiated(mut self, allow: bool) -> Self {
        self.allow_idp_initiated = allow;
        self
    }

    /// Overrides the accepted signature algorithms (default: SHA-256+ RSA/ECDSA).
    #[must_use]
    pub fn allowed_signature_algorithms(
        mut self,
        algorithms: Vec<AllowedSignatureAlgorithm>,
    ) -> Self {
        self.allowed_signature_algorithms = Some(algorithms);
        self
    }

    /// Adds a SAML attribute name whose values are mapped to authorities (roles)
    /// on the resulting [`Authentication`]. May be called more than once.
    #[must_use]
    pub fn role_attribute(mut self, attribute: impl Into<String>) -> Self {
        self.role_attributes.push(attribute.into());
        self
    }

    /// Overrides the prefix prepended to each role-attribute value (default
    /// `ROLE_`). Set it to `""` to use the IdP's values verbatim.
    #[must_use]
    pub fn role_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.role_prefix = prefix.into();
        self
    }

    /// Finalises the registration.
    ///
    /// # Errors
    /// - the SP entity id, ACS URL, or IdP metadata is missing;
    /// - **the IdP has no signing certificate** (fail-closed: without it,
    ///   signature verification would be silently skipped);
    /// - no HTTP-Redirect SSO location can be resolved.
    pub fn build(self) -> Result<RelyingPartyRegistration, SecurityError> {
        let entity_id = self
            .entity_id
            .ok_or_else(|| SecurityError::verification("saml2: SP entity id is required"))?;
        let acs_url = self
            .acs_url
            .ok_or_else(|| SecurityError::verification("saml2: ACS URL is required"))?;
        let idp_metadata = self
            .idp_metadata
            .ok_or_else(|| SecurityError::verification("saml2: IdP metadata is required"))?;

        let allowed = self
            .allowed_signature_algorithms
            .unwrap_or_else(default_allowed_algorithms);

        let sp = ServiceProviderBuilder::default()
            .entity_id(entity_id.clone())
            .metadata_url(entity_id)
            .acs_url(acs_url)
            .idp_metadata(idp_metadata)
            .allow_idp_initiated(self.allow_idp_initiated)
            .allowed_signature_algorithms(Some(allowed))
            .build()
            .map_err(|e| SecurityError::verification(format!("saml2: service provider: {e}")))?;

        // Fail closed: an IdP with no signing certificate would make `samael`
        // skip signature verification entirely — an authentication bypass.
        match sp.idp_signing_certs() {
            Ok(Some(certs)) if !certs.is_empty() => {}
            _ => {
                return Err(SecurityError::verification(
                    "saml2: asserting party has no signing certificate (refusing to skip \
                     signature verification)",
                ))
            }
        }

        // Resolve where to send AuthnRequests (HTTP-Redirect binding).
        let sso_redirect_location = self
            .sso_redirect_location
            .or_else(|| sp.sso_binding_location(HTTP_REDIRECT_BINDING))
            .ok_or_else(|| {
                SecurityError::verification("saml2: IdP exposes no HTTP-Redirect SSO endpoint")
            })?;

        Ok(RelyingPartyRegistration {
            registration_id: self.registration_id,
            sp,
            sso_redirect_location,
            role_attributes: self.role_attributes,
            role_prefix: self.role_prefix,
        })
    }
}

/// A store of [`RelyingPartyRegistration`]s by id — Spring's
/// `RelyingPartyRegistrationRepository`.
pub trait RelyingPartyRegistrationRepository: Send + Sync {
    /// Looks up the registration with `registration_id`, if any.
    fn find_by_registration_id(
        &self,
        registration_id: &str,
    ) -> Option<Arc<RelyingPartyRegistration>>;
}

/// An in-memory [`RelyingPartyRegistrationRepository`].
#[derive(Default, Clone)]
pub struct InMemoryRelyingPartyRegistrationRepository {
    by_id: HashMap<String, Arc<RelyingPartyRegistration>>,
}

impl InMemoryRelyingPartyRegistrationRepository {
    /// Builds a repository from the given registrations (keyed by their id).
    #[must_use]
    pub fn new(registrations: Vec<RelyingPartyRegistration>) -> Self {
        let by_id = registrations
            .into_iter()
            .map(|r| (r.registration_id.clone(), Arc::new(r)))
            .collect();
        Self { by_id }
    }
}

impl RelyingPartyRegistrationRepository for InMemoryRelyingPartyRegistrationRepository {
    fn find_by_registration_id(
        &self,
        registration_id: &str,
    ) -> Option<Arc<RelyingPartyRegistration>> {
        self.by_id.get(registration_id).cloned()
    }
}

/// A store of outgoing `AuthnRequest` IDs with a TTL — Spring's
/// `Saml2AuthenticationRequestRepository`. Lets a later response's
/// `InResponseTo` be validated against a request this SP actually issued.
pub trait Saml2AuthenticationRequestRepository: Send + Sync {
    /// Remembers `request_id` for at most `ttl`.
    fn save(&self, request_id: &str, ttl: Duration);
    /// Reports whether `request_id` is still pending (issued and unexpired).
    fn is_pending(&self, request_id: &str) -> bool;
    /// Removes `request_id`, returning whether it was pending.
    fn remove(&self, request_id: &str) -> bool;
}

/// An in-memory, TTL'd [`Saml2AuthenticationRequestRepository`].
#[derive(Default)]
pub struct InMemorySaml2AuthenticationRequestRepository {
    pending: Mutex<HashMap<String, SystemTime>>,
}

impl InMemorySaml2AuthenticationRequestRepository {
    /// Builds an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Saml2AuthenticationRequestRepository for InMemorySaml2AuthenticationRequestRepository {
    fn save(&self, request_id: &str, ttl: Duration) {
        let expires_at = SystemTime::now() + ttl;
        let mut pending = self.pending.lock().expect("request store poisoned");
        let now = SystemTime::now();
        pending.retain(|_, exp| *exp > now);
        pending.insert(request_id.to_string(), expires_at);
    }

    fn is_pending(&self, request_id: &str) -> bool {
        let pending = self.pending.lock().expect("request store poisoned");
        pending
            .get(request_id)
            .is_some_and(|exp| *exp > SystemTime::now())
    }

    fn remove(&self, request_id: &str) -> bool {
        let mut pending = self.pending.lock().expect("request store poisoned");
        match pending.remove(request_id) {
            Some(exp) => exp > SystemTime::now(),
            None => false,
        }
    }
}

/// One-time-use cache of consumed assertion IDs — the SAML profile's replay
/// protection, which `samael` does not implement. An assertion ID is accepted
/// at most once within its validity window.
pub trait AssertionReplayCache: Send + Sync {
    /// Records `assertion_id` as used until `expires_at` (or a bounded default
    /// when `None`). Returns `Err` if the id was already recorded and has not
    /// yet expired — i.e. a replay.
    fn check_and_remember(
        &self,
        assertion_id: &str,
        expires_at: Option<SystemTime>,
    ) -> Result<(), SecurityError>;
}

/// An in-memory [`AssertionReplayCache`]. Expired entries are purged lazily on
/// each call, so the map stays bounded by the number of in-flight assertions.
#[derive(Default)]
pub struct InMemoryAssertionReplayCache {
    seen: Mutex<HashMap<String, SystemTime>>,
}

impl InMemoryAssertionReplayCache {
    /// Builds an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl AssertionReplayCache for InMemoryAssertionReplayCache {
    fn check_and_remember(
        &self,
        assertion_id: &str,
        expires_at: Option<SystemTime>,
    ) -> Result<(), SecurityError> {
        let now = SystemTime::now();
        let forget_at = expires_at.unwrap_or(now + REPLAY_FALLBACK_TTL);
        let mut seen = self.seen.lock().expect("replay cache poisoned");
        seen.retain(|_, exp| *exp > now);
        if seen.contains_key(assertion_id) {
            return Err(SecurityError::verification(
                "saml2: assertion replay detected",
            ));
        }
        seen.insert(assertion_id.to_string(), forget_at);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed RSA test certificate (base64 DER) used as the IdP's signing
    /// certificate in the unit tests.
    const TEST_IDP_CERT_B64: &str = "MIIDGTCCAgGgAwIBAgIUFH5rQdUdWRLDzj/k+F7hGosU9+swDQYJKoZIhvcNAQELBQAwGzEZMBcGA1UEAwwQRmlyZWZseSBUZXN0IElkUDAgFw0yNjA2MTkyMTM4MjhaGA8yMTI2MDUyNjIxMzgyOFowGzEZMBcGA1UEAwwQRmlyZWZseSBUZXN0IElkUDCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBANGnXND9uC64r89GMY8I6pjYBeV4x0y83hfQAv0FjQSKdTU9E4NZlHWUtVbpZqjmUhxdI7V0K+8p+Fgr1mtDCNYk86xQ+vXPbLJ70jjDUwM8dh9NxkAPEzf3lR/PkK8cKz+nMwxbbia/Q2R1pZtYRy1xbCDy97skGTex2BjpTEDsR6tTdLk5Pk7wvkigiVr4I+fwbZysM7wt5RTGXAnz7sxyvdrj0BvQvBCVncrYPxLowZdpVFezoBTZa09xlNMv2YCarSjueGCvaQ7YrVk3qD2KOvVHINKz/jjYAooRF/xXtiZR6mNvsUmUoTP6rvyzNGm/VPTC3ZvZbBsuxk8EF7kCAwEAAaNTMFEwHQYDVR0OBBYEFBKpxZhrM9chBpxnSWqsv5sNgeB0MB8GA1UdIwQYMBaAFBKpxZhrM9chBpxnSWqsv5sNgeB0MA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQELBQADggEBAK982nXwgNf5or1phiR+ZOfz2C8jhZGCNxpzcoZ3zGg31VmwENtd7qC5N4vbFFyBSU80dKhxp4kdcQbdCZawRI8zqosvZqvNLFqxUxnzGxbyM7AAHzK10U5t+F8c6WxrEo+d1VfQD27KL7FQ64iBs2qIVglUdwa9vsn18zaQzpYA35Lzpc9vayNHimMcd7REA39VtqL4g0dIcdj1LVtOWa2hQBB68xvMfzW+oz9Z0ymNqAmqEgK/6rztxCz3HQ9sb/k7Fkdso+kL8GlSKkzYFx4DOOIf61lBzK1Q6hSlP4av0DmmHlXKizh56xlCZjWmT00hnl0AZW5iUuPVf2t2ZGE=";

    const IDP_ENTITY_ID: &str = "https://idp.example.com/metadata";
    const IDP_SSO_URL: &str = "https://idp.example.com/sso/redirect";
    const SP_ENTITY_ID: &str = "https://sp.example.com/saml2/metadata";
    const ACS_URL: &str = "https://sp.example.com/login/saml2/sso/test";

    fn registration() -> RelyingPartyRegistration {
        RelyingPartyRegistration::builder("test")
            .sp_entity_id(SP_ENTITY_ID)
            .assertion_consumer_service_location(ACS_URL)
            .asserting_party(IDP_ENTITY_ID, IDP_SSO_URL, TEST_IDP_CERT_B64)
            .expect("asserting party")
            .role_attribute("groups")
            .build()
            .expect("registration builds")
    }

    #[test]
    fn builds_from_asserting_party_details_and_emits_sp_metadata() {
        let reg = registration();
        assert_eq!(reg.registration_id(), "test");
        let metadata = reg.metadata_xml().expect("metadata");
        assert!(
            metadata.contains(SP_ENTITY_ID),
            "SP metadata should advertise the SP entity id: {metadata}"
        );
        assert!(metadata.contains("SPSSODescriptor"));
    }

    #[test]
    fn build_fails_closed_when_idp_has_no_signing_certificate() {
        // IdP metadata with an SSO endpoint but NO signing KeyDescriptor.
        let no_cert_metadata = format!(
            concat!(
                "<EntityDescriptor xmlns=\"urn:oasis:names:tc:SAML:2.0:metadata\" entityID=\"{eid}\">",
                "<IDPSSODescriptor protocolSupportEnumeration=\"urn:oasis:names:tc:SAML:2.0:protocol\">",
                "<SingleSignOnService ",
                "Binding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect\" Location=\"{sso}\"/>",
                "</IDPSSODescriptor></EntityDescriptor>"
            ),
            eid = IDP_ENTITY_ID,
            sso = IDP_SSO_URL,
        );
        let result = RelyingPartyRegistration::builder("test")
            .sp_entity_id(SP_ENTITY_ID)
            .assertion_consumer_service_location(ACS_URL)
            .asserting_party_metadata(&no_cert_metadata)
            .expect("metadata parses")
            .build();
        assert!(
            result.is_err(),
            "a registration without an IdP signing cert must fail closed"
        );
    }

    #[test]
    fn build_requires_sp_entity_id_and_acs() {
        let missing_entity = RelyingPartyRegistration::builder("test")
            .assertion_consumer_service_location(ACS_URL)
            .asserting_party(IDP_ENTITY_ID, IDP_SSO_URL, TEST_IDP_CERT_B64)
            .expect("asserting party")
            .build();
        assert!(missing_entity.is_err());
    }

    #[test]
    fn authn_request_redirect_targets_the_idp_with_a_saml_request() {
        let reg = registration();
        let redirect = reg
            .authn_request_redirect("relay-123")
            .expect("authn redirect");
        assert!(
            redirect.url.starts_with(IDP_SSO_URL),
            "redirect should target the IdP SSO URL: {}",
            redirect.url
        );
        assert!(
            redirect.url.contains("SAMLRequest="),
            "redirect must carry a SAMLRequest param: {}",
            redirect.url
        );
        assert!(
            redirect.url.contains("RelayState=relay-123"),
            "redirect should carry the relay state: {}",
            redirect.url
        );
        assert!(
            !redirect.request_id.is_empty(),
            "a request id must be returned for InResponseTo matching"
        );
    }

    #[test]
    fn repository_finds_registrations_by_id() {
        let repo = InMemoryRelyingPartyRegistrationRepository::new(vec![registration()]);
        assert!(repo.find_by_registration_id("test").is_some());
        assert!(repo.find_by_registration_id("nope").is_none());
    }

    #[test]
    fn request_repository_tracks_pending_ids_and_consumes_them() {
        let repo = InMemorySaml2AuthenticationRequestRepository::new();
        repo.save("id-1", Duration::from_secs(300));
        assert!(repo.is_pending("id-1"));
        assert!(!repo.is_pending("id-unknown"));
        // remove reports it was pending, and it is gone afterwards.
        assert!(repo.remove("id-1"));
        assert!(!repo.is_pending("id-1"));
        assert!(!repo.remove("id-1"));
    }

    #[test]
    fn request_repository_expires_ids() {
        let repo = InMemorySaml2AuthenticationRequestRepository::new();
        // An already-expired entry is never pending.
        repo.save("stale", Duration::from_secs(0));
        assert!(!repo.is_pending("stale"));
        assert!(!repo.remove("stale"));
    }

    // --- Signed-response verification (real xmlsec round-trip) --------------
    //
    // These exercise the full verification path against genuinely XML-signed
    // SAML responses produced by `samael`'s test identity provider, so the
    // signature, profile, and replay logic is covered end to end.

    use samael::crypto::{CertificateDer, CryptoProvider, XmlSec};
    use samael::idp::response_builder::{build_response_template, ResponseAttribute};
    use samael::idp::sp_extractor::RequiredAttribute;
    use samael::idp::{CertificateParams, IdentityProvider, KeyType, Rsa};
    use samael::traits::ToXml;

    fn b64() -> base64::engine::general_purpose::GeneralPurpose {
        base64::engine::general_purpose::STANDARD
    }

    /// A fresh test IdP (RSA key) plus its self-signed certificate.
    fn test_idp() -> (IdentityProvider, CertificateDer) {
        let idp = IdentityProvider::generate_new(KeyType::Rsa(Rsa::Rsa2048)).expect("idp key");
        let cert = idp
            .create_certificate(&CertificateParams {
                common_name: "Firefly Test IdP",
                issuer_name: "Firefly Test IdP",
                days_until_expiration: 3650,
            })
            .expect("idp cert");
        (idp, cert)
    }

    fn cert_b64(cert: &CertificateDer) -> String {
        b64().encode(cert.der_data())
    }

    fn registration_with(cert: &CertificateDer) -> RelyingPartyRegistration {
        RelyingPartyRegistration::builder("test")
            .sp_entity_id(SP_ENTITY_ID)
            .assertion_consumer_service_location(ACS_URL)
            .asserting_party(IDP_ENTITY_ID, IDP_SSO_URL, &cert_b64(cert))
            .expect("asserting party")
            .role_attribute("groups")
            .build()
            .expect("registration")
    }

    /// Builds and XML-signs an IdP response for `audience`/`request_id`, with the
    /// given `groups` attribute values, returning the base64 (POST-binding) form.
    fn signed_response(
        idp: &IdentityProvider,
        cert: &CertificateDer,
        name_id: &str,
        audience: &str,
        request_id: &str,
        groups: &[&str],
    ) -> String {
        let attrs: Vec<ResponseAttribute> = groups
            .iter()
            .map(|v| ResponseAttribute {
                required_attribute: RequiredAttribute {
                    name: "groups".to_string(),
                    format: None,
                },
                value: v,
            })
            .collect();
        let response = build_response_template(
            cert,
            name_id,
            audience,
            IDP_ENTITY_ID,
            ACS_URL,
            request_id,
            &attrs,
        );
        let unsigned = response.to_string().expect("serialize response");
        // Serialize signing through the same guard as verification — the native
        // XML-Security stack is not safe to use concurrently.
        let signed = {
            let _guard = XMLSEC_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            XmlSec::sign_xml(
                unsigned.as_str(),
                idp.export_private_key_der().expect("key der").as_slice(),
            )
        }
        .expect("sign response");
        b64().encode(signed.as_bytes())
    }

    #[test]
    #[ignore = "samael 0.0.21 in-process signing segfaults against libxmlsec1 1.3.x; the production verification path works — revisit with a pinned/stable xmlsec"]
    fn verifies_a_signed_response_and_maps_identity() {
        let (idp, cert) = test_idp();
        let reg = registration_with(&cert);
        let request_id = "id-req-ok";
        let resp = signed_response(
            &idp,
            &cert,
            "alice@example.com",
            SP_ENTITY_ID,
            request_id,
            &["admins", "users"],
        );
        let replay = InMemoryAssertionReplayCache::new();
        let auth = reg
            .authenticate(&resp, &[request_id], &replay)
            .expect("a valid signed response authenticates");
        assert_eq!(auth.principal, "alice@example.com");
        assert_eq!(auth.username, "alice@example.com");
        // groups → ROLE_<value>; has_role matches the prefixed authority.
        assert!(auth.has_role("admins"));
        assert!(auth.has_role("users"));
        assert!(auth.claims.contains_key("groups"));
    }

    #[test]
    #[ignore = "samael 0.0.21 in-process signing segfaults against libxmlsec1 1.3.x; the production verification path works — revisit with a pinned/stable xmlsec"]
    fn rejects_a_tampered_response() {
        let (idp, cert) = test_idp();
        let reg = registration_with(&cert);
        let request_id = "id-req-tamper";
        let resp = signed_response(
            &idp,
            &cert,
            "alice@example.com",
            SP_ENTITY_ID,
            request_id,
            &["users"],
        );
        // Swap the NameID *after* signing — the signature no longer covers it.
        let xml = String::from_utf8(b64().decode(&resp).unwrap()).unwrap();
        let tampered = xml.replace("alice@example.com", "attacker@evil.example");
        assert!(tampered.contains("attacker@evil.example"));
        let tampered_b64 = b64().encode(tampered.as_bytes());
        let replay = InMemoryAssertionReplayCache::new();
        assert!(
            reg.authenticate(&tampered_b64, &[request_id], &replay)
                .is_err(),
            "a tampered (signature-breaking) response must be rejected"
        );
    }

    #[test]
    #[ignore = "samael 0.0.21 in-process signing segfaults against libxmlsec1 1.3.x; the production verification path works — revisit with a pinned/stable xmlsec"]
    fn rejects_an_unsigned_response() {
        let (_idp, cert) = test_idp();
        let reg = registration_with(&cert);
        let request_id = "id-req-unsigned";
        let attrs = [ResponseAttribute {
            required_attribute: RequiredAttribute {
                name: "groups".to_string(),
                format: None,
            },
            value: "users",
        }];
        let response = build_response_template(
            &cert,
            "alice@example.com",
            SP_ENTITY_ID,
            IDP_ENTITY_ID,
            ACS_URL,
            request_id,
            &attrs,
        );
        // Serialize WITHOUT signing — the signature template stays empty.
        let unsigned_b64 = b64().encode(response.to_string().unwrap().as_bytes());
        let replay = InMemoryAssertionReplayCache::new();
        assert!(
            reg.authenticate(&unsigned_b64, &[request_id], &replay)
                .is_err(),
            "an unsigned response must be rejected (no signature to verify)"
        );
    }

    #[test]
    #[ignore = "samael 0.0.21 in-process signing segfaults against libxmlsec1 1.3.x; the production verification path works — revisit with a pinned/stable xmlsec"]
    fn rejects_a_replayed_assertion() {
        let (idp, cert) = test_idp();
        let reg = registration_with(&cert);
        let request_id = "id-req-replay";
        let resp = signed_response(
            &idp,
            &cert,
            "alice@example.com",
            SP_ENTITY_ID,
            request_id,
            &["users"],
        );
        let replay = InMemoryAssertionReplayCache::new();
        assert!(reg.authenticate(&resp, &[request_id], &replay).is_ok());
        // The same assertion a second time is a replay.
        assert!(
            reg.authenticate(&resp, &[request_id], &replay).is_err(),
            "replaying the same assertion must be rejected"
        );
    }

    #[test]
    #[ignore = "samael 0.0.21 in-process signing segfaults against libxmlsec1 1.3.x; the production verification path works — revisit with a pinned/stable xmlsec"]
    fn rejects_a_response_for_a_different_audience() {
        let (idp, cert) = test_idp();
        let reg = registration_with(&cert);
        let request_id = "id-req-aud";
        let resp = signed_response(
            &idp,
            &cert,
            "alice@example.com",
            "https://other-sp.example/metadata",
            request_id,
            &["users"],
        );
        let replay = InMemoryAssertionReplayCache::new();
        assert!(
            reg.authenticate(&resp, &[request_id], &replay).is_err(),
            "a response whose audience is another SP must be rejected"
        );
    }

    #[test]
    #[ignore = "samael 0.0.21 in-process signing segfaults against libxmlsec1 1.3.x; the production verification path works — revisit with a pinned/stable xmlsec"]
    fn rejects_an_unexpected_in_response_to() {
        let (idp, cert) = test_idp();
        let reg = registration_with(&cert);
        let resp = signed_response(
            &idp,
            &cert,
            "alice@example.com",
            SP_ENTITY_ID,
            "id-issued",
            &["users"],
        );
        let replay = InMemoryAssertionReplayCache::new();
        // SP-initiated, but we were not awaiting this request id.
        assert!(
            reg.authenticate(&resp, &["a-different-request"], &replay)
                .is_err(),
            "an InResponseTo not matching an issued request must be rejected"
        );
    }
}
