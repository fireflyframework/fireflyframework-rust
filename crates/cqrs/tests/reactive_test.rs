//! Coverage for the additive Reactor / WebFlux-style reactive dispatch
//! surface: [`Bus::send_mono`] / [`Bus::query_mono`] (and the
//! `*_with_context` overloads). Mirrors the async `cqrs_test.rs`
//! happy-path / error / no-handler trio, asserting the same handler
//! lookup + middleware run through a `Mono<R>` and that `CqrsError`
//! maps faithfully to `FireflyError`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use firefly_cqrs::{
    cqrs_error_to_firefly, AuthorizationResult, Bus, CqrsError, ExecutionContext, Message,
    ValidationMiddleware,
};
use serde::Serialize;

// ---- fixtures ----------------------------------------------------------

#[derive(Clone, Serialize)]
struct CreateUser {
    name: String,
}

impl Message for CreateUser {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.name.is_empty() {
            return Err(CqrsError::validation("name required"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
struct UserCreated {
    id: String,
    name: String,
}

#[derive(Clone, Serialize)]
struct GetUser {
    id: String,
}

impl Message for GetUser {
    fn cache_ttl(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(60))
    }
}

/// A command whose handler always fails with a domain error.
#[derive(Clone, Serialize)]
struct FailingCommand;

impl Message for FailingCommand {}

/// A command gated by an authorization hook that always denies.
#[derive(Clone, Serialize)]
struct DeniedCommand;

impl Message for DeniedCommand {
    fn authorize(&self, _ctx: Option<&ExecutionContext>) -> AuthorizationResult {
        AuthorizationResult::failure("command", "nope")
    }
}

fn create_bus() -> Arc<Bus> {
    let bus = Arc::new(Bus::new());
    bus.register(|c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "u1".into(),
            name: c.name,
        })
    });
    bus.register(|q: GetUser| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: q.id,
            name: "alice".into(),
        })
    });
    bus
}

// ---- happy path --------------------------------------------------------

#[tokio::test]
async fn send_mono_happy_path() {
    let bus = create_bus();
    let out = bus
        .send_mono::<_, UserCreated>(CreateUser {
            name: "alice".into(),
        })
        .block()
        .await
        .unwrap();
    assert_eq!(
        out,
        Some(UserCreated {
            id: "u1".into(),
            name: "alice".into(),
        })
    );
}

#[tokio::test]
async fn query_mono_happy_path() {
    let bus = create_bus();
    let out = bus
        .query_mono::<_, UserCreated>(GetUser { id: "42".into() })
        .block()
        .await
        .unwrap();
    assert_eq!(
        out,
        Some(UserCreated {
            id: "42".into(),
            name: "alice".into(),
        })
    );
}

#[tokio::test]
async fn send_mono_composes_with_operators() {
    // The reactive surface is usable with Reactor operators, not just
    // `block`.
    let bus = create_bus();
    let id = bus
        .send_mono::<_, UserCreated>(CreateUser { name: "bob".into() })
        .map(|u| u.id)
        .block()
        .await
        .unwrap();
    assert_eq!(id, Some("u1".to_string()));
}

#[tokio::test]
async fn query_mono_runs_caching_middleware() {
    // Same middleware chain as the async path: the cache memoises, so the
    // handler runs once across two reactive queries.
    let bus = Arc::new(Bus::new());
    let cache = firefly_cqrs::QueryCache::new();
    bus.use_middleware(cache.middleware());

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetUser| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(UserCreated {
                id: q.id,
                name: "alice".into(),
            })
        }
    });

    let first = bus
        .query_mono::<_, UserCreated>(GetUser { id: "7".into() })
        .block()
        .await
        .unwrap();
    let second = bus
        .query_mono::<_, UserCreated>(GetUser { id: "7".into() })
        .block()
        .await
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cache must short-circuit");
}

// ---- error mapping to FireflyError -------------------------------------

#[tokio::test]
async fn send_mono_maps_validation_error() {
    let bus = create_bus();
    bus.use_middleware(ValidationMiddleware::new());

    let err = bus
        .send_mono::<_, UserCreated>(CreateUser { name: "".into() })
        .block()
        .await
        .expect_err("empty name must fail validation");

    // Validation -> 422, RFC 7807 validation type.
    assert_eq!(err.status, 422);
    assert!(err.to_string().contains("name required"), "{err}");

    // The original CqrsError is preserved as the source cause.
    let cause = std::error::Error::source(&err).expect("cause chain");
    let cqrs = cause
        .downcast_ref::<CqrsError>()
        .expect("cause is a CqrsError");
    assert!(matches!(cqrs, CqrsError::Validation(_)));
}

