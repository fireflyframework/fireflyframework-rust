//! [`SessionLayer`] / [`SessionService`] — the tower middleware that loads
//! or creates a session on each request and persists it on the response,
//! the Rust port of pyfly's `SessionFilter`.
//!
//! Per request:
//! 1. Parse the `Cookie` header for the configured cookie name; if a
//!    [`SessionSigner`] is configured, verify+strip the signature to the id.
//! 2. Load the session from the [`SessionStore`]; on a miss, mint a new id.
//!    Sessions past their absolute timeout are treated as expired.
//! 3. Insert a [`Session`] handle into the request extensions and run the
//!    inner service. Handlers extract it with `axum::Extension<Session>`
//!    (or the [`crate::SessionExt`] extractor) and may mutate, rotate, or
//!    invalidate it.
//! 4. Persist: delete the pre-rotation id from the store (anti-fixation),
//!    delete on invalidation, else save when modified; append a `Set-Cookie`
//!    (or a delete-cookie on invalidation).
//!
//! Cookie attributes match pyfly: `HttpOnly` (configurable), `SameSite=Lax`
//! (configurable), sliding `Max-Age`, `Secure` auto-enabled over HTTPS or
//! `X-Forwarded-Proto: https`.

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::response::Response;
use futures::future::BoxFuture;
use http::header::{COOKIE, SET_COOKIE};
use http::{HeaderValue, Request};
use tower::{Layer, Service};

use crate::config::SessionConfig;
use crate::session::{new_session_id, Session, SessionInner, CREATED_AT_KEY};
use crate::signing::SessionSigner;
use crate::store::SessionStore;

/// Tower [`Layer`] that installs session load/persist around the inner
/// service — the Rust port of pyfly's `SessionFilter`.
///
/// Build one with [`SessionLayer::new`] (defaults) or
/// [`SessionLayer::from_config`], then attach an optional signer with
/// [`SessionLayer::with_signer`]. The replacement for pyfly's
/// `@auto_configuration` beans: explicit construction, the established
/// workspace pattern.
#[derive(Clone)]
pub struct SessionLayer {
    store: Arc<dyn SessionStore>,
    config: Arc<SessionConfig>,
    signer: Option<Arc<SessionSigner>>,
}

impl std::fmt::Debug for SessionLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLayer")
            .field("config", &self.config)
            .field("signed", &self.signer.is_some())
            .finish_non_exhaustive()
    }
}

impl SessionLayer {
    /// Creates a layer over `store` with the default [`SessionConfig`].
    #[must_use]
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            config: Arc::new(SessionConfig::default()),
            signer: None,
        }
    }

    /// Creates a layer over `store` with an explicit `config` — the analog
    /// of binding `firefly.session.*` and wiring the `SessionFilter` bean.
    #[must_use]
    pub fn from_config(config: SessionConfig, store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            config: Arc::new(config),
            signer: None,
        }
    }

    /// Enables HMAC signing of the session-id cookie value (a Rust
    /// hardening; off by default for pyfly cookie-wire parity).
    #[must_use]
    pub fn with_signer(mut self, signer: SessionSigner) -> Self {
        self.signer = Some(Arc::new(signer));
        self
    }
}

impl<S> Layer<S> for SessionLayer {
    type Service = SessionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SessionService {
            inner,
            store: self.store.clone(),
            config: self.config.clone(),
            signer: self.signer.clone(),
        }
    }
}

/// The tower service produced by [`SessionLayer`].
#[derive(Clone)]
pub struct SessionService<S> {
    inner: S,
    store: Arc<dyn SessionStore>,
    config: Arc<SessionConfig>,
    signer: Option<Arc<SessionSigner>>,
}

impl<S> Service<Request<Body>> for SessionService<S>
where
    S: Service<Request<Body>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let store = self.store.clone();
        let config = self.config.clone();
        let signer = self.signer.clone();

        // Honor `Service` contract: call the cloned-and-swapped inner so the
        // version we invoke is the one `poll_ready` accepted.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        // Determine secure-ness and the incoming cookie id before the body
        // is moved into the async block.
        let is_secure = request_is_secure(&req);
        let cookie_id = extract_cookie_id(&req, &config.cookie_name, signer.as_deref());

        Box::pin(async move {
            let session = load_or_create(&store, cookie_id, &config).await;

            // Share the handle with the handler.
            req.extensions_mut().insert(session.clone());

            let mut response = inner.call(req).await?;

            persist(&store, &session, &config).await;

            decorate_response(
                &mut response,
                &session,
                &config,
                is_secure,
                signer.as_deref(),
            )
            .await;

            Ok(response)
        })
    }
}

