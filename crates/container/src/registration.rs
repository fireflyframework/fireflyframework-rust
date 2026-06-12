//! Per-bean registration metadata and runtime metrics.
//!
//! Ports pyfly's `container.registry.Registration` and
//! `container.metrics.BeanMetrics`. Reflection-derived fields (`init_plan`,
//! `condition`) have no Rust analog and are dropped; the construction logic
//! lives entirely in the [`factory`](Registration::factory) closure, which is
//! the Rust idiom replacing pyfly's cached `__init__` injection plan.

use std::any::TypeId;
use std::sync::Mutex;

use crate::scope::{ScopeSpec, SharedInstance};
use crate::ContainerError;

/// The boxed factory closure that builds a bean.
///
/// Receives the owning [`Container`](crate::Container) so it can resolve its own
/// dependencies (constructor injection, expressed as explicit `resolve` calls).
pub type Factory =
    Box<dyn Fn(&crate::Container) -> Result<SharedInstance, ContainerError> + Send + Sync>;

/// Metadata for a single registered bean.
///
/// Mirrors pyfly's `Registration` dataclass. Each registration carries the
/// erased [`TypeId`] of its implementation, its [`scope`](Registration::scope),
/// `primary`/`order` flags, an optional `name`, the [`Factory`] that builds it,
/// and (for singletons) a cached `instance`.
pub struct Registration {
    /// The concrete implementation type id this registration builds.
    pub(crate) impl_type: TypeId,
    /// A human-readable type name for diagnostics and metrics.
    pub(crate) type_name: String,
    /// This bean's lifecycle scope.
    pub(crate) scope: ScopeSpec,
    /// The explicit bean name, or empty when anonymous.
    pub(crate) name: String,
    /// Marks this the primary candidate among several beans of one interface.
    pub(crate) primary: bool,
    /// Initialization / `list<T>` ordering (lower = earlier). Defaults to `0`.
    pub(crate) order: i32,
    /// The factory that constructs the bean.
    pub(crate) factory: Factory,
    /// The cached SINGLETON / custom-scope-eligible instance, if built.
    pub(crate) instance: Mutex<Option<SharedInstance>>,
    /// Per-bean runtime metrics.
    pub(crate) metrics: Mutex<BeanMetrics>,
}

impl Registration {
    /// The readable bean name: the explicit `name` if set, otherwise the type
    /// name. Mirrors pyfly's `Registration.display_name`.
    #[must_use]
    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            self.type_name.clone()
        } else {
            self.name.clone()
        }
    }

    /// This registration's lifecycle scope.
    #[must_use]
    pub fn scope(&self) -> &ScopeSpec {
        &self.scope
    }

    /// Whether this registration is marked primary.
    #[must_use]
    pub fn is_primary(&self) -> bool {
        self.primary
    }

    /// This registration's ordering value (lower = earlier).
    #[must_use]
    pub fn order(&self) -> i32 {
        self.order
    }

    /// The implementation type id this registration builds.
    #[must_use]
    pub fn impl_type(&self) -> TypeId {
        self.impl_type
    }
}

/// Runtime metrics collected for a single bean registration.
///
/// Mirrors pyfly's `BeanMetrics`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BeanMetrics {
    /// Wall-clock nanoseconds taken by the most recent construction.
    pub creation_time_ns: u128,
    /// Number of times this bean has been resolved.
    pub resolution_count: u64,
}

/// Bean ordering constant: highest precedence (initialized first).
///
/// Mirrors pyfly's `HIGHEST_PRECEDENCE` (`-(2**31)`).
pub const HIGHEST_PRECEDENCE: i32 = i32::MIN;

/// Bean ordering constant: lowest precedence (initialized last).
///
/// Mirrors pyfly's `LOWEST_PRECEDENCE` (`2**31 - 1`).
pub const LOWEST_PRECEDENCE: i32 = i32::MAX;
