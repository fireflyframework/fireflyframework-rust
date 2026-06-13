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

//! In-process, synchronous, type-dispatched application event bus — the
//! Rust port of pyfly's `context/events.py` (`ApplicationEventBus`,
//! `ApplicationEventPublisher`, the lifecycle `ApplicationEvent`
//! subclasses, and `@app_event_listener`).
//!
//! This is **distinct** from the asynchronous `firefly-eda` broker:
//! [`ApplicationEventBus`] is a synchronous, in-process pub/sub for
//! framework lifecycle and arbitrary in-VM domain events — listeners run
//! on the publishing thread, in `@order` order, with no transport, no
//! topics, and no fan-out across processes. Reach for it when you want
//! decoupled handlers *inside one process* (Spring's
//! `ApplicationEventPublisher` model); reach for `firefly-eda` when you
//! want a message broker.
//!
//! # Type dispatch vs pyfly `isinstance`
//!
//! pyfly dispatches with `isinstance`, so a listener registered for a
//! base `ApplicationEvent` also receives subclass events. Rust has no
//! runtime subclass relationship, so dispatch is keyed on the concrete
//! [`TypeId`] of the published value: a listener registered with
//! [`subscribe::<E>`](ApplicationEventBus::subscribe) receives exactly
//! the events published with that same `E`. The lifecycle events are
//! distinct zero-sized types, each subscribed to by its own type.
//!
//! # Ordering
//!
//! Each listener carries an `order: i32` (Spring `@Order` / pyfly's
//! `@order`). Within a single event type, listeners run in ascending
//! order; ties preserve subscription order (a stable sort). The default
//! order is `0`.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use firefly_config::{ApplicationEventBus, ApplicationReadyEvent};
//!
//! let bus = ApplicationEventBus::new();
//! let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
//!
//! let l = log.clone();
//! bus.subscribe::<ApplicationReadyEvent, _>(move |_e| l.borrow_mut().push("ready"));
//! bus.publish(&ApplicationReadyEvent);
//! assert_eq!(*log.borrow(), vec!["ready"]);
//! ```

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;

/// Marker for the lifecycle events. Implemented by
/// [`ContextRefreshedEvent`], [`ApplicationReadyEvent`] and
/// [`ContextClosedEvent`]; the Rust analog of pyfly's
/// `ApplicationEvent` base class. Arbitrary domain event types need
/// **not** implement it — any `'static` type can be published.
pub trait ApplicationEvent: Any {}

/// Published when the application context is fully initialized
/// (Spring `ContextRefreshedEvent` parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextRefreshedEvent;

/// Published when the application is ready to serve requests
/// (Spring `ApplicationReadyEvent` parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ApplicationReadyEvent;

/// Published when the application context is shutting down
/// (Spring `ContextClosedEvent` parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextClosedEvent;

/// Published after a Spring-Cloud-style runtime refresh
/// (`POST /actuator/refresh`) evicts refresh-scoped beans / re-reads the
/// configuration sources — the Rust port of pyfly's
/// `RefreshScopeRefreshedEvent` (Spring Cloud's `RefreshScopeRefreshedEvent`).
///
/// `refreshed` carries the keys that changed as a result of the refresh —
/// in pyfly the cache keys of the evicted refresh-scoped beans, and in the
/// Rust port the sorted top-level configuration keys
/// [`ReloadableConfig::reload`](crate::ReloadableConfig::reload) reports as
/// changed (added, removed, or modified). Listeners subscribe to it via
/// [`ApplicationEventBus::subscribe`] to react to a live configuration
/// change without polling.
///
/// ```
/// use std::cell::RefCell;
/// use std::rc::Rc;
/// use firefly_config::{ApplicationEventBus, RefreshScopeRefreshedEvent};
///
/// let bus = ApplicationEventBus::new();
/// let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
/// let s = seen.clone();
/// bus.subscribe::<RefreshScopeRefreshedEvent, _>(move |e| {
///     s.borrow_mut().extend(e.refreshed.iter().cloned());
/// });
/// bus.publish(&RefreshScopeRefreshedEvent::new(vec!["web".to_string()]));
/// assert_eq!(*seen.borrow(), vec!["web".to_string()]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RefreshScopeRefreshedEvent {
    /// The keys that changed as a result of the refresh (pyfly's evicted
    /// refresh-scoped bean cache keys; here the changed top-level
    /// configuration keys reported by the reload).
    pub refreshed: Vec<String>,
}

impl RefreshScopeRefreshedEvent {
    /// Builds the event carrying `refreshed` — the changed/evicted keys.
    #[must_use]
    pub fn new(refreshed: Vec<String>) -> Self {
        Self { refreshed }
    }
}

impl ApplicationEvent for ContextRefreshedEvent {}
impl ApplicationEvent for ApplicationReadyEvent {}
impl ApplicationEvent for ContextClosedEvent {}
impl ApplicationEvent for RefreshScopeRefreshedEvent {}

/// An erased dispatcher: downcasts the `&dyn Any` event to the
/// listener's concrete type and invokes the typed closure.
type Dispatch = Box<dyn Fn(&dyn Any)>;

/// A registered listener: its `@order` plus an erased [`Dispatch`].
struct Registration {
    order: i32,
    // The sequence number preserves registration order on order ties
    // (a stable sort within one TypeId).
    seq: u64,
    dispatch: Dispatch,
}

/// A synchronous, in-process, type-dispatched application event bus.
///
/// Listeners are invoked on the publishing thread, in `@order` order,
/// keyed on the concrete event [`TypeId`]. Not `Send`/`Sync` — it holds
/// non-thread-safe interior state (`RefCell`) because the canonical use
/// is single-threaded lifecycle wiring; share across threads by wrapping
/// the events themselves, not the bus.
#[derive(Default)]
pub struct ApplicationEventBus {
    listeners: RefCell<HashMap<TypeId, Vec<Registration>>>,
    seq: RefCell<u64>,
}