/// Loads the session for `cookie_id` from the store, or mints a new one.
/// A stored session past its absolute timeout (from `_created_at`) is
/// treated as expired and replaced by a fresh session.
async fn load_or_create(
    store: &Arc<dyn SessionStore>,
    cookie_id: Option<String>,
    config: &SessionConfig,
) -> Session {
    if let Some(id) = cookie_id {
        if let Ok(Some(data)) = store.get(&id).await {
            if !absolute_timeout_exceeded(&data, config) {
                return Session::new(SessionInner::load(id, data));
            }
            // Past absolute lifetime: drop the stale entry, fall through.
            let _ = store.delete(&id).await;
        }
    }
    Session::new(SessionInner::new(new_session_id()))
}

/// Whether a session's total lifetime (now − `_created_at`) exceeds the
/// configured absolute timeout. Always `false` when no absolute timeout is
/// set (pyfly behavior: sliding idle TTL only).
fn absolute_timeout_exceeded(data: &crate::store::SessionData, config: &SessionConfig) -> bool {
    let Some(max) = config.absolute_timeout() else {
        return false;
    };
    let Some(created_ms) = data.get(CREATED_AT_KEY).and_then(serde_json::Value::as_i64) else {
        return false;
    };
    let age_ms = chrono::Utc::now().timestamp_millis() - created_ms;
    age_ms >= 0 && Duration::from_millis(age_ms as u64) > max
}

/// Persists the session per pyfly's `_persist_session`: delete the
/// pre-rotation id (anti-fixation), delete on invalidation, else save when
/// modified.
async fn persist(store: &Arc<dyn SessionStore>, session: &Session, config: &SessionConfig) {
    let inner = session.lock().await;

    if let Some(prev) = inner.previous_id() {
        if prev != inner.id() {
            let _ = store.delete(prev).await;
        }
    }

    if inner.invalidated() {
        let _ = store.delete(inner.id()).await;
    } else if inner.modified() {
        let _ = store
            .save(inner.id(), inner.data(), config.idle_timeout())
            .await;
    }
}

/// Appends the `Set-Cookie` header: a sliding cookie for a non-invalidated
/// session, or a delete-cookie (`Max-Age=0`) on invalidation — matching
/// pyfly's `set_cookie` / `delete_cookie` branch.
async fn decorate_response(
    response: &mut Response,
    session: &Session,
    config: &SessionConfig,
    is_secure: bool,
    signer: Option<&SessionSigner>,
) {
    let inner = session.lock().await;
    let secure = config.secure || is_secure;

    let header = if inner.invalidated() {
        build_delete_cookie(config)
    } else {
        let value = match signer {
            Some(s) => s.sign(inner.id()),
            None => inner.id().to_string(),
        };
        build_set_cookie(&value, config, secure)
    };

    if let Ok(value) = HeaderValue::from_str(&header) {
        response.headers_mut().append(SET_COOKIE, value);
    }
}

