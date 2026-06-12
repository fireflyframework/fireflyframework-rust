//! Container error taxonomy with developer-friendly diagnostics.
//!
//! Ports pyfly's `container.exceptions`: [`ContainerError::NoSuchBean`],
//! [`ContainerError::NoUniqueBean`], and [`ContainerError::CircularDependency`].
//! Each variant renders a Spring-Boot-style multi-line message via its
//! [`std::fmt::Display`] implementation, including actionable hints and (for
//! `NoSuchBean`) hand-rolled fuzzy suggestions of similar registered types.

use thiserror::Error;

/// Errors raised while registering or resolving beans.
///
/// Mirrors pyfly's `BeanCreationException` hierarchy: every variant is a fatal
/// wiring error. The [`Display`](std::fmt::Display) text is multi-line and
/// developer-facing, matching the diagnostics pyfly emits.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContainerError {
    /// No bean is registered for the requested type or name.
    ///
    /// Analogous to pyfly's `NoSuchBeanError`. `suggestions` carries fuzzy
    /// matches of similar registered type names (see
    /// [`Container::fuzzy_suggestions`](crate::Container::fuzzy_suggestions)).
    #[error("{}", render_no_such_bean(.bean_type.as_deref(), .bean_name.as_deref(), .required_by.as_deref(), .parameter.as_deref(), .suggestions))]
    NoSuchBean {
        /// The requested type name, if resolution was by type.
        bean_type: Option<String>,
        /// The requested bean name, if resolution was by name.
        bean_name: Option<String>,
        /// The dependent bean whose construction triggered this lookup.
        required_by: Option<String>,
        /// The specific constructor parameter that could not be satisfied.
        parameter: Option<String>,
        /// Fuzzy-matched names of similar registered types.
        suggestions: Vec<String>,
    },

    /// Multiple beans match the requested type but none is marked primary.
    ///
    /// Analogous to pyfly's `NoUniqueBeanError`.
    #[error("{}", render_no_unique_bean(.bean_type, .candidates))]
    NoUniqueBean {
        /// The requested (typically interface) type name.
        bean_type: String,
        /// Names of the competing candidate registrations.
        candidates: Vec<String>,
    },

    /// A circular dependency was detected during factory resolution.
    ///
    /// Analogous to pyfly's `BeanCurrentlyInCreationError`. `chain` is the
    /// deterministic in-creation path, and `current` is the type being
    /// re-entered (which also closes the cycle).
    #[error("{}", render_circular(.chain, .current))]
    CircularDependency {
        /// The ordered in-creation path leading up to the cycle.
        chain: Vec<String>,
        /// The type being re-entered (closes the cycle).
        current: String,
    },
}

impl ContainerError {
    /// Construct a [`ContainerError::NoSuchBean`] for a missing type.
    pub(crate) fn no_such_type(bean_type: impl Into<String>, suggestions: Vec<String>) -> Self {
        ContainerError::NoSuchBean {
            bean_type: Some(bean_type.into()),
            bean_name: None,
            required_by: None,
            parameter: None,
            suggestions,
        }
    }

    /// Construct a [`ContainerError::NoSuchBean`] for a missing named bean.
    pub(crate) fn no_such_name(bean_name: impl Into<String>, suggestions: Vec<String>) -> Self {
        ContainerError::NoSuchBean {
            bean_type: None,
            bean_name: Some(bean_name.into()),
            required_by: None,
            parameter: None,
            suggestions,
        }
    }
}

fn render_no_such_bean(
    bean_type: Option<&str>,
    bean_name: Option<&str>,
    required_by: Option<&str>,
    parameter: Option<&str>,
    suggestions: &[String],
) -> String {
    let headline = if let Some(t) = bean_type {
        format!("No bean of type '{t}' is registered")
    } else if let Some(n) = bean_name {
        format!("No bean named '{n}' is registered")
    } else {
        "No matching bean is registered".to_string()
    };

    let mut lines = vec![format!("NoSuchBean: {headline}")];

    if required_by.is_some() || parameter.is_some() {
        lines.push(String::new());
        if let Some(rb) = required_by {
            lines.push(format!("  Required by: {rb}"));
        }
        if let Some(p) = parameter {
            lines.push(format!("    Parameter: {p}"));
        }
    }

    lines.push(String::new());
    lines.push("  Suggestions:".to_string());
    lines.push("    - register the type, instance, or factory on the Container".to_string());
    lines.push(
        "    - bind an interface to a concrete implementation via bind::<I, T>()".to_string(),
    );
    lines.push("    - check the scope and name used at registration".to_string());

    if !suggestions.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "  Similar registered types: {}",
            suggestions.join(", ")
        ));
    }

    lines.join("\n")
}

fn render_no_unique_bean(bean_type: &str, candidates: &[String]) -> String {
    let mut lines = vec![format!(
        "NoUniqueBean: Multiple beans of type '{bean_type}' found but none is marked primary"
    )];
    lines.push(String::new());
    lines.push(format!("  Candidates: [{}]", candidates.join(", ")));
    lines.push(String::new());
    lines.push(
        "  Fix: mark one registration primary, or resolve_named(\"name\") to disambiguate"
            .to_string(),
    );
    lines.join("\n")
}

fn render_circular(chain: &[String], current: &str) -> String {
    // Render short names (last `::` segment, generics stripped) the way pyfly
    // renders `__name__`, keeping the message readable. The stored `chain` /
    // `current` retain the full, unambiguous type paths.
    let mut chain_names: Vec<String> = chain.iter().map(|s| short(s).to_string()).collect();
    chain_names.push(short(current).to_string());
    let chain_str = chain_names.join(" -> ");
    let mut lines = vec![format!("CircularDependency: {chain_str}")];
    lines.push(String::new());
    lines.push("  Suggestion: break the cycle with a Provider<T> or a factory pattern".to_string());
    lines.join("\n")
}

/// Strip module path and generic arguments from a type name (`a::b::Foo<T>` ->
/// `Foo`) so error messages show the short name pyfly's `__name__` does.
fn short(name: &str) -> &str {
    let no_generics = name.split('<').next().unwrap_or(name);
    no_generics.rsplit("::").next().unwrap_or(no_generics)
}
