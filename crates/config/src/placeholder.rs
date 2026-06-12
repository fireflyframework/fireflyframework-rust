//! `${...}` property-placeholder resolution (pyfly / Spring Boot parity).
//!
//! Runs as a **post-merge pass** over the flat dot-keyed map: every value
//! containing `${...}` is rewritten with each placeholder resolved, in
//! precedence order:
//!
//! 1. **Environment variable, literal name** — `${MY_SECRET}` reads the
//!    `MY_SECRET` environment variable as written;
//! 2. **Environment variable, relaxed `FIREFLY_*` mapping** — `${app.name}`
//!    also honors `FIREFLY_APP_NAME` (a leading `firefly.` segment is
//!    stripped first, dots and dashes map to underscores), so environment
//!    overrides beat config references;
//! 3. **Config reference** — `${app.name}` reads the merged map itself
//!    (relaxed: kebab-case and snake_case segments are interchangeable);
//!    referenced values are resolved recursively with a depth-10 guard
//!    against circular references;
//! 4. **Default** — `${key:default}` falls back to `default` when neither
//!    environment nor config can resolve `key`.
//!
//! An unresolvable placeholder without a default raises
//! [`ConfigError::Placeholder`], exactly like pyfly's `ValueError`.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::ConfigError;
use crate::source::normalize_key;

/// Maximum recursion depth while chasing placeholder references; matches
/// pyfly's guard (`_depth > 10` raises).
const MAX_DEPTH: usize = 10;

/// The `${...}` matcher: `\$\{([^}]+)\}`.
fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$\{([^}]+)\}").expect("placeholder regex"))
}

/// Resolves every `${key}` / `${key:default}` / `${ENV_VAR}` placeholder in
/// `flat`'s values, returning a new map (the input is not mutated).
///
/// Resolution precedence is environment-beats-config (see the module docs);
/// circular references fail after a depth-10 guard with
/// [`ConfigError::Placeholder`]. Values without `${` are copied verbatim.
///
/// [`load`](crate::load) and [`bind`](crate::bind) call this automatically,
/// so most applications never invoke it directly; it is public for callers
/// that work with [`Layered::map`](crate::Layered::map) output themselves.
pub fn resolve_placeholders(
    flat: &HashMap<String, String>,
) -> Result<HashMap<String, String>, ConfigError> {
    let mut out = HashMap::with_capacity(flat.len());
    for (key, value) in flat {
        let resolved = if value.contains("${") {
            resolve_value(value, flat, 0)?
        } else {
            value.clone()
        };
        out.insert(key.clone(), resolved);
    }
    Ok(out)
}

/// Resolves all placeholders inside a single value, recursing into
/// referenced values that themselves contain placeholders.
fn resolve_value(
    value: &str,
    flat: &HashMap<String, String>,
    depth: usize,
) -> Result<String, ConfigError> {
    if depth > MAX_DEPTH {
        return Err(ConfigError::Placeholder {
            placeholder: value.to_string(),
            message: "max recursion depth exceeded; check for circular references".to_string(),
        });
    }
    let re = placeholder_re();
    let mut out = String::with_capacity(value.len());
    let mut last = 0;
    for caps in re.captures_iter(value) {
        let whole = caps.get(0).expect("match group 0");
        let inner = caps.get(1).expect("match group 1").as_str();
        out.push_str(&value[last..whole.start()]);
        out.push_str(&resolve_one(inner, flat, depth)?);
        last = whole.end();
    }
    out.push_str(&value[last..]);
    Ok(out)
}

