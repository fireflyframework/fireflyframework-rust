# `firefly-shell`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-shell` is a CLI command framework in the style of mature shell
toolkits, plus post-startup runner hooks. It provides:

* A typed command model — `ValueType`, `Value`, `ShellParam`, `CommandResult`.
* A fluent `CommandSpec` builder (args / options / flags / choices /
  availability closure).
* An async `Handler` type receiving a typed `CommandArgs` bag with
  `get_str` / `get_i64` / `get_f64` / `get_bool` getters.
* `StdShell`, the default `ShellRunner`: a hand-rolled tokenizer/parser
  supporting positional arguments, `--opt value`, `--opt=value`, boolean flags,
  choice validation, required-parameter enforcement, grouped commands,
  generated help, and a line-based interactive REPL driven by any
  `std::io::BufRead` (so tests feed scripted input).
* `ApplicationArguments` with `from_args` / `contains_option` /
  `get_option_values` semantics (prefix matching, values containing `=`).
* `CommandLineRunner` / `ApplicationRunner` async traits and an ordered
  `RunnerRegistry` for post-startup hooks.

Exit codes follow a conventional mapping: `0` success, `2` usage error (unknown
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

## Design notes

The command model is fully explicit and type-safe: a `ShellParam` carries an
`Option<Value>` default (`None` meaning "no default / required"), and parameter
types are captured by the `ValueType` enum (`Str` / `Int` / `Float` / `Bool`).
Commands are declared with the `CommandSpec` builder, with an optional
availability closure that resolves to an `Availability` to gate a command at
runtime. Handlers receive arguments through the typed `CommandArgs` getters
rather than positional or keyword binding.

A few deliberate design choices:

* **Explicit declaration, no reflection.** Command metadata is expressed
  directly via the `CommandSpec` builder rather than inferred from type
  annotations or attached by attribute macros, keeping the model transparent and
  compile-time checked.
* **No external CLI parsing dependency.** `StdShell` hand-rolls its own
  tokenizer/parser (honoring the workspace minimal-deps policy) and exposes
  `invoke` / `run` / `run_repl` / `run_interactive` entry points.
* **Async-only handlers.** Every `Handler` is uniformly async and returns a
  `BoxFuture`.
* **Explicit wiring.** Commands and runners are registered explicitly from the
  application bootstrap via `StdShell::register_command` and the ordered
  `RunnerRegistry` (which runs hooks in registration order through
  `RunnerRegistry::run_all`), rather than discovered by container-side glue.
