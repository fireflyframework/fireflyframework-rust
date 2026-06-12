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

//! Error types for the shell subsystem.

use thiserror::Error;

/// Errors raised while parsing arguments or dispatching a shell command.
///
/// The exit-code mapping mirrors pyfly's `ClickShellAdapter.invoke`:
/// a [`ShellError::Usage`] maps to exit code `2`, every other variant maps to
/// exit code `1`, and success is `0`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// The command name was not found in the registry (Click's
    /// "No such command" usage error → exit `2`).
    #[error("No such command '{0}'.")]
    NoSuchCommand(String),

    /// A usage error: bad argument count, unknown option, invalid choice,
    /// missing required option/argument, or a type-coercion failure. Maps to
    /// exit code `2`.
    #[error("{0}")]
    Usage(String),

    /// The command is currently unavailable; the contained string is the reason
    /// returned by the availability checker. Maps to exit code `1`.
    #[error("{0}")]
    Unavailable(String),

    /// The handler returned an error at runtime. Maps to exit code `1`.
    #[error("{0}")]
    Handler(String),
}

impl ShellError {
    /// Construct a [`ShellError::Usage`] from any displayable message.
    pub fn usage(msg: impl Into<String>) -> Self {
        ShellError::Usage(msg.into())
    }

    /// Construct a [`ShellError::Handler`] from any displayable message.
    pub fn handler(msg: impl Into<String>) -> Self {
        ShellError::Handler(msg.into())
    }

    /// The process-style exit code this error maps to.
    ///
    /// Usage / unknown-command errors are `2`; runtime and availability errors
    /// are `1`.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            ShellError::NoSuchCommand(_) | ShellError::Usage(_) => 2,
            ShellError::Unavailable(_) | ShellError::Handler(_) => 1,
        }
    }
}