/// Resolves one placeholder body (`key` or `key:default`).
fn resolve_one(
    inner: &str,
    flat: &HashMap<String, String>,
    depth: usize,
) -> Result<String, ConfigError> {
    // `${key:default}` — split on the first `:` only, like pyfly.
    let (ref_key, default) = match inner.split_once(':') {
        Some((key, default)) => (key, Some(default)),
        None => (inner, None),
    };

    // 1. Environment variable, literal name as written.
    if let Ok(env_val) = std::env::var(ref_key) {
        return Ok(env_val);
    }
    // 2. Environment variable, relaxed FIREFLY_* mapping — env beats config.
    if let Ok(env_val) = std::env::var(env_key(ref_key)) {
        return Ok(env_val);
    }

    // 3. Config reference (relaxed segment matching), resolved recursively.
    if let Some(referenced) = flat.get(&normalize_key(ref_key)) {
        if referenced.contains("${") {
            return resolve_value(referenced, flat, depth + 1);
        }
        return Ok(referenced.clone());
    }

    // 4. `${key:default}` fallback.
    if let Some(default) = default {
        return Ok(default.to_string());
    }

    Err(ConfigError::Placeholder {
        placeholder: format!("${{{inner}}}"),
        message: "not found in environment or config".to_string(),
    })
}

/// Maps a dotted config key to its relaxed `FIREFLY_*` environment-variable
/// name: a leading `firefly.` segment is stripped, then the rest is
/// upper-cased with dots and dashes folded to underscores
/// (`app.display-name` → `FIREFLY_APP_DISPLAY_NAME`).
fn env_key(key: &str) -> String {
    let base = key.strip_prefix("firefly.").unwrap_or(key);
    format!("FIREFLY_{}", base.to_uppercase().replace(['.', '-'], "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn env_key_strips_firefly_prefix_and_relaxes() {
        assert_eq!(env_key("app.name"), "FIREFLY_APP_NAME");
        assert_eq!(env_key("firefly.app.name"), "FIREFLY_APP_NAME");
        assert_eq!(env_key("app.display-name"), "FIREFLY_APP_DISPLAY_NAME");
    }

    // pyfly: test_resolve_config_reference
    #[test]
    fn resolves_config_reference() {
        let resolved = resolve_placeholders(&flat(&[
            ("app.name", "MyApp"),
            ("greeting", "Hello from ${app.name}"),
        ]))
        .unwrap();
        assert_eq!(resolved["greeting"], "Hello from MyApp");
    }

    // pyfly: test_resolve_with_default
    #[test]
    fn resolves_default_when_missing() {
        let resolved =
            resolve_placeholders(&flat(&[("key", "${MISSING_VAR_XYZ:fallback_value}")])).unwrap();
        assert_eq!(resolved["key"], "fallback_value");
    }

    // pyfly: test_resolve_nested — recursive resolution through references.
    #[test]
    fn resolves_nested_references() {
        let resolved = resolve_placeholders(&flat(&[
            ("base", "localhost"),
            ("host", "${base}"),
            ("url", "http://${host}:8080"),
        ]))
        .unwrap();
        assert_eq!(resolved["url"], "http://localhost:8080");
    }

    // pyfly: test_no_placeholder_passthrough
    #[test]
    fn plain_values_pass_through() {
        let resolved = resolve_placeholders(&flat(&[("key", "plain-value")])).unwrap();
        assert_eq!(resolved["key"], "plain-value");
    }

    // pyfly: test_max_recursion_guard
    #[test]
    fn circular_references_error_instead_of_looping() {
        let err = resolve_placeholders(&flat(&[("a", "${b}"), ("b", "${a}")])).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("max recursion depth") || text.contains("circular"),
            "got: {text}"
        );
    }

    // pyfly (#92): placeholder references use relaxed kebab/snake matching.
    #[test]
    fn references_are_relaxed_kebab_snake() {
        let resolved = resolve_placeholders(&flat(&[
            ("my_prop.sub_key", "V"),
            ("msg", "${my-prop.sub-key}"),
        ]))
        .unwrap();
        assert_eq!(resolved["msg"], "V");
    }

    #[test]
    fn unresolvable_without_default_is_an_error() {
        let err = resolve_placeholders(&flat(&[("key", "${nope.missing}")])).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("not found in environment or config"),
            "got: {text}"
        );
        assert!(text.contains("${nope.missing}"), "got: {text}");
    }

    #[test]
    fn default_keeps_everything_after_first_colon() {
        let resolved =
            resolve_placeholders(&flat(&[("url", "${MISSING_XYZ:http://localhost:8080}")]))
                .unwrap();
        assert_eq!(resolved["url"], "http://localhost:8080");
    }
}
