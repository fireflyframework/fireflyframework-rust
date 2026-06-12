//! Command specification, typed argument bag, handler type, and availability.
//!
//! pyfly describes commands declaratively via `@shell_method` / `@shell_option`
//! / `@shell_argument` decorators plus type-hint inference; the resolved shape
//! is a `(key, handler, help, group, params)` tuple handed to
//! `ShellRunnerPort.register_command`. Rust has no decorator/reflection layer,
//! so the same metadata is expressed through the explicit [`CommandSpec`]
//! builder. Likewise, pyfly's `@shell_method_availability("checker")` names a
//! method that returns `""` (available) or a reason; here it becomes an
//! [`AvailabilityFn`] closure evaluated at dispatch time.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::error::ShellError;
use crate::model::{ShellParam, Value, ValueType};

/// The async handler invoked when a command runs.
///
/// It receives the parsed, typed [`CommandArgs`] and returns the textual output
/// on success, or a [`ShellError`] on failure. This is the Rust analog of the
/// `Callable[..., Any]` handler pyfly stores per command (pyfly supports both
/// sync and async handlers; the Rust port is uniformly async).
pub type Handler =
    Arc<dyn Fn(CommandArgs) -> BoxFuture<'static, Result<String, ShellError>> + Send + Sync>;

/// The closure form of pyfly's `@shell_method_availability` checker.
///
/// Returns [`Availability::Available`] when the command may run, or
/// [`Availability::Unavailable`] carrying a human-readable reason otherwise.
pub type AvailabilityFn = Arc<dyn Fn() -> Availability + Send + Sync>;

/// Whether a command may currently be dispatched.
///
/// Mirrors pyfly's availability checker contract, where an empty string means
/// "available" and any non-empty string is the unavailability reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// The command may run.
    Available,
    /// The command is blocked; the string is the reason shown to the user.
    Unavailable(String),
}

impl Availability {
    /// `true` when the command may run.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }

    /// Build an [`Availability`] from a reason string, treating an empty string
    /// as "available" exactly like pyfly's checker convention.
    #[must_use]
    pub fn from_reason(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        if reason.is_empty() {
            Availability::Available
        } else {
            Availability::Unavailable(reason)
        }
    }
}

/// The typed, parsed arguments handed to a [`Handler`].
///
/// Values are keyed by the parameter name (without dashes). Typed getters
/// coerce on lookup and fall back to a parameter's default. This replaces the
/// keyword arguments Click injects into a pyfly handler.
#[derive(Debug, Clone, Default)]
pub struct CommandArgs {
    values: HashMap<String, Value>,
}

impl CommandArgs {
    /// Create an empty argument bag.
    #[must_use]
    pub fn new() -> Self {
        CommandArgs {
            values: HashMap::new(),
        }
    }

    /// Insert (or overwrite) a value for `name`.
    pub fn insert(&mut self, name: impl Into<String>, value: Value) {
        self.values.insert(name.into(), value);
    }

    /// Return `true` when a value is present for `name`.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// The number of bound parameters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the bag is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Borrow the raw [`Value`] for `name`, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// Get the value of a string parameter.
    ///
    /// Returns `None` if the parameter is absent or not a [`Value::Str`].
    #[must_use]
    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(Value::as_str)
    }

    /// Get the value of an integer parameter.
    #[must_use]
    pub fn get_i64(&self, name: &str) -> Option<i64> {
        self.values.get(name).and_then(Value::as_i64)
    }

    /// Get the value of a float parameter.
    #[must_use]
    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.values.get(name).and_then(Value::as_f64)
    }

    /// Get the value of a boolean parameter.
    ///
    /// Absent flags read as `false` (matching Click's flag default), so this
    /// returns `Some(false)` when the parameter is missing.
    #[must_use]
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        match self.values.get(name) {
            Some(v) => v.as_bool(),
            None => Some(false),
        }
    }
}

/// A fully described, registrable command.
///
/// Built with the fluent builder methods. The resolved shape — name, help,
/// group, ordered parameters, handler, and optional availability checker —
/// corresponds to pyfly's `register_command(key, handler, help_text=, group=,
/// params=)` arguments.
#[derive(Clone)]
pub struct CommandSpec {
    pub(crate) name: String,
    pub(crate) help: String,
    pub(crate) group: String,
    pub(crate) params: Vec<ShellParam>,
    pub(crate) handler: Handler,
    pub(crate) availability: Option<AvailabilityFn>,
}

impl CommandSpec {
    /// Begin building a command named `name` (the command key) bound to
    /// `handler`.
    #[must_use]
    pub fn new(name: impl Into<String>, handler: Handler) -> Self {
        CommandSpec {
            name: name.into(),
            help: String::new(),
            group: String::new(),
            params: Vec::new(),
            handler,
            availability: None,
        }
    }

    /// The command key.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The command group, or an empty string when ungrouped.
    #[must_use]
    pub fn group(&self) -> &str {
        &self.group
    }

    /// The help text.
    #[must_use]
    pub fn help_text(&self) -> &str {
        &self.help
    }

    /// The ordered parameter specs.
    #[must_use]
    pub fn params(&self) -> &[ShellParam] {
        &self.params
    }

    /// Set the help text (pyfly's `help=`).
    #[must_use]
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = help.into();
        self
    }

    /// Place the command in a sub-group (pyfly's `group=`). Invoked as
    /// `<group> <name>`.
    #[must_use]
    pub fn group_name(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Append a pre-built [`ShellParam`].
    #[must_use]
    pub fn param(mut self, param: ShellParam) -> Self {
        self.params.push(param);
        self
    }

    /// Append a required positional argument.
    #[must_use]
    pub fn arg(self, name: impl Into<String>, value_type: ValueType) -> Self {
        self.param(ShellParam::arg(name, value_type))
    }

    /// Append a value option (`--name value`). It is required unless a default
    /// is set via the returned builder.
    #[must_use]
    pub fn option(self, name: impl Into<String>, value_type: ValueType) -> Self {
        self.param(ShellParam::option(name, value_type))
    }

    /// Append a boolean flag (`--name`).
    #[must_use]
    pub fn flag(self, name: impl Into<String>) -> Self {
        self.param(ShellParam::flag(name))
    }

    /// Attach an availability checker (pyfly's `@shell_method_availability`).
    ///
    /// The closure is evaluated at dispatch time; when it returns
    /// [`Availability::Unavailable`] the command is blocked and excluded from
    /// help output.
    #[must_use]
    pub fn availability<F>(mut self, checker: F) -> Self
    where
        F: Fn() -> Availability + Send + Sync + 'static,
    {
        self.availability = Some(Arc::new(checker));
        self
    }

    /// Evaluate the availability checker, if any. A command with no checker is
    /// always available.
    #[must_use]
    pub fn evaluate_availability(&self) -> Availability {
        match &self.availability {
            Some(check) => check(),
            None => Availability::Available,
        }
    }
}

impl std::fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSpec")
            .field("name", &self.name)
            .field("help", &self.help)
            .field("group", &self.group)
            .field("params", &self.params)
            .field("has_availability", &self.availability.is_some())
            .finish()
    }
}

/// Convenience for building a [`Handler`] from an async closure.
///
/// ```
/// use firefly_shell::{handler, CommandArgs};
///
/// let h = handler(|args: CommandArgs| async move {
///     Ok(format!("hi {}", args.get_str("name").unwrap_or("there")))
/// });
/// ```
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(CommandArgs) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, ShellError>> + Send + 'static,
{
    Arc::new(move |args| Box::pin(f(args)))
}
