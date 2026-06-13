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

//! End-to-end behavioral tests: each macro is exercised against the real
//! `firefly` facade (a dev-dependency), proving the generated code compiles
//! *and* drives the framework runtime — not just that it expands.

use std::sync::Arc;

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

// ===========================================================================
// #[derive(Command)] / #[derive(Query)] + #[command_handler]/#[query_handler]
// ===========================================================================

#[derive(Clone, Serialize, Command)]
struct CreateUser {
    #[firefly(validate)]
    name: String,
}

#[derive(Clone, Debug, PartialEq)]
struct UserCreated {
    id: String,
    name: String,
}

#[derive(Clone, Serialize, Query)]
#[firefly(cache_ttl = "30s")]
struct GetUser {
    id: String,
}

#[command_handler]
async fn handle_create_user(cmd: CreateUser) -> Result<UserCreated, CqrsError> {
    Ok(UserCreated {
        id: format!("user-{}", cmd.name),
        name: cmd.name,
    })
}

#[query_handler]
async fn handle_get_user(q: GetUser) -> Result<UserCreated, CqrsError> {
    Ok(UserCreated {
        id: q.id.clone(),
        name: "looked-up".into(),
    })
}

#[tokio::test]
async fn command_derive_and_handler_round_trip() {
    let bus = Bus::new();
    register_handle_create_user(&bus);
    register_handle_get_user(&bus);

    // The generated handler is reachable through the bus.
    let created: UserCreated = bus
        .send(CreateUser { name: "ada".into() })
        .await
        .expect("handler dispatch");
    assert_eq!(
        created,
        UserCreated {
            id: "user-ada".into(),
            name: "ada".into()
        }
    );

    let looked_up: UserCreated = bus
        .query(GetUser { id: "u1".into() })
        .await
        .expect("query dispatch");
    assert_eq!(looked_up.id, "u1");
}

#[test]
fn command_derive_validate_rejects_empty_field() {
    // `#[firefly(validate)]` makes the empty-string case fail validation.
    let msg = CreateUser {
        name: String::new(),
    };
    assert!(msg.validate().is_err(), "empty name must fail validation");
    let ok = CreateUser { name: "x".into() };
    assert!(ok.validate().is_ok());
}

#[test]
fn query_derive_sets_cache_ttl() {
    // `#[firefly(cache_ttl = "30s")]` is reflected on the Message impl.
    let q = GetUser { id: "u1".into() };
    assert_eq!(q.cache_ttl(), Some(std::time::Duration::from_secs(30)));
}

// ===========================================================================
// #[derive(Component)] / #[derive(Service)] / #[derive(Repository)] + register_all!
// ===========================================================================

#[derive(Default)]
struct UserRepository {
    seed: u32,
}

impl UserRepository {
    fn count(&self) -> u32 {
        self.seed + 1
    }
}

// A repository registered with no dependencies.
#[derive(Repository, Default)]
struct UserRepo {
    inner: UserRepository,
}

impl UserRepo {
    fn count(&self) -> u32 {
        self.inner.count()
    }
}

// A service that autowires the repository as `Arc<UserRepo>`.
#[derive(Service)]
#[firefly(scope = "singleton")]
struct UserService {
    #[autowired]
    repo: Arc<UserRepo>,
}

impl UserService {
    fn report(&self) -> u32 {
        self.repo.count() * 10
    }
}

#[test]
fn component_derive_resolves_with_autowiring() {
    let container = Container::new();
    // The generated register_all! calls each type's firefly_register in order.
    firefly::register_all!(&container, [UserRepo, UserService]);

    let svc: Arc<UserService> = container.resolve::<UserService>().expect("resolve service");
    // The autowired repo was injected, so the service can call through it.
    assert_eq!(svc.report(), 10);

    let repo: Arc<UserRepo> = container.resolve::<UserRepo>().expect("resolve repo");
    assert_eq!(repo.count(), 1);
}

// ===========================================================================
// #[derive(DomainEvent)] / #[derive(AggregateRoot)]
// ===========================================================================

#[derive(Clone, Serialize, Deserialize, DomainEvent)]
struct AccountOpened {
    owner: String,
}

#[derive(Default, AggregateRoot)]
#[firefly(aggregate_type = "Account")]
struct Account {
    root: firefly::eventsourcing::AggregateRoot,
}

#[test]
fn domain_event_derive_converts_to_wire_event() {
    let ev = AccountOpened {
        owner: "ada".into(),
    };
    assert_eq!(AccountOpened::EVENT_TYPE, "AccountOpened");
    assert_eq!(ev.event_type(), "AccountOpened");

    let wire = ev.to_domain_event("acc-1", "Account", 1);
    assert_eq!(wire.aggregate_id, "acc-1");
    assert_eq!(wire.aggregate_type, "Account");
    assert_eq!(wire.event_type, "AccountOpened");
    assert_eq!(wire.version, 1);

    // The payload round-trips back through serde.
    let decoded: AccountOpened = serde_json::from_slice(&wire.payload).unwrap();
    assert_eq!(decoded.owner, "ada");
}

#[test]
fn aggregate_root_derive_exposes_embedded_root() {
    let mut acc = Account::default();
    assert_eq!(Account::AGGREGATE_TYPE, "Account");
    assert_eq!(acc.aggregate().version, 0);

    acc.aggregate_mut().raise("AccountOpened", b"{}".to_vec());
    assert_eq!(acc.aggregate().version, 1);
    assert_eq!(acc.aggregate().uncommitted().len(), 1);
}
