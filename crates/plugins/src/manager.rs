//! [`PluginManager`] — dependency-aware lifecycle with per-plugin state and a
//! shared [`ExtensionRegistry`].
//!
//! This is the Rust adaptation of pyfly's `pyfly.plugins.manager`. Where pyfly
//! discovers `@extension`/`@extension_point` inner classes by reflection and
//! defines lifecycle hooks by duck-typed method names, the Rust port drives the
//! same behaviour through the explicit [`Plugin`] trait and the
//! [`ExtensionRegistry`] passed in (or created) at construction.
//!
//! Compared to the simpler [`Registry`](crate::Registry) (which runs a single
//! all-or-nothing sweep), `PluginManager` additionally supports:
//!
//! - per-plugin start/stop with transitive cascade ([`start_plugin`] starts
//!   dependencies first; [`stop_plugin`] stops dependents first),
//! - per-plugin [`PluginState`] and [`PluginDescriptor`] tracking,
//! - skipping plugins already in the target state so hooks never double-run.
//!
//! [`start_plugin`]: PluginManager::start_plugin
//! [`stop_plugin`]: PluginManager::stop_plugin

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use crate::extension::ExtensionRegistry;
use crate::resolve::topological_order;
use crate::{Plugin, PluginError, PluginState, ResolutionError};

/// Runtime descriptor for a single managed plugin: its metadata plus current
/// lifecycle state.
///
/// Mirrors pyfly's `PluginDescriptor`. Returned by
/// [`PluginManager::get_plugin`].
#[derive(Clone)]
pub struct PluginDescriptor {
    /// Unique plugin id (its [`Plugin::name`]).
    pub id: String,
    /// The names this plugin declares it depends on.
    pub depends_on: Vec<String>,
    /// Current lifecycle state.
    pub state: PluginState,
    /// When the plugin was added to the manager.
    pub loaded_at: DateTime<Utc>,
    /// When `state` last changed.
    pub last_state_change: DateTime<Utc>,
    /// If [`state`](Self::state) is [`PluginState::Failed`], the reason; else
    /// `None`.
    pub failed_reason: Option<String>,
}

impl std::fmt::Debug for PluginDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginDescriptor")
            .field("id", &self.id)
            .field("depends_on", &self.depends_on)
            .field("state", &self.state)
            .field("loaded_at", &self.loaded_at)
            .field("last_state_change", &self.last_state_change)
            .field("failed_reason", &self.failed_reason)
            .finish()
    }
}

struct Entry {
    plugin: Arc<dyn Plugin>,
    descriptor: PluginDescriptor,
}

#[derive(Default)]
struct Inner {
    /// Plugins keyed by id, in registration order alongside `order`.
    entries: HashMap<String, Entry>,
    /// Registration order of ids (for stable tie-breaking in resolution).
    order: Vec<String>,
    started: bool,
}

