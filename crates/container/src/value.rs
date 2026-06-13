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

//! `#[firefly(value = "${key:default}")]` config-field injection.
//!
//! Ports the resolution half of pyfly's `core.value.Value` descriptor and
//! Spring's `@Value`. A `${key}` / `${key:default}` placeholder is resolved
//! against the container's installed [`ConditionContext`](crate::ConditionContext)
//! (which carries the flattened config map), then parsed into the field type
//! via [`std::str::FromStr`].
//!
//! The `#{...}` SpEL expression form is intentionally out of scope for the
//! typed-Rust idiom — a config value that needs arithmetic is computed in Rust.
//! Only the `${...}` placeholder grammar (with `:default`) is supported, which
//! is the form Firefly config uses everywhere.

use std::str::FromStr;

use crate::{Container, ContainerError};

/// Resolve a `#[firefly(value = "...")]` expression against the container's
/// config and parse it into `T`.
///
/// Expression forms:
/// - `"${key}"` — resolve from config; error if missing and no default.
/// - `"${key:default}"` — resolve from config, falling back to `default`.
/// - `"literal"` — used verbatim (no `${}` wrapper).
///
/// # Errors
/// Returns [`ContainerError::NoSuchBean`] when a required key is missing, or
/// when the resolved string cannot be parsed into `T`.
pub fn resolve_value<T>(container: &Container, expr: &str) -> Result<T, ContainerError>
where
    T: FromStr,
    <T as FromStr>::Err: std::fmt::Display,
{
    let ctx = container.condition_context();
    let raw = resolve_placeholder(expr, |key| ctx.property(key).map(str::to_string))?;
    raw.parse::<T>().map_err(|e| ContainerError::NoSuchBean {
        bean_type: Some(std::any::type_name::<T>().to_string()),
        bean_name: None,
        required_by: None,
        parameter: Some(format!(
            "#[firefly(value = {expr:?})] resolved to {raw:?}, which failed to parse as {}: {e}",
            std::any::type_name::<T>()
        )),
        suggestions: Vec::new(),
    })
}

/// Resolve a single `${key:default}` placeholder (or a bare literal) using a
/// lookup closure. Shared so both the value injector and tests use one grammar.
fn resolve_placeholder(
    expr: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String, ContainerError> {
    let trimmed = expr.trim();
    let inner = match trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        Some(inner) => inner,
        // No placeholder wrapper → literal value.
        None => return Ok(trimmed.to_string()),
    };

    if let Some((key, default)) = inner.split_once(':') {
        Ok(lookup(key.trim()).unwrap_or_else(|| default.to_string()))
    } else {
        let key = inner.trim();
        lookup(key).ok_or_else(|| ContainerError::NoSuchBean {
            bean_type: None,
            bean_name: Some(key.to_string()),
            required_by: None,
            parameter: Some(format!(
                "config key {key:?} not found and no default provided in \
                 #[firefly(value = {expr:?})]"
            )),
            suggestions: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConditionContext;

    #[test]
    fn resolves_present_key() {
        let c = Container::new();
        c.set_condition_context(ConditionContext::new().with_property("app.port", "8080"));
        let port: u16 = resolve_value(&c, "${app.port}").unwrap();
        assert_eq!(port, 8080);
    }

    #[test]
    fn falls_back_to_default() {
        let c = Container::new();
        let timeout: u64 = resolve_value(&c, "${app.timeout:30}").unwrap();
        assert_eq!(timeout, 30);
    }

    #[test]
    fn literal_passes_through() {
        let c = Container::new();
        let s: String = resolve_value(&c, "hello").unwrap();
        assert_eq!(s, "hello");
    }

    #[test]
    fn missing_required_key_errors() {
        let c = Container::new();
        let r: Result<u16, _> = resolve_value(&c, "${missing.key}");
        assert!(r.is_err());
    }

    #[test]
    fn unparseable_value_errors() {
        let c = Container::new();
        c.set_condition_context(ConditionContext::new().with_property("n", "not-a-number"));
        let r: Result<u32, _> = resolve_value(&c, "${n}");
        assert!(r.is_err());
    }
}
