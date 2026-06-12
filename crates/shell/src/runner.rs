//! Post-startup runners and parsed command-line arguments.
//!
//! Rust port of pyfly's `pyfly.shell.runner` module, which mirrors Spring
//! Boot's `CommandLineRunner`, `ApplicationRunner`, and `ApplicationArguments`.
//! pyfly discovers runner beans by structural typing and invokes them in order
//! after the context starts; here registration is explicit via
//! [`RunnerRegistry`].

use async_trait::async_trait;
use std::sync::Arc;

/// Parsed representation of command-line arguments.
///
/// Separates raw CLI tokens into *option* arguments (those starting with
/// `--`) and *non-option* arguments (everything else). This is a faithful port
/// of pyfly's `ApplicationArguments` dataclass, preserving its exact
/// `from_args` / `contains_option` / `get_option_values` semantics — including
/// prefix matching and values that themselves contain `=`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplicationArguments {
    /// The original, unmodified argument list.
    pub source_args: Vec<String>,
    /// Arguments that start with `--` (`--flag`, `--key=value`).
    pub option_args: Vec<String>,
    /// Arguments that do not start with `--`.
    pub non_option_args: Vec<String>,
}

impl ApplicationArguments {
    /// Parse raw CLI args into option (`--key=value`, `--flag`) and non-option
    /// groups.
    ///
    /// `source_args` is an independent copy, so mutating the caller's input
    /// afterward never affects the parsed result (matching pyfly's
    /// `list(args)` copy semantics).
    #[must_use]
    pub fn from_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let source: Vec<String> = args.into_iter().map(Into::into).collect();
        let option_args: Vec<String> = source
            .iter()
            .filter(|a| a.starts_with("--"))
            .cloned()
            .collect();
        let non_option_args: Vec<String> = source
            .iter()
            .filter(|a| !a.starts_with("--"))
            .cloned()
            .collect();
        ApplicationArguments {
            source_args: source,
            option_args,
            non_option_args,
        }
    }

    /// Check whether `--name` (flag) or `--name=value` is present.
    ///
    /// Matches exactly: `--verbose-mode` does **not** satisfy
    /// `contains_option("verbose")`, because only an exact `--name` or a
    /// `--name=` prefix counts.
    #[must_use]
    pub fn contains_option(&self, name: &str) -> bool {
        let prefix = format!("--{name}");
        let eq_prefix = format!("{prefix}=");
        self.option_args
            .iter()
            .any(|opt| *opt == prefix || opt.starts_with(&eq_prefix))
    }

    /// Get every value supplied as `--name=value`.
    ///
    /// A bare flag (`--name` with no `=`) contributes nothing. Values that
    /// themselves contain `=` are returned intact (e.g. `--formula=a=b` yields
    /// `["a=b"]`), because only the first `=` after the name is the separator.
    #[must_use]
    pub fn get_option_values(&self, name: &str) -> Vec<String> {
        let prefix = format!("--{name}=");
        self.option_args
            .iter()
            .filter_map(|opt| opt.strip_prefix(&prefix).map(ToString::to_string))
            .collect()
    }
}

/// Receives raw CLI args after the application context has started.
///
/// Rust port of pyfly's `CommandLineRunner` protocol. Implementors are run in
/// registration order by [`RunnerRegistry::run_all`].
#[async_trait]
pub trait CommandLineRunner: Send + Sync {
    /// Run with the raw argument tokens.
    async fn run(&self, args: &[String]);
}

/// Receives parsed [`ApplicationArguments`] after the application context has
/// started.
///
/// Rust port of pyfly's `ApplicationRunner` protocol.
#[async_trait]
pub trait ApplicationRunner: Send + Sync {
    /// Run with the parsed application arguments.
    async fn run(&self, args: &ApplicationArguments);
}

/// An ordered registry of [`CommandLineRunner`] and [`ApplicationRunner`]
/// instances.
///
/// pyfly discovers runner beans during context start and invokes them; the
/// Rust port keeps the same ordered-invocation behavior but uses explicit
/// registration. Runners are invoked in the order they were registered,
/// interleaved across both kinds by registration sequence.
#[derive(Default, Clone)]
pub struct RunnerRegistry {
    entries: Vec<RunnerEntry>,
}

#[derive(Clone)]
enum RunnerEntry {
    CommandLine(Arc<dyn CommandLineRunner>),
    Application(Arc<dyn ApplicationRunner>),
}

impl RunnerRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        RunnerRegistry {
            entries: Vec::new(),
        }
    }

    /// Register a [`CommandLineRunner`]. Returns `&mut self` for chaining.
    pub fn add_command_line_runner(&mut self, runner: Arc<dyn CommandLineRunner>) -> &mut Self {
        self.entries.push(RunnerEntry::CommandLine(runner));
        self
    }

    /// Register an [`ApplicationRunner`]. Returns `&mut self` for chaining.
    pub fn add_application_runner(&mut self, runner: Arc<dyn ApplicationRunner>) -> &mut Self {
        self.entries.push(RunnerEntry::Application(runner));
        self
    }

    /// Total number of registered runners.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry has no runners.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Invoke every registered runner in registration order.
    ///
    /// `CommandLineRunner`s receive the raw `args`; `ApplicationRunner`s receive
    /// the same arguments parsed into [`ApplicationArguments`] (parsed once and
    /// reused). This mirrors pyfly's `_invoke_runners`.
    pub async fn run_all(&self, args: &[String]) {
        let parsed = ApplicationArguments::from_args(args.iter().cloned());
        for entry in &self.entries {
            match entry {
                RunnerEntry::CommandLine(r) => r.run(args).await,
                RunnerEntry::Application(r) => r.run(&parsed).await,
            }
        }
    }
}
