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

//! Condition + profile evaluation for component scanning.
//!
//! Ports the *evaluation* half of pyfly's `context.conditions` /
//! `context.condition_evaluator` and Spring Boot's `@ConditionalOn*` /
//! `@Profile` family. A [`ConditionContext`] holds the active configuration
//! key/value pairs and active profiles; the container consults it during
//! [`scan`](crate::Container::scan) to decide whether each discovered bean is
//! registered.
//!
//! The container is config-crate-agnostic: a [`ConditionContext`] is a plain
//! value populated by the caller (the `firefly` facade fills it from
//! `firefly_config`). This keeps `firefly-container`'s dependency surface to
//! just `thiserror` + `inventory` while still delivering full conditional
//! parity.
//!
//! Conditions split into two passes, exactly as pyfly does:
//! - **Pass 1** (registry-independent): `on_property`, `on_class`, `profile`.
//! - **Pass 2** (registry-dependent): `on_bean`, `on_missing_bean`,
//!   `on_single_candidate`. These are evaluated by the
//!   [`Container`](crate::Container) during [`scan`](crate::Container::scan)
//!   *after* all pass-1 survivors are registered, so a "missing bean" check
//!   sees the beans a user already provided.

use std::collections::HashMap;

/// The set of inputs a [`Condition`] is evaluated against.
///
/// Mirrors pyfly's `ConditionEvaluator(config, container)` inputs (the config
/// half). Build one with [`ConditionContext::new`] or the builder methods, then
/// pass it to [`Container::scan_with`](crate::Container::scan_with).
///
/// ```
/// use firefly_container::ConditionContext;
///
/// let ctx = ConditionContext::new()
///     .with_profiles(["prod"])
///     .with_property("feature.cache", "true");
/// assert!(ctx.accepts_profiles("prod"));
/// assert_eq!(ctx.property("feature.cache"), Some("true"));
/// ```
#[derive(Debug, Clone, Default)]
pub struct ConditionContext {
    properties: HashMap<String, String>,
    profiles: Vec<String>,
    /// Crate/feature labels treated as "present" for `on_class`-style checks.
    classes: Vec<String>,
}

impl ConditionContext {
    /// An empty context — no properties, no active profiles.
    #[must_use]
    pub fn new() -> Self {
        ConditionContext::default()
    }

