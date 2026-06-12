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

/// Evaluates the **Spring Boot 2.4+ profile-expression grammar** of
/// pyfly's `Environment.accepts_profiles(*exprs)` against an explicit
/// active-profile list.
///
/// Returns `true` when **any** of the given `exprs` matches `active`.
/// Each expression supports:
///
/// - **Simple profiles** — `"dev"` matches when `"dev"` is active.
/// - **Negation** — `"!prod"` / `"!(prod)"`.
/// - **Boolean operators with grouping** — `"prod & cloud"`,
///   `"prod | qa"`, `"(prod & cloud) | qa"`.
/// - **Comma-OR (legacy)** — `"dev,test"` matches when either is active.
///
/// An expression is treated as a boolean expression when it contains any
/// of `&`, `|` or `(`; otherwise, if it contains `,` it is the legacy
/// comma-OR of single tokens; otherwise it is a single (optionally
/// `!`-negated) token. Whitespace around tokens and around the whole
/// expression is ignored. A malformed boolean expression evaluates to
/// `false` (never panics), matching pyfly.
///
/// Where pyfly reads the active list off the [`Environment`], the Rust
/// port takes it as a slice so it composes with [`active_profiles`]:
///
/// ```
/// use firefly_config::accepts_profiles;
///
/// let active = ["prod".to_string(), "cloud".to_string()];
/// assert!(accepts_profiles(&active, &["prod & cloud"]));
/// assert!(!accepts_profiles(&active, &["prod & staging"]));
/// assert!(accepts_profiles(&active, &["!test"]));
/// assert!(accepts_profiles(&active, &["(prod & cloud) | qa"]));
/// ```
///
/// [`Environment`]: https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/env/Environment.html
#[must_use]
pub fn accepts_profiles<S: AsRef<str>>(active: &[S], exprs: &[&str]) -> bool {
    let active: Vec<&str> = active.iter().map(AsRef::as_ref).collect();
    exprs
        .iter()
        .any(|expr| matches_profile_expression(&active, expr))
}

/// Evaluates a single profile expression against `active`.
fn matches_profile_expression(active: &[&str], expr: &str) -> bool {
    let expr = expr.trim();
    if expr.contains('&') || expr.contains('|') || expr.contains('(') {
        return eval_boolean_profile(active, expr);
    }
    if expr.contains(',') {
        return expr
            .split(',')
            .map(str::trim)
            .filter(|sub| !sub.is_empty())
            .any(|sub| matches_single(active, sub));
    }
    matches_single(active, expr)
}

/// Evaluates a single token, honoring an optional `!` negation prefix.
fn matches_single(active: &[&str], profile: &str) -> bool {
    let profile = profile.trim();
    if let Some(rest) = profile.strip_prefix('!') {
        !active.contains(&rest.trim())
    } else {
        active.contains(&profile)
    }
}

/// A token in a boolean profile expression.
#[derive(Debug, Clone, PartialEq)]
enum ProfileToken {
    /// `&`
    And,
    /// `|`
    Or,
    /// `!`
    Not,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `true` if the named profile is active, else `false`.
    Value(bool),
}

/// Evaluates a boolean profile expression (`&` / `|` / `!` / grouping).
/// Mirrors pyfly's `_eval_boolean_profile`: a parse/structure error
/// resolves to `false` rather than propagating.
fn eval_boolean_profile(active: &[&str], expr: &str) -> bool {
    let Some(tokens) = tokenize_profile(active, expr) else {
        return false;
    };
    let mut parser = ProfileParser {
        tokens: &tokens,
        pos: 0,
    };
    match parser.parse_or() {
        Some(value) if parser.pos == tokens.len() => value,
        _ => false,
    }
}

/// Splits `expr` into [`ProfileToken`]s, resolving bare identifiers to
/// `Value(active?)`. Returns `None` on an unexpected character. The
/// identifier charset matches pyfly's token regex
/// (`[A-Za-z0-9_.\-]+`).
fn tokenize_profile(active: &[&str], expr: &str) -> Option<Vec<ProfileToken>> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '&' => {
                tokens.push(ProfileToken::And);
                i += 1;
            }
            '|' => {
                tokens.push(ProfileToken::Or);
                i += 1;
            }
            '!' => {
                tokens.push(ProfileToken::Not);
                i += 1;
            }
            '(' => {
                tokens.push(ProfileToken::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(ProfileToken::RParen);
                i += 1;
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' => {
                let start = i;
                while i < chars.len() {
                    let n = chars[i];
                    if n.is_alphanumeric() || n == '_' || n == '.' || n == '-' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let ident: String = chars[start..i].iter().collect();
                tokens.push(ProfileToken::Value(active.contains(&ident.as_str())));
            }
            _ => return None,
        }
    }
    Some(tokens)
}

