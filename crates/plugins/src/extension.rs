//! [`ExtensionRegistry`] — runtime discovery of contributed extensions, keyed
//! by extension-point id and validated against a [`TypeId`].
//!
//! This is the Rust adaptation of pyfly's `pyfly.plugins.registry`. Python keys
//! an extension point by an interface *class* and validates that contributed
//! extensions are instances of it; Rust has no structural subclassing, so a
//! point is keyed by id and carries the [`TypeId`] of its interface type. When
//! a point declares an interface type, contributions register under that same
//! type and the registry rejects mismatches — the idiomatic equivalent of
//! Python's `isinstance` check. Contributions for ids with no declared point
//! type remain accepted (lenient, backward-compatible), matching pyfly.
//!
//! Extensions are stored type-erased as `Arc<dyn Any + Send + Sync>` and
//! returned highest-priority first, ties in insertion order.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Mutex;

/// An opaque, type-erased extension instance handed back by the registry.
///
/// Downcast with [`Any::downcast_ref`] (via [`AsRef`]/`as_any`) or the
/// convenience [`ExtensionRegistry::get_as`] helper.
pub type Extension = std::sync::Arc<dyn Any + Send + Sync>;

/// A declared extension point: an id plus the [`TypeId`] of its interface type.
///
/// Created by [`extension_point`] and registered via
/// [`ExtensionRegistry::register_extension_point`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionPoint {
    type_id: TypeId,
}

impl ExtensionPoint {
    /// Returns the [`TypeId`] of the interface type this point expects
    /// contributed extensions to be.
    pub fn type_id(&self) -> TypeId {
        self.type_id
    }
}

/// Declares an extension point whose interface type is `T`.
///
/// Mirrors pyfly's `@extension_point` decorator: the type argument `T` plays
/// the role of the decorated interface class. Register the returned point with
/// [`ExtensionRegistry::register_extension_point`].
///
/// ```
/// use firefly_plugins::{extension_point, ExtensionRegistry};
///
/// trait Formatter: Send + Sync {}
/// let point = extension_point::<dyn Formatter>();
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let reg = ExtensionRegistry::new();
/// reg.register_extension_point("formatters", point).await;
/// assert!(reg.has_extension_point("formatters").await);
/// # });
/// ```
pub fn extension_point<T: ?Sized + 'static>() -> ExtensionPoint {
    ExtensionPoint {
        type_id: TypeId::of::<T>(),
    }
}

/// Error returned by [`ExtensionRegistry`] operations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtensionError {
    /// An extension contributed for a declared point did not match the point's
    /// interface type. Mirrors pyfly's "does not implement extension point".
    #[error("extension does not implement extension point {0:?}")]
    DoesNotImplement(String),
    /// [`ExtensionRegistry::get_extension`] was asked for an id with no
    /// declared extension point.
    #[error("extension point {0:?} is not registered")]
    PointNotRegistered(String),
    /// [`ExtensionRegistry::get_extension`] found a declared point but no
    /// contributed extensions.
    #[error("extension point {0:?} has no registered extensions")]
    NoExtensions(String),
}

#[derive(Default)]
struct Inner {
    /// point id -> contributed `(priority, type_id, instance)`, sorted
    /// highest-priority first.
    extensions: HashMap<String, Vec<(i32, TypeId, Extension)>>,
    /// point id -> declared interface [`TypeId`].
    points: HashMap<String, TypeId>,
}

/// Registry of extension points and the extensions contributed to them.
///
/// Cheap to share behind an `Arc`; every method takes `&self`. Internally
/// guarded by a [`Mutex`] so concurrent registration/lookup is safe. Mirrors
/// pyfly's `ExtensionRegistry`.
#[derive(Default)]
pub struct ExtensionRegistry {
    inner: Mutex<Inner>,
}

impl ExtensionRegistry {
    /// Returns an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an extension point's id and the [`TypeId`] of its interface.
    ///
    /// Once a point is registered, [`register`](Self::register) validates that
    /// contributed extensions carry the same type id, mirroring Java's
    /// `DefaultExtensionRegistry`. Contributions for ids with no registered
    /// point type remain accepted (lenient, backward-compatible).
    pub async fn register_extension_point(&self, point_id: &str, point: ExtensionPoint) {
        self.inner
            .lock()
            .expect("extension registry lock poisoned")
            .points
            .insert(point_id.to_owned(), point.type_id());
    }

