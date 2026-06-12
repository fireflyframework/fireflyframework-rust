//! Startup-time validation of saga / workflow / TCC definitions — the Rust
//! spelling of pyfly's `OrchestrationValidator`
//! (`pyfly.transactional.core.validator`).
//!
//! The validator lints a dependency graph (`node -> [dependencies]`, the
//! shape produced by [`Workflow::graph`](crate::Workflow::graph)) for the
//! three structural faults pyfly checks: an empty graph, an unknown
//! dependency, and a dependency cycle. Faults surface as
//! [`ValidationIssue`]s collected into a [`ValidationReport`]; callers may
//! turn errors into a hard failure with
//! [`ValidationReport::raise_if_errors`].

use std::collections::BTreeMap;

use serde::Serialize;

/// Severity of a single [`ValidationIssue`] — pyfly's `IssueLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum IssueLevel {
    /// Advisory; does not by itself fail validation.
    Warning,
    /// Structural fault; fails [`ValidationReport::raise_if_errors`].
    Error,
}

impl IssueLevel {
    /// The canonical wire string, `"WARNING"` or `"ERROR"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }
}

impl std::fmt::Display for IssueLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single validation problem found in a definition — pyfly's
/// `ValidationIssue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    /// The definition (saga / workflow / TCC name) the issue concerns.
    pub target: String,
    /// Severity.
    pub level: IssueLevel,
    /// Human-readable description.
    pub message: String,
}

/// Error raised when a [`ValidationReport`] with errors is enforced —
/// pyfly's `OrchestrationValidationError`.
#[derive(Debug, thiserror::Error)]
#[error("firefly/orchestration: validation failed: {0}")]
pub struct ValidationError(
    /// Joined `[target] message` strings of the offending issues.
    pub String,
);

/// Aggregated result of running the validator — pyfly's `ValidationReport`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    /// Every issue found, in discovery order.
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    /// An empty report — no issues.
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when at least one [`IssueLevel::Error`] issue is present.
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.level == IssueLevel::Error)
    }

    /// `true` when no issues at all were recorded.
    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    /// Returns [`ValidationError`] joining every error-level issue when any
    /// is present — pyfly's `raise_if_errors`.
    pub fn raise_if_errors(&self) -> Result<(), ValidationError> {
        if self.has_errors() {
            Err(ValidationError(self.joined(IssueLevel::Error)))
        } else {
            Ok(())
        }
    }

    fn joined(&self, level: IssueLevel) -> String {
        self.issues
            .iter()
            .filter(|i| i.level == level)
            .map(|i| format!("[{}] {}", i.target, i.message))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Validates the structural integrity of orchestration definitions —
/// pyfly's `OrchestrationValidator`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrchestrationValidator {
    fail_on_warning: bool,
}

impl OrchestrationValidator {
    /// A validator that fails only on errors — pyfly's default.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder toggle: when `true`, [`Self::fail_if_needed`] also rejects
    /// reports carrying only warnings — pyfly's `fail_on_warning=True`.
    #[must_use]
    pub fn fail_on_warning(mut self, fail: bool) -> Self {
        self.fail_on_warning = fail;
        self
    }

    /// Validates that `graph` (`node -> [dependencies]`) is a well-formed,
    /// acyclic DAG with all dependencies declared — pyfly's `validate_dag`.
    ///
    /// Records an [`IssueLevel::Error`] for an empty graph, for any
    /// dependency referencing an undeclared node, and for any cycle.
    pub fn validate_dag(
        &self,
        target: impl Into<String>,
        graph: &BTreeMap<String, Vec<String>>,
    ) -> ValidationReport {
        let target = target.into();
        let mut report = ValidationReport::new();

        if graph.is_empty() {
            report.issues.push(ValidationIssue {
                target,
                level: IssueLevel::Error,
                message: "no steps defined".to_string(),
            });
            return report;
        }

        // Unknown-dependency check.
        for (node, deps) in graph {
            for dep in deps {
                if !graph.contains_key(dep) {
                    report.issues.push(ValidationIssue {
                        target: target.clone(),
                        level: IssueLevel::Error,
                        message: format!("node {node:?} depends on unknown {dep:?}"),
                    });
                }
            }
        }

        // Cycle detection via Kahn's algorithm: if we cannot drain every
        // node, the remaining ones form a cycle.
        if !report.has_errors() {
            if let Some(cycle) = first_cycle(graph) {
                report.issues.push(ValidationIssue {
                    target,
                    level: IssueLevel::Error,
                    message: format!("dependency cycle: {}", cycle.join(" -> ")),
                });
            }
        }

        report
    }

