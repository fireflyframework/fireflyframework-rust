//! Event filters — predicate gates that decide whether an [`Event`] is
//! delivered to a handler.
//!
//! Mirrors pyfly's `eda.filter` package (`EventFilter` /
//! `HeaderEventFilter` / `PredicateEventFilter`): a pluggable
//! delivery-gate abstraction layered *over* the broker's glob topic/type
//! matching. Where the broker decides *which* subscriptions a topic
//! reaches, a filter decides — per envelope — whether a reached
//! subscription actually *runs*. A subscription can carry a chain of
//! filters; an event that fails any filter is silently dropped before
//! the handler body executes.
//!
//! ## Attaching filters to a subscription
//!
//! [`with_filters`] wraps an existing [`Handler`] so the inner handler
//! only runs for events that every filter accepts; non-matching events
//! are dropped (the wrapped handler returns `Ok(())` without invoking
//! the inner handler). This is the Rust spelling of pyfly's
//! `@event_listener(filters=[…])`.
//!
//! ```
//! use firefly_eda::{handler, with_filters, Event, HeaderEventFilter, InMemoryBroker};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let broker = InMemoryBroker::new();
//! let inner = handler(|ev: Event| async move {
//!     // only ever reached for acme-* tenants
//!     assert!(ev.headers.get("x-tenant").unwrap().starts_with("acme-"));
//!     Ok(())
//! });
//! let gated = with_filters(inner, [HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap()]);
//! broker.subscribe("orders", gated).unwrap();
//! # });
//! ```

use std::sync::Arc;

use regex::Regex;

use crate::{handler, Event, Handler};

/// A delivery gate: decides whether `event` should be delivered to a
/// handler. The Rust analog of pyfly's `EventFilter` protocol
/// (`accepts(envelope) -> bool`).
///
/// Object-safe so heterogeneous filters can be stored together in a
/// `Vec<Arc<dyn EventFilter>>` and composed into a chain by
/// [`with_filters`].
pub trait EventFilter: Send + Sync {
    /// Returns `true` if `event` should be delivered, `false` to drop it.
    fn accepts(&self, event: &Event) -> bool;
}

/// Accepts events whose header `name` *matches* `pattern` (an anchored
/// regular expression, matched from the start of the value like pyfly's
/// `re.match`). A missing header is treated as the empty string, so it
/// matches only a pattern that also accepts empty input.
///
/// pyfly's `HeaderEventFilter(name, pattern)`.
///
/// ```
/// use firefly_eda::{Event, EventFilter, HeaderEventFilter};
///
/// let f = HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap();
/// let ev = Event::new("orders", "OrderPlaced", "svc", None).with_header("x-tenant", "acme-eu");
/// assert!(f.accepts(&ev));
/// let other = Event::new("orders", "OrderPlaced", "svc", None).with_header("x-tenant", "other");
/// assert!(!f.accepts(&other));
/// ```
pub struct HeaderEventFilter {
    name: String,
    regex: Regex,
}

impl HeaderEventFilter {
    /// Compiles `pattern` and binds it to header `name`.
    ///
    /// Returns the regex compilation error if `pattern` is invalid — the
    /// Rust analog of pyfly's `re.compile` raising at construction time.
    pub fn new(name: impl Into<String>, pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            name: name.into(),
            regex: Regex::new(pattern)?,
        })
    }
}

impl EventFilter for HeaderEventFilter {
    fn accepts(&self, event: &Event) -> bool {
        let value = event
            .headers
            .get(&self.name)
            .map(String::as_str)
            .unwrap_or("");
        // `Regex::find` matches anywhere; pyfly's `re.match` is anchored
        // at the start. Reproduce the start-anchored semantics by checking
        // the match begins at offset 0.
        self.regex.find(value).is_some_and(|m| m.start() == 0)
    }
}

/// Wraps an arbitrary predicate as an [`EventFilter`] — pyfly's
/// `PredicateEventFilter(callable)`.
///
/// ```
/// use firefly_eda::{Event, EventFilter, PredicateEventFilter};
///
/// let f = PredicateEventFilter::new(|ev: &Event| ev.event_type.starts_with("Order"));
/// let ev = Event::new("orders", "OrderPlaced", "svc", None);
/// assert!(f.accepts(&ev));
/// ```
pub struct PredicateEventFilter {
    predicate: Box<dyn Fn(&Event) -> bool + Send + Sync>,
}

impl PredicateEventFilter {
    /// Wraps `predicate` as a filter.
    pub fn new(predicate: impl Fn(&Event) -> bool + Send + Sync + 'static) -> Self {
        Self {
            predicate: Box::new(predicate),
        }
    }
}

impl EventFilter for PredicateEventFilter {
    fn accepts(&self, event: &Event) -> bool {
        (self.predicate)(event)
    }
}

