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

//! Ported from pyfly `tests/shell/test_click_adapter.py` and
//! `tests/shell/test_integration.py` — the `StdShell` runner's parser,
//! dispatch, flags, options, groups, help, availability, and exit codes.

use std::sync::{Arc, Mutex};

use firefly_shell::{
    handler, Availability, CommandArgs, CommandSpec, ShellError, ShellParam, ShellRunner, StdShell,
    Value, ValueType,
};

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

// ---------------------------------------------------------------------------
// Simple command with positional argument
// ---------------------------------------------------------------------------

#[tokio::test]
async fn positional_arg() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();

    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "echo",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = a.get_str("name").map(ToString::to_string);
                    Ok(String::new())
                }
            }),
        )
        .arg("name", ValueType::Str),
    );

    let (code, _) = shell.invoke(&args(&["echo", "hello"])).await;
    assert_eq!(code, 0);
    assert_eq!(captured.lock().unwrap().as_deref(), Some("hello"));
}

// ---------------------------------------------------------------------------
// Command with --option that has a default
// ---------------------------------------------------------------------------

#[tokio::test]
async fn option_default_used() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();

    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "deploy",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = a.get_str("env").map(ToString::to_string);
                    Ok(String::new())
                }
            }),
        )
        .param(
            ShellParam::option("env", ValueType::Str)
                .with_default(Value::Str("staging".into()))
                .with_help("Target environment"),
        ),
    );

    let (code, _) = shell.invoke(&args(&["deploy"])).await;
    assert_eq!(code, 0);
    assert_eq!(captured.lock().unwrap().as_deref(), Some("staging"));
}

#[tokio::test]
async fn option_explicit_value() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();

    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "deploy",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = a.get_str("env").map(ToString::to_string);
                    Ok(String::new())
                }
            }),
        )
        .param(
            ShellParam::option("env", ValueType::Str).with_default(Value::Str("staging".into())),
        ),
    );

    // --opt value form.
    let (code, _) = shell
        .invoke(&args(&["deploy", "--env", "production"]))
        .await;
    assert_eq!(code, 0);
    assert_eq!(captured.lock().unwrap().as_deref(), Some("production"));
}

#[tokio::test]
async fn option_equals_value() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();

    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "deploy",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = a.get_str("env").map(ToString::to_string);
                    Ok(String::new())
                }
            }),
        )
        .param(
            ShellParam::option("env", ValueType::Str).with_default(Value::Str("staging".into())),
        ),
    );

    // --opt=value form.
    let (code, _) = shell.invoke(&args(&["deploy", "--env=production"])).await;
    assert_eq!(code, 0);
    assert_eq!(captured.lock().unwrap().as_deref(), Some("production"));
}

// ---------------------------------------------------------------------------
// Command with --flag (boolean)
// ---------------------------------------------------------------------------

fn flag_shell(captured: Arc<Mutex<Option<bool>>>) -> StdShell {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "build",
            handler(move |a: CommandArgs| {
                let cap = captured.clone();
                async move {
                    *cap.lock().unwrap() = a.get_bool("verbose");
                    Ok(String::new())
                }
            }),
        )
        .flag("verbose"),
    );
    shell
}

#[tokio::test]
async fn flag_absent_defaults_false() {
    let captured: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let shell = flag_shell(captured.clone());
    let (code, _) = shell.invoke(&args(&["build"])).await;
    assert_eq!(code, 0);
    assert_eq!(*captured.lock().unwrap(), Some(false));
}

#[tokio::test]
async fn flag_present_sets_true() {
    let captured: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let shell = flag_shell(captured.clone());
    let (code, _) = shell.invoke(&args(&["build", "--verbose"])).await;
    assert_eq!(code, 0);
    assert_eq!(*captured.lock().unwrap(), Some(true));
}

