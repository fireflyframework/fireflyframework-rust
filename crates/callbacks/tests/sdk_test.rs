//! SDK round-trip tests: [`CallbacksClient`] against the real admin
//! handler served on a random localhost port (the SDK rides on
//! `firefly-client`'s reqwest transport, so a real socket is needed —
//! the analog of the Go SDK hitting `httptest.NewServer(Handler(store))`).

use std::sync::Arc;

use firefly_callbacks::{handler, CallbacksClient, MemoryStore, Store, Target};

async fn spawn_admin(store: Arc<MemoryStore>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, handler(store)).await.expect("serve");
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn sdk_targets_upsert_delete_roundtrip() {
    let store = Arc::new(MemoryStore::new());
    let base = spawn_admin(store.clone()).await;
    let client = CallbacksClient::new(&base);

    // Empty to start.
    let targets = client.targets().await.expect("targets");
    assert!(targets.is_empty());

    // Upsert.
    let saved = client
        .upsert(&Target {
            id: "t1".into(),
            url: "https://example.com/cb".into(),
            secret: "never-sent".into(),
            active: true,
            event_types: vec!["order.placed".into()],
            ..Target::default()
        })
        .await
        .expect("upsert");
    assert_eq!(saved.id, "t1");
    assert_eq!(saved.event_types, vec!["order.placed".to_string()]);
    // The secret never crossed the wire (Go's `json:"-"`).
    assert!(saved.secret.is_empty());
    assert!(store.get_target("t1").await.unwrap().secret.is_empty());

    // Listed.
    let targets = client.targets().await.expect("targets");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, "t1");

    // Deleted.
    client.delete("t1").await.expect("delete");
    assert!(client.targets().await.expect("targets").is_empty());
}

#[tokio::test]
async fn sdk_delete_of_missing_target_surfaces_404() {
    let store = Arc::new(MemoryStore::new());
    let base = spawn_admin(store).await;
    let client = CallbacksClient::new(&base);

    let err = client.delete("missing").await.expect_err("404 expected");
    assert_eq!(err.status(), Some(404));
    let fe = err.as_firefly().expect("firefly error");
    // The admin handler answers Go's http.Error text body.
    assert_eq!(fe.detail, "firefly/callbacks: not found\n");
}
