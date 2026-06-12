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

//! Role hierarchy — higher roles imply lower ones (Spring Security's
//! `RoleHierarchy`; pyfly: `pyfly.security.role_hierarchy`).
//!
//! Declare `ADMIN > USER` to mean an `ADMIN` also has every authority
//! of `USER`. The [`FilterChain`](crate::FilterChain) consults a
//! configured hierarchy (via
//! [`FilterChain::with_role_hierarchy`](crate::FilterChain::with_role_hierarchy)),
//! so a `require(..., &["USER"])` rule is satisfied for an `ADMIN`.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::authentication::Authentication;

/// Directed role-implication graph with transitive expansion.
///
/// ```rust
/// use firefly_security::RoleHierarchy;
///
/// let h = RoleHierarchy::from_string("ADMIN > MANAGER\nMANAGER > USER");
/// let roles = h.expand(["ADMIN".to_string()]);
/// assert!(roles.contains("MANAGER") && roles.contains("USER"));
/// ```
#[derive(Debug, Clone, Default)]
pub struct RoleHierarchy {
    /// `implies[X]` = roles directly implied by `X`.
    implies: HashMap<String, HashSet<String>>,
}

impl RoleHierarchy {
    /// Returns an empty hierarchy (no implications).
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses a hierarchy spec: one `HIGHER > LOWER` rule per line (or
    /// `;`-separated). Lines without a `>` are ignored, as are blank
    /// sides — byte-for-byte the pyfly `RoleHierarchy.from_string`
    /// parsing rules.
    ///
    /// ```rust
    /// use firefly_security::RoleHierarchy;
    ///
    /// let h = RoleHierarchy::from_string("ADMIN > USER ; USER > GUEST");
    /// assert!(h.expand(["ADMIN".to_string()]).contains("GUEST"));
    /// ```
    pub fn from_string(spec: &str) -> Self {
        let mut hierarchy = Self::new();
        for raw in spec.replace(';', "\n").lines() {
            let line = raw.trim();
            let Some((higher, lower)) = line.split_once('>') else {
                continue;
            };
            let (higher, lower) = (higher.trim(), lower.trim());
            if !higher.is_empty() && !lower.is_empty() {
                hierarchy.add_implication(higher, lower);
            }
        }
        hierarchy
    }

    /// Declares that `higher` implies (includes) `lower`.
    pub fn add_implication(&mut self, higher: impl Into<String>, lower: impl Into<String>) {
        self.implies
            .entry(higher.into())
            .or_default()
            .insert(lower.into());
    }

    /// Returns `roles` plus every role transitively implied by them.
    /// The result is an ordered set for deterministic iteration.
    pub fn expand<I, S>(&self, roles: I) -> BTreeSet<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut result = BTreeSet::new();
        let mut stack: Vec<String> = roles.into_iter().map(Into::into).collect();
        while let Some(role) = stack.pop() {
            if result.contains(&role) {
                continue;
            }
            if let Some(implied) = self.implies.get(&role) {
                stack.extend(implied.iter().cloned());
            }
            result.insert(role);
        }
        result
    }

    /// Returns a copy of `auth` whose roles are expanded through this
    /// hierarchy (sorted) — handy for feeding hierarchy-aware data to
    /// [`AuthorizationGuard`](crate::AuthorizationGuard) predicates or
    /// any role check that doesn't consult the hierarchy itself.
    pub fn expand_authentication(&self, auth: &Authentication) -> Authentication {
        let mut expanded = auth.clone();
        expanded.roles = self
            .expand(auth.roles.iter().cloned())
            .into_iter()
            .collect();
        expanded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(roles: &[&str]) -> BTreeSet<String> {
        roles.iter().map(|r| r.to_string()).collect()
    }

    // Ported from pyfly: test_expand_transitive
    #[test]
    fn expand_transitive() {
        let h = RoleHierarchy::from_string("ADMIN > MANAGER\nMANAGER > USER");
        assert_eq!(
            h.expand(["ADMIN".to_string()]),
            set(&["ADMIN", "MANAGER", "USER"])
        );
        assert_eq!(h.expand(["MANAGER".to_string()]), set(&["MANAGER", "USER"]));
        assert_eq!(h.expand(["USER".to_string()]), set(&["USER"]));
        assert_eq!(h.expand(Vec::<String>::new()), BTreeSet::new());
    }

    // Ported from pyfly: test_from_string_separators_and_noise
    #[test]
    fn from_string_separators_and_noise() {
        let h = RoleHierarchy::from_string("ADMIN > USER ; USER > GUEST\n\nnonsense-without-arrow");
        assert_eq!(
            h.expand(["ADMIN".to_string()]),
            set(&["ADMIN", "USER", "GUEST"])
        );
    }

    #[test]
    fn expand_does_not_grant_unrelated_roles() {
        let h = RoleHierarchy::from_string("ADMIN > USER");
        let expanded = h.expand(["ADMIN".to_string()]);
        assert!(!expanded.contains("SUPERUSER"));
        assert!(expanded.contains("ADMIN")); // still has its own role
    }

    #[test]
    fn expand_handles_cycles_without_looping() {
        let h = RoleHierarchy::from_string("A > B\nB > A");
        assert_eq!(h.expand(["A".to_string()]), set(&["A", "B"]));
    }

    #[test]
    fn expand_authentication_replaces_roles() {
        let h = RoleHierarchy::from_string("ADMIN > USER");
        let auth = Authentication {
            principal: "u1".into(),
            roles: vec!["ADMIN".into()],
            ..Default::default()
        };
        let expanded = h.expand_authentication(&auth);
        assert_eq!(expanded.roles, vec!["ADMIN".to_string(), "USER".into()]);
        assert_eq!(expanded.principal, "u1");
        assert!(auth.roles.len() == 1, "input untouched");
    }
}
