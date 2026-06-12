//! # firefly-starter-application
//!
//! [`Core`] + plugins [`Registry`] — the port of the Go
//! `starterapplication` module (Java original:
//! `firefly-starter-application`, .NET:
//! `FireflyFramework.Starter.Application`).
//!
//! [`Application::new`] composes [`firefly_starter_core`] with a
//! pre-wired plugin [`Registry`] — the canonical application-tier
//! wiring used by services that load plugins (IdP, orchestration,
//! rule-engine integrations) at startup.
//!
//! The starter name defaults to `"starter-application"` (overriding
//! the [`Core`] default `"starter-core"`) so the actuator `/info` and
//! the startup banner correctly identify the tier; any other starter
//! name set explicitly in the config is preserved.
//!
//! [`Application`] derefs to [`Core`] — the Rust spelling of Go's
//! struct embedding — so every core field (`bus`, `cache`, `broker`,
//! `health`, …) and convenience method ([`Core::apply_middleware`],
//! [`Core::actuator_router`], [`Core::new_application`],
//! [`Core::print_banner`]) is reachable directly on the application.
//! That includes the pyfly-parity batteries on [`CoreConfig`] (CORS,
//! security headers, CSRF, request-log, request-metrics, http-exchanges,
//! loggers, redaction — all OFF by default) and the admin-dashboard
//! accessors ([`Core::cqrs_bus`], [`Core::scheduler`],
//! [`Core::http_exchanges`], …).
//!
//! ## Quick start
//!
//! ```no_run
//! use firefly_starter_application::{Application, CoreConfig};
//!
//! # struct IdpPlugin;
//! # #[async_trait::async_trait]
//! # impl firefly_starter_application::Plugin for IdpPlugin {
//! #     fn name(&self) -> &str { "idp" }
//! #     async fn start(&self) -> Result<(), firefly_starter_application::BoxError> { Ok(()) }
//! #     async fn stop(&self) -> Result<(), firefly_starter_application::BoxError> { Ok(()) }
//! # }
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let app = Application::new(CoreConfig {
//!     app_name: "approval-engine".into(),
//!     ..CoreConfig::default()
//! });
//!
//! app.plugins.register(std::sync::Arc::new(IdpPlugin));
//!
//! app.plugins.start_all().await?;
//! // ... application runs ...
//! app.plugins.stop_all().await?;
//! # Ok::<(), firefly_starter_application::PluginError>(())
//! # }).unwrap();
//! ```

#![warn(missing_docs)]

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub use firefly_plugins::{BoxError, Plugin, PluginError, Registry};
pub use firefly_starter_core::{Core, CoreConfig};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = firefly_starter_core::VERSION;

/// [`Core`] + [`Registry`] — the Rust spelling of Go's
/// `starterapplication.Application` (which embeds `*startercore.Core`
/// and carries `Plugins *plugins.Registry`).
pub struct Application {
    /// The wired infrastructure core — also reachable through deref,
    /// mirroring Go's embedded-field promotion.
    pub core: Core,
    /// The pre-wired plugin registry — Go's `Plugins *plugins.Registry`.
    pub plugins: Arc<Registry>,
}

impl Application {
    /// Wires the application starter — Go's `starterapplication.New(cfg)`.
    ///
    /// Identical to [`Core::new`] except the `"starter-core"` default
    /// starter name becomes `"starter-application"`; any other starter
    /// name configured explicitly is preserved.
    pub fn new(cfg: CoreConfig) -> Self {
        let mut core = Core::new(cfg);
        if core.starter_name == "starter-core" {
            core.starter_name = "starter-application".to_string();
        }
        Application {
            core,
            plugins: Arc::new(Registry::new()),
        }
    }
}

impl Deref for Application {
    type Target = Core;

    fn deref(&self) -> &Core {
        &self.core
    }
}

impl DerefMut for Application {
    fn deref_mut(&mut self) -> &mut Core {
        &mut self.core
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use firefly_cqrs::{CqrsError, Message};
    use serde::Serialize;

    use super::*;

    /// Minimal lifecycle stub mirroring the plugins crate test doubles.
    #[derive(Default)]
    struct StubPlugin {
        started: AtomicBool,
        stopped: AtomicBool,
    }

    #[async_trait::async_trait]
    impl Plugin for StubPlugin {
        fn name(&self) -> &str {
            "stub"
        }

        async fn start(&self) -> Result<(), BoxError> {
            self.started.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn stop(&self) -> Result<(), BoxError> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Clone, Serialize)]
    struct CreateOrder {
        id: String,
    }

    impl Message for CreateOrder {}

    #[derive(Clone, Debug, PartialEq)]
    struct OrderCreated {
        id: String,
    }

    /// Go: TestApplicationWiring.
    #[tokio::test]
    async fn application_wiring() {
        let a = Application::new(CoreConfig {
            app_name: "svc".into(),
            ..CoreConfig::default()
        });

        // Plugin registry wired (Go: `a.Plugins == nil` check) — empty
        // and immediately usable.
        assert!(a.plugins.names().is_empty(), "plugin registry not wired");

        // Core dependencies wired (Go: `a.Bus == nil || a.Cache == nil`)
        // — reachable through deref like Go's embedded promotion, and
        // live: the bus dispatches a registered command.
        assert_eq!(a.cache.name(), "memory");
        a.bus.register(|cmd: CreateOrder| async move {
            Ok::<_, CqrsError>(OrderCreated { id: cmd.id })
        });
        let created: OrderCreated = a.bus.send(CreateOrder { id: "o1".into() }).await.unwrap();
        assert_eq!(created, OrderCreated { id: "o1".into() });

        assert_eq!(a.starter_name, "starter-application");

        // Plugin registry start/stop happy path.
        a.plugins.start_all().await.expect("start_all");
        a.plugins.stop_all().await.expect("stop_all");
    }

    /// New() falls back to the canonical defaults, with the application
    /// tier's starter name in place of the core default.
    #[test]
    fn defaults_fall_back_to_canonical_names() {
        let a = Application::new(CoreConfig::default());
        assert_eq!(a.app_name, "firefly-app");
        assert_eq!(a.starter_name, "starter-application");
        assert_eq!(a.log.service, "firefly-app");
    }

    /// An explicitly configured starter name survives New(), exactly
    /// like Go's `if c.StarterName == "starter-core"` guard.
    #[test]
    fn explicit_starter_name_preserved() {
        let a = Application::new(CoreConfig {
            starter_name: "starter-custom".into(),
            ..CoreConfig::default()
        });
        assert_eq!(a.starter_name, "starter-custom");
    }

    /// The banner identifies the application tier, like Go's startup
    /// banner driven by `StarterName`.
    #[test]
    fn banner_identifies_application_tier() {
        let a = Application::new(CoreConfig {
            app_name: "svc".into(),
            ..CoreConfig::default()
        });
        let banner = a.banner();
        assert!(banner.contains("starter-application"), "banner: {banner}");
        assert!(banner.contains("svc"), "banner: {banner}");
    }

    /// A plugin registered on the wired registry starts and stops.
    #[tokio::test]
    async fn registered_plugin_roundtrip() {
        let a = Application::new(CoreConfig::default());
        let plugin = Arc::new(StubPlugin::default());
        a.plugins.register(plugin.clone());
        assert_eq!(a.plugins.names(), vec!["stub"]);

        a.plugins.start_all().await.expect("start_all");
        assert!(plugin.started.load(Ordering::SeqCst), "plugin not started");

        a.plugins.stop_all().await.expect("stop_all");
        assert!(plugin.stopped.load(Ordering::SeqCst), "plugin not stopped");
    }

    /// Deref and DerefMut reach the embedded core, mirroring Go's
    /// embedded-field promotion (reads and writes).
    #[test]
    fn deref_reaches_embedded_core() {
        let mut a = Application::new(CoreConfig {
            app_name: "svc".into(),
            ..CoreConfig::default()
        });
        assert_eq!(a.new_application().name(), "svc"); // method via Deref
        a.starter_name = "renamed".into(); // field via DerefMut
        assert_eq!(a.core.starter_name, "renamed");
    }

    #[test]
    fn version_matches_workspace() {
        assert_eq!(VERSION, firefly_starter_core::VERSION);
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn application_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Application>();
    }
}