// ---------------------------------------------------------------------------
// Unknown command -> non-zero exit code
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_command_returns_nonzero() {
    let shell = StdShell::new("app", "");
    let (code, _) = shell.invoke(&args(&["nonexistent"])).await;
    assert_ne!(code, 0);
    assert_eq!(code, 2);
}

// ---------------------------------------------------------------------------
// Help text -> exit 0
// ---------------------------------------------------------------------------

#[tokio::test]
async fn help_shows_command_help() {
    let mut shell = StdShell::new("myapp", "My cool app");
    shell.register_command(
        CommandSpec::new("greet", handler(|_a| async { Ok(String::new()) })).help("Say hello"),
    );

    let (code, output) = shell.invoke(&args(&["greet", "--help"])).await;
    assert_eq!(code, 0);
    assert!(output.contains("Say hello"));
}

#[tokio::test]
async fn top_level_help_lists_commands() {
    let mut shell = StdShell::new("myapp", "My cool app");
    shell.register_command(
        CommandSpec::new("greet", handler(|_a| async { Ok(String::new()) })).help("Say hello"),
    );
    let (code, output) = shell.invoke(&args(&["--help"])).await;
    assert_eq!(code, 0);
    assert!(output.contains("My cool app"));
    assert!(output.contains("greet"));
}

// ---------------------------------------------------------------------------
// async run() method returns exit code
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_returns_exit_code() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();

    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "echo",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = a.get_str("name").map(ToString::to_string);
                    Ok(String::new())
                }
            }),
        )
        .arg("name", ValueType::Str),
    );

    let code = ShellRunner::run(&shell, &args(&["echo", "hello"])).await;
    assert_eq!(code, 0);
    assert_eq!(captured.lock().unwrap().as_deref(), Some("hello"));
}

// ---------------------------------------------------------------------------
// Async handler support
// ---------------------------------------------------------------------------

#[tokio::test]
async fn async_handler_is_invoked() {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();

    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "greet",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    // Simulate async work without sleeping.
                    tokio::task::yield_now().await;
                    *cap.lock().unwrap() = a.get_str("name").map(ToString::to_string);
                    Ok(String::new())
                }
            }),
        )
        .arg("name", ValueType::Str),
    );

    let (code, _) = shell.invoke(&args(&["greet", "world"])).await;
    assert_eq!(code, 0);
    assert_eq!(captured.lock().unwrap().as_deref(), Some("world"));
}

// ---------------------------------------------------------------------------
// Grouped (sub) commands
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subgroup_command() {
    let ran = Arc::new(Mutex::new(false));
    let r = ran.clone();

    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "migrate",
            handler(move |_a| {
                let r = r.clone();
                async move {
                    *r.lock().unwrap() = true;
                    Ok(String::new())
                }
            }),
        )
        .group_name("db"),
    );

    let (code, _) = shell.invoke(&args(&["db", "migrate"])).await;
    assert_eq!(code, 0);
    assert!(*ran.lock().unwrap());
}

#[tokio::test]
async fn multiple_commands_in_same_group() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let mut shell = StdShell::new("app", "");
    {
        let cap = captured.clone();
        shell.register_command(
            CommandSpec::new(
                "migrate",
                handler(move |_a| {
                    let cap = cap.clone();
                    async move {
                        cap.lock().unwrap().push("migrate".to_string());
                        Ok(String::new())
                    }
                }),
            )
            .group_name("db"),
        );
    }
    {
        let cap = captured.clone();
        shell.register_command(
            CommandSpec::new(
                "seed",
                handler(move |_a| {
                    let cap = cap.clone();
                    async move {
                        cap.lock().unwrap().push("seed".to_string());
                        Ok(String::new())
                    }
                }),
            )
            .group_name("db"),
        );
    }

    let (code, _) = shell.invoke(&args(&["db", "migrate"])).await;
    assert_eq!(code, 0);
    assert_eq!(*captured.lock().unwrap(), vec!["migrate"]);

    let (code, _) = shell.invoke(&args(&["db", "seed"])).await;
    assert_eq!(code, 0);
    assert_eq!(*captured.lock().unwrap(), vec!["migrate", "seed"]);
}

