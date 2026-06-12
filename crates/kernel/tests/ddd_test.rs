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

//! Port of pyfly's `tests/domain/` suite — `test_specification.py`,
//! `test_entity.py`, `test_aggregate_root.py`, `test_exceptions.py` —
//! adapted to the Rust idiom: `&`/`|`/`~` become `.and()`/`.or()`/
//! `.not()`, `Specification.of(callable)` becomes the closure blanket
//! impl, the `Entity`/`AggregateRoot` base classes become the `Entity`
//! trait + `PendingEvents` buffer, and the `DomainException` hierarchy
//! becomes `FireflyError` constructors.

use firefly_kernel::ddd::{
    BoxedDomainEvent, Entity, EventMeta, PendingEvents, Specification, TransientDomainEvent,
};
use firefly_kernel::{FireflyError, FixedClock};
use serde_json::json;

// ---------------------------------------------------------------------------
// Specification — tests/domain/test_specification.py
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Customer {
    name: &'static str,
    age: u32,
    is_premium: bool,
}

impl Customer {
    fn new(name: &'static str, age: u32, is_premium: bool) -> Self {
        Self {
            name,
            age,
            is_premium,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IsAdult;

impl Specification<Customer> for IsAdult {
    fn is_satisfied_by(&self, candidate: &Customer) -> bool {
        candidate.age >= 18
    }
}

#[derive(Debug, Clone, Copy)]
struct IsPremium;

impl Specification<Customer> for IsPremium {
    fn is_satisfied_by(&self, candidate: &Customer) -> bool {
        candidate.is_premium
    }
}

#[test]
fn basic_specification_evaluates_predicate() {
    let spec = IsAdult;
    assert!(spec.is_satisfied_by(&Customer::new("Ada", 30, false)));
    assert!(!spec.is_satisfied_by(&Customer::new("Bea", 12, false)));
}

#[test]
fn specification_works_with_iterator_filter() {
    // pyfly: list(filter(spec, customers)) — the spec is callable.
    let spec = IsAdult;
    let customers = [
        Customer::new("Ada", 30, false),
        Customer::new("Bea", 12, false),
        Customer::new("Coco", 18, false),
    ];
    let adults: Vec<&'static str> = customers
        .iter()
        .filter(|c| spec.is_satisfied_by(c))
        .map(|c| c.name)
        .collect();
    assert_eq!(adults, vec!["Ada", "Coco"]);
}

#[test]
fn and_combinator() {
    let spec = IsAdult.and(IsPremium);
    assert!(spec.is_satisfied_by(&Customer::new("Ada", 30, true)));
    assert!(!spec.is_satisfied_by(&Customer::new("Ada", 30, false)));
    assert!(!spec.is_satisfied_by(&Customer::new("Bea", 12, true)));
}

#[test]
fn or_combinator() {
    let spec = IsAdult.or(IsPremium);
    assert!(spec.is_satisfied_by(&Customer::new("Ada", 30, false)));
    assert!(spec.is_satisfied_by(&Customer::new("Bea", 12, true)));
    assert!(!spec.is_satisfied_by(&Customer::new("Bea", 12, false)));
}

#[test]
fn not_combinator() {
    let spec = IsAdult.not();
    assert!(!spec.is_satisfied_by(&Customer::new("Ada", 30, false)));
    assert!(spec.is_satisfied_by(&Customer::new("Bea", 12, false)));
}

#[test]
fn combinators_compose() {
    // (adult AND premium) OR (NOT adult)
    let spec = IsAdult.and(IsPremium).or(IsAdult.not());
    assert!(spec.is_satisfied_by(&Customer::new("Ada", 30, true))); // adult premium
    assert!(spec.is_satisfied_by(&Customer::new("Bea", 12, false))); // not adult
    assert!(!spec.is_satisfied_by(&Customer::new("Coco", 30, false))); // adult, not premium
}

#[test]
fn closure_blanket_impl_replaces_specification_of() {
    // pyfly: Specification.of(lambda c: c.age == 25)
    let spec = |c: &Customer| c.age == 25;
    assert!(spec.is_satisfied_by(&Customer::new("Ada", 25, false)));
    assert!(!spec.is_satisfied_by(&Customer::new("Bea", 30, false)));
}

#[test]
fn closure_specification_composes_like_a_named_spec() {
    let is_minor = |c: &Customer| c.age < 18;
    let spec = is_minor.and(IsPremium);
    assert!(spec.is_satisfied_by(&Customer::new("Bea", 12, true)));
    assert!(!spec.is_satisfied_by(&Customer::new("Bea", 12, false)));
}

#[test]
fn combinator_structs_are_reusable_specifications() {
    // Rust-specific: AndSpec/OrSpec/NotSpec are themselves specs and
    // keep composing arbitrarily deep.
    let spec = IsAdult.and(IsPremium).not().or(|c: &Customer| c.age > 99);
    assert!(spec.is_satisfied_by(&Customer::new("Bea", 12, false)));
    assert!(!spec.is_satisfied_by(&Customer::new("Ada", 30, true)));
}

// ---------------------------------------------------------------------------
// Entity — tests/domain/test_entity.py
// ---------------------------------------------------------------------------

struct Account {
    id: Option<i64>,
    balance: i64,
}

impl Account {
    fn new(id: Option<i64>, balance: i64) -> Self {
        Self { id, balance }
    }
}

impl Entity for Account {
    type Id = i64;

    fn id(&self) -> Option<&i64> {
        self.id.as_ref()
    }
}

// pyfly inherits __eq__ from Entity; Rust consumers implement
// PartialEq via the same_identity helper.
impl PartialEq for Account {
    fn eq(&self, other: &Self) -> bool {
        self.same_identity(other)
    }
}

#[test]
fn two_entities_with_same_id_share_identity() {
    let a = Account::new(Some(1), 100);
    let b = Account::new(Some(1), 999); // different state, same identity
    assert!(a.same_identity(&b));
    assert!(a == b);
    let _ = a.balance + b.balance; // state remains accessible
}

#[test]
fn entities_with_different_ids_do_not_share_identity() {
    let a = Account::new(Some(1), 0);
    let b = Account::new(Some(2), 0);
    assert!(!a.same_identity(&b));
    assert!(a != b);
}

// pyfly's "different subclasses are never equal" check is enforced
// statically in Rust: same_identity only accepts &Self, so comparing
// an Account to another entity type does not compile.

#[test]
fn transient_entities_never_share_identity() {
    let a = Account::new(None, 0);
    let b = Account::new(None, 0);
    assert!(a.is_transient());
    assert!(b.is_transient());
    assert!(!a.same_identity(&b));
    // pyfly: transient entities still equal *themselves* by object
    // identity; in Rust identity-by-address is not equality, so even
    // self-comparison through ids is false while transient.
    assert!(!a.same_identity(&a));
}

#[test]
fn assigning_id_makes_entity_non_transient() {
    let mut a = Account::new(None, 0);
    assert!(a.is_transient());
    a.id = Some(42);
    assert!(!a.is_transient());
    assert_eq!(a.id(), Some(&42));
}

#[test]
fn entity_id_type_is_hashable() {
    // pyfly hashes entities by (type, id); the Rust contract requires
    // Id: Hash so consumers can do the same in HashSet/HashMap keys.
    use std::collections::HashSet;
    let a = Account::new(Some(1), 0);
    let b = Account::new(Some(1), 1);
    let seen: HashSet<&i64> = [a.id().unwrap(), b.id().unwrap()].into_iter().collect();
    assert_eq!(seen.len(), 1);
}

// ---------------------------------------------------------------------------
// AggregateRoot / PendingEvents — tests/domain/test_aggregate_root.py
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct OrderPlaced {
    meta: EventMeta,
    order_id: String,
    total: i64,
}

impl TransientDomainEvent for OrderPlaced {
    fn meta(&self) -> &EventMeta {
        &self.meta
    }
}

#[derive(Debug, Clone)]
struct OrderShipped {
    meta: EventMeta,
    order_id: String,
}

impl TransientDomainEvent for OrderShipped {
    fn meta(&self) -> &EventMeta {
        &self.meta
    }
}

#[derive(Debug, Clone)]
enum OrderEvent {
    Placed(OrderPlaced),
    Shipped(OrderShipped),
}

struct Order {
    id: Option<String>,
    shipped: bool,
    events: PendingEvents<OrderEvent>,
}

impl Entity for Order {
    type Id = String;

    fn id(&self) -> Option<&String> {
        self.id.as_ref()
    }
}

impl Order {
    fn new(id: &str) -> Self {
        Self {
            id: Some(id.to_owned()),
            shipped: false,
            events: PendingEvents::new(),
        }
    }

    fn place(&mut self, total: i64) {
        let order_id = self.id.clone().expect("id required");
        self.events.raise(OrderEvent::Placed(OrderPlaced {
            meta: EventMeta::new(),
            order_id,
            total,
        }));
    }

    fn ship(&mut self) {
        let order_id = self.id.clone().expect("id required");
        self.shipped = true;
        self.events.raise(OrderEvent::Shipped(OrderShipped {
            meta: EventMeta::new(),
            order_id,
        }));
    }
}

#[test]
fn new_aggregate_has_no_pending_events() {
    let o = Order::new("o-1");
    assert!(o.events.pending().is_empty());
    assert!(o.events.is_empty());
    assert_eq!(o.events.len(), 0);
}

#[test]
fn raise_event_appends_to_pending_list() {
    let mut o = Order::new("o-1");
    o.place(100);

    let events = o.events.pending();
    assert_eq!(events.len(), 1);
    match &events[0] {
        OrderEvent::Placed(e) => {
            assert_eq!(e.order_id, "o-1");
            assert_eq!(e.total, 100);
        }
        other => panic!("expected OrderPlaced, got {other:?}"),
    }
}

#[test]
fn pending_events_snapshot_does_not_grow() {
    // pyfly: pending_events() returns a copy; the owned-snapshot analog
    // is pending().to_vec().
    let mut o = Order::new("o-1");
    o.place(100);
    let snapshot = o.events.pending().to_vec();

    o.ship();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(o.events.pending().len(), 2);
}

#[test]
fn drain_returns_pending_and_clears_buffer() {
    let mut o = Order::new("o-1");
    o.place(100);
    o.ship();

    let drained = o.events.drain();

    assert_eq!(drained.len(), 2);
    assert!(matches!(drained[0], OrderEvent::Placed(_)));
    match &drained[1] {
        OrderEvent::Shipped(e) => assert_eq!(e.order_id, "o-1"),
        other => panic!("expected OrderShipped, got {other:?}"),
    }
    assert!(o.events.pending().is_empty());
    assert!(o.shipped);
}

#[test]
fn aggregate_root_inherits_entity_identity_semantics() {
    let mut a = Order::new("o-1");
    let b = Order::new("o-1");
    a.place(100);
    // Despite different pending events, they share an identity — the
    // event log is not part of identity.
    assert!(a.same_identity(&b));
}

#[test]
fn event_meta_assigns_id_and_timestamp_automatically() {
    let e1 = OrderPlaced {
        meta: EventMeta::new(),
        order_id: "o-1".into(),
        total: 10,
    };
    let e2 = OrderPlaced {
        meta: EventMeta::new(),
        order_id: "o-1".into(),
        total: 10,
    };

    assert_ne!(e1.event_id(), e2.event_id());
    assert!(e1.occurred_at() <= chrono::Utc::now());
    assert_eq!(e1.event_type(), "OrderPlaced");
}

#[test]
fn event_meta_with_clock_uses_injected_time() {
    let t = chrono::DateTime::UNIX_EPOCH;
    let meta = EventMeta::with_clock(&FixedClock(t));
    assert_eq!(meta.occurred_at, t);
}

#[test]
fn event_type_defaults_to_short_type_name() {
    let placed = OrderPlaced {
        meta: EventMeta::new(),
        order_id: "o-1".into(),
        total: 1,
    };
    let shipped = OrderShipped {
        meta: EventMeta::new(),
        order_id: "o-1".into(),
    };
    // pyfly: type(self).__name__ — the module path is trimmed.
    assert_eq!(placed.event_type(), "OrderPlaced");
    assert_eq!(shipped.event_type(), "OrderShipped");
}

#[test]
fn boxed_domain_events_support_heterogeneous_buffers() {
    // The Box<dyn> fallback mirrors pyfly's untyped list[DomainEvent].
    let mut events: PendingEvents<BoxedDomainEvent> = PendingEvents::new();
    events.raise(Box::new(OrderPlaced {
        meta: EventMeta::new(),
        order_id: "o-1".into(),
        total: 100,
    }));
    events.raise(Box::new(OrderShipped {
        meta: EventMeta::new(),
        order_id: "o-1".into(),
    }));

    let types: Vec<&'static str> = events.pending().iter().map(|e| e.event_type()).collect();
    assert_eq!(types, vec!["OrderPlaced", "OrderShipped"]);

    let drained = events.drain();
    assert_eq!(drained.len(), 2);
    assert_ne!(drained[0].event_id(), drained[1].event_id());
    assert!(events.is_empty());
}

#[test]
fn pending_events_default_is_empty() {
    let events: PendingEvents<OrderEvent> = PendingEvents::default();
    assert!(events.is_empty());
}

// ---------------------------------------------------------------------------
// Domain errors — tests/domain/test_exceptions.py
// ---------------------------------------------------------------------------

#[test]
fn business_rule_violation_carries_rule_in_fields() {
    let err = FireflyError::business_rule("orders-cannot-ship-twice", "");
    assert_eq!(err.code, "DOMAIN_RULE_VIOLATION");
    assert_eq!(err.status, 422);
    assert_eq!(
        err.fields.get("rule"),
        Some(&json!("orders-cannot-ship-twice"))
    );
    // pyfly default message: "Business rule violated: <rule>".
    assert_eq!(
        err.detail,
        "Business rule violated: orders-cannot-ship-twice"
    );
    assert!(err.to_string().contains("orders-cannot-ship-twice"));
}

#[test]
fn business_rule_violation_accepts_custom_detail() {
    let err = FireflyError::business_rule("must-be-active", "Account is closed")
        .with_field("account_id", "a-1");
    assert_eq!(err.detail, "Account is closed");
    assert_eq!(err.code, "DOMAIN_RULE_VIOLATION");
    assert_eq!(err.fields.get("rule"), Some(&json!("must-be-active")));
    assert_eq!(err.fields.get("account_id"), Some(&json!("a-1")));
}

#[test]
fn business_rule_violation_renders_as_problem() {
    // DomainException extends BusinessException, so RFC 7807 mappers
    // already know how to translate it — same here via to_problem().
    let problem = FireflyError::business_rule("must-be-active", "").to_problem();
    assert_eq!(problem.status, 422);
    assert_eq!(problem.problem_type, "DOMAIN_RULE_VIOLATION");
    assert_eq!(
        problem.extensions.get("rule"),
        Some(&json!("must-be-active"))
    );
}

#[test]
fn aggregate_not_found_carries_type_and_id() {
    let err = FireflyError::aggregate_not_found("Order", "o-1");
    assert_eq!(err.code, "DOMAIN_AGGREGATE_NOT_FOUND");
    assert_eq!(err.status, 404);
    assert_eq!(err.fields.get("aggregate_type"), Some(&json!("Order")));
    // Structured `id` carries the bare `str(id)` form (unquoted), as in pyfly.
    assert_eq!(err.fields.get("id"), Some(&json!("o-1")));
    assert!(err.to_string().contains("Order"));
    assert!(err.to_string().contains("o-1"));
    // pyfly builds the detail with `{id!r}` (Python repr), which quotes
    // string ids: AggregateNotFound("Order", "o-1") ==
    //   "Order with id='o-1' not found".
    assert_eq!(err.detail, "Order with id='o-1' not found");
}

#[test]
fn aggregate_not_found_stringifies_non_string_ids() {
    // pyfly: context["id"] = str(id); the detail uses `{id!r}`, which for an
    // int is unquoted: AggregateNotFound("Account", 42) ==
    //   "Account with id=42 not found".
    let err = FireflyError::aggregate_not_found("Account", 42);
    assert_eq!(err.fields.get("id"), Some(&json!("42")));
    assert_eq!(err.detail, "Account with id=42 not found");
}

// Regression: bug 1 — the detail must mirror pyfly's `{id!r}` (Python repr).
// String ids are wrapped in single quotes; numeric ids are left bare; the
// structured `id` field stays the unquoted `str(id)` form in every case.
#[test]
fn aggregate_not_found_detail_matches_pyfly_repr() {
    // Owned String id is quoted, like a &str id.
    let owned = FireflyError::aggregate_not_found("Wallet", String::from("wlt-7"));
    assert_eq!(owned.detail, "Wallet with id='wlt-7' not found");
    assert_eq!(owned.fields.get("id"), Some(&json!("wlt-7")));

    // A &String reference id is quoted too.
    let id = String::from("wlt-7");
    let by_ref = FireflyError::aggregate_not_found("Wallet", &id);
    assert_eq!(by_ref.detail, "Wallet with id='wlt-7' not found");

    // Signed/unsigned integers and a wide id are left bare.
    let signed = FireflyError::aggregate_not_found("Account", -3i64);
    assert_eq!(signed.detail, "Account with id=-3 not found");
    assert_eq!(signed.fields.get("id"), Some(&json!("-3")));

    let wide = FireflyError::aggregate_not_found("Account", 9_000_000_000u64);
    assert_eq!(wide.detail, "Account with id=9000000000 not found");

    // Python repr switches to a double-quote delimiter when the id contains a
    // single quote but no double quote: repr("o'1") == "\"o'1\"".
    let apostrophe = FireflyError::aggregate_not_found("Order", "o'1");
    assert_eq!(apostrophe.detail, "Order with id=\"o'1\" not found");
    assert_eq!(apostrophe.fields.get("id"), Some(&json!("o'1")));

    // With both quote kinds present, repr keeps single-quote delimiters and
    // escapes the embedded single quote: repr("a'b\"c") == "'a\\'b\"c'".
    let both = FireflyError::aggregate_not_found("Order", "a'b\"c");
    assert_eq!(both.detail, "Order with id='a\\'b\"c' not found");
    assert_eq!(both.fields.get("id"), Some(&json!("a'b\"c")));
}

// ---------------------------------------------------------------------------
// Rust-specific bounds
// ---------------------------------------------------------------------------

#[test]
fn ddd_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EventMeta>();
    assert_send_sync::<PendingEvents<BoxedDomainEvent>>();
    assert_send_sync::<BoxedDomainEvent>();
}
