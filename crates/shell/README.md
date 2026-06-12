# `firefly-shell`

> **Tier:** Platform · **Status:** Full · **Java original:** Spring Shell (`@ShellMethod`/`@ShellOption`) + Spring Boot `CommandLineRunner`/`ApplicationRunner` · **pyfly package:** `pyfly.shell`

## Overview

`firefly-shell` is a Spring-Shell-style CLI command framework plus Spring
Boot's post-startup runner hooks. It provides:

* A typed command model — `ValueType`, `Value`, `ShellParam`, `CommandResult`.
* A fluent `CommandSpec` builder (args / options / flags / choices /
  availability closure) replacing pyfly's `@shell_method`/`@shell_option`/
  `@shell_argument` decorators.
* An async `Handler` type receiving a typed `CommandArgs` bag with
  `get_str` / `get_i64` / `get_f64` / `get_bool` getters.
* `StdShell`, the default `ShellRunner`: a hand-rolled tokenizer/parser
  supporting positional arguments, `--opt value`, `--opt=value`, boolean flags,
  choice validation, required-parameter enforcement, grouped commands,
  generated help, and a line-based interactive REPL driven by any
  `std::io::BufRead` (so tests feed scripted input).
* `ApplicationArguments` with pyfly-exact `from_args` / `contains_option` /
  `get_option_values` semantics (prefix matching, values containing `=`).
* `CommandLineRunner` / `ApplicationRunner` async traits and an ordered
  `RunnerRegistry` for post-startup hooks.

Exit codes follow pyfly's mapping: `0` success, `2` usage error (unknown
command, bad args, invalid choice, missing required), `1` runtime / availability
error.

```rust
use firefly_shell::{handler, CommandArgs, CommandSpec, ShellParam, StdShell, Value, ValueType};

# async fn demo() {
let mut shell = StdShell::new("app", "Demo CLI");

shell.register_command(
    CommandSpec::new(
        "greet",
        handler(|args: CommandArgs| async move {
            let name = args.get_str("name").unwrap_or("world");
            let times = args.get_i64("times").unwrap_or(1);
            Ok((0..times).map(|_| format!("Hello, {name}!")).collect::<Vec<_>>().join(" "))
        }),
    )
    .help("Greet someone")
    .arg("name", ValueType::Str)
    .param(ShellParam::option("times", ValueType::Int).with_default(Value::Int(1))),
);

let (code, output) = shell.invoke(&["greet".into(), "John".into(), "--times=2".into()]).await;
assert_eq!(code, 0);
assert_eq!(output, "Hello, John! Hello, John!");
# }
```

## pyfly parity

This crate is the Rust port of pyfly's `pyfly.shell` package. pyfly is
decorator- and reflection-driven (decorators attach metadata that type-hint
inference resolves into `ShellParam`s, dispatched by a `Click` adapter). Rust
has no decorator/reflection layer, so the port replaces those mechanisms with
explicit, type-safe equivalents while preserving behavior and wire semantics.

| pyfly | firefly-shell |
|-------|---------------|
| `ShellParam` dataclass / `MISSING` sentinel | `ShellParam` with `Option<Value>` defaults (`None` == `MISSING`) |
| `param_type: type` | `ValueType` enum (`Str`/`Int`/`Float`/`Bool`) |
| `CommandResult` | `CommandResult` |
| `@shell_method` + `@shell_option` / `@shell_argument` | `CommandSpec` builder |
| `@shell_method_availability("checker")` | `CommandSpec::availability` closure → `Availability` |
| Click handler keyword args | `CommandArgs` typed getters |
| `ShellRunnerPort` protocol | `ShellRunner` trait |
| `ClickShellAdapter` (`invoke`/`run`/`run_interactive`) | `StdShell` (`invoke`/`run`/`run_repl`/`run_interactive`) |
| `ApplicationArguments` | `ApplicationArguments` (exact `from_args`/`contains_option`/`get_option_values`) |
| `CommandLineRunner` / `ApplicationRunner` protocols | `CommandLineRunner` / `ApplicationRunner` async traits |
| context-side `_invoke_runners` (ordered) | `RunnerRegistry::run_all` (registration order) |

### Deliberate divergences

* **Param inference excluded.** pyfly's `infer_params` reflects over Python type
  hints; the brief scopes this out as Python-runtime-specific. The same metadata
  is expressed explicitly via the `CommandSpec` builder, so the
  `test_param_inference.py` / `test_decorators.py` / `test_wave_shell_option.py`
  cases (which assert decorator-attribute bookkeeping) have no Rust analog.
* **No Click dependency.** pyfly delegates parsing to Click; the Rust port
  hand-rolls an equivalent parser (honoring the workspace minimal-deps policy),
  matching the exit-code and dispatch behavior the pyfly adapter tests assert.
* **Async-only handlers.** pyfly supports both sync and async handlers via an
  `asyncio.run` bridge; the Rust port is uniformly async (`Handler` returns a
  `BoxFuture`).
* **Auto-configuration / DI wiring** (`ShellAutoConfiguration`,
  `_wire_shell_commands`) is container-side glue; the Rust analog is explicit
  `RunnerRegistry` / `StdShell::register_command` calls from the application
  bootstrap, so those pyfly tests map onto the `RunnerRegistry` ordering and
  trait-conformance tests here.

Ported test files: `tests/model_tests.rs` (← `test_models.py`),
`tests/runner_tests.rs` (← `test_runner.py` + runner-invocation parts of
`test_context_integration.py`), `tests/shell_tests.rs`
(← `test_click_adapter.py` + `test_integration.py`), `tests/repl_tests.rs`
(← `run_interactive` REPL path), `tests/port_tests.rs`
(← `test_port.py` + `test_exports.py`).
