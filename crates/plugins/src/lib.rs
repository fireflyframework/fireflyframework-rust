//! firefly-plugins — plugin lifecycle SPI + composite Registry.
//!
//! This crate ships the framework's **plugin lifecycle SPI** — a typed
//! [`Plugin`] trait and a composite [`Registry`] that starts every plugin in
//! registration order and stops them in reverse on shutdown.
//!
//! Rust's static-binary model, like Go's, does not support hot reload out of
//! the box. The Java port uses PF4J; the .NET port uses
//! `McMaster.NETCore.Plugins`. This crate focuses on the **lifecycle
//! contract** — services that need dynamic loading integrate a loader (e.g.
//! `libloading`) at the application entry point and feed the discovered
//! values into the same [`Registry`].
//!
//! # Semantics
//!
//! - [`Registry::register`] adds a plugin; re-registering by name overwrites
//!   in place, preserving the original position in the start order.
//! - [`Registry::start_all`] starts every plugin in registration order. The
//!   first failure short-circuits and triggers [`Plugin::stop`] on the
//!   plugins that already started (in reverse order); the start error and any
//!   rollback errors are aggregated.
//! - [`Registry::stop_all`] stops started plugins in reverse order. Errors do
//!   not short-circuit — every started plugin gets its stop call — and are
//!   joined into a single [`PluginError`].
//!
//! # Quick start
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use firefly_plugins::{BoxError, Plugin, Registry};
//!
//! struct SchedulerPlugin;
//!
//! #[async_trait::async_trait]
//! impl Plugin for SchedulerPlugin {
//!     fn name(&self) -> &str {
//!         "scheduler"
//!     }
//!
//!     async fn start(&self) -> Result<(), BoxError> {
//!         println!("scheduler starting");
//!         Ok(())
//!     }
//!
//!     async fn stop(&self) -> Result<(), BoxError> {
//!         println!("scheduler stopping");
//!         Ok(())
//!     }
//! }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let registry = Registry::new();
//! registry.register(Arc::new(SchedulerPlugin));
//!
//! registry.start_all().await?;
//! // ... application runs ...
//! registry.stop_all().await?;
//! # Ok::<(), firefly_plugins::PluginError>(())
//! # }).unwrap();
//! ```

use std::fmt;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

/// Boxed error type returned by plugin lifecycle hooks.
///
/// Plays the role of Go's plain `error` return: plugins surface whatever
/// failure type they like, and the [`Registry`] wraps it with the plugin
/// name and lifecycle phase in a [`PluginError`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Plugin is the lifecycle contract every Firefly plugin satisfies.
///
/// [`Plugin::start`] runs once at application boot; [`Plugin::stop`] runs at
/// graceful shutdown. Both must be idempotent.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Returns the unique plugin name used for replace-by-name registration
    /// and error reporting.
    fn name(&self) -> &str;

    /// Starts the plugin. Runs once at application boot.
    async fn start(&self) -> Result<(), BoxError>;

    /// Stops the plugin. Runs at graceful shutdown (or as rollback when a
    /// downstream plugin fails to start).
    async fn stop(&self) -> Result<(), BoxError>;
}

/// Error raised by [`Registry`] lifecycle operations.
///
/// Single-plugin failures carry the plugin name and the underlying error;
/// multiple failures (a stop sweep with several errors, or a start failure
/// plus rollback errors) are aggregated. The [`fmt::Display`] output joins
/// aggregated messages with `\n`, matching Go's `errors.Join`, and each leaf
/// message matches the Go wrapping format `plugin "name" start: <cause>`.
#[derive(Debug, Error)]
pub enum PluginError {
    /// A plugin's [`Plugin::start`] hook failed.
    #[error("plugin {name:?} start: {source}")]
    Start {
        /// Name of the plugin that failed to start.
        name: String,
        /// The underlying error returned by the plugin.
        source: BoxError,
    },
    /// A plugin's [`Plugin::stop`] hook failed.
    #[error("plugin {name:?} stop: {source}")]
    Stop {
        /// Name of the plugin that failed to stop.
        name: String,
        /// The underlying error returned by the plugin.
        source: BoxError,
    },
    /// Multiple lifecycle errors joined together, in occurrence order.
    #[error("{}", join_messages(.0))]
    Aggregate(Vec<PluginError>),
}