    /// Returns whether `point_id` has a declared extension point.
    pub async fn has_extension_point(&self, point_id: &str) -> bool {
        self.inner
            .lock()
            .expect("extension registry lock poisoned")
            .points
            .contains_key(point_id)
    }

    /// Returns the ids of all declared extension points.
    pub async fn extension_point_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("extension registry lock poisoned")
            .points
            .keys()
            .cloned()
            .collect()
    }

    /// Contributes `instance` to `point_id` with the default priority `0`.
    ///
    /// The concrete type `E` of the instance is recorded so that, when the
    /// point declares an interface via [`register_extension_point`] keyed on
    /// `E`'s type, validation passes. See [`register_with_priority`].
    ///
    /// [`register_with_priority`]: Self::register_with_priority
    ///
    /// # Errors
    ///
    /// [`ExtensionError::DoesNotImplement`] if the point declares an interface
    /// type and `E` is not that type.
    pub async fn register<E>(
        &self,
        point_id: &str,
        instance: std::sync::Arc<E>,
    ) -> Result<(), ExtensionError>
    where
        E: Any + Send + Sync + 'static,
    {
        self.register_with_priority(point_id, instance, 0).await
    }

    /// Contributes `instance` to `point_id` with an explicit `priority`.
    ///
    /// Higher priority sorts first in [`get`](Self::get) /
    /// [`get_extension`](Self::get_extension); ties keep insertion order.
    ///
    /// # Errors
    ///
    /// [`ExtensionError::DoesNotImplement`] if the point declares an interface
    /// type and `E` is not that type.
    pub async fn register_with_priority<E>(
        &self,
        point_id: &str,
        instance: std::sync::Arc<E>,
        priority: i32,
    ) -> Result<(), ExtensionError>
    where
        E: Any + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock().expect("extension registry lock poisoned");
        let ext_type = TypeId::of::<E>();
        if let Some(point_type) = inner.points.get(point_id) {
            if *point_type != ext_type {
                return Err(ExtensionError::DoesNotImplement(point_id.to_owned()));
            }
        }
        let entries = inner.extensions.entry(point_id.to_owned()).or_default();
        entries.push((priority, ext_type, instance));
        // Stable sort, descending priority — preserves insertion order on ties.
        entries.sort_by_key(|e| std::cmp::Reverse(e.0));
        Ok(())
    }

    /// Removes the first extension registered under `point_id` whose concrete
    /// instance pointer equals `instance`. Returns whether one was removed.
    pub async fn unregister<E>(&self, point_id: &str, instance: &std::sync::Arc<E>) -> bool
    where
        E: Any + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock().expect("extension registry lock poisoned");
        let target = std::sync::Arc::as_ptr(instance) as *const ();
        if let Some(entries) = inner.extensions.get_mut(point_id) {
            if let Some(pos) = entries
                .iter()
                .position(|(_, _, ext)| std::sync::Arc::as_ptr(ext) as *const () == target)
            {
                entries.remove(pos);
                return true;
            }
        }
        false
    }

    /// Returns all extensions contributed to `point_id`, highest priority
    /// first. Empty when nothing was contributed.
    pub async fn get(&self, point_id: &str) -> Vec<Extension> {
        self.inner
            .lock()
            .expect("extension registry lock poisoned")
            .extensions
            .get(point_id)
            .map(|entries| entries.iter().map(|(_, _, ext)| ext.clone()).collect())
            .unwrap_or_default()
    }

    /// Returns the contributed extensions for `point_id` downcast to `E`,
    /// highest priority first, skipping any that are not of type `E`.
    pub async fn get_as<E>(&self, point_id: &str) -> Vec<std::sync::Arc<E>>
    where
        E: Any + Send + Sync + 'static,
    {
        self.get(point_id)
            .await
            .into_iter()
            .filter_map(|ext| ext.downcast::<E>().ok())
            .collect()
    }

    /// Returns the single highest-priority extension for `point_id`.
    ///
    /// # Errors
    ///
    /// - [`ExtensionError::PointNotRegistered`] if `point_id` is not a declared
    ///   extension point.
    /// - [`ExtensionError::NoExtensions`] if the point is declared but has no
    ///   contributed extensions.
    pub async fn get_extension(&self, point_id: &str) -> Result<Extension, ExtensionError> {
        let inner = self.inner.lock().expect("extension registry lock poisoned");
        if !inner.points.contains_key(point_id) {
            return Err(ExtensionError::PointNotRegistered(point_id.to_owned()));
        }
        match inner.extensions.get(point_id).and_then(|e| e.first()) {
            Some((_, _, ext)) => Ok(ext.clone()),
            None => Err(ExtensionError::NoExtensions(point_id.to_owned())),
        }
    }

    /// Returns the ids that have at least one contributed extension.
    pub async fn points(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("extension registry lock poisoned")
            .extensions
            .keys()
            .cloned()
            .collect()
    }
}

