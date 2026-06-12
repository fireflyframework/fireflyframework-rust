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

/// Reads the active configuration profiles from the **comma-separated**
/// `FIREFLY_PROFILE` environment variable (pyfly multi-profile parity).
///
/// Each entry is trimmed and lower-cased; empty entries are dropped. When
/// the variable is unset or blank, the list is `[fallback]`. Order is
/// preserved — later profiles overlay earlier ones in
/// [`multi_profile_sources`].
///
/// `FIREFLY_PROFILE=dev,cloud` → `["dev", "cloud"]`.
pub fn active_profiles(fallback: &str) -> Vec<String> {
    let value = std::env::var("FIREFLY_PROFILE").unwrap_or_default();
    let profiles: Vec<String> = value
        .split(',')
        .map(|profile| profile.trim().to_lowercase())
        .filter(|profile| !profile.is_empty())
        .collect();
    if profiles.is_empty() {
        vec![fallback.to_string()]
    } else {
        profiles
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

/// Multi-profile variant of [`profile_sources`]: the base file plus one
/// overlay **per profile, in order** (later profiles override earlier
/// ones), mirroring pyfly's profile-overlay loop:
///
/// ```text
/// dir/application.yaml            (always loaded if present)
/// dir/application-{p}.yaml        (one per profile, in list order)
/// ```
///
/// All files are tolerated absent. An empty `app_name` defaults to
/// `"application"`.
pub fn multi_profile_sources(
    dir: impl AsRef<Path>,
    app_name: &str,
    profiles: &[String],
) -> Vec<Box<dyn Source>> {
    let app = if app_name.is_empty() {
        "application"
    } else {
        app_name
    };
    let dir = dir.as_ref();
    let mut sources: Vec<Box<dyn Source>> = vec![Box::new(from_optional_yaml(
        dir.join(format!("{app}.yaml")),
    ))];
    for profile in profiles {
        sources.push(Box::new(from_optional_yaml(
            dir.join(format!("{app}-{profile}.yaml")),
        )));
    }
    sources
}

/// Convenience composition of [`active_profiles`] and
/// [`multi_profile_sources`] plus a final `FIREFLY_*` environment layer —
/// the most common application bootstrap shape.
///
/// With a single active profile this is exactly the historical behavior;
/// a comma-separated `FIREFLY_PROFILE` (`dev,cloud`) now overlays every
/// listed profile in order (pyfly multi-profile parity).
pub fn load_from_profile<T: DeserializeOwned>(
    dir: impl AsRef<Path>,
    app_name: &str,
    fallback_profile: &str,
) -> Result<T, ConfigError> {
    let profiles = active_profiles(fallback_profile);
    let mut sources = multi_profile_sources(dir, app_name, &profiles);
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

    #[test]
    fn multi_profile_sources_overlays_each_profile_in_order() {
        let profiles = vec!["dev".to_string(), "cloud".to_string()];
        let sources = multi_profile_sources("/etc/firefly", "", &profiles);
        assert_eq!(sources.len(), 3);
        assert_eq!(sources[0].name(), "yaml(/etc/firefly/application.yaml)");
        assert_eq!(sources[1].name(), "yaml(/etc/firefly/application-dev.yaml)");
        assert_eq!(
            sources[2].name(),
            "yaml(/etc/firefly/application-cloud.yaml)"
        );
    }
}