/// Wraps `h` so it only runs for events accepted by *every* filter in
/// `filters`; a non-matching event is dropped (the wrapped handler
/// returns `Ok(())` without invoking `h`). With an empty filter chain
/// the original handler is returned unchanged (zero overhead) — the same
/// pass-through fast path the no-op [`wrap_listener`](crate::wrap_listener)
/// takes.
///
/// This is the Rust spelling of attaching a filter chain to a
/// subscription (pyfly's `@event_listener(filters=[…])`): filters run in
/// order and short-circuit on the first rejection.
pub fn with_filters<I>(h: Handler, filters: I) -> Handler
where
    I: IntoIterator,
    I::Item: EventFilter + 'static,
{
    let filters: Vec<Arc<dyn EventFilter>> = filters
        .into_iter()
        .map(|f| Arc::new(f) as Arc<dyn EventFilter>)
        .collect();
    if filters.is_empty() {
        return h;
    }
    let filters = Arc::new(filters);
    handler(move |ev: Event| {
        let h = Arc::clone(&h);
        let filters = Arc::clone(&filters);
        async move {
            if filters.iter().all(|f| f.accepts(&ev)) {
                h(ev).await
            } else {
                Ok(())
            }
        }
    })
}

/// Like [`with_filters`] but accepts pre-boxed `Arc<dyn EventFilter>`
/// values, so a heterogeneous, dynamically built filter chain can be
/// attached to a handler. With an empty chain the handler is returned
/// unchanged.
pub fn with_filter_chain(h: Handler, filters: Vec<Arc<dyn EventFilter>>) -> Handler {
    if filters.is_empty() {
        return h;
    }
    let filters = Arc::new(filters);
    handler(move |ev: Event| {
        let h = Arc::clone(&h);
        let filters = Arc::clone(&filters);
        async move {
            if filters.iter().all(|f| f.accepts(&ev)) {
                h(ev).await
            } else {
                Ok(())
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;

    fn envelope(headers: &[(&str, &str)]) -> Event {
        let mut ev = Event::new("orders.events", "OrderPlaced", "svc", None);
        for (k, v) in headers {
            ev = ev.with_header(*k, *v);
        }
        ev
    }

    /// pyfly `test_header_filter`: matches the regex against the named
    /// header value; a missing/other value is rejected.
    #[test]
    fn header_filter_matches_regex() {
        let f = HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap();
        assert!(f.accepts(&envelope(&[("x-tenant", "acme-eu")])));
        assert!(!f.accepts(&envelope(&[("x-tenant", "other")])));
    }

    /// A missing header is treated as the empty string (pyfly's
    /// `headers.get(name, "")`), so a `.+` pattern rejects it.
    #[test]
    fn header_filter_missing_header_is_empty() {
        let f = HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap();
        assert!(!f.accepts(&envelope(&[])));
    }

    /// The pattern is anchored at the start (pyfly's `re.match`), so a
    /// mid-string match is rejected.
    #[test]
    fn header_filter_is_start_anchored() {
        let f = HeaderEventFilter::new("region", r"eu").unwrap();
        assert!(f.accepts(&envelope(&[("region", "eu-west")])));
        assert!(!f.accepts(&envelope(&[("region", "ap-eu")])));
    }

    /// An invalid regex surfaces at construction time, not at match time.
    #[test]
    fn header_filter_rejects_invalid_pattern() {
        assert!(HeaderEventFilter::new("h", r"(").is_err());
    }

    /// pyfly `test_predicate_filter`: an arbitrary predicate gates
    /// delivery on any envelope property.
    #[test]
    fn predicate_filter_evaluates_closure() {
        let f = PredicateEventFilter::new(|e: &Event| e.event_type.starts_with("Order"));
        assert!(f.accepts(&envelope(&[])));
        let f2 = PredicateEventFilter::new(|e: &Event| e.event_type == "X");
        assert!(!f2.accepts(&envelope(&[])));
    }

    /// `with_filters` runs the inner handler only for accepted events and
    /// drops the rest without invoking it.
    #[tokio::test]
    async fn with_filters_gates_delivery() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&calls);
        let inner = handler(move |_ev: Event| {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        let gated = with_filters(
            inner,
            [HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap()],
        );

        // Accepted: inner runs.
        gated(envelope(&[("x-tenant", "acme-eu")])).await.unwrap();
        // Rejected: dropped, inner not run, no error.
        gated(envelope(&[("x-tenant", "other")])).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A filter chain short-circuits on the first rejection: an event
    /// must pass *every* filter to be delivered.
    #[tokio::test]
    async fn with_filter_chain_requires_all() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&calls);
        let inner = handler(move |_ev: Event| {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        let chain: Vec<Arc<dyn EventFilter>> = vec![
            Arc::new(HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap()),
            Arc::new(PredicateEventFilter::new(|e: &Event| {
                e.event_type == "OrderPlaced"
            })),
        ];
        let gated = with_filter_chain(inner, chain);

        // Passes header filter but fails predicate (wrong type) → dropped.
        let mut wrong = envelope(&[("x-tenant", "acme-eu")]);
        wrong.event_type = "OrderCancelled".into();
        gated(wrong).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // Passes both → delivered.
        gated(envelope(&[("x-tenant", "acme-eu")])).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// An empty filter chain returns the original handler `Arc`
    /// unchanged — zero overhead, like the no-op listener wrapper.
    #[test]
    fn empty_filter_chain_passes_through() {
        let inner = handler(|_ev: Event| async { Ok(()) });
        let same = with_filters(Arc::clone(&inner), Vec::<HeaderEventFilter>::new());
        assert!(Arc::ptr_eq(&inner, &same));
        let same2 = with_filter_chain(Arc::clone(&inner), Vec::new());
        assert!(Arc::ptr_eq(&inner, &same2));
    }
}