/// Builds a `Set-Cookie` value for a live session.
fn build_set_cookie(value: &str, config: &SessionConfig, secure: bool) -> String {
    let mut cookie = format!("{}={}", config.cookie_name, value);
    cookie.push_str(&format!("; Path={}", config.path));
    if let Some(domain) = &config.domain {
        cookie.push_str(&format!("; Domain={domain}"));
    }
    cookie.push_str(&format!("; Max-Age={}", config.idle_timeout_seconds));
    cookie.push_str(&format!("; SameSite={}", config.same_site.as_str()));
    if config.http_only {
        cookie.push_str("; HttpOnly");
    }
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// Builds a delete-cookie (`Max-Age=0`) matching the live cookie's name and
/// path so the browser drops it.
fn build_delete_cookie(config: &SessionConfig) -> String {
    let mut cookie = format!("{}=", config.cookie_name);
    cookie.push_str(&format!("; Path={}", config.path));
    if let Some(domain) = &config.domain {
        cookie.push_str(&format!("; Domain={domain}"));
    }
    cookie.push_str("; Max-Age=0");
    cookie
}

/// Whether the request arrived over HTTPS, honoring `X-Forwarded-Proto`
/// (first value, case-insensitive) and falling back to the request URI
/// scheme — the Rust port of pyfly's `_is_secure_request`.
fn request_is_secure(req: &Request<Body>) -> bool {
    if let Some(forwarded) = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    {
        return forwarded
            .split(',')
            .next()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
            == Some("https");
    }
    req.uri().scheme_str() == Some("https")
}

/// Extracts the session id from the `Cookie` header for `cookie_name`,
/// verifying+stripping the signature when a `signer` is configured. Returns
/// `None` when the cookie is absent or its signature does not verify.
fn extract_cookie_id(
    req: &Request<Body>,
    cookie_name: &str,
    signer: Option<&SessionSigner>,
) -> Option<String> {
    let raw = cookie_value(req, cookie_name)?;
    match signer {
        Some(s) => s.verify(&raw),
        None => Some(raw),
    }
}

/// Parses the `Cookie` request header (`a=1; b=2`) and returns the value for
/// `name`, or `None`. Hand-rolled (no cookie crate in the catalog); trims
/// surrounding whitespace per RFC 6265 cookie-pair parsing.
fn cookie_value(req: &Request<Body>, name: &str) -> Option<String> {
    let header = req.headers().get(COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_cookie(header: &str) -> Request<Body> {
        Request::builder()
            .header(COOKIE, header)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn cookie_value_parses_pairs() {
        let req = req_with_cookie("a=1; PYFLY_SESSION=abc; b=2");
        assert_eq!(cookie_value(&req, "PYFLY_SESSION").as_deref(), Some("abc"));
        assert_eq!(cookie_value(&req, "a").as_deref(), Some("1"));
        assert_eq!(cookie_value(&req, "missing"), None);
    }

    #[test]
    fn cookie_value_absent_header() {
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(cookie_value(&req, "PYFLY_SESSION"), None);
    }

    #[test]
    fn extract_with_signer_verifies() {
        let signer = SessionSigner::new("k");
        let signed = signer.sign("sid-1");
        let req = req_with_cookie(&format!("PYFLY_SESSION={signed}"));
        assert_eq!(
            extract_cookie_id(&req, "PYFLY_SESSION", Some(&signer)).as_deref(),
            Some("sid-1")
        );
        // Unsigned value under a signer fails to verify.
        let raw = req_with_cookie("PYFLY_SESSION=sid-1");
        assert_eq!(
            extract_cookie_id(&raw, "PYFLY_SESSION", Some(&signer)),
            None
        );
    }

    #[test]
    fn request_is_secure_via_forwarded_proto() {
        let req = Request::builder()
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap();
        assert!(request_is_secure(&req));

        let req = Request::builder()
            .header("x-forwarded-proto", "http")
            .body(Body::empty())
            .unwrap();
        assert!(!request_is_secure(&req));
    }

    #[test]
    fn request_is_secure_via_scheme() {
        let req = Request::builder()
            .uri("https://example.com/")
            .body(Body::empty())
            .unwrap();
        assert!(request_is_secure(&req));
        let req = Request::builder()
            .uri("http://example.com/")
            .body(Body::empty())
            .unwrap();
        assert!(!request_is_secure(&req));
    }

    #[test]
    fn set_cookie_has_pyfly_attributes() {
        let config = SessionConfig::default();
        let cookie = build_set_cookie("abc", &config, false);
        assert!(cookie.starts_with("PYFLY_SESSION=abc"));
        assert!(cookie.contains("; Path=/"));
        assert!(cookie.contains("; Max-Age=1800"));
        assert!(cookie.contains("; SameSite=Lax"));
        assert!(cookie.contains("; HttpOnly"));
        assert!(!cookie.contains("; Secure"));
    }

    #[test]
    fn set_cookie_secure_when_requested() {
        let config = SessionConfig::default();
        assert!(build_set_cookie("abc", &config, true).contains("; Secure"));
    }

    #[test]
    fn delete_cookie_zeroes_max_age() {
        let cookie = build_delete_cookie(&SessionConfig::default());
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.starts_with("PYFLY_SESSION="));
    }
}
