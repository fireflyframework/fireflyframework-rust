//! Core data models for the shell subsystem: [`ValueType`], [`Value`],
//! [`ShellParam`], and [`CommandResult`].
//!
//! These are the Rust port of pyfly's `pyfly.shell.result` module
//! (`ShellParam`, `CommandResult`, and the `MISSING` sentinel). Python uses a
//! reflective `type` object to describe a parameter's value type; Rust replaces
//! that with the closed [`ValueType`] enum, and the `MISSING` / default
//! distinction is modelled with `Option<Value>` (`None` == `MISSING`).

use std::fmt;

/// The value type of a shell parameter.
///
/// This is the Rust analog of pyfly's `param_type: type` field, which maps the
/// Python types `str`, `int`, `float`, and `bool` onto Click parameter types.
/// Because Rust has no first-class runtime `type` value, the four supported
/// kinds are enumerated explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    /// A UTF-8 string value (Python `str`).
    Str,
    /// A 64-bit signed integer value (Python `int`).
    Int,
    /// A 64-bit floating-point value (Python `float`).
    Float,
    /// A boolean value (Python `bool`).
    Bool,
}

impl ValueType {
    /// Return the lowercase canonical name of this type (`"str"`, `"int"`,
    /// `"float"`, `"bool"`), matching the Python type `__name__`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ValueType::Str => "str",
            ValueType::Int => "int",
            ValueType::Float => "float",
            ValueType::Bool => "bool",
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A concrete typed value produced by parsing a token, or supplied as a
/// parameter default.
///
/// This is the Rust replacement for the dynamically typed values Click coerces
/// in pyfly. The variant always matches the owning parameter's [`ValueType`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A string value.
    Str(String),
    /// A 64-bit signed integer value.
    Int(i64),
    /// A 64-bit floating-point value.
    Float(f64),
    /// A boolean value.
    Bool(bool),
}

impl Value {
    /// The [`ValueType`] of this value.
    #[must_use]
    pub const fn value_type(&self) -> ValueType {
        match self {
            Value::Str(_) => ValueType::Str,
            Value::Int(_) => ValueType::Int,
            Value::Float(_) => ValueType::Float,
            Value::Bool(_) => ValueType::Bool,
        }
    }

    /// Borrow this value as a string, if it is a [`Value::Str`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Return this value as an `i64`, if it is a [`Value::Int`].
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Return this value as an `f64`, if it is a [`Value::Float`].
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Return this value as a `bool`, if it is a [`Value::Bool`].
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Render this value the way Click would stringify it when it flows into a
    /// handler that joins arguments — used by tests and the default REPL.
    #[must_use]
    pub fn to_display_string(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_display_string())
    }
}

/// Describes a single parameter (positional argument, value option, or flag)
/// of a shell command.
///
/// This is the Rust port of pyfly's frozen `ShellParam` dataclass. The Python
/// `MISSING` sentinel that marks "no default given" is represented here by
/// `default == None`: a required positional or option has `default: None`,
/// while one with a value has `default: Some(..)`.
///
/// Construct these directly, or via [`ShellParam::arg`], [`ShellParam::option`],
/// and [`ShellParam::flag`].
#[derive(Debug, Clone, PartialEq)]
pub struct ShellParam {
    /// The parameter name (without any leading dashes).
    pub name: String,
    /// The expected value type.
    pub value_type: ValueType,
    /// `true` for a `--option` / `--flag`; `false` for a positional argument.
    pub is_option: bool,
    /// `true` when this is a boolean flag (`--verbose`) that takes no value.
    pub is_flag: bool,
    /// `true` when the parameter must be supplied. Mirrors Click's `required`:
    /// an option/argument with a default is not required; one without is.
    pub required: bool,
    /// The default value applied when the parameter is omitted. `None` is the
    /// Rust analog of pyfly's `MISSING` sentinel.
    pub default: Option<Value>,
    /// Help text shown in generated help output.
    pub help: String,
    /// Allowed values; when set, a supplied value outside this list is a usage
    /// error.
    pub choices: Option<Vec<String>>,
}

impl ShellParam {
    /// Create a required positional argument with the given name and value type.
    #[must_use]
    pub fn arg(name: impl Into<String>, value_type: ValueType) -> Self {
        ShellParam {
            name: name.into(),
            value_type,
            is_option: false,
            is_flag: false,
            required: true,
            default: None,
            help: String::new(),
            choices: None,
        }
    }

    /// Create a value option (`--name value`) with the given name and value type.
    ///
    /// By default the option is required (no default). Call
    /// [`ShellParam::with_default`] to make it optional.
    #[must_use]
    pub fn option(name: impl Into<String>, value_type: ValueType) -> Self {
        ShellParam {
            name: name.into(),
            value_type,
            is_option: true,
            is_flag: false,
            required: true,
            default: None,
            help: String::new(),
            choices: None,
        }
    }

    /// Create a boolean flag (`--name`).
    ///
    /// A flag is never required and defaults to `false` (matching Click's flag
    /// semantics in pyfly's `_build_click_param`).
    #[must_use]
    pub fn flag(name: impl Into<String>) -> Self {
        ShellParam {
            name: name.into(),
            value_type: ValueType::Bool,
            is_option: true,
            is_flag: true,
            required: false,
            default: Some(Value::Bool(false)),
            help: String::new(),
            choices: None,
        }
    }

    /// Set a default value, which also marks the parameter as not required.
    #[must_use]
    pub fn with_default(mut self, default: Value) -> Self {
        self.default = Some(default);
        self.required = false;
        self
    }

    /// Set the help text.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = help.into();
        self
    }

    /// Set the allowed value choices.
    #[must_use]
    pub fn with_choices<I, S>(mut self, choices: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.choices = Some(choices.into_iter().map(Into::into).collect());
        self
    }

    /// Explicitly mark this parameter as (not) required.
    #[must_use]
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }
}

/// The result of executing a shell command.
///
/// Rust port of pyfly's `CommandResult` dataclass: an `output` string plus an
/// `exit_code`, with `is_success` true when the code is zero.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandResult {
    /// The textual output produced by the command.
    pub output: String,
    /// The process-style exit code (`0` == success).
    pub exit_code: i32,
}

impl CommandResult {
    /// Create a successful result (exit code `0`) with the given output.
    #[must_use]
    pub fn ok(output: impl Into<String>) -> Self {
        CommandResult {
            output: output.into(),
            exit_code: 0,
        }
    }

    /// Create a result with explicit output and exit code.
    #[must_use]
    pub fn new(output: impl Into<String>, exit_code: i32) -> Self {
        CommandResult {
            output: output.into(),
            exit_code,
        }
    }

    /// Return `true` when the command exited cleanly (exit code `0`).
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.exit_code == 0
    }
}
