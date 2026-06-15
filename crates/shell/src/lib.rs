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

//! firefly-shell — Spring-Shell-style command registry, parser, REPL,
//! ApplicationArguments, and post-startup runners.
//!
//! This crate is the Rust port of pyfly's `pyfly.shell` package, which itself
//! mirrors Spring Shell (`@ShellMethod` / `@ShellOption` / `@ShellArgument`,
//! command availability, grouped help, interactive REPL) and Spring Boot's
//! post-startup hooks (`CommandLineRunner`, `ApplicationRunner`,
//! `ApplicationArguments`).
//!
//! # Mapping from pyfly
//!
//! pyfly is decorator- and reflection-driven: `@shell_method` /
//! `@shell_option` / `@shell_argument` attach metadata that
//! [`param_inference`][pyfly] resolves into `ShellParam`s, and a `Click`
//! adapter dispatches them. Rust has no decorator/reflection layer, so this
//! port replaces those mechanisms with explicit, type-safe equivalents:
//!
//! | pyfly | firefly-shell |
//! |-------|---------------|
//! | `ShellParam` dataclass / `MISSING` | [`ShellParam`] with `Option<Value>` defaults |
//! | `param_type: type` | [`ValueType`] enum (`Str`/`Int`/`Float`/`Bool`) |
//! | `CommandResult` | [`CommandResult`] |
//! | `@shell_method` + `@shell_option`/`@shell_argument` | [`CommandSpec`] builder |
//! | `@shell_method_availability("checker")` | [`CommandSpec::availability`] closure |
//! | Click handler kwargs | [`CommandArgs`] typed getters |
//! | `ShellRunnerPort` protocol | [`ShellRunner`] trait |
//! | `ClickShellAdapter` | [`StdShell`] |
//! | `ApplicationArguments` | [`ApplicationArguments`] (exact semantics) |
//! | `CommandLineRunner` / `ApplicationRunner` | [`CommandLineRunner`] / [`ApplicationRunner`] traits |
//! | context-side runner invocation | [`RunnerRegistry`] |
//!
//! [pyfly]: https://github.com/fireflyframework/fireflyframework-pyfly
//!
//! # Example
//!
//! ```
//! use firefly_shell::{handler, CommandArgs, CommandSpec, ShellParam, StdShell, ValueType};
//!
//! # async fn demo() {
//! let mut shell = StdShell::new("app", "Demo CLI");
//! shell.register_command(
//!     CommandSpec::new(
//!         "greet",
//!         handler(|args: CommandArgs| async move {
//!             Ok(format!("Hello, {}!", args.get_str("name").unwrap_or("world")))
//!         }),
//!     )
//!     .help("Greet someone")
//!     .arg("name", ValueType::Str)
//!     .option("times", ValueType::Int)
//!     .param(ShellParam::option("times", ValueType::Int).with_default(firefly_shell::Value::Int(1))),
//! );
//!
//! let (code, output) = shell.invoke(&["greet".into(), "John".into()]).await;
//! assert_eq!(code, 0);
//! assert_eq!(output, "Hello, John!");
//! # }
//! ```

mod command;
mod error;
mod model;
mod runner;
mod shell;

pub use command::{handler, Availability, AvailabilityFn, CommandArgs, CommandSpec, Handler};
pub use error::ShellError;
pub use model::{CommandResult, ShellParam, Value, ValueType};
pub use runner::{ApplicationArguments, ApplicationRunner, CommandLineRunner, RunnerRegistry};
pub use shell::{ShellRunner, StdShell};

/// Framework version stamp.
pub const VERSION: &str = "26.6.10";
