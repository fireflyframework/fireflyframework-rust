//! The [`ShellRunner`] trait and the default [`StdShell`] implementation.
//!
//! [`StdShell`] is the hand-rolled Rust analog of pyfly's `ClickShellAdapter`:
//! a command registry plus an argument tokenizer/parser supporting positional
//! arguments, `--opt value`, `--opt=value`, boolean flags, choice validation,
//! required-option/argument enforcement, grouped commands, generated help, and
//! a line-based interactive REPL. Exit codes follow pyfly's mapping: `0` on
//! success, `2` for usage errors (unknown command, bad args, invalid choice,
//! missing required), and `1` for runtime/availability errors.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use async_trait::async_trait;

use crate::command::{Availability, CommandArgs, CommandSpec};
use crate::error::ShellError;
use crate::model::{ShellParam, Value, ValueType};

/// Abstract shell runner interface.
///
/// Rust port of pyfly's `ShellRunnerPort` protocol. Any shell adapter must
/// support registering commands, batch-dispatching an argument vector (yielding
/// an exit code), and running an interactive REPL.
#[async_trait]
pub trait ShellRunner: Send + Sync {
    /// Register a command spec with this runner (pyfly's `register_command`).
    fn register(&mut self, spec: CommandSpec);

    /// Dispatch `args`, returning the process-style exit code (pyfly's
    /// `run`).
    async fn run(&self, args: &[String]) -> i32;

    /// Run the interactive REPL until end-of-input (pyfly's `run_interactive`).
    async fn run_interactive(&self) -> std::io::Result<()>;
}

/// The default in-process shell runner.
///
/// Holds a registry of [`CommandSpec`]s keyed by their dispatch path
/// (`"name"` or `"group name"`) and provides [`StdShell::invoke`], the direct
/// analog of pyfly's `ClickShellAdapter.invoke` returning `(exit_code,
/// output)`.
#[derive(Default)]
pub struct StdShell {
    name: String,
    help_text: String,
    /// Ungrouped commands, keyed by command name. `BTreeMap` keeps help output
    /// deterministic.
    commands: BTreeMap<String, CommandSpec>,
    /// Grouped commands: group -> (command name -> spec).
    groups: BTreeMap<String, BTreeMap<String, CommandSpec>>,
}

