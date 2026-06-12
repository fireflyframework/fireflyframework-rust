//! Port of the kernel-scoped slice of pyfly's context/correlation
//! surface: the `X-Request-Id` / `X-Tenant-Id` task-local scopes that
//! mirror the existing correlation-id scope (pyfly
//! `pyfly.observability.correlation` context vars + the
//! `RequestContext` request-id contract from
//! `tests/context/test_request_context.py`).

use firefly_kernel::{
    correlation_id, new_request_id, request_id, tenant_id, with_correlation_id, with_request_id,
    with_request_id_sync, with_tenant_id, with_tenant_id_sync, HEADER_REQUEST_ID, HEADER_TENANT_ID,
};

// ---------------------------------------------------------------------------
// Request id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_id_none_outside_scope() {
    // pyfly: RequestContext.current() is None outside a request.
    assert!(request_id().is_none());
}

#[tokio::test]
async fn with_request_id_scopes_and_retrieves() {
    let rid = new_request_id();
    let got = with_request_id(rid.clone(), async { request_id() }).await;
    assert_eq!(got, Some(rid));
    assert!(request_id().is_none(), "scope must not leak");
}

#[tokio::test]
async fn empty_request_id_yields_none() {
    let got = with_request_id("", async { request_id() }).await;
    assert!(got.is_none());
}

#[tokio::test]
async fn request_id_scopes_nest_like_child_contexts() {
    let got = with_request_id("outer", async {
        let inner = with_request_id("inner", async { request_id() }).await;
        (inner, request_id())
    })
    .await;
    assert_eq!(got, (Some("inner".to_owned()), Some("outer".to_owned())));
}

#[test]
fn request_id_sync_scope() {
    assert!(request_id().is_none());
    let got = with_request_id_sync("req-123", request_id);
    assert_eq!(got, Some("req-123".to_owned()));
}

#[test]
fn new_request_id_is_32_char_hex() {
    // pyfly generates uuid4().hex when the header is absent.
    let id = new_request_id();
    assert_eq!(id.len(), 32);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(new_request_id(), id);
}

// ---------------------------------------------------------------------------
// Tenant id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tenant_id_none_outside_scope() {
    // pyfly: absent X-Tenant-Id means "unscoped"; never generated.
    assert!(tenant_id().is_none());
}

#[tokio::test]
async fn with_tenant_id_scopes_and_retrieves() {
    let got = with_tenant_id("acme", async { tenant_id() }).await;
    assert_eq!(got, Some("acme".to_owned()));
    assert!(tenant_id().is_none(), "scope must not leak");
}

#[tokio::test]
async fn empty_tenant_id_yields_none() {
    let got = with_tenant_id("", async { tenant_id() }).await;
    assert!(got.is_none());
}

#[tokio::test]
async fn tenant_id_scopes_nest_like_child_contexts() {
    let got = with_tenant_id("outer-tenant", async {
        let inner = with_tenant_id("inner-tenant", async { tenant_id() }).await;
        (inner, tenant_id())
    })
    .await;
    assert_eq!(
        got,
        (
            Some("inner-tenant".to_owned()),
            Some("outer-tenant".to_owned())
        )
    );
}

#[test]
fn tenant_id_sync_scope() {
    assert!(tenant_id().is_none());
    let got = with_tenant_id_sync("tenant-1", tenant_id);
    assert_eq!(got, Some("tenant-1".to_owned()));
}

// ---------------------------------------------------------------------------
// Cross-cutting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_three_scopes_are_independent() {
    // pyfly binds five separate context vars; correlation, request and
    // tenant ids must not bleed into each other.
    let got = with_correlation_id("cid", async {
        with_request_id("rid", async {
            with_tenant_id("tid", async {
                (correlation_id(), request_id(), tenant_id())
            })
            .await
        })
        .await
    })
    .await;
    assert_eq!(
        got,
        (
            Some("cid".to_owned()),
            Some("rid".to_owned()),
            Some("tid".to_owned())
        )
    );

    // A tenant scope alone leaves the other two unset.
    let got = with_tenant_id("only-tenant", async { (correlation_id(), request_id()) }).await;
    assert_eq!(got, (None, None));
}

#[tokio::test]
async fn scopes_are_isolated_between_tasks() {
    // pyfly: contextvars copy-on-task-create — each asyncio task gets
    // its own context. tokio task-locals scope per future.
    let a = tokio::spawn(with_tenant_id("tenant-a", async {
        tokio::task::yield_now().await;
        tenant_id()
    }));
    let b = tokio::spawn(with_tenant_id("tenant-b", async {
        tokio::task::yield_now().await;
        tenant_id()
    }));
    assert_eq!(a.await.unwrap(), Some("tenant-a".to_owned()));
    assert_eq!(b.await.unwrap(), Some("tenant-b".to_owned()));
}

#[test]
fn header_names_match_pyfly() {
    assert_eq!(HEADER_REQUEST_ID, "X-Request-Id");
    assert_eq!(HEADER_TENANT_ID, "X-Tenant-Id");
}
