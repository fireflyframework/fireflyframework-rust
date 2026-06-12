//! Profile selection: `FIREFLY_PROFILE` and the canonical
//! `application.yaml` / `application-<profile>.yaml` source chain.

use std::path::Path;

use serde::de::DeserializeOwned;

use crate::binder::load;
use crate::error::ConfigError;
use crate::source::{from_env, Source};
use crate::yaml::from_optional_yaml;

/// Reads the currently active configuration profile from the
/// `FIREFLY_PROFILE` environment variable, falling back to `fallback`.
///
/// Profile names are case-insensitive (the value is trimmed and
/// lower-cased); the canonical set across the platform is: `dev`, `test`,
/// `staging`, `prod`.
pub fn active_profile(fallback: &str) -> String {
    let value = std::env::var("FIREFLY_PROFILE").unwrap_or_default();
    let value = value.trim().to_lowercase();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

/// Returns the canonical set of YAML sources for an application named
/// `app_name` under `dir`, picking up the base file and the
/// profile-specific override:
///
/// ```text
/// dir/application.yaml           (always loaded if present)
/// dir/application-{profile}.yaml (loaded after base, overrides)
/// ```
///
/// Both files are tolerated absent — services that hard-code their
/// configuration in Rust can omit YAML entirely. An empty `app_name`
/// defaults to `"application"`.
pub fn profile_sources(
    dir: impl AsRef<Path>,
    app_name: &str,
    profile: &str,
) -> Vec<Box<dyn Source>> {
    let app = if app_name.is_empty() {
        "application"
    } else {
        app_name
    };
    let dir = dir.as_ref();
    let base = dir.join(format!("{app}.yaml"));
    let prof = dir.join(format!("{app}-{profile}.yaml"));
    vec![
        Box::new(from_optional_yaml(base)),
        Box::new(from_optional_yaml(prof)),
    ]
}

/// Convenience composition of [`active_profile`] and [`profile_sources`]
/// plus a final `FIREFLY_*` environment layer — the most common
/// application bootstrap shape.
pub fn load_from_profile<T: DeserializeOwned>(
    dir: impl AsRef<Path>,
    app_name: &str,
    fallback_profile: &str,
) -> Result<T, ConfigError> {
    let profile = active_profile(fallback_profile);
    let mut sources = profile_sources(dir, app_name, &profile);
    sources.push(Box::new(from_env("FIREFLY")));
    load(&sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_sources_defaults_app_name_and_orders_base_first() {
        let sources = profile_sources("/etc/firefly", "", "dev");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].name(), "yaml(/etc/firefly/application.yaml)");
        assert_eq!(sources[1].name(), "yaml(/etc/firefly/application-dev.yaml)");
    }

    #[test]
    fn profile_sources_uses_given_app_name() {
        let sources = profile_sources("/etc/orders", "orders", "prod");
        assert_eq!(sources[0].name(), "yaml(/etc/orders/orders.yaml)");
        assert_eq!(sources[1].name(), "yaml(/etc/orders/orders-prod.yaml)");
    }
}
