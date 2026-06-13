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

//! Component scanning — the Rust analog of pyfly's `scan_package` /
//! `scan_module_classes` / `_auto_bind_interfaces`.
//!
//! Rust has no runtime package introspection, so discovery is link-time: every
//! stereotype derive in `firefly-macros` emits an [`inventory::submit!`] of a
//! [`ComponentRegistration`] thunk. [`Container::scan`](crate::Container::scan)
//! collects every submitted thunk across the whole crate graph via
//! [`inventory::iter`] and registers them — honoring conditionals and profiles
//! exactly like pyfly's `ApplicationContext._evaluate_conditions` /
//! `_filter_by_profile`.
//!
//! Generic types cannot be inventoried (a monomorphization is chosen at the use
//! site, not at definition), so they are registered with the explicit
//! `register_all!(container, [Foo::<T>, ...])` fallback. This is documented on
//! [`Container::scan`](crate::Container::scan).

use crate::condition::Condition;
use crate::scope::Scope;
use crate::Container;

/// A stereotype label, mirroring pyfly's `_make_stereotype` names.
///
/// Carried in each [`ComponentRegistration`] so the admin dashboard's
/// `/beans` view (and [`Container::beans`](crate::Container::beans)) can group
/// beans by layer just like pyfly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stereotype {
    /// A generic managed bean (`#[derive(Component)]`).
    Component,
    /// A business-logic bean (`#[derive(Service)]`).
    Service,
    /// A data-access bean (`#[derive(Repository)]`).
    Repository,
    /// A bean-factory holder (`#[derive(Configuration)]`).
    Configuration,
    /// A web controller bean (`#[derive(Controller)]`).
    Controller,
    /// A `@ConfigurationProperties` bean (`#[derive(ConfigProperties)]`).
    ConfigProperties,
    /// A `@Bean` factory-method product on a configuration holder.
    Bean,
}

impl Stereotype {
    /// The lower-case label pyfly uses (`component`, `service`, …).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Stereotype::Component => "component",
            Stereotype::Service => "service",
            Stereotype::Repository => "repository",
            Stereotype::Configuration => "configuration",
            Stereotype::Controller => "controller",
            Stereotype::ConfigProperties => "config_properties",
            Stereotype::Bean => "bean",
        }
    }
}

/// A link-time component-scan thunk.
///
/// One is `inventory::submit!`-ted per stereotype-derived type. [`register`]
/// performs the actual `Container::register_*` call (and any interface
/// auto-binding); the metadata fields let [`Container::scan`](crate::Container::scan)
/// apply conditionals/profiles *before* calling it.
///
/// [`register`]: ComponentRegistration::register
pub struct ComponentRegistration {
    /// The short type name (e.g. `OrderService`), for diagnostics + `/beans`.
    pub type_name: &'static str,
    /// The explicit bean name, or `""` when anonymous.
    pub bean_name: &'static str,
    /// This bean's stereotype.
    pub stereotype: Stereotype,
    /// The bean's lifecycle scope.
    pub scope: Scope,
    /// Whether the bean is the primary candidate among its interface peers.
    pub primary: bool,
    /// The bean's initialization / `resolve_all` ordering.
    pub order: i32,
    /// The thunk that performs registration (and auto-binding) on a container.
    pub register: fn(&Container),
    /// The conditions/profiles guarding this registration.
    pub conditions: fn() -> Vec<Condition>,
}

inventory::collect!(ComponentRegistration);

/// Iterate every component-scan thunk submitted across the crate graph.
///
/// Used by [`Container::scan`](crate::Container::scan); exposed so tooling can
/// enumerate the discoverable beans without registering them.
pub fn discovered() -> impl Iterator<Item = &'static ComponentRegistration> {
    inventory::iter::<ComponentRegistration>.into_iter()
}

/// A snapshot of one registered bean, for admin introspection (`/beans`).
///
/// Ports the shape pyfly's `BeansProvider.get_beans` returns
/// (`name`/`type`/`scope`/`stereotype`/`primary` + a resolution count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeanDescriptor {
    /// The bean name (explicit name, else the type name).
    pub name: String,
    /// The fully-qualified Rust type name.
    pub type_name: String,
    /// The lifecycle scope name (`singleton`, `transient`, …).
    pub scope: String,
    /// The stereotype label, or `None` for hand-registered factory beans.
    pub stereotype: Option<String>,
    /// Whether the bean is primary.
    pub primary: bool,
    /// Whether a singleton instance has been built.
    pub initialized: bool,
    /// How many times the bean has been resolved.
    pub resolution_count: u64,
}

/// Aggregate counts for the admin overview (`beans.total` + `stereotypes`).
///
/// Mirrors pyfly `OverviewProvider`'s `beans` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeanStats {
    /// Total registered beans.
    pub total: usize,
    /// Count of beans per stereotype label (`component`, `service`, …).
    pub stereotypes: std::collections::BTreeMap<String, usize>,
}

/// Compile-time metadata for one `#[rest_controller]` route.
///
/// Emitted by the `#[rest_controller]` macro both as a `Controller::ROUTES`
/// const and as an [`inventory::submit!`]. The OpenAPI generator (a separate
/// crate) enumerates every route via [`routes`] without re-parsing source —
/// the Rust analog of Spring's `RequestMappingHandlerMapping` and the actuator
/// `/mappings` endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDescriptor {
    /// The controller type name (`OrderApi`).
    pub controller: &'static str,
    /// The HTTP verb, upper-cased (`GET`, `POST`, …).
    pub method: &'static str,
    /// The fully-joined route path (`/api/v1/orders/:id`).
    pub path: &'static str,
    /// The handler method name (`get_order`).
    pub handler: &'static str,
}

inventory::collect!(RouteDescriptor);

/// Iterate every `#[rest_controller]` route discovered across the crate graph.
///
/// Used by the OpenAPI generator to build a spec from the live route table.
pub fn routes() -> impl Iterator<Item = &'static RouteDescriptor> {
    inventory::iter::<RouteDescriptor>.into_iter()
}
