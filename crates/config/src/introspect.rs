//! Property-source introspection (pyfly `Config.property_sources()` /
//! Spring Boot `/actuator/env` parity).
//!
//! [`Layered::property_sources`] returns the ordered sources — highest
//! precedence first — with every value masked via [`crate::mask`] and
//! attributed to its origin. A synthetic `systemEnvironment` source
//! listing the `FIREFLY_*` process environment leads the list (environment
//! overrides outrank every file source), mirroring Spring's ordering.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::ConfigError;
use crate::mask::mask_value;
use crate::source::Layered;

/// Origin string used for entries of the synthetic `systemEnvironment`
/// property source.
pub const SYSTEM_ENVIRONMENT_ORIGIN: &str = "System Environment Property";

/// Name of the synthetic environment property source.
pub const SYSTEM_ENVIRONMENT_SOURCE: &str = "systemEnvironment";

/// One masked property inside a [`PropertySourceView`]: the (sanitized)
/// value plus the name of the source it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropertyView {
    /// The property value, masked via [`crate::mask::mask_value`].
    pub value: String,
    /// Origin attribution (the source's name, or
    /// [`SYSTEM_ENVIRONMENT_ORIGIN`] for environment entries).
    pub origin: String,
}

/// One ordered property source, Spring Boot `/actuator/env` style:
/// `{"name": ..., "properties": {key: {"value": ..., "origin": ...}}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropertySourceView {
    /// Source name (`yaml(<path>)`, `flags`, `systemEnvironment`, …).
    pub name: String,
    /// Masked properties keyed by dotted path (sorted for stable output).
    pub properties: BTreeMap<String, PropertyView>,
}

impl Layered {
    /// Returns the ordered property sources — **highest precedence first**
    /// — with sensitive values masked (pyfly `Config.property_sources()`).
    ///
    /// The list opens with a synthetic [`SYSTEM_ENVIRONMENT_SOURCE`] entry
    /// containing every `FIREFLY_*` process environment variable (omitted
    /// when none are set), followed by this chain's sources in *reverse*
    /// merge order, each reporting its raw keys with `origin` set to the
    /// source's name. Source failures short-circuit, wrapped with the
    /// failing source's name like [`Layered::map`].
    pub fn property_sources(&self) -> Result<Vec<PropertySourceView>, ConfigError> {
        let mut views = Vec::with_capacity(self.sources().len() + 1);

        // 1. systemEnvironment — every FIREFLY_* override, highest precedence.
        let mut env_props = BTreeMap::new();
        for (name, value) in std::env::vars() {
            if name.starts_with("FIREFLY_") {
                env_props.insert(
                    name.clone(),
                    PropertyView {
                        value: mask_value(&name, &value),
                        origin: SYSTEM_ENVIRONMENT_ORIGIN.to_string(),
                    },
                );
            }
        }
        if !env_props.is_empty() {
            views.push(PropertySourceView {
                name: SYSTEM_ENVIRONMENT_SOURCE.to_string(),
                properties: env_props,
            });
        }

        // 2. Chain sources — last loaded wins, so reverse to list highest
        //    precedence first (matching Spring's ordering).
        for source in self.sources().iter().rev() {
            let name = source.name();
            let entries = source.load().map_err(|err| ConfigError::Source {
                name: name.clone(),
                source: Box::new(err),
            })?;
            let properties = entries
                .into_iter()
                .map(|(key, value)| {
                    let masked = mask_value(&key, &value);
                    (
                        key,
                        PropertyView {
                            value: masked,
                            origin: name.clone(),
                        },
                    )
                })
                .collect();
            views.push(PropertySourceView { name, properties });
        }
        Ok(views)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::mask::MASK;
    use crate::source::{Source, StaticSource};

    fn entries(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // pyfly: test_property_sources_attribute_value_and_origin
    #[test]
    fn reports_value_and_origin_per_source() {
        let layered = Layered::new(vec![Box::new(StaticSource::new(
            "applicationConfig",
            entries(&[("app.name", "filename")]),
        ))]);
        let sources = layered.property_sources().unwrap();
        let flat: BTreeMap<&str, &PropertyView> = sources
            .iter()
            .flat_map(|s| s.properties.iter().map(|(k, v)| (k.as_str(), v)))
            .collect();
        assert_eq!(flat["app.name"].value, "filename");
        assert_eq!(flat["app.name"].origin, "applicationConfig");
    }

    // pyfly ordering: later-merged sources outrank earlier ones, so the
    // view lists them in reverse merge order.
    #[test]
    fn sources_listed_highest_precedence_first() {
        let layered = Layered::new(vec![
            Box::new(StaticSource::new("defaults", entries(&[("a", "1")]))),
            Box::new(StaticSource::new("overrides", entries(&[("a", "2")]))),
        ]);
        let sources = layered.property_sources().unwrap();
        let names: Vec<&str> = sources
            .iter()
            .map(|s| s.name.as_str())
            .filter(|n| *n != SYSTEM_ENVIRONMENT_SOURCE)
            .collect();
        assert_eq!(names, vec!["overrides", "defaults"]);
    }

    #[test]
    fn masks_sensitive_values_in_views() {
        let layered = Layered::new(vec![Box::new(StaticSource::new(
            "secrets",
            entries(&[
                ("db.password", "hunter2"),
                ("db.url", "postgresql://user:hunter2@localhost/db"),
            ]),
        ))]);
        let sources = layered.property_sources().unwrap();
        let view = sources
            .iter()
            .find(|s| s.name == "secrets")
            .expect("secrets source");
        assert_eq!(view.properties["db.password"].value, MASK);
        assert_eq!(
            view.properties["db.url"].value,
            "postgresql://user:******@localhost/db"
        );
    }

    #[test]
    fn failing_source_is_wrapped_with_name() {
        struct Failing;
        impl Source for Failing {
            fn name(&self) -> String {
                "boom".to_string()
            }
            fn load(&self) -> Result<HashMap<String, String>, ConfigError> {
                Err(ConfigError::Message("nope".to_string()))
            }
        }
        let layered = Layered::new(vec![Box::new(Failing)]);
        let err = layered.property_sources().unwrap_err();
        assert!(err.to_string().contains("\"boom\""), "got: {err}");
    }

    #[test]
    fn views_serialize_with_spring_field_names() {
        let view = PropertySourceView {
            name: "x".to_string(),
            properties: BTreeMap::from([(
                "app.name".to_string(),
                PropertyView {
                    value: "svc".to_string(),
                    origin: "x".to_string(),
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