#[tokio::test]
async fn unknown_subcommand_in_group_is_usage_error() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new("migrate", handler(|_a| async { Ok(String::new()) })).group_name("db"),
    );
    let (code, _) = shell.invoke(&args(&["db", "nope"])).await;
    assert_eq!(code, 2);
}

// ---------------------------------------------------------------------------
// Full integration flow (ported from test_integration.py) — handler output is
// returned as the command output string.
// ---------------------------------------------------------------------------

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn greeting_shell() -> StdShell {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "greet",
            handler(|a: CommandArgs| async move {
                let name = a.get_str("name").unwrap_or_default();
                Ok(format!("Hello, {}!", title_case(name)))
            }),
        )
        .help("Greet a user by name")
        .arg("name", ValueType::Str),
    );
    shell.register_command(
        CommandSpec::new(
            "farewell",
            handler(|a: CommandArgs| async move {
                let name = title_case(a.get_str("name").unwrap_or_default());
                if a.get_bool("formal").unwrap_or(false) {
                    Ok(format!("Farewell, {name}. Until we meet again."))
                } else {
                    Ok(format!("Bye, {name}!"))
                }
            }),
        )
        .help("Say goodbye to a user")
        .arg("name", ValueType::Str)
        .flag("formal"),
    );
    shell
}

#[tokio::test]
async fn full_cli_flow() {
    let shell = greeting_shell();

    let (code, output) = shell.invoke(&args(&["greet", "john"])).await;
    assert_eq!(code, 0);
    assert_eq!(output, "Hello, John!");

    let (code, output) = shell.invoke(&args(&["farewell", "john", "--formal"])).await;
    assert_eq!(code, 0);
    assert_eq!(output, "Farewell, John. Until we meet again.");

    let (code, output) = shell.invoke(&args(&["farewell", "jane"])).await;
    assert_eq!(code, 0);
    assert_eq!(output, "Bye, Jane!");
}

#[tokio::test]
async fn async_shell_method() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "fetch",
            handler(|a: CommandArgs| async move {
                let url = a.get_str("url").unwrap_or_default().to_string();
                Ok(format!("Fetched {url}"))
            }),
        )
        .help("Fetch a URL")
        .arg("url", ValueType::Str),
    );

    let (code, output) = shell.invoke(&args(&["fetch", "https://example.com"])).await;
    assert_eq!(code, 0);
    assert_eq!(output, "Fetched https://example.com");
}

// ---------------------------------------------------------------------------
// Required / missing arguments & options -> usage error (exit 2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_required_positional_is_usage_error() {
    let shell = greeting_shell();
    let (code, output) = shell.invoke(&args(&["greet"])).await;
    assert_eq!(code, 2);
    assert!(output.contains("Missing argument"));
}

#[tokio::test]
async fn missing_required_option_is_usage_error() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new("deploy", handler(|_a| async { Ok(String::new()) }))
            .option("env", ValueType::Str),
    );
    let (code, output) = shell.invoke(&args(&["deploy"])).await;
    assert_eq!(code, 2);
    assert!(output.contains("Missing option"));
}

#[tokio::test]
async fn unknown_option_is_usage_error() {
    let shell = greeting_shell();
    let (code, _) = shell.invoke(&args(&["greet", "john", "--bogus"])).await;
    assert_eq!(code, 2);
}

#[tokio::test]
async fn extra_positional_is_usage_error() {
    let shell = greeting_shell();
    let (code, _) = shell.invoke(&args(&["greet", "john", "extra"])).await;
    assert_eq!(code, 2);
}

// ---------------------------------------------------------------------------
// Choices validation
// ---------------------------------------------------------------------------