#[tokio::test]
async fn send_mono_maps_handler_error() {
    let bus = Arc::new(Bus::new());
    bus.register(
        |_c: FailingCommand| async move { Err::<UserCreated, _>(CqrsError::handler("boom")) },
    );

    let err = bus
        .send_mono::<_, UserCreated>(FailingCommand)
        .block()
        .await
        .expect_err("handler error must propagate");

    // Handler domain failure -> 500.
    assert_eq!(err.status, 500);
    assert!(err.to_string().contains("boom"), "{err}");
}

#[tokio::test]
async fn send_mono_maps_authorization_denial() {
    let bus = Arc::new(Bus::new());
    bus.use_middleware(firefly_cqrs::AuthorizationMiddleware::new());
    bus.register(|_c: DeniedCommand| async move {
        Ok::<_, CqrsError>(UserCreated {
            id: "x".into(),
            name: "x".into(),
        })
    });

    let err = bus
        .send_mono::<_, UserCreated>(DeniedCommand)
        .block()
        .await
        .expect_err("denied command must fail authorization");

    // Authorization denial -> 403.
    assert_eq!(err.status, 403);
    let cause = std::error::Error::source(&err).expect("cause chain");
    let cqrs = cause
        .downcast_ref::<CqrsError>()
        .expect("cause is a CqrsError");
    assert!(cqrs.is_authorization());
}

// ---- no handler --------------------------------------------------------

#[tokio::test]
async fn send_mono_no_handler() {
    let bus = Arc::new(Bus::new());
    let err = bus
        .send_mono::<_, UserCreated>(CreateUser {
            name: "alice".into(),
        })
        .block()
        .await
        .expect_err("unregistered command must fail");

    // NoHandler -> 500.
    assert_eq!(err.status, 500);
    let cause = std::error::Error::source(&err).expect("cause chain");
    let cqrs = cause
        .downcast_ref::<CqrsError>()
        .expect("cause is a CqrsError");
    assert!(cqrs.is_no_handler());
}

#[tokio::test]
async fn query_mono_no_handler() {
    let bus = Arc::new(Bus::new());
    let err = bus
        .query_mono::<_, UserCreated>(GetUser { id: "1".into() })
        .block()
        .await
        .expect_err("unregistered query must fail");
    assert_eq!(err.status, 500);
}

// ---- with-context overloads --------------------------------------------

#[tokio::test]
async fn send_mono_with_context_threads_context() {
    let bus = Arc::new(Bus::new());
    bus.register_with_context(|c: CreateUser, ctx: Option<ExecutionContext>| async move {
        let user = ctx.and_then(|c| c.user_id);
        Ok::<_, CqrsError>(UserCreated {
            id: user.unwrap_or_default(),
            name: c.name,
        })
    });

    let ctx = ExecutionContext::builder().with_user_id("operator").build();
    let out = bus
        .send_mono_with_context::<_, UserCreated>(
            CreateUser {
                name: "alice".into(),
            },
            ctx,
        )
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.id, "operator");
}

#[tokio::test]
async fn query_mono_with_context_threads_context() {
    let bus = Arc::new(Bus::new());
    bus.register_with_context(|q: GetUser, ctx: Option<ExecutionContext>| async move {
        let tenant = ctx.and_then(|c| c.tenant_id);
        Ok::<_, CqrsError>(UserCreated {
            id: q.id,
            name: tenant.unwrap_or_default(),
        })
    });

    let ctx = ExecutionContext::builder().with_tenant_id("acme").build();
    let out = bus
        .query_mono_with_context::<_, UserCreated>(GetUser { id: "1".into() }, ctx)
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.name, "acme");
}

// ---- direct error-mapper coverage --------------------------------------

#[test]
fn error_mapper_status_table() {
    assert_eq!(
        cqrs_error_to_firefly(CqrsError::validation("x")).status,
        422
    );
    assert_eq!(cqrs_error_to_firefly(CqrsError::handler("x")).status, 500);
    assert_eq!(
        cqrs_error_to_firefly(CqrsError::NoHandler { type_name: "Foo" }).status,
        500
    );
    assert_eq!(
        cqrs_error_to_firefly(CqrsError::authorization(AuthorizationResult::failure(
            "resource", "denied"
        )))
        .status,
        403
    );
}
