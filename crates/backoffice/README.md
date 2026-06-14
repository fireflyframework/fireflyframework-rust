# `firefly-backoffice`

> **Tier:** Starter · **Status:** Stable

## Overview

`firefly-backoffice` composes [`firefly-starter-application`](../starter-application/)
with **back-office context middleware** that requires every request
to carry the canonical operator headers:

| Header                  | Purpose                                               |
|-------------------------|-------------------------------------------------------|
| `X-BackOffice-Branch`   | Branch / tenant identifier the operator is scoped to  |
| `X-BackOffice-Operator` | The operator's stable user id                         |

Both must be present; the middleware emits a 400
`application/problem+json` response when either is missing (or empty).
Successful requests have the values stored on the request — as a
`BackOfficeContext` extension *and* a tokio task-local scope — and
exposed via `firefly_backoffice::branch()` /
`firefly_backoffice::operator()`.

## Public surface

```rust
pub const HEADER_BRANCH: &str = "X-BackOffice-Branch";
pub const HEADER_OPERATOR: &str = "X-BackOffice-Operator";

pub fn branch() -> Option<String>;
pub fn operator() -> Option<String>;
pub async fn with_back_office<F: Future>(ctx: BackOfficeContext, fut: F) -> F::Output;
pub fn with_back_office_sync<F: FnOnce() -> R, R>(ctx: BackOfficeContext, f: F) -> R;

pub struct BackOfficeContext { pub branch: String, pub operator: String }

pub struct BackOfficeLayer;            // emits the back-office guard middleware
pub struct BackOfficeService<S>;       // the tower service it produces

pub struct BackOffice { pub app: Application }   // Deref → Application → Core
impl BackOffice {
    pub fn new(cfg: CoreConfig) -> Self;
    pub fn apply_middleware_chain(&self, router: Router) -> Router;
}
```

`apply_middleware_chain()` returns the core chain composed with the
back-office middleware as the innermost layer — apply it once and
every handler gets problem rendering, correlation, idempotency, AND
the back-office guard. The execution order is `Problem → Correlation →
Idempotency → BackOffice → router`.

`BackOffice` dereferences to `Application` (which dereferences to
`Core`) — a two-level deref chain — so
`bo.plugins`, `bo.bus`, `bo.cache`, `bo.actuator_router(..)`, and the
core-only `bo.apply_middleware(..)` are all reachable directly. The
starter name defaults to `"starter-backoffice"`; an explicitly
configured custom name is preserved.

## Quick start

```rust,no_run
use axum::{routing::get, Router};
use firefly_backoffice::{BackOffice, CoreConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bo = BackOffice::new(CoreConfig {
        app_name: "loan-bo".into(),
        ..CoreConfig::default()
    });

    let router = Router::new().route(
        "/admin/loans",
        get(|| async {
            let branch = firefly_backoffice::branch().unwrap_or_default();
            let operator = firefly_backoffice::operator().unwrap_or_default();
            tracing::info!("op {operator} @ branch {branch} listing loans");
            // … domain logic …
            "[]"
        }),
    );

    let app = bo.apply_middleware_chain(router);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

Handlers can equivalently extract the context as an axum extension:

```rust,ignore
use axum::Extension;
use firefly_backoffice::BackOfficeContext;

async fn list_loans(Extension(ctx): Extension<BackOfficeContext>) -> String {
    format!("op {} @ branch {}", ctx.operator, ctx.branch)
}
```

A request without both headers receives:

```text
400 Bad Request
Content-Type: application/problem+json

{"detail":"missing back-office headers","status":400,
 "title":"Bad Request",
 "type":"https://fireflyframework.org/problems/bad-request"}
```

## Testing

```bash
cargo test -p firefly-backoffice
```

Covers missing-headers 400 (either header, empty values, exact
problem-JSON bytes, handler never invoked), both-headers happy path
with context propagation (task-local accessors and request
extension), pre-merged middleware chain ordering (correlation id on
both 400 and 200 responses), the standalone `BackOfficeLayer`,
task-local scope nesting, and starter-name wiring.
