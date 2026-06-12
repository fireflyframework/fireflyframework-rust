//! `firefly info` and `firefly doctor` — environment diagnostics.
//!
//! Rust-adapted port of pyfly's `info.py` and `doctor.py`. Where pyfly probes
//! the Python interpreter and importable extras, the Rust port probes the
//! toolchain (`rustc`, `cargo`, `git`) and reports the build environment.

use std::process::Command;

/// One row of a diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The tool or property being reported.
    pub name: String,
    /// `true` when present/passing.
    pub ok: bool,
    /// A short detail (version string, description, or "not found").
    pub detail: String,
}

/// The result of a full `doctor` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// Required tools (a missing one fails the run).
    pub required: Vec<Check>,
    /// Optional tools (a missing one is merely noted).
    pub optional: Vec<Check>,
    /// `true` when every required tool was found.
    pub all_ok: bool,
}

/// The framework version this CLI was built from.
pub const FRAMEWORK_VERSION: &str = "26.6.1";

/// Run a `<tool> --version` and return its first trimmed output line, or `None`
/// when the tool is missing or errors.
fn tool_version(tool: &str) -> Option<String> {
    let output = Command::new(tool).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().next().map(|l| l.trim().to_string())
}

/// Required tools for a firefly-rust workspace, with descriptions.
const REQUIRED_TOOLS: &[(&str, &str)] = &[
    ("rustc", "Rust compiler"),
    ("cargo", "Rust package manager"),
];

/// Optional tools that enhance the workflow.
const OPTIONAL_TOOLS: &[(&str, &str)] = &[
    ("git", "Version control"),
    ("clippy-driver", "Linter (cargo clippy)"),
    ("rustfmt", "Formatter (cargo fmt)"),
    ("docker", "Container builds"),
];

/// Run the doctor checks and return a structured report.
///
/// This is pure data (no printing) so it can be asserted in tests; the binary
/// renders it. Mirrors pyfly's `doctor_command` check set, retargeted to Rust.
pub fn run_doctor() -> DoctorReport {
    let required: Vec<Check> = REQUIRED_TOOLS
        .iter()
        .map(|(tool, desc)| match tool_version(tool) {
            Some(v) => Check {
                name: (*tool).to_string(),
                ok: true,
                detail: v,
            },
            None => Check {
                name: (*tool).to_string(),
                ok: false,
                detail: format!("{desc} (not found)"),
            },
        })
        .collect();
    let optional: Vec<Check> = OPTIONAL_TOOLS
        .iter()
        .map(|(tool, desc)| match tool_version(tool) {
            Some(v) => Check {
                name: (*tool).to_string(),
                ok: true,
                detail: v,
            },
            None => Check {
                name: (*tool).to_string(),
                ok: false,
                detail: format!("{desc} (not found)"),
            },
        })
        .collect();
    let all_ok = required.iter().all(|c| c.ok);
    DoctorReport {
        required,
        optional,
        all_ok,
    }
}

/// A single environment/info row for `firefly info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoRow {
    /// Property key.
    pub key: String,
    /// Property value.
    pub value: String,
}

/// Gather framework + environment info rows.
///
/// Mirrors pyfly's `info_command` environment table, with Rust-relevant keys.
pub fn run_info() -> Vec<InfoRow> {
    let row = |k: &str, v: String| InfoRow {
        key: k.to_string(),
        value: v,
    };
    vec![
        row(
            "Framework",
            format!("Firefly for Rust v{FRAMEWORK_VERSION}"),
        ),
        row("OS", std::env::consts::OS.to_string()),
        row("Architecture", std::env::consts::ARCH.to_string()),
        row(
            "rustc",
            tool_version("rustc").unwrap_or_else(|| "not found".to_string()),
        ),
        row(
            "cargo",
            tool_version("cargo").unwrap_or_else(|| "not found".to_string()),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_reports_rustc_and_cargo() {
        // Running under cargo test guarantees the toolchain is present.
        let report = run_doctor();
        assert!(report.required.iter().any(|c| c.name == "rustc"));
        assert!(report.required.iter().any(|c| c.name == "cargo"));
        // The test runner implies cargo+rustc exist, so the run should pass.
        assert!(report.all_ok);
    }

    #[test]
    fn info_includes_framework_version() {
        let rows = run_info();
        let fw = rows.iter().find(|r| r.key == "Framework").unwrap();
        assert!(fw.value.contains(FRAMEWORK_VERSION));
        assert!(rows.iter().any(|r| r.key == "OS"));
        assert!(rows.iter().any(|r| r.key == "Architecture"));
    }
}