impl StdShell {
    /// Create a shell with an application `name` and top-level `help_text`
    /// (pyfly's `ClickShellAdapter(name=, help_text=)`).
    #[must_use]
    pub fn new(name: impl Into<String>, help_text: impl Into<String>) -> Self {
        StdShell {
            name: name.into(),
            help_text: help_text.into(),
            commands: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }

    /// Register a command spec.
    pub fn register_command(&mut self, spec: CommandSpec) {
        if spec.group.is_empty() {
            self.commands.insert(spec.name.clone(), spec);
        } else {
            self.groups
                .entry(spec.group.clone())
                .or_default()
                .insert(spec.name.clone(), spec);
        }
    }

    /// The application name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Resolve a token sequence to a command and its remaining argument tokens.
    ///
    /// Handles the two-level dispatch (`group cmd ...`) used by pyfly's Click
    /// sub-groups, and recognises a bare `--help` / `-h` request.
    fn resolve<'a>(&'a self, tokens: &'a [String]) -> Result<Resolved<'a>, ShellError> {
        if tokens.is_empty() {
            // No command: show top-level help (Click prints group help, exit 0).
            return Ok(Resolved::TopHelp);
        }

        let first = tokens[0].as_str();

        if first == "--help" || first == "-h" {
            return Ok(Resolved::TopHelp);
        }

        // Top-level command?
        if let Some(spec) = self.commands.get(first) {
            return Ok(Resolved::Command {
                spec,
                rest: &tokens[1..],
            });
        }

        // A group?
        if let Some(group_map) = self.groups.get(first) {
            if tokens.len() < 2 {
                return Ok(Resolved::GroupHelp(first));
            }
            let sub = tokens[1].as_str();
            if sub == "--help" || sub == "-h" {
                return Ok(Resolved::GroupHelp(first));
            }
            if let Some(spec) = group_map.get(sub) {
                return Ok(Resolved::Command {
                    spec,
                    rest: &tokens[2..],
                });
            }
            return Err(ShellError::NoSuchCommand(sub.to_string()));
        }

        Err(ShellError::NoSuchCommand(first.to_string()))
    }

    /// Invoke the shell with the given tokens, returning `(exit_code, output)`.
    ///
    /// Direct analog of pyfly's `ClickShellAdapter.invoke`. Errors are mapped
    /// to exit codes per [`ShellError::exit_code`]; a successful handler yields
    /// exit code `0` and its returned string as output.
    pub async fn invoke(&self, tokens: &[String]) -> (i32, String) {
        match self.resolve(tokens) {
            Ok(Resolved::TopHelp) => (0, self.render_help()),
            Ok(Resolved::GroupHelp(group)) => (0, self.render_group_help(group)),
            Ok(Resolved::Command { spec, rest }) => self.dispatch(spec, rest).await,
            Err(err) => (err.exit_code(), err.to_string()),
        }
    }

    /// Parse `rest` against `spec`, enforce availability, and run the handler.
    async fn dispatch(&self, spec: &CommandSpec, rest: &[String]) -> (i32, String) {
        // A bare --help on the command shows the command's help.
        if rest.iter().any(|t| t == "--help" || t == "-h") {
            return (0, self.render_command_help(spec));
        }

        match spec.evaluate_availability() {
            Availability::Available => {}
            Availability::Unavailable(reason) => {
                let err = ShellError::Unavailable(reason);
                return (err.exit_code(), err.to_string());
            }
        }

        let args = match parse_args(spec, rest) {
            Ok(args) => args,
            Err(err) => return (err.exit_code(), err.to_string()),
        };

        match (spec.handler)(args).await {
            Ok(output) => (0, output),
            Err(err) => (err.exit_code(), err.to_string()),
        }
    }

    // ---- help rendering -----------------------------------------------------

    /// Render top-level help: usage line, optional description, and grouped
    /// command listing. Unavailable commands are omitted (matching pyfly's
    /// "hidden from help" behavior).
    #[must_use]
    pub fn render_help(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Usage: {} [OPTIONS] COMMAND [ARGS]...\n",
            self.name
        ));
        if !self.help_text.is_empty() {
            out.push('\n');
            out.push_str(&self.help_text);
            out.push('\n');
        }

        let available: Vec<&CommandSpec> = self
            .commands
            .values()
            .filter(|c| c.evaluate_availability().is_available())
            .collect();

        if !available.is_empty() {
            out.push_str("\nCommands:\n");
            for spec in available {
                out.push_str(&format!("  {}{}\n", spec.name, help_suffix(spec)));
            }
        }