    /// Set the active profiles (replacing any previously set), builder-style.
    #[must_use]
    pub fn with_profiles<I, S>(mut self, profiles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.profiles = profiles
            .into_iter()
            .map(|p| p.into().trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect();
        self
    }

    /// Add a single configuration property, builder-style.
    #[must_use]
    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Bulk-set the configuration properties from a map, builder-style.
    #[must_use]
    pub fn with_properties(mut self, properties: HashMap<String, String>) -> Self {
        self.properties = properties;
        self
    }

    /// Register a "class"/feature label as present (the Rust analog of pyfly's
    /// `conditional_on_class` import probe — a token your build declares
    /// available), builder-style.
    #[must_use]
    pub fn with_class(mut self, label: impl Into<String>) -> Self {
        self.classes.push(label.into());
        self
    }

    /// Look up a configuration property by key.
    #[must_use]
    pub fn property(&self, key: &str) -> Option<&str> {
        self.properties.get(key).map(String::as_str)
    }

    /// A clone of the full property map.
    #[must_use]
    pub fn properties(&self) -> HashMap<String, String> {
        self.properties.clone()
    }

    /// The property map restricted to keys under `prefix`, with the prefix (and
    /// its trailing `.`) stripped.
    ///
    /// `prefix = "app.db"` turns `{"app.db.url": "x", "other": "y"}` into
    /// `{"url": "x"}`. An empty prefix returns the whole map. This is the input
    /// shape `firefly_config::bind` expects for a `#[derive(ConfigProperties)]`
    /// bean — the Rust analog of Spring's `@ConfigurationProperties(prefix)`.
    #[must_use]
    pub fn properties_with_prefix(&self, prefix: &str) -> HashMap<String, String> {
        let prefix = prefix.trim().trim_end_matches('.');
        if prefix.is_empty() {
            return self.properties.clone();
        }
        let full = format!("{prefix}.");
        self.properties
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(&full)
                    .map(|stripped| (stripped.to_string(), v.clone()))
            })
            .collect()
    }

    /// The active profiles (lower-cased).
    #[must_use]
    pub fn profiles(&self) -> &[String] {
        &self.profiles
    }

    /// Whether a "class"/feature label was registered as present.
    #[must_use]
    pub fn has_class(&self, label: &str) -> bool {
        self.classes.iter().any(|c| c == label)
    }

    /// Evaluate a profile expression against the active profiles.
    ///
    /// Supports the Spring Boot 2.4+ grammar pyfly uses: a profile name
    /// (`prod`), negation (`!test`), conjunction (`prod & cloud`), disjunction
    /// (`dev | test`), comma-as-OR (`dev,test`), and parentheses. Matching is
    /// case-insensitive. An empty expression always matches.
    #[must_use]
    pub fn accepts_profiles(&self, expr: &str) -> bool {
        let expr = expr.trim();
        if expr.is_empty() {
            return true;
        }
        let active: Vec<&str> = self.profiles.iter().map(String::as_str).collect();
        eval_profile_expr(&active, expr)
    }

    /// Evaluate the registry-independent (pass-1) part of `conditions`.
    ///
    /// Returns `true` when every property/class/profile condition passes. Bean
    /// conditions (`on_bean`/`on_missing_bean`/`on_single_candidate`) are
    /// skipped here — the [`Container`](crate::Container) evaluates them in
    /// pass 2.
    #[must_use]
    pub fn pass1(&self, conditions: &[Condition]) -> bool {
        conditions.iter().all(|c| match c {
            Condition::Profile(expr) => self.accepts_profiles(expr),
            Condition::OnProperty {
                key,
                having_value,
                match_if_missing,
            } => self.eval_on_property(key, having_value.as_deref(), *match_if_missing),
            Condition::OnClass(label) => self.has_class(label),
            // Bean-dependent conditions belong to pass 2; treat as pass here.
            Condition::OnBean(_)
            | Condition::OnMissingBean(_)
            | Condition::OnSingleCandidate(_) => true,
        })
    }

    fn eval_on_property(
        &self,
        key: &str,
        having_value: Option<&str>,
        match_if_missing: bool,
    ) -> bool {
        match self.property(key) {
            None => match_if_missing,
            Some(value) => match having_value {
                Some(expected) => value.eq_ignore_ascii_case(expected),
                None => !value.trim().eq_ignore_ascii_case("false"),
            },
        }
    }
}

/// One condition guarding a bean registration.
///
/// Mirrors the dicts pyfly stores in `__pyfly_conditions__` plus the `@profile`
/// expression. Conditions are produced by the stereotype derives in
/// `firefly-macros` and evaluated by [`Container::scan`](crate::Container::scan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// `#[profile("expr")]` — register only when the expression matches the
    /// active profiles.
    Profile(String),
    /// `#[firefly(condition_on_property = "key=value")]` — register only when a
    /// config property matches.
    OnProperty {
        /// The config key to look up.
        key: String,
        /// The expected value (case-insensitive), or `None` for "present and
        /// not `false`".
        having_value: Option<String>,
        /// Whether to match when the property is absent.
        match_if_missing: bool,
    },
    /// `#[firefly(condition_on_class = "label")]` — register only when the
    /// build declares the labelled feature/crate present.
    OnClass(String),
    /// `#[firefly(condition_on_bean = "Type")]` — register only when a bean of
    /// the named type is already registered (pass 2).
    OnBean(String),
    /// `#[firefly(condition_on_missing_bean = "Type")]` — register only when no
    /// bean of the named type is registered (pass 2).
    OnMissingBean(String),
    /// `#[firefly(condition_on_single_candidate = "Type")]` — register only
    /// when exactly one candidate of the named type exists (pass 2).
    OnSingleCandidate(String),
}