fn color_shell() -> StdShell {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "paint",
            handler(|a: CommandArgs| async move {
                Ok(format!(
                    "painted {}",
                    a.get_str("color").unwrap_or_default()
                ))
            }),
        )
        .param(
            ShellParam::option("color", ValueType::Str)
                .with_default(Value::Str("red".into()))
                .with_choices(["red", "green", "blue"]),
        ),
    );
    shell
}

#[tokio::test]
async fn valid_choice_accepted() {
    let shell = color_shell();
    let (code, output) = shell.invoke(&args(&["paint", "--color", "green"])).await;
    assert_eq!(code, 0);
    assert_eq!(output, "painted green");
}

#[tokio::test]
async fn invalid_choice_is_usage_error() {
    let shell = color_shell();
    let (code, output) = shell.invoke(&args(&["paint", "--color", "magenta"])).await;
    assert_eq!(code, 2);
    assert!(output.contains("not one of"));
}

// ---------------------------------------------------------------------------
// Type coercion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn int_coercion() {
    let captured: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "repeat",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = a.get_i64("count");
                    Ok(String::new())
                }
            }),
        )
        .arg("count", ValueType::Int),
    );
    let (code, _) = shell.invoke(&args(&["repeat", "5"])).await;
    assert_eq!(code, 0);
    assert_eq!(*captured.lock().unwrap(), Some(5));
}

#[tokio::test]
async fn int_coercion_failure_is_usage_error() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new("repeat", handler(|_a| async { Ok(String::new()) }))
            .arg("count", ValueType::Int),
    );
    let (code, _) = shell.invoke(&args(&["repeat", "notanumber"])).await;
    assert_eq!(code, 2);
}

#[tokio::test]
async fn float_coercion() {
    let captured: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));
    let cap = captured.clone();
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new(
            "scale",
            handler(move |a: CommandArgs| {
                let cap = cap.clone();
                async move {
                    *cap.lock().unwrap() = a.get_f64("factor");
                    Ok(String::new())
                }
            }),
        )
        .arg("factor", ValueType::Float),
    );
    let (code, _) = shell.invoke(&args(&["scale", "2.5"])).await;
    assert_eq!(code, 0);
    assert_eq!(*captured.lock().unwrap(), Some(2.5));
}

// ---------------------------------------------------------------------------
// Availability gating (pyfly @shell_method_availability)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unavailable_command_blocked() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new("reset-db", handler(|_a| async { Ok("done".to_string()) }))
            .availability(|| Availability::Unavailable("Requires ADMIN role".into())),
    );
    let (code, output) = shell.invoke(&args(&["reset-db"])).await;
    assert_eq!(code, 1);
    assert!(output.contains("Requires ADMIN role"));
}

#[tokio::test]
async fn available_command_runs() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new("reset-db", handler(|_a| async { Ok("done".to_string()) }))
            .availability(|| Availability::Available),
    );
    let (code, output) = shell.invoke(&args(&["reset-db"])).await;
    assert_eq!(code, 0);
    assert_eq!(output, "done");
}

#[tokio::test]
async fn unavailable_command_hidden_from_help() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(
        CommandSpec::new("visible", handler(|_a| async { Ok(String::new()) })).help("shown"),
    );
    shell.register_command(
        CommandSpec::new("secret", handler(|_a| async { Ok(String::new()) }))
            .help("hidden")
            .availability(|| Availability::Unavailable("nope".into())),
    );
    let help = shell.render_help();
    assert!(help.contains("visible"));
    assert!(!help.contains("secret"));
}

// ---------------------------------------------------------------------------
// Handler runtime error -> exit 1
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handler_error_maps_to_exit_one() {
    let mut shell = StdShell::new("app", "");
    shell.register_command(CommandSpec::new(
        "boom",
        handler(|_a| async { Err(ShellError::handler("kaboom")) }),
    ));
    let (code, output) = shell.invoke(&args(&["boom"])).await;
    assert_eq!(code, 1);
    assert!(output.contains("kaboom"));
}
