// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The local config-introspection bridge for `/actuator/env`.
//!
//! Spring Boot's `/actuator/env` exposes the *ordered property sources* that
//! produced the effective configuration — `{activeProfiles, propertySources:
//! [{name, properties: {key: {value, origin}}}]}` — with sensitive values
//! masked. The capability to render that view lives in `firefly-config`
//! ([`Layered::property_sources`]), but the actuator crate must stay
//! decoupled from any concrete config crate (it is wired into many
//! deployments, some without `firefly-config`).
//!
//! This module defines a tiny local [`EnvSource`] trait the starter
//! implements over `firefly-config` and injects via
//! [`ActuatorConfig::env_source`](crate::ActuatorConfig::env_source). When an
//! [`EnvSource`] is present the `/actuator/env` handler renders the full
//! Spring shape; when it is absent the handler falls back to the legacy flat
//! redacted process-environment map, so existing deployments are unaffected.
//!
//! The [`PropertyView`] / [`PropertySourceView`] structs are byte-identical on
//! the wire to `firefly-config`'s own views (`{value, origin}` /
//! `{name, properties}`) and to pyfly's `Config.property_sources()`, so log
//! and config tooling parses every port unchanged.

use std::collections::BTreeMap;

use serde::Serialize;

/// One masked property inside a [`PropertySourceView`]: the (already
/// sanitized) value plus the name of the source it originated from. Mirrors
/// `firefly_config::PropertyView` and pyfly's per-property `{value, origin}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropertyView {
    /// The property value, already masked by the [`EnvSource`] implementer.
    pub value: String,
    /// Origin attribution — the source's name (`systemEnvironment`,
    /// `yaml(<path>)`, …).
    pub origin: String,
}

/// One ordered property source, Spring Boot `/actuator/env` style:
/// `{"name": …, "properties": {key: {"value": …, "origin": …}}}`. Mirrors
/// `firefly_config::PropertySourceView`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropertySourceView {
    /// Source name (`systemEnvironment`, `yaml(<path>)`, `flags`, …).
    pub name: String,
    /// Masked properties keyed by dotted path (sorted for stable output).
    pub properties: BTreeMap<String, PropertyView>,
}

/// The local bridge between `/actuator/env` and the application's
/// configuration layer — the actuator-side counterpart of pyfly's
/// `EnvEndpoint` reading `context.environment` + `config.property_sources()`.
///
/// A starter implements this over `firefly-config`
/// ([`Layered::property_sources`] + `active_profiles`) and injects it via
/// [`ActuatorConfig::env_source`](crate::ActuatorConfig::env_source). Keeping
/// the trait local means the actuator crate never takes a hard dependency on
/// any config crate.
///
/// ```
/// use std::collections::BTreeMap;
/// use firefly_actuator::{EnvSource, PropertySourceView, PropertyView};
///
/// struct StaticEnv;
/// impl EnvSource for StaticEnv {
///     fn active_profiles(&self) -> Vec<String> {
///         vec!["dev".into(), "test".into()]
///     }
///     fn property_sources(&self) -> Vec<PropertySourceView> {
///         vec![PropertySourceView {
///             name: "applicationConfig".into(),
///             properties: BTreeMap::from([(
///                 "app.name".into(),
///                 PropertyView { value: "orders".into(), origin: "applicationConfig".into() },
///             )]),
///         }]
///     }
/// }
/// let env = StaticEnv;
/// assert_eq!(env.active_profiles(), vec!["dev", "test"]);
/// assert_eq!(env.property_sources().len(), 1);
/// ```
pub trait EnvSource: Send + Sync {
    /// The active configuration profiles, highest precedence first — pyfly's
    /// `environment.active_profiles`. Empty when no profile is set.
    fn active_profiles(&self) -> Vec<String>;

    /// The ordered, masked property sources — **highest precedence first** —
    /// exactly the list `/actuator/env` renders under `propertySources`.
    /// Implementers must mask sensitive values before returning.
    fn property_sources(&self) -> Vec<PropertySourceView>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_serialize_with_spring_field_names() {
        let view = PropertySourceView {
            name: "x".into(),
            properties: BTreeMap::from([(
                "app.name".into(),
                PropertyView {
                    value: "svc".into(),
                    origin: "x".into(),
                },
            )]),
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "x",
                "properties": {"app.name": {"value": "svc", "origin": "x"}}
            })
        );
    }
}