impl Condition {
    /// Whether this condition depends on the bean registry (evaluated in
    /// pass 2 by the container).
    #[must_use]
    pub fn is_bean_dependent(&self) -> bool {
        matches!(
            self,
            Condition::OnBean(_) | Condition::OnMissingBean(_) | Condition::OnSingleCandidate(_)
        )
    }

    /// Parse a `key=value` / `key` property spec into [`Condition::OnProperty`].
    ///
    /// `"feature.cache=true"` → equals check; `"feature.cache"` → present-and-not-false.
    #[must_use]
    pub fn on_property(spec: &str) -> Condition {
        match spec.split_once('=') {
            Some((key, value)) => Condition::OnProperty {
                key: key.trim().to_string(),
                having_value: Some(value.trim().to_string()),
                match_if_missing: false,
            },
            None => Condition::OnProperty {
                key: spec.trim().to_string(),
                having_value: None,
                match_if_missing: false,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Profile-expression evaluator (Spring Boot 2.4+ grammar, dependency-free).
// ---------------------------------------------------------------------------

/// Evaluate a profile expression against the active profile set.
fn eval_profile_expr(active: &[&str], expr: &str) -> bool {
    let expr = expr.trim();
    // Comma is a top-level OR shorthand (`dev,test`).
    if !expr.contains('&') && !expr.contains('|') && !expr.contains('(') && expr.contains(',') {
        return expr
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(|s| eval_profile_atom(active, s));
    }
    let tokens = tokenize_profile(expr);
    let mut pos = 0;
    let result = parse_or(active, &tokens, &mut pos);
    // Trailing junk → be permissive and just return the parsed result.
    result
}

#[derive(Debug, Clone, PartialEq)]
enum PTok {
    Name(String),
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize_profile(expr: &str) -> Vec<PTok> {
    let mut tokens = Vec::new();
    let mut name = String::new();
    let flush = |name: &mut String, tokens: &mut Vec<PTok>| {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            tokens.push(PTok::Name(trimmed.to_string()));
        }
        name.clear();
    };
    for ch in expr.chars() {
        match ch {
            '&' => {
                flush(&mut name, &mut tokens);
                // Collapse `&&` into one And.
                if tokens.last() != Some(&PTok::And) {
                    tokens.push(PTok::And);
                }
            }
            '|' | ',' => {
                flush(&mut name, &mut tokens);
                if tokens.last() != Some(&PTok::Or) {
                    tokens.push(PTok::Or);
                }
            }
            '!' => {
                flush(&mut name, &mut tokens);
                tokens.push(PTok::Not);
            }
            '(' => {
                flush(&mut name, &mut tokens);
                tokens.push(PTok::LParen);
            }
            ')' => {
                flush(&mut name, &mut tokens);
                tokens.push(PTok::RParen);
            }
            c if c.is_whitespace() => flush(&mut name, &mut tokens),
            c => name.push(c),
        }
    }
    flush(&mut name, &mut tokens);
    tokens
}

fn eval_profile_atom(active: &[&str], name: &str) -> bool {
    active.iter().any(|a| a.eq_ignore_ascii_case(name))
}

fn parse_or(active: &[&str], tokens: &[PTok], pos: &mut usize) -> bool {
    let mut value = parse_and(active, tokens, pos);
    while matches!(tokens.get(*pos), Some(PTok::Or)) {
        *pos += 1;
        let rhs = parse_and(active, tokens, pos);
        value = value || rhs;
    }
    value
}

fn parse_and(active: &[&str], tokens: &[PTok], pos: &mut usize) -> bool {
    let mut value = parse_unary(active, tokens, pos);
    while matches!(tokens.get(*pos), Some(PTok::And)) {
        *pos += 1;
        let rhs = parse_unary(active, tokens, pos);
        value = value && rhs;
    }
    value
}

fn parse_unary(active: &[&str], tokens: &[PTok], pos: &mut usize) -> bool {
    if matches!(tokens.get(*pos), Some(PTok::Not)) {
        *pos += 1;
        return !parse_unary(active, tokens, pos);
    }
    parse_primary(active, tokens, pos)
}

fn parse_primary(active: &[&str], tokens: &[PTok], pos: &mut usize) -> bool {
    match tokens.get(*pos) {
        Some(PTok::LParen) => {
            *pos += 1;
            let value = parse_or(active, tokens, pos);
            if matches!(tokens.get(*pos), Some(PTok::RParen)) {
                *pos += 1;
            }
            value
        }
        Some(PTok::Name(name)) => {
            let result = eval_profile_atom(active, name);
            *pos += 1;
            result
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_atoms_and_negation() {
        let ctx = ConditionContext::new().with_profiles(["prod"]);
        assert!(ctx.accepts_profiles("prod"));
        assert!(!ctx.accepts_profiles("dev"));
        assert!(ctx.accepts_profiles("!dev"));
        assert!(!ctx.accepts_profiles("!prod"));
        assert!(ctx.accepts_profiles("")); // empty always matches
    }

    #[test]
    fn profile_boolean_grammar() {
        let ctx = ConditionContext::new().with_profiles(["prod", "cloud"]);
        assert!(ctx.accepts_profiles("prod & cloud"));
        assert!(!ctx.accepts_profiles("prod & dev"));
        assert!(ctx.accepts_profiles("dev | cloud"));
        assert!(ctx.accepts_profiles("dev,cloud"));
        assert!(ctx.accepts_profiles("(dev | prod) & cloud"));
        assert!(!ctx.accepts_profiles("(dev | test) & cloud"));
        assert!(ctx.accepts_profiles("prod & !test"));
    }

    #[test]
    fn on_property_having_value() {
        let ctx = ConditionContext::new().with_property("a.b", "ON");
        assert!(ctx.eval_on_property("a.b", Some("on"), false));
        assert!(!ctx.eval_on_property("a.b", Some("off"), false));
        assert!(!ctx.eval_on_property("missing", Some("on"), false));
        assert!(ctx.eval_on_property("missing", Some("on"), true));
    }

    #[test]
    fn on_property_presence() {
        let present = ConditionContext::new().with_property("flag", "true");
        let falsey = ConditionContext::new().with_property("flag", "false");
        assert!(present.eval_on_property("flag", None, false));
        assert!(!falsey.eval_on_property("flag", None, false));
        assert!(!present.eval_on_property("absent", None, false));
    }

    #[test]
    fn condition_on_property_parsing() {
        assert_eq!(
            Condition::on_property("k=v"),
            Condition::OnProperty {
                key: "k".into(),
                having_value: Some("v".into()),
                match_if_missing: false
            }
        );
        assert_eq!(
            Condition::on_property("k"),
            Condition::OnProperty {
                key: "k".into(),
                having_value: None,
                match_if_missing: false
            }
        );
    }

    #[test]
    fn pass1_combines_conditions() {
        let ctx = ConditionContext::new()
            .with_profiles(["prod"])
            .with_property("feature", "on");
        let conds = vec![
            Condition::Profile("prod".into()),
            Condition::on_property("feature=on"),
        ];
        assert!(ctx.pass1(&conds));
        // bean-dependent conditions are ignored in pass1
        let conds2 = vec![Condition::OnMissingBean("X".into())];
        assert!(ctx.pass1(&conds2));
        // a failing profile rejects
        let conds3 = vec![Condition::Profile("dev".into())];
        assert!(!ctx.pass1(&conds3));
    }
}
