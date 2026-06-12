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

//! Kahn topological ordering of plugins by declared dependencies.
//!
//! Mirrors pyfly's `PluginDependencyResolver`. The ordering is **stable with
//! respect to registration order**: when no plugin declares any dependency the
//! result is exactly the input order, so the Go-parity registration-order
//! semantics of [`crate::Registry`] are preserved unchanged. When dependencies
//! are declared, each dependency is placed before the plugin that needs it.

use crate::ResolutionError;

/// Computes a topological start order for the given plugins.
///
/// `nodes` is the list of `(name, depends_on)` pairs in **registration
/// order**. The returned vector is a list of indices into `nodes` describing
/// the order in which plugins should be started: every dependency appears
/// before the plugin that declares it.
///
/// Ties (plugins whose dependencies are all already satisfied) are broken by
/// registration order, so a graph with no edges yields `0, 1, 2, ...` — i.e.
/// plain registration order, matching the pre-existing behaviour.
///
/// # Errors
///
/// - [`ResolutionError::MissingDependency`] if a plugin depends on a name that
///   is not present in `nodes`.
/// - [`ResolutionError::Cycle`] if the dependency graph contains a cycle.
///
/// Duplicate names are not expected (the [`crate::Registry`] dedups by name on
/// registration); if present, the first occurrence wins for name resolution.
pub(crate) fn topological_order(
    nodes: &[(String, Vec<String>)],
) -> Result<Vec<usize>, ResolutionError> {
    let n = nodes.len();

    // Map each name to its first registration index.
    let mut index_of: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, (name, _)) in nodes.iter().enumerate() {
        index_of.entry(name.as_str()).or_insert(i);
    }

    // in_degree[i] = number of (existing) dependencies plugin i waits on.
    // dependents[d] = indices of plugins that depend on plugin d.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, (name, deps)) in nodes.iter().enumerate() {
        for dep in deps {
            let Some(&dep_idx) = index_of.get(dep.as_str()) else {
                return Err(ResolutionError::MissingDependency {
                    plugin: name.clone(),
                    missing: dep.clone(),
                });
            };
            in_degree[i] += 1;
            dependents[dep_idx].push(i);
        }
    }

    // Kahn's algorithm. The ready set is kept as a sorted-by-registration-index
    // list so ties resolve to registration order (degrading to plain
    // registration order when there are no edges at all).
    let mut ready: std::collections::BTreeSet<usize> =
        (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut ordered = Vec::with_capacity(n);

    while let Some(&current) = ready.iter().next() {
        ready.remove(&current);
        ordered.push(current);
        for &dep in &dependents[current] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                ready.insert(dep);
            }
        }
    }

    if ordered.len() != n {
        return Err(ResolutionError::Cycle);
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, deps: &[&str]) -> (String, Vec<String>) {
        (
            name.to_owned(),
            deps.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    fn names(nodes: &[(String, Vec<String>)], order: &[usize]) -> Vec<String> {
        order.iter().map(|&i| nodes[i].0.clone()).collect()
    }

    #[test]
    fn no_dependencies_preserves_registration_order() {
        let nodes = vec![node("c", &[]), node("a", &[]), node("b", &[])];
        let order = topological_order(&nodes).expect("order");
        assert_eq!(names(&nodes, &order), vec!["c", "a", "b"]);
    }

    #[test]
    fn dependency_starts_before_dependent() {
        // b depends on a; registered b then a.
        let nodes = vec![node("b", &["a"]), node("a", &[])];
        let order = topological_order(&nodes).expect("order");
        assert_eq!(names(&nodes, &order), vec!["a", "b"]);
    }

    #[test]
    fn chain_a_b_c() {
        // Registered out of order: c, a, b with c<-b<-a.
        let nodes = vec![node("c", &["b"]), node("a", &[]), node("b", &["a"])];
        let order = topological_order(&nodes).expect("order");
        assert_eq!(names(&nodes, &order), vec!["a", "b", "c"]);
    }

    #[test]
    fn missing_dependency_is_rejected() {
        let nodes = vec![node("b", &["a"])];
        let err = topological_order(&nodes).expect_err("missing dep");
        assert_eq!(
            err,
            ResolutionError::MissingDependency {
                plugin: "b".into(),
                missing: "a".into(),
            }
        );
    }

    #[test]
    fn cycle_is_rejected() {
        let nodes = vec![node("a", &["b"]), node("b", &["a"])];
        let err = topological_order(&nodes).expect_err("cycle");
        assert_eq!(err, ResolutionError::Cycle);
    }

    #[test]
    fn independent_ties_break_by_registration_order() {
        // d depends on a; b and c are independent. Expected: a, b, c first
        // (in registration order, all degree-0), then d once a is done.
        let nodes = vec![
            node("a", &[]),
            node("b", &[]),
            node("c", &[]),
            node("d", &["a"]),
        ];
        let order = topological_order(&nodes).expect("order");
        assert_eq!(names(&nodes, &order), vec!["a", "b", "c", "d"]);
    }
}
