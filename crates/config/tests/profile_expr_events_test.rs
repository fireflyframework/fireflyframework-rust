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

//! Port of pyfly's `tests/context/test_profile_expressions.py`
//! (`accepts_profiles` grammar) and `tests/context/test_events.py`
//! (`ApplicationEventBus`), adapted to the Rust surface: `accepts_profiles`
//! takes an explicit active-profile slice (rather than reading an
//! `Environment`), and the bus is synchronous + `TypeId`-dispatched with
//! closures instead of `isinstance` + coroutines.

use std::cell::RefCell;
use std::rc::Rc;

use firefly_config::{
    accepts_profiles, ApplicationEventBus, ApplicationEventPublisher, ApplicationReadyEvent,
    ContextClosedEvent, ContextRefreshedEvent,
};

// ---------------------------------------------------------------------------
// Profile expressions (test_profile_expressions.py)
// ---------------------------------------------------------------------------

fn active(profiles: &[&str]) -> Vec<String> {
    profiles.iter().map(|p| (*p).to_string()).collect()
}

#[test]
fn test_and() {
    let a = active(&["prod", "cloud"]);
    assert!(accepts_profiles(&a, &["prod & cloud"]));
    assert!(!accepts_profiles(&a, &["prod & staging"]));
}

#[test]
fn test_or() {
    let a = active(&["prod"]);
    assert!(accepts_profiles(&a, &["prod | qa"]));
    assert!(!accepts_profiles(&a, &["dev | qa"]));
}

#[test]
fn test_not() {
    let a = active(&["prod"]);
    assert!(accepts_profiles(&a, &["!test"]));
    assert!(!accepts_profiles(&a, &["!prod"]));
    assert!(accepts_profiles(&a, &["prod & !test"]));
}

#[test]
fn test_grouping() {
    let a = active(&["cloud", "qa"]);
    assert!(accepts_profiles(&a, &["(prod & cloud) | qa"])); // qa branch
    assert!(!accepts_profiles(&a, &["(prod & cloud) & qa"])); // prod not active
    assert!(accepts_profiles(&a, &["!(prod | dev)"])); // neither active
}

#[test]
fn test_legacy_comma_and_simple_still_work() {
    let a = active(&["dev"]);
    assert!(accepts_profiles(&a, &["dev,test"])); // legacy comma-OR
    assert!(accepts_profiles(&a, &["dev"]));
    assert!(!accepts_profiles(&a, &["test"]));
}

#[test]
fn test_any_of_multiple_expressions() {
    let a = active(&["qa"]);
    assert!(accepts_profiles(&a, &["prod", "qa"]));
    assert!(!accepts_profiles(&a, &["prod", "dev"]));
}

// ---------------------------------------------------------------------------
// ApplicationEventBus (test_events.py)
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_events_are_distinct_types() {
    // Rust analog of pyfly's `isinstance(event, ApplicationEvent)`:
    // each lifecycle event is its own zero-sized type. (Construction is
    // the meaningful assertion — they implement ApplicationEvent.)
    fn assert_event<E: firefly_config::ApplicationEvent>(_e: E) {}
    assert_event(ContextRefreshedEvent);
    assert_event(ApplicationReadyEvent);
    assert_event(ContextClosedEvent);
}

#[test]
fn publish_calls_listeners() {
    let bus = ApplicationEventBus::new();
    let received: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let r = received.clone();
    bus.subscribe::<ApplicationReadyEvent, _>(move |_| *r.borrow_mut() += 1);
    bus.publish(&ApplicationReadyEvent);
    assert_eq!(*received.borrow(), 1);
}

#[test]
fn publish_only_matching_type() {
    let bus = ApplicationEventBus::new();
    let received: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let r = received.clone();
    bus.subscribe::<ApplicationReadyEvent, _>(move |_| *r.borrow_mut() += 1);
    bus.publish(&ContextClosedEvent);
    assert_eq!(*received.borrow(), 0);
}

#[test]
fn multiple_listeners() {
    let bus = ApplicationEventBus::new();
    let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let l1 = log.clone();
    bus.subscribe::<ApplicationReadyEvent, _>(move |_| l1.borrow_mut().push("first"));
    let l2 = log.clone();
    bus.subscribe::<ApplicationReadyEvent, _>(move |_| l2.borrow_mut().push("second"));
    bus.publish(&ApplicationReadyEvent);
    assert_eq!(*log.borrow(), vec!["first", "second"]);
}

#[test]
fn listeners_called_in_order() {
    let bus = ApplicationEventBus::new();
    let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let l2 = log.clone();
    bus.subscribe_ordered::<ContextRefreshedEvent, _>(2, move |_| l2.borrow_mut().push("second"));
    let l1 = log.clone();
    bus.subscribe_ordered::<ContextRefreshedEvent, _>(1, move |_| l1.borrow_mut().push("first"));
    bus.publish(&ContextRefreshedEvent);
    assert_eq!(*log.borrow(), vec!["first", "second"]);
}

#[test]
fn unordered_listeners_default_zero() {
    let bus = ApplicationEventBus::new();
    let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let early = log.clone();
    bus.subscribe_ordered::<ContextRefreshedEvent, _>(-1, move |_| {
        early.borrow_mut().push("early")
    });
    let default = log.clone();
    // Default order is 0, so it runs after the -1 listener.
    bus.subscribe::<ContextRefreshedEvent, _>(move |_| default.borrow_mut().push("default"));
    bus.publish(&ContextRefreshedEvent);
    assert_eq!(*log.borrow(), vec!["early", "default"]);
}

#[test]
fn publisher_fans_into_shared_bus() {
    let bus = Rc::new(ApplicationEventBus::new());
    let received: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let r = received.clone();
    bus.subscribe::<ApplicationReadyEvent, _>(move |_| *r.borrow_mut() += 1);

    let publisher = ApplicationEventPublisher::new(bus.clone());
    publisher.publish(&ApplicationReadyEvent);
    publisher.publish(&ApplicationReadyEvent);
    assert_eq!(*received.borrow(), 2);
}

#[test]
fn arbitrary_domain_event_dispatch() {
    // Any 'static type can be published — not only lifecycle events.
    #[derive(Clone)]
    struct UserCreated {
        user_id: String,
    }
    let bus = ApplicationEventBus::new();
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let s = seen.clone();
    bus.subscribe::<UserCreated, _>(move |e| s.borrow_mut().push(e.user_id.clone()));
    bus.publish(&UserCreated {
        user_id: "u-1".into(),
    });
    assert_eq!(*seen.borrow(), vec!["u-1".to_string()]);
    assert_eq!(bus.listener_count::<UserCreated>(), 1);
    assert_eq!(bus.listener_count::<ApplicationReadyEvent>(), 0);
}