        if !self.groups.is_empty() {
            out.push_str("\nGroups:\n");
            for group in self.groups.keys() {
                out.push_str(&format!("  {group}\n"));
            }
        }
        out
    }

    /// Render the command listing for a single group.
    #[must_use]
    pub fn render_group_help(&self, group: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Usage: {} {} COMMAND [ARGS]...\n",
            self.name, group
        ));
        if let Some(map) = self.groups.get(group) {
            out.push_str("\nCommands:\n");
            for spec in map.values() {
                if spec.evaluate_availability().is_available() {
                    out.push_str(&format!("  {}{}\n", spec.name, help_suffix(spec)));
                }
            }
        }
        out
    }

    /// Render the help for a single command (its usage line + parameter help).
    #[must_use]
    pub fn render_command_help(&self, spec: &CommandSpec) -> String {
        let mut out = String::new();
        let mut usage = format!("Usage: {} {}", self.name, spec.name);
        for p in &spec.params {
            if p.is_option {
                if p.required {
                    usage.push_str(&format!(" --{} <{}>", p.name, p.value_type));
                } else {
                    usage.push_str(&format!(" [--{}]", p.name));
                }
            } else if p.required {
                usage.push_str(&format!(" {}", p.name.to_uppercase()));
            } else {
                usage.push_str(&format!(" [{}]", p.name.to_uppercase()));
            }
        }
        usage.push('\n');
        out.push_str(&usage);
        if !spec.help.is_empty() {
            out.push('\n');
            out.push_str(&spec.help);
            out.push('\n');
        }
        out
    }

    // ---- REPL ---------------------------------------------------------------

    /// Run a line-based REPL reading from `input` and writing prompts/output to
    /// `output`.
    ///
    /// This is the generic, testable form of pyfly's `run_interactive`: each
    /// non-blank line is tokenized on whitespace and dispatched via
    /// [`StdShell::invoke`]; non-empty command output is written back. The loop
    /// ends at end-of-input (the Rust analog of pyfly's `EOFError`). Tests feed
    /// scripted input through any [`std::io::BufRead`].
    pub async fn run_repl<R, W>(&self, mut input: R, mut output: W) -> std::io::Result<()>
    where
        R: BufRead,
        W: Write,
    {
        loop {
            write!(output, "> ")?;
            output.flush()?;

            let mut line = String::new();
            let read = input.read_line(&mut line)?;
            if read == 0 {
                // EOF.
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let tokens: Vec<String> = line.split_whitespace().map(ToString::to_string).collect();
            let (_code, out) = self.invoke(&tokens).await;
            if !out.is_empty() {
                writeln!(output, "{out}")?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ShellRunner for StdShell {
    fn register(&mut self, spec: CommandSpec) {
        self.register_command(spec);
    }

    async fn run(&self, args: &[String]) -> i32 {
        let (code, _) = self.invoke(args).await;
        code
    }

    async fn run_interactive(&self) -> std::io::Result<()> {
        // Read each line and write output without holding a non-`Send` stdio
        // lock across an `.await`, so this future stays `Send` (as
        // `#[async_trait]` requires). `std::io::Stdin`/`Stdout` handles are
        // themselves `Send`; only their `*Lock` guards are not, so we scope the
        // locking into synchronous blocks.
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        loop {
            {
                use std::io::Write as _;
                let mut out = stdout.lock();
                write!(out, "> ")?;
                out.flush()?;
            }
            let mut line = String::new();
            let read = {
                use std::io::BufRead as _;
                stdin.lock().read_line(&mut line)?
            };
            if read == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let tokens: Vec<String> = trimmed
                .split_whitespace()
                .map(ToString::to_string)
                .collect();
            let (_code, output) = self.invoke(&tokens).await;
            if !output.is_empty() {
                use std::io::Write as _;
                let mut out = stdout.lock();
                writeln!(out, "{output}")?;
            }
        }
        Ok(())
    }
}

/// The outcome of resolving a token sequence to a target.
enum Resolved<'a> {
    /// Show the top-level help.
    TopHelp,
    /// Show a group's command listing.
    GroupHelp(&'a str),
    /// Dispatch a concrete command with the remaining argument tokens.
    Command {
        spec: &'a CommandSpec,
        rest: &'a [String],
    },
}

/// Parse the argument tokens for a command into a typed [`CommandArgs`] bag.
///
/// Implements the parser semantics exercised by pyfly's Click adapter tests:
/// positional arguments consumed in order, `--opt value` / `--opt=value`
/// options, boolean flags, choice validation, type coercion, and
/// required-parameter enforcement. Unknown options and surplus positionals are
/// usage errors.
fn parse_args(spec: &CommandSpec, tokens: &[String]) -> Result<CommandArgs, ShellError> {
    // Partition parameter specs.
    let positionals: Vec<&ShellParam> = spec.params.iter().filter(|p| !p.is_option).collect();
    let options: Vec<&ShellParam> = spec.params.iter().filter(|p| p.is_option).collect();

    let find_option = |name: &str| -> Option<&ShellParam> {
        options
            .iter()
            .copied()
            .find(|p| p.name == name || p.name.replace('_', "-") == name)
    };

    let mut args = CommandArgs::new();
    let mut seen: Vec<String> = Vec::new();
    let mut positional_values: Vec<String> = Vec::new();

    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if let Some(stripped) = tok.strip_prefix("--") {
            // Split `--name=value` at the first `=`.
            let (name, inline_value) = match stripped.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (stripped, None),
            };
            let Some(param) = find_option(name) else {
                return Err(ShellError::usage(format!("No such option: --{name}")));
            };

            if param.is_flag {
                if inline_value.is_some() {
                    return Err(ShellError::usage(format!(
                        "Option --{name} is a flag and takes no value"
                    )));
                }
                args.insert(param.name.clone(), Value::Bool(true));
                seen.push(param.name.clone());
                i += 1;
                continue;
            }

            // Value option: take inline value or the next token.
            let raw = match inline_value {
                Some(v) => v,
                None => {
                    i += 1;
                    if i >= tokens.len() {
                        return Err(ShellError::usage(format!(
                            "Option --{name} requires an argument"
                        )));
                    }
                    tokens[i].clone()
                }
            };
            validate_choice(param, &raw)?;
            let value = coerce(param.value_type, &raw, &param.name)?;
            args.insert(param.name.clone(), value);
            seen.push(param.name.clone());
            i += 1;
        } else {
            positional_values.push(tok.clone());
            i += 1;
        }
    }

    // Bind positionals in order.
    if positional_values.len() > positionals.len() {
        let extra = &positional_values[positionals.len()];
        return Err(ShellError::usage(format!(
            "Got unexpected extra argument ({extra})"
        )));
    }
    for (param, raw) in positionals.iter().zip(positional_values.iter()) {
        validate_choice(param, raw)?;
        let value = coerce(param.value_type, raw, &param.name)?;
        args.insert(param.name.clone(), value);
        seen.push(param.name.clone());
    }

    // Apply defaults and enforce required parameters.
    for param in &spec.params {
        if seen.iter().any(|s| s == &param.name) {
            continue;
        }
        if let Some(default) = &param.default {
            args.insert(param.name.clone(), default.clone());
        } else if param.required {
            if param.is_option {
                return Err(ShellError::usage(format!(
                    "Missing option '--{}'.",
                    param.name
                )));
            }
            return Err(ShellError::usage(format!(
                "Missing argument '{}'.",
                param.name.to_uppercase()
            )));
        }
    }

    Ok(args)
}

/// Validate a raw token against a parameter's `choices`, if any.
fn validate_choice(param: &ShellParam, raw: &str) -> Result<(), ShellError> {
    if let Some(choices) = &param.choices {
        if !choices.iter().any(|c| c == raw) {
            let joined = choices.join(", ");
            return Err(ShellError::usage(format!(
                "Invalid value for '{}': '{raw}' is not one of {joined}.",
                param.name
            )));
        }
    }
    Ok(())
}

/// Coerce a raw token to the parameter's [`ValueType`], producing a usage error
/// on failure (matching Click's type-coercion error → exit 2).
fn coerce(ty: ValueType, raw: &str, name: &str) -> Result<Value, ShellError> {
    match ty {
        ValueType::Str => Ok(Value::Str(raw.to_string())),
        ValueType::Int => raw.parse::<i64>().map(Value::Int).map_err(|_| {
            ShellError::usage(format!("'{raw}' is not a valid integer for '{name}'."))
        }),
        ValueType::Float => raw
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| ShellError::usage(format!("'{raw}' is not a valid float for '{name}'."))),
        ValueType::Bool => parse_bool(raw).map(Value::Bool).ok_or_else(|| {
            ShellError::usage(format!("'{raw}' is not a valid boolean for '{name}'."))
        }),
    }
}

/// Parse a boolean the way Click does (`true/false`, `1/0`, `yes/no`,
/// `t/f`, `y/n`, `on/off`, case-insensitive).
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "f" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

/// Build the trailing `  help text` shown beside a command in listings.
fn help_suffix(spec: &CommandSpec) -> String {
    if spec.help.is_empty() {
        String::new()
    } else {
        format!("  {}", spec.help)
    }
}
