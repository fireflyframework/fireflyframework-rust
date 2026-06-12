//! Port of the Go module's `config_test.go`, plus Rust-specific cases.
//!
//! Tests that mutate process environment variables serialize on a shared
//! mutex and clean up via drop guards, because the test harness runs
//! tests in parallel threads within this binary.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use firefly_config::{
    active_profile, from_env, from_optional_yaml, from_yaml, load, load_from_profile, FlagSource,
    Source, StaticSource,
};
use serde::Deserialize;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Sets an environment variable for the test's lifetime; removes on drop.
struct EnvVar {
    key: &'static str,
}

impl EnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        std::env::set_var(key, value);
        EnvVar { key }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        std::env::remove_var(self.key);
    }
}

fn entries(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[derive(Debug, Default, Deserialize, PartialEq)]
struct Web {
    port: i32,
    host: String,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
struct Cache {
    adapter: String,
    ttl: i64,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
struct AppCfg {
    web: Web,
    cache: Cache,
    tags: Vec<String>,
    on: bool,
}

// Go: TestLoadFromStaticEnvAndYAML
#[test]
fn load_from_static_env_and_yaml() {
    let _guard = env_lock();
    let _port = EnvVar::set("FIREFLY_WEB_PORT", "9090");
    let _tags = EnvVar::set("FIREFLY_TAGS", "alpha,beta,gamma");

    let dir = tempfile::tempdir().unwrap();
    let yaml = "
web:
  port: 8080
  host: 0.0.0.0
cache:
  adapter: redis
  ttl: 60
on: true
tags:
  - one
  - two
";
    let path = dir.path().join("application.yaml");
    std::fs::write(&path, yaml).unwrap();

    let sources: Vec<Box<dyn Source>> =
        vec![Box::new(from_yaml(path)), Box::new(from_env("FIREFLY"))];
    let cfg: AppCfg = load(&sources).unwrap();

    // Env overrides YAML.
    assert_eq!(cfg.web.port, 9090, "port: {}", cfg.web.port);
    assert_eq!(cfg.web.host, "0.0.0.0", "host: {}", cfg.web.host);
    assert_eq!(cfg.cache.adapter, "redis", "cache: {:?}", cfg.cache);
    assert_eq!(cfg.cache.ttl, 60, "cache: {:?}", cfg.cache);
    assert!(cfg.on, "bool not bound");
    assert_eq!(
        cfg.tags.join(","),
        "alpha,beta,gamma",
        "tags from env: {:?}",
        cfg.tags
    );
}

// Go: TestProfileSelection
#[test]
fn profile_selection() {
    let _guard = env_lock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("application.yaml"),
        "\nweb:\n  port: 8080\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("application-prod.yaml"),
        "\nweb:\n  port: 443\n",
    )
    .unwrap();

    let _profile = EnvVar::set("FIREFLY_PROFILE", "prod");
    let cfg: AppCfg = load_from_profile(dir.path(), "application", "dev").unwrap();
    assert_eq!(cfg.web.port, 443, "profile override missed: {cfg:?}");

    assert_eq!(active_profile("dev"), "prod", "active profile mismatch");
}

// Go: TestFlagSourceWins
#[test]
fn flag_source_wins() {
    let flags = FlagSource::new();
    flags.set("web.port", "1234");

    let sources: Vec<Box<dyn Source>> = vec![
        Box::new(StaticSource::new(
            "default",
            entries(&[("web.port", "8080")]),
        )),
        Box::new(flags),
    ];
    let cfg: AppCfg = load(&sources).unwrap();
    assert_eq!(cfg.web.port, 1234, "flag override missed: {}", cfg.web.port);
}

// Go: TestOptionalYAMLAbsence
#[test]
fn optional_yaml_absence() {
    let sources: Vec<Box<dyn Source>> =
        vec![Box::new(from_optional_yaml("/nonexistent/firefly.yaml"))];
    let cfg: AppCfg = load(&sources).unwrap();
    assert_eq!(
        cfg.web.port, 0,
        "optional yaml should produce zero values: {cfg:?}"
    );
    assert_eq!(cfg, AppCfg::default());
}

// Go: TestParseDurationViaInt
#[test]
fn parse_duration_via_int() {
    #[derive(Debug, Default, Deserialize)]
    struct Server {
        timeoutms: i64,
    }
    #[derive(Debug, Default, Deserialize)]
    struct Cfg {
        server: Server,
    }

    let sources: Vec<Box<dyn Source>> = vec![Box::new(StaticSource::new(
        "x",
        entries(&[("server.timeoutms", "5000")]),
    ))];
    let cfg: Cfg = load(&sources).unwrap();
    let timeout = Duration::from_millis(u64::try_from(cfg.server.timeoutms).unwrap());
    assert_eq!(timeout, Duration::from_secs(5), "duration: {cfg:?}");
}

// ---- Rust-specific additions ----

#[test]
fn required_yaml_absence_is_an_error() {
    let sources: Vec<Box<dyn Source>> = vec![Box::new(from_yaml("/nonexistent/firefly.yaml"))];
    let err = load::<AppCfg>(&sources).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("config source \"yaml(/nonexistent/firefly.yaml)\""),
        "got: {text}"
    );
}

#[test]
fn env_source_maps_prefixed_underscores_to_dots() {
    let _guard = env_lock();
    let _var = EnvVar::set("FIREFLY_FOO_BAR", "baz");
    let _other = EnvVar::set("UNRELATED_FOO", "nope");

    let flat = from_env("FIREFLY").load().unwrap();
    assert_eq!(flat.get("foo.bar").map(String::as_str), Some("baz"));
    assert!(!flat.values().any(|v| v == "nope"));
}

#[test]
fn env_prefix_is_uppercased_for_matching() {
    let _guard = env_lock();
    let _var = EnvVar::set("FIREFLY_CASE_CHECK", "yes");
    let flat = from_env("firefly").load().unwrap();
    assert_eq!(flat.get("case.check").map(String::as_str), Some("yes"));
}

#[test]
fn active_profile_trims_lowercases_and_falls_back() {
    let _guard = env_lock();
    {
        let _profile = EnvVar::set("FIREFLY_PROFILE", "  PROD  ");
        assert_eq!(active_profile("dev"), "prod");
    }
    // Removed by the drop guard: fallback applies.
    assert_eq!(active_profile("dev"), "dev");
    // Blank value also falls back.
    let _profile = EnvVar::set("FIREFLY_PROFILE", "   ");
    assert_eq!(active_profile("staging"), "staging");
}

#[test]
fn load_from_profile_uses_fallback_when_env_unset() {
    let _guard = env_lock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("application.yaml"), "web:\n  port: 8080\n").unwrap();
    std::fs::write(
        dir.path().join("application-dev.yaml"),
        "web:\n  port: 3000\n",
    )
    .unwrap();

    let cfg: AppCfg = load_from_profile(dir.path(), "application", "dev").unwrap();
    assert_eq!(cfg.web.port, 3000);
}

