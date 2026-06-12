//! Ported from pyfly `tests/shell/test_runner.py` — `ApplicationArguments`
//! parsing plus the `CommandLineRunner` / `ApplicationRunner` traits and the
//! ordered `RunnerRegistry` (pyfly's context-side runner invocation).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use firefly_shell::{ApplicationArguments, ApplicationRunner, CommandLineRunner, RunnerRegistry};

// ---------------------------------------------------------------------------
// ApplicationArguments.from_args
// ---------------------------------------------------------------------------

#[test]
fn from_args_mixed_args() {
    let raw = vec![
        "serve".to_string(),
        "--port=8080".to_string(),
        "--verbose".to_string(),
        "extra".to_string(),
    ];
    let args = ApplicationArguments::from_args(raw);
    assert_eq!(
        args.source_args,
        vec!["serve", "--port=8080", "--verbose", "extra"]
    );
    assert_eq!(args.option_args, vec!["--port=8080", "--verbose"]);
    assert_eq!(args.non_option_args, vec!["serve", "extra"]);
}

#[test]
fn from_args_empty_args() {
    let args = ApplicationArguments::from_args(Vec::<String>::new());
    assert!(args.source_args.is_empty());
    assert!(args.option_args.is_empty());
    assert!(args.non_option_args.is_empty());
}

#[test]
fn from_args_only_options() {
    let args = ApplicationArguments::from_args(["--debug", "--level=3"]);
    assert_eq!(args.option_args, vec!["--debug", "--level=3"]);
    assert!(args.non_option_args.is_empty());
}

#[test]
fn from_args_only_non_options() {
    let args = ApplicationArguments::from_args(["run", "server", "fast"]);
    assert!(args.option_args.is_empty());
    assert_eq!(args.non_option_args, vec!["run", "server", "fast"]);
}

#[test]
fn source_args_is_a_copy() {
    let mut raw = vec!["--flag".to_string()];
    let args = ApplicationArguments::from_args(raw.clone());
    raw.push("mutated".to_string());
    assert!(!args.source_args.contains(&"mutated".to_string()));
}

// ---------------------------------------------------------------------------
// contains_option
// ---------------------------------------------------------------------------

#[test]
fn contains_option_flag_present() {
    let args = ApplicationArguments::from_args(["--verbose"]);
    assert!(args.contains_option("verbose"));
}

#[test]
fn contains_option_key_value_present() {
    let args = ApplicationArguments::from_args(["--port=8080"]);
    assert!(args.contains_option("port"));
}

#[test]
fn contains_option_absent() {
    let args = ApplicationArguments::from_args(["--verbose"]);
    assert!(!args.contains_option("debug"));
}

#[test]
fn contains_option_partial_name_does_not_match() {
    let args = ApplicationArguments::from_args(["--verbose-mode"]);
    assert!(!args.contains_option("verbose"));
}

#[test]
fn contains_option_empty_args() {
    let args = ApplicationArguments::from_args(Vec::<String>::new());
    assert!(!args.contains_option("anything"));
}

// ---------------------------------------------------------------------------
// get_option_values
// ---------------------------------------------------------------------------

#[test]
fn get_option_values_single_value() {
    let args = ApplicationArguments::from_args(["--port=8080"]);
    assert_eq!(args.get_option_values("port"), vec!["8080".to_string()]);
}

#[test]
fn get_option_values_multiple_values() {
    let args = ApplicationArguments::from_args(["--tag=alpha", "--tag=beta"]);
    assert_eq!(
        args.get_option_values("tag"),
        vec!["alpha".to_string(), "beta".to_string()]
    );
}

#[test]
fn get_option_values_missing_option() {
    let args = ApplicationArguments::from_args(["--port=8080"]);
    assert!(args.get_option_values("host").is_empty());
}

#[test]
fn get_option_values_flag_without_value() {
    let args = ApplicationArguments::from_args(["--verbose"]);
    assert!(args.get_option_values("verbose").is_empty());
}

#[test]
fn get_option_values_value_with_equals_sign() {
    let args = ApplicationArguments::from_args(["--formula=a=b"]);
    assert_eq!(args.get_option_values("formula"), vec!["a=b".to_string()]);
}

// ---------------------------------------------------------------------------
// CommandLineRunner / ApplicationRunner + RunnerRegistry
// ---------------------------------------------------------------------------

struct RecordingCliRunner {
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl CommandLineRunner for RecordingCliRunner {
    async fn run(&self, args: &[String]) {
        self.seen.lock().unwrap().push(args.to_vec());
    }
}

struct RecordingAppRunner {
    seen: Arc<Mutex<Vec<ApplicationArguments>>>,
}

#[async_trait]
impl ApplicationRunner for RecordingAppRunner {
    async fn run(&self, args: &ApplicationArguments) {
        self.seen.lock().unwrap().push(args.clone());
    }
}

#[tokio::test]
async fn command_line_runner_invoked_with_raw_args() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut registry = RunnerRegistry::new();
    registry.add_command_line_runner(Arc::new(RecordingCliRunner { seen: seen.clone() }));

    registry
        .run_all(&["serve".to_string(), "--port=8080".to_string()])
        .await;

    let captured = seen.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0], vec!["serve", "--port=8080"]);
}

#[tokio::test]
async fn application_runner_invoked_with_parsed_args() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut registry = RunnerRegistry::new();
    registry.add_application_runner(Arc::new(RecordingAppRunner { seen: seen.clone() }));

    registry
        .run_all(&["serve".to_string(), "--port=8080".to_string()])
        .await;

    let captured = seen.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].option_args, vec!["--port=8080"]);
    assert_eq!(captured[0].non_option_args, vec!["serve"]);
}

#[tokio::test]
async fn runners_invoked_in_registration_order() {
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    struct OrderCli {
        tag: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait]
    impl CommandLineRunner for OrderCli {
        async fn run(&self, _args: &[String]) {
            self.order.lock().unwrap().push(self.tag);
        }
    }
    struct OrderApp {
        tag: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
    }
    #[async_trait]
    impl ApplicationRunner for OrderApp {
        async fn run(&self, _args: &ApplicationArguments) {
            self.order.lock().unwrap().push(self.tag);
        }
    }

    let mut registry = RunnerRegistry::new();
    registry.add_command_line_runner(Arc::new(OrderCli {
        tag: "first",
        order: order.clone(),
    }));
    registry.add_application_runner(Arc::new(OrderApp {
        tag: "second",
        order: order.clone(),
    }));
    registry.add_command_line_runner(Arc::new(OrderCli {
        tag: "third",
        order: order.clone(),
    }));

    assert_eq!(registry.len(), 3);
    registry.run_all(&[]).await;
    assert_eq!(*order.lock().unwrap(), vec!["first", "second", "third"]);
}

#[test]
fn empty_registry() {
    let registry = RunnerRegistry::new();
    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);
}