/// Loads, starts, stops and unloads plugins in dependency order, tracking each
/// plugin's [`PluginState`] and sharing one [`ExtensionRegistry`].
///
/// Cheap to share behind an `Arc`; every method takes `&self`. Mutations are
/// serialized by an async [`Mutex`].
pub struct PluginManager {
    inner: Mutex<Inner>,
    registry: Arc<ExtensionRegistry>,
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginManager {
    /// Creates a manager with a fresh [`ExtensionRegistry`].
    pub fn new() -> Self {
        Self::with_registry(Arc::new(ExtensionRegistry::new()))
    }

    /// Creates a manager sharing the given [`ExtensionRegistry`], so plugins
    /// can contribute extensions that other components discover.
    pub fn with_registry(registry: Arc<ExtensionRegistry>) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            registry,
        }
    }

    /// Returns the shared [`ExtensionRegistry`].
    pub fn registry(&self) -> &Arc<ExtensionRegistry> {
        &self.registry
    }

    /// Adds `plugin` in [`PluginState::Loaded`] state.
    ///
    /// Re-adding by name replaces the existing entry and resets it to
    /// [`PluginState::Loaded`], matching [`Registry`](crate::Registry)
    /// semantics.
    pub async fn add(&self, plugin: Arc<dyn Plugin>) {
        let now = Utc::now();
        let id = plugin.name().to_owned();
        let depends_on = plugin.depends_on();
        let mut inner = self.inner.lock().await;
        if !inner.entries.contains_key(&id) {
            inner.order.push(id.clone());
        }
        let descriptor = PluginDescriptor {
            id: id.clone(),
            depends_on,
            state: PluginState::Loaded,
            loaded_at: now,
            last_state_change: now,
            failed_reason: None,
        };
        inner.entries.insert(id, Entry { plugin, descriptor });
    }

    /// Returns the descriptor for `id`, or `None` if no such plugin is loaded.
    pub async fn get_plugin(&self, id: &str) -> Option<PluginDescriptor> {
        let inner = self.inner.lock().await;
        inner.entries.get(id).map(|e| e.descriptor.clone())
    }

    /// Returns every loaded plugin's descriptor, in registration order.
    pub async fn list_plugins(&self) -> Vec<PluginDescriptor> {
        let inner = self.inner.lock().await;
        inner
            .order
            .iter()
            .filter_map(|id| inner.entries.get(id).map(|e| e.descriptor.clone()))
            .collect()
    }

    /// Returns the loaded plugin ids in registration order.
    pub async fn ids(&self) -> Vec<String> {
        self.inner.lock().await.order.clone()
    }

    // ------------------------------------------------------------------
    // Resolution helpers
    // ------------------------------------------------------------------

    /// Snapshots `(id, depends_on)` in registration order.
    fn nodes(inner: &Inner) -> Vec<(String, Vec<String>)> {
        inner
            .order
            .iter()
            .filter_map(|id| {
                inner
                    .entries
                    .get(id)
                    .map(|e| (id.clone(), e.descriptor.depends_on.clone()))
            })
            .collect()
    }

    /// Full topological order of loaded plugin ids.
    fn full_order(inner: &Inner) -> Result<Vec<String>, ResolutionError> {
        let nodes = Self::nodes(inner);
        let order = topological_order(&nodes)?;
        Ok(order.into_iter().map(|i| nodes[i].0.clone()).collect())
    }

    /// All transitive dependency ids of `id` (excluding `id`).
    fn transitive_deps(inner: &Inner, id: &str) -> HashSet<String> {
        let mut visited = HashSet::new();
        let mut queue: Vec<String> = inner
            .entries
            .get(id)
            .map(|e| e.descriptor.depends_on.clone())
            .unwrap_or_default();
        while let Some(dep) = queue.pop() {
            if !visited.insert(dep.clone()) {
                continue;
            }
            if let Some(entry) = inner.entries.get(&dep) {
                queue.extend(entry.descriptor.depends_on.clone());
            }
        }
        visited
    }

    /// All transitive dependents of `id` (excluding `id`).
    fn transitive_dependents(inner: &Inner, id: &str) -> HashSet<String> {
        let mut dependents: HashSet<String> = HashSet::new();
        let mut changed = true;
        while changed {
            changed = false;
            for entry in inner.entries.values() {
                let pid = &entry.descriptor.id;
                if dependents.contains(pid) {
                    continue;
                }
                let deps: HashSet<&String> = entry.descriptor.depends_on.iter().collect();
                let touches_target = deps.contains(&id.to_owned());
                let touches_dependent = deps.iter().any(|d| dependents.contains(*d));
                if touches_target || touches_dependent {
                    dependents.insert(pid.clone());
                    changed = true;
                }
            }
        }
        dependents
    }

    fn transition(entry: &mut Entry, state: PluginState, reason: Option<String>) {
        entry.descriptor.state = state;
        entry.descriptor.last_state_change = Utc::now();
        entry.descriptor.failed_reason = reason;
    }

    // ------------------------------------------------------------------
    // Per-plugin lifecycle
    // ------------------------------------------------------------------

    /// Starts `id` and all its transitive dependencies, dependencies first.
    ///
    /// Plugins already [`Started`](PluginState::Started) are skipped, so their
    /// start hooks never run twice. On a start-hook failure the plugin is
    /// marked [`Failed`](PluginState::Failed).
    ///
    /// # Errors
    ///
    /// - [`PluginError::Resolution`] if `id` is unknown or the graph is
    ///   unresolvable.
    /// - [`PluginError::Start`] if a start hook fails.
    pub async fn start_plugin(&self, id: &str) -> Result<(), PluginError> {
        let to_start = {
            let inner = self.inner.lock().await;
            if !inner.entries.contains_key(id) {
                return Err(PluginError::Resolution(
                    ResolutionError::MissingDependency {
                        plugin: id.to_owned(),
                        missing: id.to_owned(),
                    },
                ));
            }
            let full = Self::full_order(&inner)?;
            let mut subset = Self::transitive_deps(&inner, id);
            subset.insert(id.to_owned());
            full.into_iter()
                .filter(|pid| subset.contains(pid))
                .collect::<Vec<_>>()
        };
        self.run_start(&to_start).await
    }

    /// Stops `id` and all plugins that transitively depend on it, dependents
    /// first.
    ///
    /// Plugins already [`Stopped`](PluginState::Stopped) or
    /// [`Loaded`](PluginState::Loaded) are skipped. On a stop-hook failure the
    /// plugin is marked [`Failed`](PluginState::Failed).
    ///
    /// # Errors
    ///
    /// - [`PluginError::Resolution`] if `id` is unknown or the graph is
    ///   unresolvable.
    /// - [`PluginError::Stop`] if a stop hook fails.
    pub async fn stop_plugin(&self, id: &str) -> Result<(), PluginError> {
        let to_stop = {
            let inner = self.inner.lock().await;
            if !inner.entries.contains_key(id) {
                return Err(PluginError::Resolution(
                    ResolutionError::MissingDependency {
                        plugin: id.to_owned(),
                        missing: id.to_owned(),
                    },
                ));
            }
            let full = Self::full_order(&inner)?;
            let mut subset = Self::transitive_dependents(&inner, id);
            subset.insert(id.to_owned());
            full.into_iter()
                .rev()
                .filter(|pid| subset.contains(pid))
                .collect::<Vec<_>>()
        };
        self.run_stop(&to_stop).await
    }

    // ------------------------------------------------------------------
    // Bulk lifecycle
    // ------------------------------------------------------------------

    /// Starts every loaded plugin in dependency order. No-op if already
    /// started. Plugins already [`Started`](PluginState::Started) (e.g. via an
    /// earlier [`start_plugin`](Self::start_plugin)) are skipped so their hooks
    /// don't double-run.
    ///
    /// # Errors
    ///
    /// [`PluginError::Resolution`] if the graph is unresolvable;
    /// [`PluginError::Start`] if a start hook fails.
    pub async fn start_all(&self) -> Result<(), PluginError> {
        let order = {
            let inner = self.inner.lock().await;
            if inner.started {
                return Ok(());
            }
            Self::full_order(&inner)?
        };
        self.run_start(&order).await?;
        self.inner.lock().await.started = true;
        Ok(())
    }

    /// Stops every loaded plugin in reverse dependency order. No-op if not
    /// started.
    ///
    /// # Errors
    ///
    /// [`PluginError::Resolution`] if the graph is unresolvable;
    /// [`PluginError::Stop`] if a stop hook fails.
    pub async fn stop_all(&self) -> Result<(), PluginError> {
        let order = {
            let inner = self.inner.lock().await;
            if !inner.started {
                return Ok(());
            }
            let mut o = Self::full_order(&inner)?;
            o.reverse();
            o
        };
        self.run_stop(&order).await?;
        self.inner.lock().await.started = false;
        Ok(())
    }

    /// Unloads `id`, running its [`Plugin::stop`] hook if it is still running.
    /// Returns whether the plugin existed.
    pub async fn remove(&self, id: &str) -> bool {
        let entry_plugin = {
            let inner = self.inner.lock().await;
            inner
                .entries
                .get(id)
                .map(|e| (e.plugin.clone(), e.descriptor.state))
        };
        let Some((plugin, state)) = entry_plugin else {
            return false;
        };
        if state == PluginState::Started {
            // Best-effort stop; ignore errors on unload path.
            let _ = plugin.stop().await;
        }
        let mut inner = self.inner.lock().await;
        inner.entries.remove(id);
        inner.order.retain(|pid| pid != id);
        true
    }

    /// Unloads every plugin in reverse dependency order.
    ///
    /// # Errors
    ///
    /// [`PluginError::Resolution`] if the graph is unresolvable.
    pub async fn unload_all(&self) -> Result<(), PluginError> {
        let order = {
            let inner = self.inner.lock().await;
            let mut o = Self::full_order(&inner)?;
            o.reverse();
            o
        };
        for id in order {
            self.remove(&id).await;
        }
        self.inner.lock().await.started = false;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal hook runners
    // ------------------------------------------------------------------

    async fn run_start(&self, ids: &[String]) -> Result<(), PluginError> {
        for id in ids {
            let plugin = {
                let inner = self.inner.lock().await;
                match inner.entries.get(id) {
                    Some(e) if e.descriptor.state == PluginState::Started => continue,
                    Some(e) => e.plugin.clone(),
                    None => continue,
                }
            };
            if let Err(err) = plugin.start().await {
                let reason = err.to_string();
                if let Some(entry) = self.inner.lock().await.entries.get_mut(id) {
                    Self::transition(entry, PluginState::Failed, Some(reason));
                }
                return Err(PluginError::Start {
                    name: id.clone(),
                    source: err,
                });
            }
            if let Some(entry) = self.inner.lock().await.entries.get_mut(id) {
                Self::transition(entry, PluginState::Started, None);
            }
        }
        Ok(())
    }

    async fn run_stop(&self, ids: &[String]) -> Result<(), PluginError> {
        for id in ids {
            let plugin = {
                let inner = self.inner.lock().await;
                match inner.entries.get(id) {
                    Some(e)
                        if matches!(
                            e.descriptor.state,
                            PluginState::Stopped | PluginState::Loaded
                        ) =>
                    {
                        continue
                    }
                    Some(e) => e.plugin.clone(),
                    None => continue,
                }
            };
            if let Err(err) = plugin.stop().await {
                let reason = err.to_string();
                if let Some(entry) = self.inner.lock().await.entries.get_mut(id) {
                    Self::transition(entry, PluginState::Failed, Some(reason));
                }
                return Err(PluginError::Stop {
                    name: id.clone(),
                    source: err,
                });
            }
            if let Some(entry) = self.inner.lock().await.entries.get_mut(id) {
                Self::transition(entry, PluginState::Stopped, None);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;

    use super::*;
    use crate::BoxError;

    /// A plugin that records start/stop into a shared log.
    struct Recorder {
        id: &'static str,
        deps: Vec<String>,
        log: Arc<StdMutex<Vec<String>>>,
        fail_start: bool,
        fail_stop: bool,
    }

    impl Recorder {
        fn new(id: &'static str, deps: &[&str], log: &Arc<StdMutex<Vec<String>>>) -> Arc<Self> {
            Arc::new(Self {
                id,
                deps: deps.iter().map(|s| (*s).to_owned()).collect(),
                log: Arc::clone(log),
                fail_start: false,
                fail_stop: false,
            })
        }

        fn failing_start(id: &'static str, log: &Arc<StdMutex<Vec<String>>>) -> Arc<Self> {
            Arc::new(Self {
                id,
                deps: Vec::new(),
                log: Arc::clone(log),
                fail_start: true,
                fail_stop: false,
            })
        }
    }

    #[async_trait]
    impl Plugin for Recorder {
        fn name(&self) -> &str {
            self.id
        }
        fn depends_on(&self) -> Vec<String> {
            self.deps.clone()
        }
        async fn start(&self) -> Result<(), BoxError> {
            self.log.lock().unwrap().push(self.id.to_owned());
            if self.fail_start {
                return Err("kaboom".into());
            }
            Ok(())
        }
        async fn stop(&self) -> Result<(), BoxError> {
            self.log.lock().unwrap().push(self.id.to_owned());
            if self.fail_stop {
                return Err("nope".into());
            }
            Ok(())
        }
    }

    // Port of pyfly test_lifecycle_in_dependency_order.
    #[tokio::test]
    async fn lifecycle_in_dependency_order() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("a", &[], &log)).await;
        mgr.add(Recorder::new("b", &["a"], &log)).await;
        // start_all then stop_all
        mgr.start_all().await.unwrap();
        let starts = log.lock().unwrap().clone();
        log.lock().unwrap().clear();
        mgr.stop_all().await.unwrap();
        let stops = log.lock().unwrap().clone();
        assert_eq!(starts, vec!["a", "b"]);
        assert_eq!(stops, vec!["b", "a"]);
    }

    // Port of pyfly test_start_plugin_cascades_dependencies.
    #[tokio::test]
    async fn start_plugin_cascades_dependencies() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("a", &[], &log)).await;
        mgr.add(Recorder::new("b", &["a"], &log)).await;
        mgr.add(Recorder::new("c", &["b"], &log)).await;
        mgr.start_plugin("c").await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["a", "b", "c"]);
        for id in ["a", "b", "c"] {
            assert_eq!(
                mgr.get_plugin(id).await.unwrap().state,
                PluginState::Started
            );
        }
    }

    // Port of pyfly test_start_plugin_skips_already_started.
    #[tokio::test]
    async fn start_plugin_skips_already_started() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("a", &[], &log)).await;
        mgr.add(Recorder::new("b", &["a"], &log)).await;
        mgr.add(Recorder::new("c", &["b"], &log)).await;
        mgr.start_plugin("a").await.unwrap();
        log.lock().unwrap().clear();
        mgr.start_plugin("c").await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["b", "c"]);
    }

    // Port of pyfly test_stop_plugin_cascades_dependents.
    #[tokio::test]
    async fn stop_plugin_cascades_dependents() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("a", &[], &log)).await;
        mgr.add(Recorder::new("b", &["a"], &log)).await;
        mgr.add(Recorder::new("c", &["b"], &log)).await;
        mgr.start_plugin("c").await.unwrap();
        log.lock().unwrap().clear();
        mgr.stop_plugin("a").await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["c", "b", "a"]);
        for id in ["a", "b", "c"] {
            assert_eq!(
                mgr.get_plugin(id).await.unwrap().state,
                PluginState::Stopped
            );
        }
    }

    // Port of pyfly test_failed_start_sets_failed_state.
    #[tokio::test]
    async fn failed_start_sets_failed_state() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::failing_start("bad", &log)).await;
        let err = mgr.start_plugin("bad").await.expect_err("should fail");
        assert!(matches!(err, PluginError::Start { .. }));
        let desc = mgr.get_plugin("bad").await.unwrap();
        assert_eq!(desc.state, PluginState::Failed);
        assert_eq!(desc.failed_reason.as_deref(), Some("kaboom"));
    }

    // Port of pyfly test_get_plugin_returns_none_for_unknown.
    #[tokio::test]
    async fn get_plugin_returns_none_for_unknown() {
        let mgr = PluginManager::new();
        assert!(mgr.get_plugin("nope").await.is_none());
    }

    // Port of pyfly test_start_plugin_raises_state_error_for_unknown.
    #[tokio::test]
    async fn start_plugin_unknown_errors() {
        let mgr = PluginManager::new();
        assert!(matches!(
            mgr.start_plugin("ghost").await,
            Err(PluginError::Resolution(_))
        ));
    }

    // Port of pyfly test_stop_plugin_raises_state_error_for_unknown.
    #[tokio::test]
    async fn stop_plugin_unknown_errors() {
        let mgr = PluginManager::new();
        assert!(matches!(
            mgr.stop_plugin("ghost").await,
            Err(PluginError::Resolution(_))
        ));
    }

    // Port of pyfly test_start_all_sets_started_state / test_stop_all_sets_stopped_state.
    #[tokio::test]
    async fn start_all_and_stop_all_set_state() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("s1", &[], &log)).await;
        mgr.add(Recorder::new("s2", &["s1"], &log)).await;
        mgr.start_all().await.unwrap();
        for id in ["s1", "s2"] {
            assert_eq!(
                mgr.get_plugin(id).await.unwrap().state,
                PluginState::Started
            );
        }
        mgr.stop_all().await.unwrap();
        for id in ["s1", "s2"] {
            assert_eq!(
                mgr.get_plugin(id).await.unwrap().state,
                PluginState::Stopped
            );
        }
    }

    // Port of pyfly test_start_all_does_not_double_run_already_started_plugin.
    #[tokio::test]
    async fn start_all_does_not_double_run_already_started() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("p1", &[], &log)).await;
        mgr.add(Recorder::new("p2", &["p1"], &log)).await;
        mgr.start_plugin("p1").await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["p1"]);
        mgr.start_all().await.unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["p1", "p2"]);
    }

    #[tokio::test]
    async fn remove_unknown_returns_false() {
        let mgr = PluginManager::new();
        assert!(!mgr.remove("nope").await);
    }

    #[tokio::test]
    async fn remove_stops_started_plugin_and_forgets_it() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("x", &[], &log)).await;
        mgr.start_all().await.unwrap();
        log.lock().unwrap().clear();
        assert!(mgr.remove("x").await);
        // stop hook ran on removal
        assert_eq!(*log.lock().unwrap(), vec!["x"]);
        assert!(mgr.get_plugin("x").await.is_none());
        assert!(mgr.list_plugins().await.is_empty());
    }

    #[tokio::test]
    async fn unload_all_clears_manager() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("p1", &[], &log)).await;
        mgr.add(Recorder::new("p2", &["p1"], &log)).await;
        mgr.start_all().await.unwrap();
        mgr.unload_all().await.unwrap();
        assert!(mgr.list_plugins().await.is_empty());
    }

    #[tokio::test]
    async fn cycle_is_rejected_by_start_all() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("a", &["b"], &log)).await;
        mgr.add(Recorder::new("b", &["a"], &log)).await;
        assert!(matches!(
            mgr.start_all().await,
            Err(PluginError::Resolution(ResolutionError::Cycle))
        ));
    }

    #[tokio::test]
    async fn descriptor_captures_depends_on_and_loaded_at() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let mgr = PluginManager::new();
        mgr.add(Recorder::new("b", &["a"], &log)).await;
        let desc = mgr.get_plugin("b").await.unwrap();
        assert_eq!(desc.id, "b");
        assert_eq!(desc.depends_on, vec!["a".to_owned()]);
        assert_eq!(desc.state, PluginState::Loaded);
    }

    #[tokio::test]
    async fn shared_registry_is_accessible() {
        let registry = Arc::new(ExtensionRegistry::new());
        let mgr = PluginManager::with_registry(registry.clone());
        assert!(Arc::ptr_eq(mgr.registry(), &registry));
    }
}
