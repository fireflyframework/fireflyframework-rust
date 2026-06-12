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

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod binder;
mod error;
mod profile;
mod source;
mod yaml;

pub use binder::{bind, load};
pub use error::ConfigError;
pub use profile::{active_profile, load_from_profile, profile_sources};
pub use source::{from_env, EnvSource, FlagSource, Layered, Source, StaticSource};
pub use yaml::{from_optional_yaml, from_yaml, YamlSource};
