//! A minimal **multi-crate** firefly service: the `@Controller` and its
//! `@Component` collaborator live in the separate `linkspike-core` crate; this
//! `-web` binary force-links them with [`firefly::link!`] so the framework's
//! link-time discovery (`container.scan()` + `#[rest_controller]` auto-mount)
//! sees them, and guards the wiring with [`firefly::assert_discovered`].
//!
//! This is the smallest faithful demonstration of the link-time wiring every
//! layered service (`-interfaces` / `-models` / `-core` / `-web`) needs.

// LINK-TIME WIRING — DO NOT REMOVE. Force-links the layer crate's inventory so
// its bean + controller are not dead-stripped out of discovery.
firefly::link!(linkspike_core);

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("linkspike").run().await
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// The cross-crate `#[rest_controller]` is discovered, mounts, and resolves
    /// its cross-crate `#[autowired]` bean — proving `firefly::link!` makes a
    /// layer crate's inventory visible and that `ControllerMount`'s container
    /// resolution works across crate boundaries.
    #[tokio::test]
    async fn cross_crate_controller_mounts_and_resolves_its_bean() {
        let app = firefly::FireflyApplication::new("linkspike")
            .bootstrap()
            .await
            .expect("bootstrap");

        // Guard: the force-linked core crate contributed its bean + controller.
        firefly::assert_discovered(&app.container, 8, 1);

        let res = app
            .api_router
            .oneshot(Request::get("/spike/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"hello-from-core-crate");
    }
}
