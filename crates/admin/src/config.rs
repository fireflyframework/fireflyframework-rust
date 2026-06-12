//! Admin dashboard configuration — the Rust rendering of pyfly's
//! `AdminProperties` / `AdminServerProperties` / `AdminClientProperties`
//! (`pyfly.admin.*`).
//!
//! Every field is `serde`-deserializable so the structs can be bound from a
//! `firefly-config` document under the `firefly.admin` prefix; the `Default`
//! impls reproduce pyfly's defaults exactly.

use serde::{Deserialize, Serialize};

/// Configuration for the admin dashboard itself (`firefly.admin.*`) — the
/// Rust counterpart of pyfly's `AdminProperties`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminConfig {
    /// Whether the dashboard is mounted at all. The starter consults this
    /// before calling [`mount`](crate::mount); the router itself is always
    /// built when `mount` is invoked.
    pub enabled: bool,
    /// Base path the SPA + `/api` routes are mounted under (default `/admin`).
    /// Trailing slashes are stripped when routes are built.
    pub path: String,
    /// Window title rendered by the SPA and returned from `/admin/api/settings`.
    pub title: String,
    /// Initial theme hint (`auto` / `dark` / `light`) surfaced to the SPA.
    pub theme: String,
    /// When set, every `/admin/api/*` route is guarded: a caller must present
    /// an authenticated [`Authentication`](firefly_security::Authentication)
    /// carrying one of [`allowed_roles`](Self::allowed_roles).
    pub require_auth: bool,
    /// Roles permitted through the auth guard when
    /// [`require_auth`](Self::require_auth) is on.
    pub allowed_roles: Vec<String>,
    /// SSE / client refresh cadence in milliseconds, surfaced to the SPA and
    /// used as the tick interval of the live streams.
    pub refresh_interval: u64,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "/admin".to_string(),
            title: "Firefly Admin".to_string(),
            theme: "auto".to_string(),
            require_auth: false,
            allowed_roles: vec!["ADMIN".to_string()],
            refresh_interval: 5000,
        }
    }
}

impl AdminConfig {
    /// The mount path with any trailing slash removed (pyfly's
    /// `path.rstrip("/")`). An empty or `/`-only path normalises to `""`,
    /// mounting the dashboard at the router root.
    pub fn base_path(&self) -> String {
        let trimmed = self.path.trim_end_matches('/');
        trimmed.to_string()
    }

    /// The `<base href>` value injected into `index.html` so the SPA's
    /// relative asset URLs resolve regardless of a trailing slash —
    /// the base path plus a single trailing slash.
    pub fn base_href(&self) -> String {
        format!("{}/", self.base_path())
    }
}

/// One statically-configured downstream instance (server mode) — pyfly's
/// `instances[]` entry. `name` + `url` are required; `metadata` is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// Unique instance name (the registry key).
    pub name: String,
    /// Base URL of the instance's admin/actuator endpoints.
    pub url: String,
    /// Free-form metadata propagated into the registry entry.
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Configuration for admin **server mode** (`firefly.admin.server.*`) — the
/// Rust counterpart of pyfly's `AdminServerProperties`. When `enabled`, the
/// router wires an [`InstanceRegistry`](crate::InstanceRegistry) seeded from
/// [`instances`](Self::instances) and exposes the `/admin/api/instances`
/// register / deregister routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminServerConfig {
    /// Whether server mode (the instance registry + routes) is active.
    pub enabled: bool,
    /// Health-poll cadence in milliseconds for registered instances.
    pub poll_interval: u64,
    /// Connect timeout in milliseconds when polling instances.
    pub connect_timeout: u64,
    /// Read timeout in milliseconds when polling instances.
    pub read_timeout: u64,
    /// Statically-discovered instances seeded into the registry at startup.
    pub instances: Vec<InstanceConfig>,
}

impl Default for AdminServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval: 10000,
            connect_timeout: 2000,
            read_timeout: 5000,
            instances: Vec::new(),
        }
    }
}

/// Configuration for admin **client mode** (`firefly.admin.client.*`) — the
/// Rust counterpart of pyfly's `AdminClientProperties`. When `auto_register`
/// is on, the application self-registers with the admin server at `url` on
/// lifecycle start and deregisters on stop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminClientConfig {
    /// Base URL of the remote admin server to register with.
    pub url: String,
    /// Whether to self-register/deregister on lifecycle start/stop.
    pub auto_register: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_pyfly() {
        let cfg = AdminConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.path, "/admin");
        assert_eq!(cfg.theme, "auto");
        assert!(!cfg.require_auth);
        assert_eq!(cfg.allowed_roles, vec!["ADMIN".to_string()]);
        assert_eq!(cfg.refresh_interval, 5000);
    }

    #[test]
    fn base_path_strips_trailing_slash() {
        let cfg = AdminConfig {
            path: "/admin/".to_string(),
            ..AdminConfig::default()
        };
        assert_eq!(cfg.base_path(), "/admin");
        assert_eq!(cfg.base_href(), "/admin/");
    }

    #[test]
    fn server_config_defaults() {
        let cfg = AdminServerConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.poll_interval, 10000);
        assert!(cfg.instances.is_empty());
    }

    #[test]
    fn deserializes_from_json() {
        let cfg: AdminConfig = serde_json::from_value(serde_json::json!({
            "path": "/ops",
            "require_auth": true,
            "allowed_roles": ["ADMIN", "SRE"],
        }))
        .unwrap();
        assert_eq!(cfg.path, "/ops");
        assert!(cfg.require_auth);
        assert_eq!(
            cfg.allowed_roles,
            vec!["ADMIN".to_string(), "SRE".to_string()]
        );
        // Untouched fields keep their defaults.
        assert_eq!(cfg.refresh_interval, 5000);
    }
}