/// Joins the display messages of `errors` with `\n`, mirroring `errors.Join`.
fn join_messages(errors: &[PluginError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Registry holds an ordered list of plugins.
///
/// Cheap to share behind an [`Arc`]; all methods take `&self`. Start/stop
/// sweeps are serialized against each other, so concurrent `start_all` /
/// `stop_all` calls never interleave lifecycle hooks.
#[derive(Default)]
pub struct Registry {
    /// Registered plugins in registration order.
    plugins: StdMutex<Vec<Arc<dyn Plugin>>>,
    /// Plugins started by the most recent sweep, in start order. The async
    /// lock doubles as the lifecycle critical section.
    started: AsyncMutex<Vec<Arc<dyn Plugin>>>,
}

impl Registry {
    /// Returns an empty Registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `plugin` to the registry. Re-registering by name overwrites the
    /// existing entry in place, keeping its position in the start order.
    pub fn register(&self, plugin: Arc<dyn Plugin>) {
        let mut plugins = self.plugins.lock().expect("plugins lock poisoned");
        if let Some(slot) = plugins
            .iter_mut()
            .find(|existing| existing.name() == plugin.name())
        {
            *slot = plugin;
        } else {
            plugins.push(plugin);
        }
    }

    /// Starts every plugin in registration order. The first error
    /// short-circuits and triggers [`Plugin::stop`] on the plugins that
    /// already started; rollback errors are aggregated with the start error.
    ///
    /// Plugins registered after the sweep snapshot is taken are not started
    /// by an in-flight call.
    pub async fn start_all(&self) -> Result<(), PluginError> {
        let mut started = self.started.lock().await;
        started.clear();
        let snapshot: Vec<Arc<dyn Plugin>> =
            self.plugins.lock().expect("plugins lock poisoned").clone();
        for plugin in snapshot {
            if let Err(err) = plugin.start().await {
                let start_err = PluginError::Start {
                    name: plugin.name().to_owned(),
                    source: err,
                };
                return match stop_started(&mut started).await {
                    None => Err(start_err),
                    Some(PluginError::Aggregate(stops)) => {
                        let mut joined = vec![start_err];
                        joined.extend(stops);
                        Err(PluginError::Aggregate(joined))
                    }
                    Some(stop_err) => Err(PluginError::Aggregate(vec![start_err, stop_err])),
                };
            }
            started.push(plugin);
        }
        Ok(())
    }

    /// Stops started plugins in reverse order. Errors do not short-circuit;
    /// they are joined into a single [`PluginError`].
    pub async fn stop_all(&self) -> Result<(), PluginError> {
        let mut started = self.started.lock().await;
        match stop_started(&mut started).await {
            None => Ok(()),
            Some(err) => Err(err),
        }
    }

    /// Returns the registered plugin names in registration order.
    pub fn names(&self) -> Vec<String> {
        self.plugins
            .lock()
            .expect("plugins lock poisoned")
            .iter()
            .map(|p| p.name().to_owned())
            .collect()
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("plugins", &self.names())
            .finish()
    }
}

/// Stops `started` plugins in reverse order, draining the list. Returns the
/// single stop error, the aggregate of several, or `None` on a clean sweep.
async fn stop_started(started: &mut Vec<Arc<dyn Plugin>>) -> Option<PluginError> {
    let mut errors = Vec::new();
    while let Some(plugin) = started.pop() {
        if let Err(err) = plugin.stop().await {
            errors.push(PluginError::Stop {
                name: plugin.name().to_owned(),
                source: err,
            });
        }
    }
    if errors.len() > 1 {
        Some(PluginError::Aggregate(errors))
    } else {
        errors.pop()
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    /// Test double mirroring the Go suite's `stub` plugin.
    struct Stub {
        name: &'static str,
        start_err: Option<&'static str>,
        stop_err: Option<&'static str>,
        started: AtomicBool,
        stopped: AtomicBool,
        start_calls: AtomicUsize,
        stop_calls: AtomicUsize,
        log: Option<Arc<StdMutex<Vec<String>>>>,
    }

    impl Stub {
        fn new(name: &'static str) -> Arc<Self> {
            Self::build(name, None, None, None)
        }

        fn failing_start(name: &'static str, msg: &'static str) -> Arc<Self> {
            Self::build(name, Some(msg), None, None)
        }

        fn failing_stop(name: &'static str, msg: &'static str) -> Arc<Self> {
            Self::build(name, None, Some(msg), None)
        }

        fn logged(name: &'static str, log: &Arc<StdMutex<Vec<String>>>) -> Arc<Self> {
            Self::build(name, None, None, Some(Arc::clone(log)))
        }

        fn build(
            name: &'static str,
            start_err: Option<&'static str>,
            stop_err: Option<&'static str>,
            log: Option<Arc<StdMutex<Vec<String>>>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                name,
                start_err,
                stop_err,
                started: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                start_calls: AtomicUsize::new(0),
                stop_calls: AtomicUsize::new(0),
                log,
            })
        }

        fn started(&self) -> bool {
            self.started.load(Ordering::SeqCst)
        }

        fn stopped(&self) -> bool {
            self.stopped.load(Ordering::SeqCst)
        }

        fn start_calls(&self) -> usize {
            self.start_calls.load(Ordering::SeqCst)
        }

        fn stop_calls(&self) -> usize {
            self.stop_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Plugin for Stub {
        fn name(&self) -> &str {
            self.name
        }

        async fn start(&self) -> Result<(), BoxError> {
            self.start_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(log) = &self.log {
                log.lock().unwrap().push(format!("start:{}", self.name));
            }
            if let Some(msg) = self.start_err {
                return Err(msg.into());
            }
            self.started.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn stop(&self) -> Result<(), BoxError> {
            self.stop_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(log) = &self.log {
                log.lock().unwrap().push(format!("stop:{}", self.name));
            }
            if let Some(msg) = self.stop_err {
                return Err(msg.into());
            }
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    // Port of Go TestRegistryStartStop.
    #[tokio::test]
    async fn registry_start_stop() {
        let registry = Registry::new();
        let a = Stub::new("a");
        let b = Stub::new("b");
        registry.register(a.clone());
        registry.register(b.clone());

        registry.start_all().await.expect("start_all");
        assert!(a.started() && b.started(), "plugins not started");

        registry.stop_all().await.expect("stop_all");
        assert!(a.stopped() && b.stopped(), "plugins not stopped");
    }

    // Port of Go TestRegistryRollbackOnStartFailure.
    #[tokio::test]
    async fn registry_rollback_on_start_failure() {
        let registry = Registry::new();
        let a = Stub::new("a");
        let b = Stub::failing_start("b", "boom");
        registry.register(a.clone());
        registry.register(b.clone());

        let err = registry.start_all().await;
        assert!(err.is_err(), "expected start failure");
        assert!(a.stopped(), "a should be stopped after b's start failed");
        assert_eq!(b.stop_calls(), 0, "b never started, must not be stopped");
    }

    // Port of Go TestRegistryReplaceByName.
    #[tokio::test]
    async fn registry_replace_by_name() {
        let registry = Registry::new();
        registry.register(Stub::new("a"));
        registry.register(Stub::new("a"));
        let names = registry.names();
        assert_eq!(names.len(), 1, "dedup failed: {names:?}");
    }

    #[tokio::test]
    async fn start_order_and_reverse_stop_order() {
        let log = Arc::new(StdMutex::new(Vec::new()));
        let registry = Registry::new();
        registry.register(Stub::logged("a", &log));
        registry.register(Stub::logged("b", &log));
        registry.register(Stub::logged("c", &log));

        registry.start_all().await.expect("start_all");
        registry.stop_all().await.expect("stop_all");

        let events = log.lock().unwrap().clone();
        assert_eq!(
            events,
            vec!["start:a", "start:b", "start:c", "stop:c", "stop:b", "stop:a"],
        );
    }

    #[tokio::test]
    async fn start_failure_error_message_matches_go_wrapping() {
        let registry = Registry::new();
        registry.register(Stub::failing_start("b", "boom"));

        let err = registry.start_all().await.expect_err("expected failure");
        assert_eq!(err.to_string(), "plugin \"b\" start: boom");
        assert!(matches!(err, PluginError::Start { .. }));
        assert_eq!(err.source().expect("source").to_string(), "boom");
    }

    #[tokio::test]
    async fn stop_all_joins_errors_in_reverse_order() {
        let registry = Registry::new();
        registry.register(Stub::failing_stop("a", "ouch-a"));
        registry.register(Stub::failing_stop("b", "ouch-b"));

        registry.start_all().await.expect("start_all");
        let err = registry.stop_all().await.expect_err("expected failure");
        assert_eq!(
            err.to_string(),
            "plugin \"b\" stop: ouch-b\nplugin \"a\" stop: ouch-a",
        );
        assert!(matches!(&err, PluginError::Aggregate(errs) if errs.len() == 2));
    }

    #[tokio::test]
    async fn single_stop_failure_is_not_aggregated() {
        let registry = Registry::new();
        registry.register(Stub::new("a"));
        registry.register(Stub::failing_stop("b", "ouch"));

        registry.start_all().await.expect("start_all");
        let err = registry.stop_all().await.expect_err("expected failure");
        assert!(matches!(err, PluginError::Stop { .. }));
        assert_eq!(err.to_string(), "plugin \"b\" stop: ouch");
    }

    #[tokio::test]
    async fn rollback_failure_joins_start_and_stop_errors() {
        let registry = Registry::new();
        registry.register(Stub::failing_stop("a", "ouch"));
        registry.register(Stub::failing_start("b", "boom"));

        let err = registry.start_all().await.expect_err("expected failure");
        assert_eq!(
            err.to_string(),
            "plugin \"b\" start: boom\nplugin \"a\" stop: ouch",
        );
        assert!(matches!(&err, PluginError::Aggregate(errs) if errs.len() == 2));
    }

    #[tokio::test]
    async fn stop_all_without_start_is_noop() {
        let registry = Registry::new();
        let a = Stub::new("a");
        registry.register(a.clone());

        registry.stop_all().await.expect("stop_all");
        assert_eq!(a.stop_calls(), 0, "unstarted plugin must not be stopped");
    }

    #[tokio::test]
    async fn stop_all_clears_started_list() {
        let registry = Registry::new();
        let a = Stub::new("a");
        registry.register(a.clone());

        registry.start_all().await.expect("start_all");
        registry.stop_all().await.expect("first stop_all");
        registry.stop_all().await.expect("second stop_all");
        assert_eq!(a.stop_calls(), 1, "second stop_all must be a no-op");
    }

    #[tokio::test]
    async fn start_all_can_run_again_after_stop_all() {
        let registry = Registry::new();
        let a = Stub::new("a");
        registry.register(a.clone());

        registry.start_all().await.expect("first start_all");
        registry.stop_all().await.expect("stop_all");
        registry.start_all().await.expect("second start_all");
        assert_eq!(a.start_calls(), 2);
    }

    #[tokio::test]
    async fn replace_by_name_keeps_position_and_uses_latest() {
        let registry = Registry::new();
        let first = Stub::new("a");
        let b = Stub::new("b");
        let replacement = Stub::new("a");
        registry.register(first.clone());
        registry.register(b.clone());
        registry.register(replacement.clone());

        assert_eq!(registry.names(), vec!["a", "b"]);

        registry.start_all().await.expect("start_all");
        assert!(!first.started(), "replaced plugin must not start");
        assert!(replacement.started() && b.started());
    }

    #[tokio::test]
    async fn names_returns_registration_order() {
        let registry = Registry::new();
        registry.register(Stub::new("c"));
        registry.register(Stub::new("a"));
        registry.register(Stub::new("b"));
        assert_eq!(registry.names(), vec!["c", "a", "b"]);
    }

    #[test]
    fn registry_default_is_empty() {
        let registry = Registry::default();
        assert!(registry.names().is_empty());
        assert_eq!(format!("{registry:?}"), "Registry { plugins: [] }");
    }

    #[test]
    fn send_sync_bounds() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Registry>();
        assert_send_sync::<PluginError>();
        assert_send_sync::<Arc<dyn Plugin>>();
    }
}
