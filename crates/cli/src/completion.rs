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

//! `firefly completion <shell>` — shell-completion script generation.
//!
//! Rust port of pyfly's `completion.py` (which leans on Click's
//! `shell_completion`). Here the work is done for free by
//! [`clap_complete::generate`] against the existing [`crate::cli::Cli`] clap
//! [`Parser`](clap::Parser) derive: clap knows every subcommand, argument, and
//! value-parser choice, so the emitted script is always in sync with the CLI.

use std::io::Write;

use clap::CommandFactory;
use clap_complete::Shell;

/// The binary name the generated completion script drives. Kept in one place so
/// the script and the `[[bin]]` name in `Cargo.toml` never drift.
pub const BIN_NAME: &str = "firefly";

/// Render the shell-completion script for `shell` to `out`.
///
/// This builds the clap [`Command`](clap::Command) from the `firefly` CLI
/// definition and hands it to [`clap_complete::generate`], which writes a
/// ready-to-source completion script for the requested shell.
///
/// Install it by sourcing the output, e.g.:
/// - bash:       `eval "$(firefly completion bash)"`   (add to `~/.bashrc`)
/// - zsh:        `eval "$(firefly completion zsh)"`     (add to `~/.zshrc`)
/// - fish:       `firefly completion fish | source`     (add to fish config)
/// - powershell: `firefly completion powershell | Out-String | Invoke-Expression`
///
/// # Examples
/// ```
/// use clap_complete::Shell;
/// let mut buf = Vec::new();
/// firefly_cli::completion::write_completion(Shell::Bash, &mut buf);
/// assert!(!buf.is_empty());
/// ```
pub fn write_completion<W: Write>(shell: Shell, out: &mut W) {
    let mut cmd = crate::cli::Cli::command();
    clap_complete::generate(shell, &mut cmd, BIN_NAME, out);
}

/// Render the shell-completion script for `shell` into a `String`.
///
/// Convenience wrapper over [`write_completion`] for callers (and tests) that
/// want the script as text rather than streaming it to a writer. The generated
/// scripts are UTF-8, so the lossy conversion is a no-op in practice.
///
/// # Examples
/// ```
/// use clap_complete::Shell;
/// let script = firefly_cli::completion::completion_script(Shell::Zsh);
/// assert!(!script.is_empty());
/// ```
pub fn completion_script(shell: Shell) -> String {
    let mut buf = Vec::new();
    write_completion(shell, &mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_produces_a_non_empty_script() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let script = completion_script(shell);
            assert!(
                !script.trim().is_empty(),
                "completion script for {shell:?} was empty"
            );
        }
    }

    #[test]
    fn scripts_reference_the_binary_name() {
        // Each shell mentions the prog name somewhere in the emitted script.
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
            let script = completion_script(shell);
            assert!(
                script.contains(BIN_NAME),
                "completion script for {shell:?} did not mention {BIN_NAME}"
            );
        }
    }

    #[test]
    fn write_completion_streams_into_a_writer() {
        let mut buf = Vec::new();
        write_completion(Shell::Bash, &mut buf);
        assert!(!buf.is_empty());
        // A bash completion script registers a `complete` directive.
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("complete"));
    }
}