/// A tiny recursive-descent parser for boolean profile expressions with
/// the grammar `or := and ('|' and)*`, `and := unary ('&' unary)*`,
/// `unary := '!' unary | primary`, `primary := '(' or ')' | value`.
/// `|` and `&` bind looser than `!`, and grouping wins, matching the
/// precedence of Python's `or`/`and`/`not` that pyfly leans on.
struct ProfileParser<'a> {
    tokens: &'a [ProfileToken],
    pos: usize,
}

impl ProfileParser<'_> {
    fn peek(&self) -> Option<&ProfileToken> {
        self.tokens.get(self.pos)
    }

    fn parse_or(&mut self) -> Option<bool> {
        let mut value = self.parse_and()?;
        while matches!(self.peek(), Some(ProfileToken::Or)) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            value = value || rhs;
        }
        Some(value)
    }

    fn parse_and(&mut self) -> Option<bool> {
        let mut value = self.parse_unary()?;
        while matches!(self.peek(), Some(ProfileToken::And)) {
            self.pos += 1;
            let rhs = self.parse_unary()?;
            value = value && rhs;
        }
        Some(value)
    }

    fn parse_unary(&mut self) -> Option<bool> {
        if matches!(self.peek(), Some(ProfileToken::Not)) {
            self.pos += 1;
            return Some(!self.parse_unary()?);
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<bool> {
        match self.peek() {
            Some(ProfileToken::LParen) => {
                self.pos += 1;
                let value = self.parse_or()?;
                if matches!(self.peek(), Some(ProfileToken::RParen)) {
                    self.pos += 1;
                    Some(value)
                } else {
                    None
                }
            }
            Some(ProfileToken::Value(v)) => {
                let v = *v;
                self.pos += 1;
                Some(v)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_profiles_and() {
        let active = ["prod".to_string(), "cloud".to_string()];
        assert!(accepts_profiles(&active, &["prod & cloud"]));
        assert!(!accepts_profiles(&active, &["prod & staging"]));
    }

    #[test]
    fn accepts_profiles_or() {
        let active = ["prod".to_string()];
        assert!(accepts_profiles(&active, &["prod | qa"]));
        assert!(!accepts_profiles(&active, &["dev | qa"]));
    }

    #[test]
    fn accepts_profiles_not() {
        let active = ["prod".to_string()];
        assert!(accepts_profiles(&active, &["!test"]));
        assert!(!accepts_profiles(&active, &["!prod"]));
        assert!(accepts_profiles(&active, &["prod & !test"]));
    }

    #[test]
    fn accepts_profiles_grouping() {
        let active = ["cloud".to_string(), "qa".to_string()];
        assert!(accepts_profiles(&active, &["(prod & cloud) | qa"]));
        assert!(!accepts_profiles(&active, &["(prod & cloud) & qa"]));
        assert!(accepts_profiles(&active, &["!(prod | dev)"]));
    }

    #[test]
    fn accepts_profiles_legacy_comma_and_simple() {
        let active = ["dev".to_string()];
        assert!(accepts_profiles(&active, &["dev,test"]));
        assert!(accepts_profiles(&active, &["dev"]));
        assert!(!accepts_profiles(&active, &["test"]));
    }

    #[test]
    fn accepts_profiles_any_of_many() {
        let active = ["qa".to_string()];
        assert!(accepts_profiles(&active, &["prod", "qa"]));
        assert!(!accepts_profiles(&active, &["prod", "dev"]));
        assert!(!accepts_profiles::<String>(&active, &[]));
    }

    #[test]
    fn accepts_profiles_malformed_is_false() {
        let active = ["prod".to_string()];
        assert!(!accepts_profiles(&active, &["prod &"]));
        assert!(!accepts_profiles(&active, &["(prod"]));
        assert!(!accepts_profiles(&active, &[")"]));
    }

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
