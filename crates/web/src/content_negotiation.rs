//! HTTP message converters and `Accept`-driven content negotiation —
//! the Rust port of pyfly's `pyfly.web.message_converters` (Spring's
//! `HttpMessageConverter` equivalent) plus the `dict_to_xml` /
//! `xml_to_dict` helpers from `pyfly.web.converters`.
//!
//! An ordered, pluggable [`MessageConverterRegistry`], each converter
//! bound to media types, used for BOTH reading request bodies and
//! writing responses. Negotiation honors the `Accept` header with
//! q-values on write ([`parse_accept`]) and the `Content-Type` on read.
//! Ships JSON and XML converters (XML via `quick-xml`); register more
//! (e.g. CBOR) by implementing [`MessageConverter`].
//!
//! Converters exchange [`serde_json::Value`] — the Rust spelling of the
//! Python dict pyfly's converters pass around. Typed handlers go
//! through `serde_json::to_value` / `from_value` at the edges.
//!
//! Response-side wiring is the [`Negotiate`] responder plus
//! [`ContentNegotiationLayer`]: a handler returns `Negotiate(dto)`,
//! which renders JSON by default; when the layer is installed it
//! rewrites the body with the converter the request's `Accept` header
//! selects.

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use firefly_kernel::{FireflyError, ProblemDetail};
use futures::future::BoxFuture;
use http::{header, HeaderValue, Request};
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use serde::Serialize;
use serde_json::{Map, Value};
use tower::{Layer, Service};

use crate::problem::problem_response;

/// Parses an `Accept` header into media types ordered by descending
/// q-value, byte-compatible with pyfly's `parse_accept`: `None` (or
/// empty) defaults to `["application/json"]`, parameters are stripped,
/// types are lowercased, and equal q-values preserve header order.
pub fn parse_accept(accept: Option<&str>) -> Vec<String> {
    let Some(accept) = accept.filter(|a| !a.trim().is_empty()) else {
        return vec!["application/json".to_string()];
    };
    let mut items: Vec<(String, f64)> = Vec::new();
    for (index, part) in accept.split(',').enumerate() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut tokens = part.split(';');
        let media_type = tokens.next().unwrap_or_default().trim().to_lowercase();
        let mut quality = 1.0f64;
        for token in tokens {
            let token = token.trim();
            if let Some(q) = token.strip_prefix("q=") {
                quality = q.parse().unwrap_or(1.0);
            }
        }
        // Stable within equal q: preserve header order via the index
        // tiebreak (identical to pyfly).
        items.push((media_type, quality - index as f64 * 1e-6));
    }
    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    items
        .into_iter()
        .map(|(media_type, _)| media_type)
        .collect()
}

/// Base converter: reads/writes HTTP bodies for its
/// [`MessageConverter::media_types`]. The unit of exchange is
/// [`serde_json::Value`] (pyfly passes Python dicts).
pub trait MessageConverter: Send + Sync {
    /// The media types this converter handles (lowercase, no params).
    fn media_types(&self) -> &[&str];

    /// Whether this converter handles `media_type` (`*/*` matches any).
    /// Parameters (`; charset=…`) are stripped before matching.
    fn supports(&self, media_type: &str) -> bool {
        let base = media_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        base == "*/*" || self.media_types().contains(&base.as_str())
    }

    /// Deserializes a request body into a [`Value`].
    fn read(&self, body: &[u8]) -> Result<Value, FireflyError>;

    /// Serializes `value`; returns `(body_bytes, content_type)`.
    fn write(&self, value: &Value) -> Result<(Vec<u8>, String), FireflyError>;
}

/// JSON converter — `application/json`, via `serde_json`.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonMessageConverter;

impl JsonMessageConverter {
    /// Returns the converter. It carries no state.
    pub fn new() -> Self {
        Self
    }
}

impl MessageConverter for JsonMessageConverter {
    fn media_types(&self) -> &[&str] {
        &["application/json"]
    }

    fn read(&self, body: &[u8]) -> Result<Value, FireflyError> {
        if body.is_empty() {
            // pyfly decodes an empty body as JSON `null`.
            return Ok(Value::Null);
        }
        serde_json::from_slice(body)
            .map_err(|err| FireflyError::bad_request(format!("invalid JSON: {err}")))
    }

    fn write(&self, value: &Value) -> Result<(Vec<u8>, String), FireflyError> {
        let body = serde_json::to_vec(value)
            .map_err(|err| FireflyError::internal(format!("JSON serialization failed: {err}")))?;
        Ok((body, "application/json".to_string()))
    }
}

