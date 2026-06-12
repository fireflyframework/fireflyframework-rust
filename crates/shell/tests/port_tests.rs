//! Ported from pyfly `tests/shell/test_port.py` and `tests/shell/test_exports.py`
//! — `ShellRunner` trait-object usability and the crate's public re-exports.

use firefly_shell::{
    handler, ApplicationArguments, ApplicationRunner, Availability, CommandArgs, CommandLineRunner,
    CommandResult, CommandSpec, RunnerRegistry, ShellError, ShellParam, ShellRunner, StdShell,
    Value, ValueType,
};

/// pyfly `test_port.py`: any adapter satisfying the protocol is usable through
/// the abstract interface. Here we drive `StdShell` purely through the
/// `ShellRunner` trait object.
#[tokio::test]
async fn std_shell_is_usable_as_dyn_shell_runner() {
    let mut shell = StdShell::new("app", "");
    shell.register(
        CommandSpec::new(
            "greet",
            handler(|a: CommandArgs| async move {
                Ok(format!("Hello, {}!", a.get_str("name").unwrap_or("world")))
            }),
        )
        .arg("name", ValueType::Str),
    );

    let runner: Box<dyn ShellRunner> = Box::new(shell);
    let code = runner
        .run(&["greet".to_string(), "World".to_string()])
        .await;
    assert_eq!(code, 0);
}

/// pyfly `test_exports.py`: the public surface is importable. In Rust this is a
/// compile-time guarantee; constructing each type asserts it is exported and
/// usable.
#[test]
fn public_surface_is_exported() {
    let _result = CommandResult::ok("ok");
    let _param = ShellParam::arg("x", ValueType::Str);
    let _value = Value::Int(1);
    let _vt = ValueType::Bool;
    let _avail = Availability::Available;
    let _err = ShellError::usage("bad");
    let _args = ApplicationArguments::from_args(["--x=1"]);
    let _registry = RunnerRegistry::new();
    let _spec = CommandSpec::new("c", handler(|_a: CommandArgs| async { Ok(String::new()) }));
    let _shell = StdShell::new("app", "");

    // Trait items exist and are object-safe.
    fn _assert_traits<C: CommandLineRunner, A: ApplicationRunner, S: ShellRunner>() {}
}