impl std::fmt::Debug for ExtensionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().expect("extension registry lock poisoned");
        f.debug_struct("ExtensionRegistry")
            .field("points", &inner.points.keys().collect::<Vec<_>>())
            .field("extensions", &inner.extensions.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    // Interface type used as the point's marker.
    trait Greeter: Send + Sync {
        fn name(&self) -> &str;
    }

    struct English;
    impl Greeter for English {
        fn name(&self) -> &str {
            "english"
        }
    }

    // Port of pyfly test_register_point_then_validate (#218).
    #[tokio::test]
    async fn register_point_then_validate() {
        let reg = ExtensionRegistry::new();
        reg.register_extension_point("greeters", extension_point::<English>())
            .await;
        assert!(reg.has_extension_point("greeters").await);
        assert!(reg
            .extension_point_ids()
            .await
            .contains(&"greeters".to_owned()));

        reg.register("greeters", Arc::new(English))
            .await
            .expect("conforms");
        assert_eq!(reg.get("greeters").await.len(), 1);

        // A type that does not match the declared point type is rejected.
        struct NotAGreeter;
        let err = reg
            .register("greeters", Arc::new(NotAGreeter))
            .await
            .expect_err("mismatch");
        assert_eq!(err, ExtensionError::DoesNotImplement("greeters".to_owned()));
    }

    // Port of pyfly test_unknown_point_is_lenient.
    #[tokio::test]
    async fn unknown_point_is_lenient() {
        let reg = ExtensionRegistry::new();
        reg.register("freeform", Arc::new(English))
            .await
            .expect("lenient");
        assert_eq!(reg.get("freeform").await.len(), 1);
        assert!(!reg.has_extension_point("freeform").await);
    }

    #[tokio::test]
    async fn priority_orders_descending_ties_insertion() {
        let reg = ExtensionRegistry::new();
        reg.register_with_priority("p", Arc::new(5i32), 5)
            .await
            .unwrap();
        reg.register_with_priority("p", Arc::new(10i32), 10)
            .await
            .unwrap();
        reg.register_with_priority("p", Arc::new(5i32), 5)
            .await
            .unwrap();
        let got: Vec<i32> = reg.get_as::<i32>("p").await.iter().map(|a| **a).collect();
        assert_eq!(got, vec![10, 5, 5]);
    }

    #[tokio::test]
    async fn get_extension_returns_highest_priority() {
        let reg = ExtensionRegistry::new();
        reg.register_extension_point("greeters", extension_point::<English>())
            .await;
        reg.register("greeters", Arc::new(English)).await.unwrap();
        let ext = reg.get_extension("greeters").await.expect("extension");
        let greeter = ext.downcast::<English>().expect("downcast");
        assert_eq!(greeter.name(), "english");
    }

    #[tokio::test]
    async fn get_extension_unknown_point_errors() {
        let reg = ExtensionRegistry::new();
        let err = reg.get_extension("nope").await.expect_err("unknown");
        assert_eq!(err, ExtensionError::PointNotRegistered("nope".to_owned()));
    }

    #[tokio::test]
    async fn get_extension_empty_point_errors() {
        let reg = ExtensionRegistry::new();
        reg.register_extension_point("empty", extension_point::<English>())
            .await;
        let err = reg.get_extension("empty").await.expect_err("empty");
        assert_eq!(err, ExtensionError::NoExtensions("empty".to_owned()));
    }

    #[tokio::test]
    async fn unregister_removes_instance() {
        let reg = ExtensionRegistry::new();
        let inst = Arc::new(English);
        reg.register("things", inst.clone()).await.unwrap();
        assert_eq!(reg.get("things").await.len(), 1);
        assert!(reg.unregister("things", &inst).await);
        assert!(reg.get("things").await.is_empty());
        assert!(!reg.unregister("things", &inst).await);
    }

    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExtensionRegistry>();
        assert_send_sync::<ExtensionPoint>();
    }
}