/// XML converter — `application/xml` + `text/xml`, via `quick-xml`,
/// using the same value↔XML mapping as pyfly's stdlib-`ElementTree`
/// converters ([`value_to_xml`] / [`xml_to_value`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct XmlMessageConverter;

impl XmlMessageConverter {
    /// Returns the converter. It carries no state.
    pub fn new() -> Self {
        Self
    }
}

impl MessageConverter for XmlMessageConverter {
    fn media_types(&self) -> &[&str] {
        &["application/xml", "text/xml"]
    }

    fn read(&self, body: &[u8]) -> Result<Value, FireflyError> {
        let text = std::str::from_utf8(body)
            .map_err(|err| FireflyError::bad_request(format!("invalid XML encoding: {err}")))?;
        let parsed = xml_to_value(text)?;
        // Unwrap the root tag, exactly like pyfly's XmlMessageConverter.
        match parsed {
            Value::Object(map) if map.len() == 1 => Ok(map
                .into_iter()
                .next()
                .map(|(_, v)| v)
                .unwrap_or(Value::Null)),
            other => Ok(other),
        }
    }

    fn write(&self, value: &Value) -> Result<(Vec<u8>, String), FireflyError> {
        let xml = value_to_xml(value, "response")?;
        Ok((xml.into_bytes(), "application/xml".to_string()))
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn write_element<W: std::io::Write>(
    writer: &mut quick_xml::Writer<W>,
    key: &str,
    value: &Value,
) -> Result<(), quick_xml::Error> {
    match value {
        Value::Object(map) => {
            writer.write_event(Event::Start(BytesStart::new(key)))?;
            for (k, v) in map {
                write_element(writer, k, v)?;
            }
            writer.write_event(Event::End(BytesEnd::new(key)))?;
        }
        Value::Array(items) => {
            // Lists produce repeated sibling elements named after the key.
            for item in items {
                write_element(writer, key, item)?;
            }
        }
        Value::Null => {
            writer.write_event(Event::Empty(BytesStart::new(key)))?;
        }
        scalar => {
            writer.write_event(Event::Start(BytesStart::new(key)))?;
            writer.write_event(Event::Text(BytesText::new(&scalar_text(scalar))))?;
            writer.write_event(Event::End(BytesEnd::new(key)))?;
        }
    }
    Ok(())
}

/// Converts a [`Value`] to an XML string under `root_tag` — the Rust
/// port of pyfly's `dict_to_xml`. Objects nest, arrays repeat sibling
/// elements (top-level arrays use `<item>`), `null` produces an empty
/// element, scalars become text content. Booleans render as
/// `true`/`false` (deliberate divergence from Python's `str(True)` —
/// `"True"` — both spellings parse identically on every port).
pub fn value_to_xml(value: &Value, root_tag: &str) -> Result<String, FireflyError> {
    let mut writer = quick_xml::Writer::new(Vec::new());
    let fail = |err: quick_xml::Error| FireflyError::internal(format!("XML write failed: {err}"));
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))
        .map_err(fail)?;
    match value {
        Value::Object(map) => {
            writer
                .write_event(Event::Start(BytesStart::new(root_tag)))
                .map_err(fail)?;
            for (k, v) in map {
                write_element(&mut writer, k, v).map_err(fail)?;
            }
            writer
                .write_event(Event::End(BytesEnd::new(root_tag)))
                .map_err(fail)?;
        }
        Value::Array(items) => {
            writer
                .write_event(Event::Start(BytesStart::new(root_tag)))
                .map_err(fail)?;
            for item in items {
                write_element(&mut writer, "item", item).map_err(fail)?;
            }
            writer
                .write_event(Event::End(BytesEnd::new(root_tag)))
                .map_err(fail)?;
        }
        Value::Null => {
            writer
                .write_event(Event::Empty(BytesStart::new(root_tag)))
                .map_err(fail)?;
        }
        scalar => {
            writer
                .write_event(Event::Start(BytesStart::new(root_tag)))
                .map_err(fail)?;
            writer
                .write_event(Event::Text(BytesText::new(&scalar_text(scalar))))
                .map_err(fail)?;
            writer
                .write_event(Event::End(BytesEnd::new(root_tag)))
                .map_err(fail)?;
        }
    }
    String::from_utf8(writer.into_inner())
        .map_err(|err| FireflyError::internal(format!("XML write produced invalid UTF-8: {err}")))
}