impl ApplicationEventBus {
    /// Creates an empty event bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes `listener` to events of type `E`, with the default
    /// order (`0`). Returns nothing — registration is fire-and-forget,
    /// matching pyfly's `subscribe`.
    pub fn subscribe<E, F>(&self, listener: F)
    where
        E: 'static,
        F: Fn(&E) + 'static,
    {
        self.subscribe_ordered::<E, F>(0, listener);
    }

    /// Subscribes `listener` to events of type `E` with an explicit
    /// `@order`. Lower orders run first; ties keep subscription order.
    pub fn subscribe_ordered<E, F>(&self, order: i32, listener: F)
    where
        E: 'static,
        F: Fn(&E) + 'static,
    {
        let seq = {
            let mut s = self.seq.borrow_mut();
            *s += 1;
            *s
        };
        let dispatch = Box::new(move |event: &dyn Any| {
            if let Some(typed) = event.downcast_ref::<E>() {
                listener(typed);
            }
        });
        let mut listeners = self.listeners.borrow_mut();
        let entries = listeners.entry(TypeId::of::<E>()).or_default();
        entries.push(Registration {
            order,
            seq,
            dispatch,
        });
        // Pre-sort so publish() does not sort per invocation. Stable on
        // (order, seq) — equal orders preserve subscription order.
        entries.sort_by(|a, b| a.order.cmp(&b.order).then(a.seq.cmp(&b.seq)));
    }

    /// Publishes `event` to every listener registered for its concrete
    /// type `E`, in `@order` order. A no-op when there are no listeners.
    pub fn publish<E>(&self, event: &E)
    where
        E: 'static,
    {
        // Snapshot is unnecessary — dispatch closures don't mutate the
        // listener map; borrow it for the duration of the fan-out.
        let listeners = self.listeners.borrow();
        if let Some(entries) = listeners.get(&TypeId::of::<E>()) {
            let any_event: &dyn Any = event;
            for reg in entries {
                (reg.dispatch)(any_event);
            }
        }
    }

    /// Returns the number of listeners registered for event type `E`.
    #[must_use]
    pub fn listener_count<E: 'static>(&self) -> usize {
        self.listeners
            .borrow()
            .get(&TypeId::of::<E>())
            .map_or(0, Vec::len)
    }
}

/// An injectable publisher that fires events into an
/// [`ApplicationEventBus`] — the Rust analog of pyfly's
/// `ApplicationEventPublisher` (Spring's `ApplicationEventPublisher`).
///
/// Hand one to any component that should publish without depending on
/// the bus directly. It holds a shared reference to the bus
/// ([`Rc`](std::rc::Rc)) so multiple publishers fan into the same
/// listener set.
pub struct ApplicationEventPublisher {
    bus: std::rc::Rc<ApplicationEventBus>,
}

impl ApplicationEventPublisher {
    /// Wraps a shared [`ApplicationEventBus`].
    #[must_use]
    pub fn new(bus: std::rc::Rc<ApplicationEventBus>) -> Self {
        Self { bus }
    }

    /// Publishes `event` to the underlying bus.
    pub fn publish<E: 'static>(&self, event: &E) {
        self.bus.publish(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn publish_calls_listeners() {
        let bus = ApplicationEventBus::new();
        let received = Rc::new(RefCell::new(0));
        let r = received.clone();
        bus.subscribe::<ApplicationReadyEvent, _>(move |_| *r.borrow_mut() += 1);
        bus.publish(&ApplicationReadyEvent);
        assert_eq!(*received.borrow(), 1);
    }

    #[test]
    fn publish_only_matching_type() {
        let bus = ApplicationEventBus::new();
        let received = Rc::new(RefCell::new(0));
        let r = received.clone();
        bus.subscribe::<ApplicationReadyEvent, _>(move |_| *r.borrow_mut() += 1);
        bus.publish(&ContextClosedEvent);
        assert_eq!(*received.borrow(), 0);
    }

    #[test]
    fn multiple_listeners_in_subscription_order() {
        let bus = ApplicationEventBus::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let l1 = log.clone();
        bus.subscribe::<ApplicationReadyEvent, _>(move |_| l1.borrow_mut().push("first"));
        let l2 = log.clone();
        bus.subscribe::<ApplicationReadyEvent, _>(move |_| l2.borrow_mut().push("second"));
        bus.publish(&ApplicationReadyEvent);
        assert_eq!(*log.borrow(), vec!["first", "second"]);
    }

    #[test]
    fn refresh_scope_refreshed_event_carries_keys() {
        let bus = ApplicationEventBus::new();
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        bus.subscribe::<RefreshScopeRefreshedEvent, _>(move |e| {
            *s.borrow_mut() = e.refreshed.clone();
        });
        bus.publish(&RefreshScopeRefreshedEvent::new(vec![
            "cache".to_string(),
            "web".to_string(),
        ]));
        assert_eq!(*seen.borrow(), vec!["cache".to_string(), "web".to_string()]);
    }

    #[test]
    fn ordered_listeners() {
        let bus = ApplicationEventBus::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        let l2 = log.clone();
        bus.subscribe_ordered::<ContextRefreshedEvent, _>(2, move |_| {
            l2.borrow_mut().push("second");
        });
        let l1 = log.clone();
        bus.subscribe_ordered::<ContextRefreshedEvent, _>(1, move |_| {
            l1.borrow_mut().push("first");
        });
        bus.publish(&ContextRefreshedEvent);
        assert_eq!(*log.borrow(), vec!["first", "second"]);
    }
}
