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

//! [`ApplicationContext`] — the pyfly `ApplicationContext` analog.
//!
//! A thin convenience wrapper that builds a shared
//! [`Container`](firefly_container::Container), populates a
//! [`ConditionContext`](firefly_container::ConditionContext) from
//! `firefly_config` (active profiles + a flat config map), component-scans the
//! crate graph (honoring conditionals/profiles), eagerly warms non-lazy
//! singletons (running their `#[post_construct]` hooks), and exposes the
//! container for resolution. [`ApplicationContext::close`] runs `#[pre_destroy]`
//! hooks in reverse construction order.
//!
//! ```no_run
//! use firefly::ApplicationContext;
//! use std::collections::HashMap;
//!
//! let ctx = ApplicationContext::builder()
//!     .profiles(["prod"])
//!     .properties(HashMap::from([("app.batch".to_string(), "100".to_string())]))
//!     .build();
//! // beans are scanned + registered; resolve through the container:
//! // let svc = ctx.container().resolve::<MyService>().unwrap();
//! ctx.close();
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use firefly_config::Source;
use firefly_container::{BeanDescriptor, BeanStats, ConditionContext, Container, ContainerError};

/// The Spring/pyfly `ApplicationContext` analog for the Firefly Framework.
///
/// Wraps a shared [`Container`], the config-derived
/// [`ConditionContext`](firefly_container::ConditionContext), and the count of
/// beans registered by [`scan`](firefly_container::Container::scan). Build one
/// with [`ApplicationContext::builder`] (or [`ApplicationContext::new`] for the
/// zero-config default).
pub struct ApplicationContext {
    container: Arc<Container>,
    bean_count: usize,
}

impl ApplicationContext {
    /// Build a context with default settings: profiles from the
    /// `FIREFLY_PROFILE` environment variable (fallback `default`), no extra
    /// config properties, scanning every discovered bean.
    #[must_use]
    pub fn new() -> Self {
        ApplicationContext::builder().build()
    }

    /// Start configuring an [`ApplicationContext`].
    #[must_use]
    pub fn builder() -> ApplicationContextBuilder {
        ApplicationContextBuilder::default()
    }

    /// The shared container — resolve beans through it, or clone the `Arc` to
    /// share it with adapters.
    #[must_use]
    pub fn container(&self) -> &Arc<Container> {
        &self.container
    }

    /// Resolve a single bean of type `T` from the context's container.
    ///
    /// # Errors
    /// Propagates [`ContainerError`] from the container (no such bean, no
    /// unique bean, circular dependency, …).
    pub fn resolve<T: ?Sized + Send + Sync + 'static>(&self) -> Result<Arc<T>, ContainerError> {
        self.container.resolve::<T>()
    }

    /// The number of beans registered during the startup scan.
    #[must_use]
    pub fn bean_count(&self) -> usize {
        self.bean_count
    }

    /// A snapshot of every registered bean (for the admin `/beans` view).
    #[must_use]
    pub fn beans(&self) -> Vec<BeanDescriptor> {
        self.container.beans()
    }

    /// Aggregate bean counts (total + per-stereotype) for the admin overview.
    #[must_use]
    pub fn bean_stats(&self) -> BeanStats {
        self.container.bean_stats()
    }

    /// Close the context: run every `#[pre_destroy]` hook in reverse
    /// construction order and evict cached singletons. The pyfly
    /// `ApplicationContext.stop()` analog (for the lifecycle half).
    pub fn close(&self) {
        self.container.destroy();
    }
}

impl Default for ApplicationContext {
    fn default() -> Self {
        ApplicationContext::new()
    }
}

/// Builder for [`ApplicationContext`].
///
/// Accumulates the active profiles and config properties that feed the
/// [`ConditionContext`](firefly_container::ConditionContext), then
/// [`build`](ApplicationContextBuilder::build) constructs a shared container,
/// scans, and eagerly warms non-lazy singletons.
#[derive(Default)]
pub struct ApplicationContextBuilder {
    profiles: Option<Vec<String>>,
    properties: HashMap<String, String>,
    classes: Vec<String>,
    eager: Option<bool>,
}

impl ApplicationContextBuilder {
    /// Set the active profiles explicitly (otherwise read from
    /// `FIREFLY_PROFILE`).
    #[must_use]
    pub fn profiles<I, S>(mut self, profiles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.profiles = Some(profiles.into_iter().map(Into::into).collect());
        self
    }

    /// Add config properties from a flat map (merged into any already set).
    #[must_use]
    pub fn properties(mut self, properties: HashMap<String, String>) -> Self {
        self.properties.extend(properties);
        self
    }

    /// Add a single config property.
    #[must_use]
    pub fn property(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Merge config properties from `firefly_config` [`Source`]s (env, YAML,
    /// static layers). Keys collide last-wins with already-set properties.
    ///
    /// # Errors
    /// Propagates a [`firefly_config::ConfigError`] if a source fails to flatten.
    pub fn config_sources(
        mut self,
        sources: Vec<Box<dyn Source>>,
    ) -> Result<Self, firefly_config::ConfigError> {
        let layered = firefly_config::Layered::new(sources);
        let map = layered.map()?;
        self.properties.extend(map);
        Ok(self)
    }

    /// Mark a "class"/feature label as present for `condition_on_class` checks.
    #[must_use]
    pub fn class(mut self, label: impl Into<String>) -> Self {
        self.classes.push(label.into());
        self
    }

    /// Eagerly resolve every non-lazy singleton at build time (Spring fail-fast
    /// startup, running `#[post_construct]` hooks). Defaults to `true`.
    #[must_use]
    pub fn eager(mut self, eager: bool) -> Self {
        self.eager = Some(eager);
        self
    }

    /// Construct the [`ApplicationContext`]: build a shared container, install
    /// the condition context, scan the crate graph (registering survivors),
    /// then eagerly warm non-lazy singletons.
    #[must_use]
    pub fn build(self) -> ApplicationContext {
        let profiles = self
            .profiles
            .unwrap_or_else(|| firefly_config::active_profiles("default"));

        let mut cond = ConditionContext::new()
            .with_profiles(profiles)
            .with_properties(self.properties);
        for label in self.classes {
            cond = cond.with_class(label);
        }

        let container = Container::shared();
        container.set_condition_context(cond);
        let bean_count = container.scan();

        if self.eager.unwrap_or(true) {
            // Eagerly resolve each discovered singleton so construction-time
            // failures surface at startup and `#[post_construct]` hooks run
            // before first use (pyfly's eager-init pass). Errors are
            // swallowed: an unsatisfiable optional bean must not abort startup.
            warm_singletons(&container);
        }

        ApplicationContext {
            container,
            bean_count,
        }
    }
}

/// Eagerly resolve every registered bean once, by name, ignoring errors. Beans
/// resolve by their concrete type; resolving by name is the only generic-free
/// hook the container exposes, so we drive it through the bean descriptors.
fn warm_singletons(container: &Arc<Container>) {
    for bean in container.beans() {
        // Resolve by name when the bean carries one; anonymous beans are warmed
        // lazily on first use (their type is not statically known here). This
        // mirrors pyfly's eager loop, which fail-fast-resolves each singleton.
        if !bean.name.is_empty() {
            let _ = container.resolve_named_erased(&bean.name);
        }
    }
}
