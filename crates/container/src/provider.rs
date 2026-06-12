//! [`Provider<T>`] — deferred dependency lookup.
//!
//! Ports pyfly's `container.provider.Provider` (Spring's `ObjectFactory`).

use std::marker::PhantomData;
use std::sync::Arc;

use crate::{Container, ContainerError};

/// A lazy handle to a bean — call [`get`](Provider::get) to resolve it.
///
/// Hold a `Provider<T>` instead of a `T` to defer resolution: each
/// [`get`](Provider::get) returns a freshly-resolved bean, so a singleton can
/// obtain new transient instances, and construction-time cycles or expensive
/// beans can be deferred until first use.
///
/// Mirrors pyfly's `Provider[T]`. Obtain one with
/// [`Container::provider`](crate::Container::provider).
///
/// ```
/// use firefly_container::{Container, Scope};
/// use std::sync::Arc;
///
/// #[derive(Default)]
/// struct Job;
///
/// let c = Arc::new(Container::new());
/// c.register_factory::<Job, _>(Scope::Transient, |_| Ok(Job));
/// let provider = c.provider::<Job>();
/// let _job: Arc<Job> = provider.get().unwrap();
/// ```
pub struct Provider<T: ?Sized> {
    container: Arc<Container>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Clone for Provider<T> {
    fn clone(&self) -> Self {
        Provider {
            container: Arc::clone(&self.container),
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized + Send + Sync + 'static> Provider<T> {
    /// Create a provider backed by `container`.
    #[must_use]
    pub fn new(container: Arc<Container>) -> Self {
        Provider {
            container,
            _marker: PhantomData,
        }
    }

    /// Resolve and return the bean (a fresh instance for transient scope).
    ///
    /// Mirrors pyfly's `Provider.get()`.
    pub fn get(&self) -> Result<Arc<T>, ContainerError> {
        self.container.resolve::<T>()
    }
}