    /// Enforces a report: errors always fail; when
    /// [`Self::fail_on_warning`] is set any issue fails — pyfly's
    /// `fail_if_needed`.
    pub fn fail_if_needed(&self, report: &ValidationReport) -> Result<(), ValidationError> {
        if self.fail_on_warning && !report.is_empty() {
            let all = report
                .issues
                .iter()
                .map(|i| format!("[{}] {}", i.target, i.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ValidationError(all));
        }
        report.raise_if_errors()
    }
}

/// Returns the nodes that remain after a topological drain (Kahn's
/// algorithm) — a non-empty result means those nodes form a cycle.
fn first_cycle(graph: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    // in_degree[n] = number of unsatisfied dependencies of n.
    let mut remaining: BTreeMap<&str, usize> = graph
        .iter()
        .map(|(n, deps)| (n.as_str(), deps.len()))
        .collect();

    loop {
        // Find a node whose dependencies are all resolved.
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&n, _)| n)
            .collect();
        if ready.is_empty() {
            break;
        }
        for node in ready {
            remaining.remove(node);
            // Removing `node` resolves one dependency for each dependent.
            for (dependent, deps) in graph {
                if deps.iter().any(|d| d == node) {
                    if let Some(deg) = remaining.get_mut(dependent.as_str()) {
                        *deg = deg.saturating_sub(1);
                    }
                }
            }
        }
    }

    if remaining.is_empty() {
        None
    } else {
        let mut nodes: Vec<String> = remaining.keys().map(|s| s.to_string()).collect();
        nodes.sort();
        Some(nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(n, deps)| (n.to_string(), deps.iter().map(|d| d.to_string()).collect()))
            .collect()
    }

    // Port of pyfly test_dag_valid_returns_no_errors.
    #[test]
    fn dag_valid_returns_no_errors() {
        let v = OrchestrationValidator::new();
        let report = v.validate_dag("test", &graph(&[("a", &[]), ("b", &["a"])]));
        assert!(!report.has_errors());
        assert!(report.raise_if_errors().is_ok());
    }

    // Port of pyfly test_dag_with_cycle_reports_error.
    #[test]
    fn dag_with_cycle_reports_error() {
        let v = OrchestrationValidator::new();
        let report = v.validate_dag("test", &graph(&[("a", &["b"]), ("b", &["a"])]));
        assert!(report.has_errors());
        let err = report.raise_if_errors().expect_err("must fail");
        assert!(err.to_string().contains("cycle"));
    }

    // Port of pyfly test_dag_with_missing_dependency_reports_error.
    #[test]
    fn dag_with_missing_dependency_reports_error() {
        let v = OrchestrationValidator::new();
        let report = v.validate_dag("test", &graph(&[("a", &["nope"])]));
        assert!(report.has_errors());
        assert!(report.issues[0].message.contains("nope"));
    }

    // Port of pyfly test_empty_graph_reports_error.
    #[test]
    fn empty_graph_reports_error() {
        let v = OrchestrationValidator::new();
        let report = v.validate_dag("test", &BTreeMap::new());
        assert!(report.has_errors());
        assert_eq!(report.issues[0].message, "no steps defined");
    }

    // Rust-specific: a real Workflow's graph validates clean.
    #[test]
    fn validates_a_workflow_graph() {
        use crate::{Node, Workflow};
        let workflow = Workflow::new("approval")
            .node(Node::new("submit", || async { Ok(()) }))
            .node(Node::new("approve", || async { Ok(()) }).depends_on(["submit"]));
        let v = OrchestrationValidator::new();
        let report = v.validate_dag(workflow.name(), &workflow.graph());
        assert!(!report.has_errors(), "{report:?}");
    }

    // Rust-specific: fail_on_warning escalates a warning-only report.
    #[test]
    fn fail_on_warning_escalates_warnings() {
        let v = OrchestrationValidator::new().fail_on_warning(true);
        let report = ValidationReport {
            issues: vec![ValidationIssue {
                target: "t".into(),
                level: IssueLevel::Warning,
                message: "slow step".into(),
            }],
        };
        assert!(!report.has_errors());
        assert!(v.fail_if_needed(&report).is_err());
        // Without the toggle a warning-only report passes.
        assert!(OrchestrationValidator::new()
            .fail_if_needed(&report)
            .is_ok());
    }

    #[test]
    fn issue_level_wire_strings() {
        assert_eq!(IssueLevel::Warning.to_string(), "WARNING");
        assert_eq!(IssueLevel::Error.to_string(), "ERROR");
    }

    // A longer cycle (a -> b -> c -> a) is also caught.
    #[test]
    fn detects_three_node_cycle() {
        let v = OrchestrationValidator::new();
        let report = v.validate_dag("t", &graph(&[("a", &["c"]), ("b", &["a"]), ("c", &["b"])]));
        assert!(report.has_errors());
    }
}