// Regression (bug): numeric-looking scalars used to be re-rendered
// through typed YAML nodes, silently corrupting them ("1.10" -> "1.1",
// "0x1A" -> "26", "1e3" -> "1000.0", "2.50" -> "2.5"). The Go scanner
// keeps the source lexeme verbatim, so binding onto String fields must
// yield exactly what was written in application.yaml.
#[test]
fn yaml_scalars_bind_verbatim_onto_string_fields() {
    #[derive(Debug, Deserialize)]
    struct Cfg {
        version: String,
        build: String,
        num: String,
        ratio: String,
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("application.yaml");
    std::fs::write(&path, "version: 1.10\nbuild: 0x1A\nnum: 1e3\nratio: 2.50\n").unwrap();

    let sources: Vec<Box<dyn Source>> = vec![Box::new(from_yaml(path))];
    let cfg: Cfg = load(&sources).unwrap();
    assert_eq!(cfg.version, "1.10");
    assert_eq!(cfg.build, "0x1A");
    assert_eq!(cfg.num, "1e3");
    assert_eq!(cfg.ratio, "2.50");
}

// Regression (bug): YAML documents that load fine on the Go port used to
// hard-fail the whole load() here — duplicate keys raised "duplicate
// entry" and out-of-range integer literals raised an u128 parse error.
// Go applies last-write-wins and stores the lexeme as a string.
#[test]
fn yaml_accepts_duplicate_keys_and_out_of_range_integer_literals() {
    #[derive(Debug, Deserialize)]
    struct DupWeb {
        port: i32,
    }
    #[derive(Debug, Deserialize)]
    struct Cfg {
        web: DupWeb,
        big: String,
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("application.yaml");
    std::fs::write(
        &path,
        "web:\n  port: 1\nweb:\n  port: 2\nbig: 12345678901234567890123\n",
    )
    .unwrap();

    let sources: Vec<Box<dyn Source>> = vec![Box::new(from_yaml(path))];
    let cfg: Cfg = load(&sources).unwrap();
    assert_eq!(cfg.web.port, 2, "duplicate keys must be last-write-wins");
    assert_eq!(cfg.big, "12345678901234567890123");
}

#[test]
fn sources_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<StaticSource>();
    assert_send_sync::<FlagSource>();
    assert_send_sync::<firefly_config::EnvSource>();
    assert_send_sync::<firefly_config::YamlSource>();
    assert_send_sync::<Box<dyn Source>>();
    assert_send_sync::<Vec<Box<dyn Source>>>();
}

#[tokio::test]
async fn load_works_inside_async_tasks() {
    let handle = tokio::spawn(async {
        let sources: Vec<Box<dyn Source>> = vec![Box::new(StaticSource::new(
            "defaults",
            entries(&[("web.port", "7"), ("web.host", "localhost")]),
        ))];
        load::<AppCfg>(&sources).unwrap()
    });
    let cfg = handle.await.unwrap();
    assert_eq!(cfg.web.port, 7);
    assert_eq!(cfg.web.host, "localhost");
}
