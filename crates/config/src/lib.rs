//! `firefly-config` — Spring Boot–style **typed, layered configuration
//! binding** for Rust.
//!
//! Application authors declare a `serde`-deserializable struct and call
//! [`load`]; the loader merges the sources in precedence order, resolves
//! the active profile, and binds the flat dot-keyed map onto the struct.
//! This is the Rust port of the Go module `config` (Java original:
//! Spring Boot `@ConfigurationProperties`).
//!
//! # Source precedence
//!
//! [`Layered::new(s1, s2, ...)`](Layered::new) merges from left to right —
//! **last write wins**. The canonical chain is:
//!
//! 1. **Defaults** ([`StaticSource`])
//! 2. **Base YAML** ([`from_optional_yaml("application.yaml")`](from_optional_yaml))
//! 3. **Profile YAML** ([`from_optional_yaml("application-prod.yaml")`](from_optional_yaml))
//! 4. **Environment** ([`from_env("FIREFLY")`](from_env) — `FIREFLY_WEB_PORT` → `web.port`)
//! 5. **CLI flags** ([`FlagSource::new()`](FlagSource::new) — `flags.set("web.port", "9090")`)
//!
//! So an environment override always beats a YAML file, and a CLI
//! override always beats both.
//!
//! # Profile selection
//!
//! `FIREFLY_PROFILE` selects the profile-specific YAML file. The
//! canonical helper [`load_from_profile`] reads `application.yaml`, then
//! `application-{FIREFLY_PROFILE,fallback}.yaml`, then `FIREFLY_*`
//! environment variables.
//!
//! # Example
//!
//! ```
//! use std::collections::HashMap;
//! use firefly_config::{load, FlagSource, Source, StaticSource};
//! use serde::Deserialize;
//!
//! #[derive(Debug, Deserialize)]
//! struct Web {
//!     port: u16,
//!     host: String,
//! }
//!
//! #[derive(Debug, Deserialize)]
//! struct AppCfg {
//!     web: Web,
//!     tags: Vec<String>,
//! }
//!
//! let defaults = StaticSource::new(
//!     "defaults",
//!     HashMap::from([
//!         ("web.port".to_string(), "8080".to_string()),
//!         ("web.host".to_string(), "0.0.0.0".to_string()),
//!         ("tags".to_string(), "alpha,beta".to_string()),
//!     ]),
//! );
//! let flags = FlagSource::new();
//! flags.set("web.port", "9090");
//!
//! let sources: Vec<Box<dyn Source>> = vec![Box::new(defaults), Box::new(flags)];
//! let cfg: AppCfg = load(&sources)?;
//! assert_eq!(cfg.web.port, 9090); // flags beat defaults
//! assert_eq!(cfg.tags, vec!["alpha", "beta"]);
//! # Ok::<(), firefly_config::ConfigError>(())
//! ```
//!
//! # pyfly parity layer
//!
//! On top of the Go-parity surface above, this crate ports pyfly's
//! configuration subsystem:
//!
//! - **`${...}` placeholders** — [`load`]/[`bind`] resolve `${key}`,
//!   `${key:default}` and `${ENV_VAR}` placeholders post-merge
//!   (environment beats config, depth-10 circular-reference guard); see
//!   [`resolve_placeholders`].
//! - **Relaxed keys** — merge and binder fold kebab-case to snake_case,
//!   so `graceful-timeout:` in YAML binds a `graceful_timeout` field.
//! - **Runtime reload** — [`ReloadableConfig`] replays the source chain
//!   on [`reload`](ReloadableConfig::reload) and reports changed top-level
//!   keys; the [`Refresher`] trait is the `/actuator/refresh` hook.
//! - **Introspection** — [`Layered::property_sources`] returns ordered,
//!   origin-attributed property sources with sensitive values masked via
//!   the [`mask`] module (Spring `Sanitizer` parity).
//! - **Multi-profile** — [`active_profiles`] reads a comma-separated
//!   `FIREFLY_PROFILE`, overlaid by [`multi_profile_sources`].
//! - **Remote config** — [`ConfigClient`] fetches a Spring-Cloud-Config
//!   `/{app}/{profile}/{label}` document and flattens it into a
//!   [`StaticSource`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod binder;
mod client;
mod error;
mod introspect;
pub mod mask;
mod placeholder;
mod profile;
mod reload;
mod source;
mod yaml;

pub use binder::{bind, load};
pub use client::ConfigClient;
pub use error::ConfigError;
pub use introspect::{
    PropertySourceView, PropertyView, SYSTEM_ENVIRONMENT_ORIGIN, SYSTEM_ENVIRONMENT_SOURCE,
};
pub use placeholder::resolve_placeholders;
pub use profile::{
    active_profile, active_profiles, load_from_profile, multi_profile_sources, profile_sources,
};
pub use reload::{Refresher, ReloadableConfig};
pub use source::{from_env, EnvSource, FlagSource, Layered, Source, StaticSource};
pub use yaml::{from_optional_yaml, from_yaml, YamlSource};
