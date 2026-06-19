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
//! lifting — XML-signature verification, canonicalization, and most SAML profile
//! checks (recipient, `InResponseTo`, status, conditions, subject confirmation)
//! — is delegated to the [`samael`] crate (which links the battle-tested
//! `xmlsec`/`libxml2`/OpenSSL stack). This module is the Spring-faithful,
//! **hardened** wrapper around it:
//!
//! 1. [`RelyingPartyRegistration`] holds one SP↔IdP relationship. Building one
//!    **fails closed** when the asserting party (IdP) has no signing
//!    certificate — without it `samael` would skip signature verification
//!    entirely, which is an authentication bypass.
//! 2. Signature verification is pinned to a safe **allow-list of algorithms**
//!    (RSA/ECDSA-SHA256+) — `samael`'s default of "all algorithms" is open to
//!    algorithm-substitution attacks.
//! 3. The **audience restriction** is enforced fail-closed: `samael` skips it
//!    when the assertion omits `AudienceRestriction`, so [`authenticate`] requires
//!    this SP's entity id to be a listed audience.
//! 4. [`AssertionReplayCache`] adds **one-time-use** assertion-ID replay
//!    protection, which the SAML profile requires but `samael` does not do.
//! 5. [`Saml2AuthenticationRequestRepository`] tracks outgoing `AuthnRequest`
//!    IDs (with a TTL) so a response's `InResponseTo` can be matched to a
//!    request this SP actually issued.
//!
//! [`authenticate`]: RelyingPartyRegistration::authenticate
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
/// run concurrently (concurrent use segfaults). Every signature verification —
/// the only entry into the native XML-Security stack — is serialized through
/// this guard. Verification is fast and logins are not a hot path, so the
/// serialization cost is acceptable.
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
    /// The SP entity id, which is the SAML audience this SP requires. Kept here
    /// so [`authenticate`](Self::authenticate) can enforce the AudienceRestriction
    /// itself (`samael` skips that check when the restriction is absent).
    audience: String,
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
    /// certificate and this registration's allowed algorithms), recipient,
    /// status, `InResponseTo`, and the response/assertion/subject-confirmation
    /// time conditions. On top of that, this method enforces the **audience
    /// restriction** (fail-closed — `samael` skips it when the assertion omits
    /// `AudienceRestriction`) and **one-time-use** of the assertion via
    /// `replay_cache` (which `samael` does not track), and rejects an assertion
    /// that carries no usable `NameID`.
    ///
    /// `expected_request_ids` are the `AuthnRequest` IDs this SP issued and is
    /// still awaiting; the response's `InResponseTo` must match one of them
    /// unless the registration allows IdP-initiated SSO. The caller **must** pass
    /// the actual per-login pending id(s) (never a static value) and, after a
    /// successful call, retire the matched id via
    /// [`Saml2AuthenticationRequestRepository::remove`] so the same `InResponseTo`
    /// cannot be reused.
    ///
    /// # Errors
    /// Any signature, profile, audience, replay, or decoding failure — all
    /// surfaced as a [`SecurityError::Verification`].
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

        // Enforce the audience restriction ourselves: `samael` skips the check
        // entirely when the assertion has no Conditions/AudienceRestriction, so
        // require this SP's entity id to be a listed audience (fail closed).
        if !audience_includes(&assertion, &self.audience) {
            return Err(SecurityError::verification(
                "saml2: assertion audience does not include this service provider",
            ));
        }

        // One-time-use: reject a replayed assertion (samael does not track this).
        // Retain the id slightly past its validity window to cover clock skew.
        let expires_at = assertion_expiry(&assertion).map(|t| t + REPLAY_SKEW_MARGIN);
        replay_cache.check_and_remember(&assertion.id, expires_at)?;

        let auth = self.map_assertion(assertion);
        // A verified login must name a principal; an empty NameID is anonymous
        // and would alias across logins, so reject it.
        if auth.principal.trim().is_empty() {
            return Err(SecurityError::verification(
                "saml2: assertion carries no NameID / principal",
            ));
        }
        Ok(auth)
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
        // Accumulate per attribute name so that duplicate `<Attribute>` blocks
        // (a legal, if uncommon, IdP shape) merge rather than overwrite — keeping
        // `claims` consistent with the roles gathered from every block.
        let mut attribute_values: HashMap<String, Vec<String>> = HashMap::new();
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
                attribute_values
                    .entry(name.to_string())
                    .or_default()
                    .extend(values);
            }
        }
        let claims = attribute_values
            .into_iter()
            .map(|(name, values)| {
                (
                    name,
                    Value::Array(values.into_iter().map(Value::String).collect()),
                )
            })
            .collect();

        Authentication {
            principal: principal.clone(),
            username: principal,
            roles,
            authorities: Vec::new(),
            claims,
        }
    }
}