fn parse_children(reader: &mut quick_xml::Reader<&[u8]>) -> Result<Value, FireflyError> {
    let bad = |err: String| FireflyError::bad_request(format!("invalid XML: {err}"));
    let mut children: Vec<(String, Value)> = Vec::new();
    // `None` until a Text/CData event fires, mirroring ElementTree's
    // `element.text`: a genuinely empty `<foo></foo>` leaves `text` as
    // `None` (→ null), whereas any text content — including
    // whitespace-only — is preserved verbatim (no trimming), exactly
    // like pyfly's `_element_to_dict` returning `element.text` unchanged.
    let mut text: Option<String> = None;
    loop {
        match reader.read_event().map_err(|e| bad(e.to_string()))? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let value = parse_children(reader)?;
                children.push((name, value));
            }
            Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                children.push((name, Value::Null));
            }
            Event::Text(t) => {
                text.get_or_insert_with(String::new)
                    .push_str(&t.unescape().map_err(|e| bad(e.to_string()))?);
            }
            Event::CData(c) => {
                text.get_or_insert_with(String::new)
                    .push_str(&String::from_utf8_lossy(&c.into_inner()));
            }
            Event::End(_) => break,
            Event::Eof => return Err(bad("unexpected end of document".to_string())),
            _ => {}
        }
    }
    if children.is_empty() {
        // ElementTree preserves leaf text verbatim (no trim); a leaf with
        // no text node at all is `None`.
        return Ok(match text {
            Some(s) => Value::String(s),
            None => Value::Null,
        });
    }
    let mut map = Map::new();
    for (key, value) in children {
        match map.get_mut(&key) {
            Some(Value::Array(arr)) => arr.push(value),
            Some(existing) => {
                let first = existing.take();
                *existing = Value::Array(vec![first, value]);
            }
            None => {
                map.insert(key, value);
            }
        }
    }
    Ok(Value::Object(map))
}

/// Parses an XML string into a [`Value`] — the Rust port of pyfly's
/// `xml_to_dict`. The root element becomes the single top-level key;
/// repeated sibling tags collect into arrays; leaf text stays a string
/// preserved verbatim, including leading/trailing whitespace (XML is
/// untyped — callers coerce, as pydantic does on the Python side);
/// elements with no text node (`<foo></foo>` or `<foo/>`) become `null`.
/// Attributes are ignored, exactly like the ElementTree original.
pub fn xml_to_value(xml: &str) -> Result<Value, FireflyError> {
    let bad = |err: String| FireflyError::bad_request(format!("invalid XML: {err}"));
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event().map_err(|e| bad(e.to_string()))? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let value = parse_children(&mut reader)?;
                let mut map = Map::new();
                map.insert(name, value);
                return Ok(Value::Object(map));
            }
            Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let mut map = Map::new();
                map.insert(name, Value::Null);
                return Ok(Value::Object(map));
            }
            Event::Eof => return Err(bad("no root element".to_string())),
            _ => {}
        }
    }
}

/// Ordered converters; first match wins. Reads by `Content-Type`,
/// writes by `Accept` (q-value ordered). User-added converters take
/// priority — the Rust port of pyfly's `MessageConverterRegistry`.
#[derive(Clone, Default)]
pub struct MessageConverterRegistry {
    converters: Vec<Arc<dyn MessageConverter>>,
}

impl std::fmt::Debug for MessageConverterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageConverterRegistry")
            .field("converters", &self.converters.len())
            .finish()
    }
}

impl MessageConverterRegistry {
    /// An empty registry (use [`default_message_converters`] for the
    /// built-in JSON + XML pair).
    pub fn new(converters: Vec<Arc<dyn MessageConverter>>) -> Self {
        Self { converters }
    }

    /// Registers `converter` at the front (highest priority).
    pub fn add(&mut self, converter: Arc<dyn MessageConverter>) {
        self.converters.insert(0, converter);
    }

    /// The registered converters, highest priority first.
    pub fn converters(&self) -> &[Arc<dyn MessageConverter>] {
        &self.converters
    }

    /// The converter for a request `Content-Type` (falls back to the
    /// first registered converter when none matches).
    pub fn find_reader(&self, content_type: Option<&str>) -> Option<&Arc<dyn MessageConverter>> {
        if let Some(content_type) = content_type {
            if let Some(found) = self.converters.iter().find(|c| c.supports(content_type)) {
                return Some(found);
            }
        }
        self.converters.first()
    }

