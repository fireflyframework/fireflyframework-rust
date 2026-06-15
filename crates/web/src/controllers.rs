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

//! Compile-time controller auto-mounting — the Rust analog of Spring MVC's
//! `RequestMappingHandlerMapping` wiring every `@RestController` into the
//! dispatcher.
//!
//! Where [`RouteDescriptor`](firefly_container::RouteDescriptor) carries only
//! route *metadata* (for OpenAPI + `/mappings`), a [`ControllerMount`] carries a
//! thunk that actually *builds* the controller's `axum::Router` by resolving its
//! state bean from the DI [`Container`] and calling the macro-generated
//! `routes(state)`. The `#[rest_controller]` macro submits one
//! `ControllerMount` per controller via [`inventory`]; [`mount_controllers`]
//! collects them across the whole crate graph and merges them into a single
//! router — so a service never hand-mounts a controller.

use axum::Router;
use firefly_container::Container;

/// A link-time controller-mount thunk, `inventory::submit!`-ted once per
/// `#[rest_controller]` impl.
///
/// [`mount`](Self::mount) resolves the controller's state bean from the
/// container (constructed by the DI graph / autowiring) and returns the
/// controller's `axum::Router`. The state type must be a registered, `Clone`
/// bean — auto-mounting fails fast (panics with a clear message) otherwise,
/// mirroring Spring's startup failure when a `@RestController`'s dependencies
/// cannot be satisfied.
pub struct ControllerMount {
    /// The controller type name (`WalletApi`), for diagnostics + ordering.
    pub controller: &'static str,
    /// Builds the controller's router by resolving its state from the container.
    pub mount: fn(&Container) -> Router,
}

inventory::collect!(ControllerMount);

/// Builds one `axum::Router` from every `#[rest_controller]` discovered across
/// the crate graph, each mounted against the supplied [`Container`] — the
/// turnkey replacement for hand-calling `Type::routes(state).merge(...)` at a
/// composition root.
///
/// Controllers are merged in a stable order (by controller type name) so the
/// resulting route table is deterministic across builds.
pub fn mount_controllers(container: &Container) -> Router {
    let mut mounts: Vec<&'static ControllerMount> =
        inventory::iter::<ControllerMount>.into_iter().collect();
    mounts.sort_by_key(|m| m.controller);
    let mut router = Router::new();
    for entry in mounts {
        router = router.merge((entry.mount)(container));
    }
    router
}

/// The number of controllers discovered for auto-mounting — useful for the
/// startup report and tests.
#[must_use]
pub fn controller_count() -> usize {
    inventory::iter::<ControllerMount>.into_iter().count()
}

/// A DI bean that contributes extra (non-`#[rest_controller]`) routes to the
/// application — the Rust analog of a Spring `WebMvcConfigurer`/`RouterFunction`.
///
/// Implement it on a `#[derive(Service)]` bean (autowire whatever state the
/// routes need) and mark it `#[firefly(provides = "dyn RouteContributor")]`;
/// [`mount_route_contributors`] (and `FireflyApplication`) then merges its
/// routes alongside the auto-mounted controllers — so a feature-gated endpoint
/// (e.g. a reactive stream) is wired by declaring a bean, never by a composition
/// root.
pub trait RouteContributor: Send + Sync {
    /// The routes this bean contributes.
    fn routes(&self) -> Router;
}

/// Merges the routes of every [`RouteContributor`] bean registered in the
/// container (resolved as the `dyn RouteContributor` port). An app with none
/// yields an empty router.
pub fn mount_route_contributors(container: &Container) -> Router {
    let mut router = Router::new();
    for contributor in container
        .resolve_all::<dyn RouteContributor>()
        .unwrap_or_default()
    {
        router = router.merge(contributor.routes());
    }
    router
}