/// Whether `assertion` carries an `AudienceRestriction` that lists `audience`.
/// Returns `false` (fail-closed) when there is no `Conditions` /
/// `AudienceRestriction` at all — `samael` would otherwise skip the check.
fn audience_includes(assertion: &Assertion, audience: &str) -> bool {
    assertion
        .conditions
        .as_ref()
        .and_then(|c| c.audience_restrictions.as_ref())
        .is_some_and(|restrictions| {
            restrictions
                .iter()
                .any(|r| r.audience.iter().any(|a| a == audience))
        })
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
    ///
    /// **Caveat:** enabling this disables `InResponseTo` request-binding (per the
    /// IdP-initiated profile — there is no SP request to bind to), so the
    /// [`AssertionReplayCache`] becomes the **sole** freshness control. A
    /// multi-instance / load-balanced deployment must then supply a *shared*
    /// replay cache — the [`InMemoryAssertionReplayCache`] is per-process and
    /// cannot stop a replay against a different instance.
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
        // The SP entity id is the audience this SP requires of an assertion.
        let audience = entity_id.clone();
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
            audience,
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
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let now = SystemTime::now();
        pending.retain(|_, exp| *exp > now);
        pending.insert(request_id.to_string(), expires_at);
    }

    fn is_pending(&self, request_id: &str) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending
            .get(request_id)
            .is_some_and(|exp| *exp > SystemTime::now())
    }

    fn remove(&self, request_id: &str) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
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
///
/// **Per-process only:** it cannot detect a replay presented to a *different*
/// instance. A multi-instance / load-balanced deployment should supply a shared
/// (e.g. Redis-backed) [`AssertionReplayCache`] instead — especially with
/// [IdP-initiated SSO](RelyingPartyRegistrationBuilder::allow_idp_initiated),
/// where the cache is the only freshness control.
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
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
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

    // --- Response handling: verification integration + mapping -------------
    //
    // Production verifies an IdP-signed response through `samael`, whose own
    // crypto test-suite proves it accepts valid signatures and rejects bad ones
    // against the installed xmlsec. These tests cover *this module's* logic: the
    // attribute → authorities mapping (on a real `samael` Assertion), one-time-use
    // replay protection, and that an unsigned response is rejected end to end.

    use samael::crypto::{decode_x509_cert, CertificateDer};
    use samael::idp::response_builder::{build_response_template, ResponseAttribute};
    use samael::idp::sp_extractor::RequiredAttribute;
    use samael::schema::Response;
    use samael::traits::ToXml;

    fn b64() -> base64::engine::general_purpose::GeneralPurpose {
        base64::engine::general_purpose::STANDARD
    }

    fn idp_cert() -> CertificateDer {
        decode_x509_cert(TEST_IDP_CERT_B64).expect("decode test IdP cert")
    }

    /// Builds an IdP response carrying the given `groups` attribute values (with
    /// only the empty signature template — it is never signed).
    fn build_response(
        name_id: &str,
        audience: &str,
        request_id: &str,
        groups: &[&str],
    ) -> Response {
        let cert = idp_cert();
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
        build_response_template(
            &cert,
            name_id,
            audience,
            IDP_ENTITY_ID,
            ACS_URL,
            request_id,
            &attrs,
        )
    }

    #[test]
    fn maps_a_verified_assertion_to_authentication() {
        // `map_assertion` runs on the Assertion `samael` returns from a verified
        // response; build a structurally-identical one and map it.
        let reg = registration();
        let response = build_response(
            "alice@example.com",
            SP_ENTITY_ID,
            "id-req",
            &["admins", "users"],
        );
        let assertion = response.assertion.expect("assertion present");
        let auth = reg.map_assertion(assertion);
        assert_eq!(auth.principal, "alice@example.com");
        assert_eq!(auth.username, "alice@example.com");
        // Each configured role-attribute value → ROLE_<value>; has_role matches
        // the prefixed authority. Every attribute is also exposed in claims.
        assert!(auth.has_role("admins"));
        assert!(auth.has_role("users"));
        assert!(auth.claims.contains_key("groups"));
    }

    #[test]
    fn maps_no_roles_when_no_role_attribute_is_configured() {
        // A registration without a role_attribute grants no roles, but still
        // exposes the attributes as claims.
        let reg = RelyingPartyRegistration::builder("test")
            .sp_entity_id(SP_ENTITY_ID)
            .assertion_consumer_service_location(ACS_URL)
            .asserting_party(IDP_ENTITY_ID, IDP_SSO_URL, TEST_IDP_CERT_B64)
            .expect("asserting party")
            .build()
            .expect("registration");
        let response = build_response("bob@example.com", SP_ENTITY_ID, "id-req", &["admins"]);
        let auth = reg.map_assertion(response.assertion.expect("assertion"));
        assert_eq!(auth.principal, "bob@example.com");
        assert!(auth.roles.is_empty());
        assert!(auth.claims.contains_key("groups"));
    }

    #[test]
    fn rejects_an_unsigned_response() {
        // End to end: an unsigned response is rejected by the verification path
        // (samael finds no valid signature for the configured IdP cert).
        let reg = registration();
        let request_id = "id-req-unsigned";
        let xml = build_response("alice@example.com", SP_ENTITY_ID, request_id, &["users"])
            .to_string()
            .expect("serialize response");
        let resp_b64 = b64().encode(xml.as_bytes());
        let replay = InMemoryAssertionReplayCache::new();
        assert!(
            reg.authenticate(&resp_b64, &[request_id], &replay).is_err(),
            "an unsigned response must be rejected"
        );
    }

    #[test]
    fn rejects_non_base64_and_oversized_responses() {
        let reg = registration();
        let replay = InMemoryAssertionReplayCache::new();
        // Not valid base64.
        assert!(reg
            .authenticate("@@@not base64@@@", &["id"], &replay)
            .is_err());
        // Oversized input is rejected before allocating the decoded buffer.
        let huge = "A".repeat(11 * 1024 * 1024);
        assert!(reg.authenticate(&huge, &["id"], &replay).is_err());
    }

    #[test]
    fn replay_cache_enforces_one_time_use() {
        let cache = InMemoryAssertionReplayCache::new();
        // First use of an assertion id is accepted; the second is a replay.
        assert!(cache.check_and_remember("assertion-1", None).is_ok());
        assert!(cache.check_and_remember("assertion-1", None).is_err());
        // A different id is independent.
        assert!(cache.check_and_remember("assertion-2", None).is_ok());
        // An already-expired entry never blocks a later use.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        assert!(cache.check_and_remember("assertion-3", Some(past)).is_ok());
        assert!(cache.check_and_remember("assertion-3", Some(past)).is_ok());
    }

    #[test]
    fn audience_restriction_is_enforced_fail_closed() {
        // A matching audience is accepted.
        let matching = build_response("alice@example.com", SP_ENTITY_ID, "id", &[])
            .assertion
            .unwrap();
        assert!(audience_includes(&matching, SP_ENTITY_ID));
        // A different SP's audience is rejected.
        let other = build_response(
            "alice@example.com",
            "https://other-sp.example/metadata",
            "id",
            &[],
        )
        .assertion
        .unwrap();
        assert!(!audience_includes(&other, SP_ENTITY_ID));
        // No AudienceRestriction at all → fail closed (samael would skip it).
        let mut absent = build_response("alice@example.com", SP_ENTITY_ID, "id", &[])
            .assertion
            .unwrap();
        absent.conditions = None;
        assert!(!audience_includes(&absent, SP_ENTITY_ID));
    }

    #[test]
    fn missing_name_id_maps_to_an_anonymous_principal() {
        // An assertion with no Subject/NameID maps to an empty principal, which
        // is not authenticated (authenticate() rejects it before returning Ok).
        let reg = registration();
        let mut assertion = build_response("alice@example.com", SP_ENTITY_ID, "id", &["users"])
            .assertion
            .unwrap();
        assertion.subject = None;
        let auth = reg.map_assertion(assertion);
        assert!(auth.principal.is_empty());
        assert!(!auth.is_authenticated());
    }

    #[test]
    fn duplicate_attribute_blocks_merge_into_claims() {
        // `&["admins", "users"]` produces two <Attribute Name="groups"> blocks;
        // both contribute roles AND both values survive in claims (not last-wins).
        let reg = registration();
        let assertion = build_response(
            "alice@example.com",
            SP_ENTITY_ID,
            "id",
            &["admins", "users"],
        )
        .assertion
        .unwrap();
        let auth = reg.map_assertion(assertion);
        assert!(auth.has_role("admins"));
        assert!(auth.has_role("users"));
        let groups = auth
            .claims
            .get("groups")
            .and_then(|v| v.as_array())
            .expect("groups claim is an array");
        assert_eq!(
            groups.len(),
            2,
            "both attribute blocks must survive in claims"
        );
    }
}