    /// The best converter for an `Accept` header (q-value ordered;
    /// falls back to the first registered converter).
    pub fn find_writer(&self, accept: Option<&str>) -> Option<&Arc<dyn MessageConverter>> {
        for media_type in parse_accept(accept) {
            if let Some(found) = self.converters.iter().find(|c| c.supports(&media_type)) {
                return Some(found);
            }
        }
        self.converters.first()
    }
}

/// The built-in registry: JSON (first/default) then XML — the Rust
/// port of pyfly's `default_message_converters`.
pub fn default_message_converters() -> MessageConverterRegistry {
    MessageConverterRegistry::new(vec![
        Arc::new(JsonMessageConverter::new()),
        Arc::new(XmlMessageConverter::new()),
    ])
}

/// The negotiable payload [`Negotiate`] stages in the response
/// extensions for [`ContentNegotiationLayer`] to re-render.
#[derive(Debug, Clone)]
pub struct NegotiablePayload(pub Value);

/// Content-negotiating responder: wrap any `Serialize` handler return
/// value and the response format follows the request's `Accept` header
/// (via [`ContentNegotiationLayer`]). Without the layer it renders
/// JSON — the pyfly default when no converter matches.
///
/// ```
/// use axum::{routing::get, Router};
/// use firefly_web::{ContentNegotiationLayer, Negotiate};
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Widget { name: String, qty: u32 }
///
/// async fn widget() -> Negotiate<Widget> {
///     Negotiate(Widget { name: "gadget".into(), qty: 3 })
/// }
///
/// let app: Router = Router::new()
///     .route("/widget", get(widget))
///     .layer(ContentNegotiationLayer::default());
/// # let _ = app;
/// ```
#[derive(Debug, Clone)]
pub struct Negotiate<T>(pub T);

impl<T: Serialize> IntoResponse for Negotiate<T> {
    fn into_response(self) -> Response {
        let value = match serde_json::to_value(&self.0) {
            Ok(value) => value,
            Err(err) => {
                return problem_response(&ProblemDetail::internal(format!(
                    "serialization failed: {err}"
                )))
            }
        };
        let body = serde_json::to_vec(&value).unwrap_or_default();
        let mut res = Response::new(Body::from(body));
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        res.extensions_mut().insert(NegotiablePayload(value));
        res
    }
}

/// Rewrites [`Negotiate`] responses with the converter selected by the
/// request's `Accept` header — the tower spelling of pyfly's
/// `handle_return_value` wiring. Non-negotiable responses (no
/// [`NegotiablePayload`] extension) pass through untouched.
#[derive(Debug, Clone)]
pub struct ContentNegotiationLayer {
    registry: Arc<MessageConverterRegistry>,
}

impl ContentNegotiationLayer {
    /// Builds the layer over `registry`.
    pub fn new(registry: MessageConverterRegistry) -> Self {
        Self {
            registry: Arc::new(registry),
        }
    }
}

impl Default for ContentNegotiationLayer {
    fn default() -> Self {
        Self::new(default_message_converters())
    }
}

impl<S> Layer<S> for ContentNegotiationLayer {
    type Service = ContentNegotiationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ContentNegotiationService {
            inner,
            registry: Arc::clone(&self.registry),
        }
    }
}

/// The tower service produced by [`ContentNegotiationLayer`].
#[derive(Debug, Clone)]
pub struct ContentNegotiationService<S> {
    inner: S,
    registry: Arc<MessageConverterRegistry>,
}

impl<S> Service<Request<Body>> for ContentNegotiationService<S>
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

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let registry = Arc::clone(&self.registry);
        let accept = req
            .headers()
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);

        Box::pin(async move {
            let mut res = inner.call(req).await?;
            let Some(NegotiablePayload(value)) = res.extensions_mut().remove::<NegotiablePayload>()
            else {
                return Ok(res);
            };
            let Some(writer) = registry.find_writer(accept.as_deref()) else {
                return Ok(res);
            };
            match writer.write(&value) {
                Ok((body, content_type)) => {
                    let (mut parts, _) = res.into_parts();
                    parts.headers.remove(header::CONTENT_LENGTH);
                    if let Ok(ct) = HeaderValue::from_str(&content_type) {
                        parts.headers.insert(header::CONTENT_TYPE, ct);
                    }
                    Ok(Response::from_parts(parts, Body::from(body)))
                }
                Err(err) => Ok(problem_response(&err.to_problem())),
            }
        })
    }
}
