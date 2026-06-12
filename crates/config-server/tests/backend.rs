//! pyfly-parity tests for the config-server backends.
//!
//! Ports the pyfly `tests/config_server/` suite — `test_config_server.py`,
//! `test_tiered_overlay.py`, `test_git_backend.py`, and the server overlay
//! cases from `test_config_server.py` / `test_backend_selection.py` — to
//! Rust. Git tests create a **local** repository in a tempdir by shelling
//! out to the system `git` binary; no network access is used.

use std::path::Path;
use std::process::Command;

use firefly_config_server::{
    ConfigBackend, ConfigServer, ConfigSource, FsStore, GitStore, MemoryBackend, Properties,
};

/// Builds a `Properties` map from `(key, json-value)` pairs.
fn props(pairs: &[(&str, serde_json::Value)]) -> Properties {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// Writes a YAML config file at `<dir>/<app>-<profile>.yaml`.
fn write_yaml(dir: &Path, app: &str, profile: &str, data: &Properties) {
    std::fs::create_dir_all(dir).unwrap();
    let text = serde_yaml::to_string(data).unwrap();
    std::fs::write(dir.join(format!("{app}-{profile}.yaml")), text).unwrap();
}

// ---------------------------------------------------------------------------
// MemoryBackend / FsStore — port of test_config_server.py
// ---------------------------------------------------------------------------

// Port of test_in_memory_round_trip.
#[tokio::test]
async fn in_memory_round_trip() {
    let backend = MemoryBackend::new();
    backend
        .save(ConfigSource::new(
            "orders",
            "prod",
            props(&[("x", serde_json::json!(1))]),
        ))
        .await
        .unwrap();
    let fetched = backend.fetch("orders", "prod", "main").await.unwrap();
    let fetched = fetched.expect("should be present");
    assert_eq!(fetched.properties, props(&[("x", serde_json::json!(1))]));
}

// Port of test_filesystem_round_trip.
#[tokio::test]
async fn filesystem_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let backend = FsStore::new(dir.path()).unwrap();
    backend
        .save(ConfigSource::new(
            "orders",
            "dev",
            props(&[("y", serde_json::json!("v"))]),
        ))
        .await
        .unwrap();
    let fetched = backend
        .fetch("orders", "dev", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(fetched.properties, props(&[("y", serde_json::json!("v"))]));
    let listed = backend.list().await.unwrap();
    assert!(listed.iter().any(|s| s.application == "orders"));
}

// Port of test_filesystem_save_updates_existing_yaml — save must update
// the file fetch() reads (a pre-existing .yaml), not write a shadowed
// .json that fetch ignores.
#[tokio::test]
async fn filesystem_save_updates_existing_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let yaml_dir = dir.path().join("main");
    std::fs::create_dir_all(&yaml_dir).unwrap();
    std::fs::write(
        yaml_dir.join("orders-prod.yaml"),
        serde_yaml::to_string(&props(&[("v", serde_json::json!("old"))])).unwrap(),
    )
    .unwrap();

    let backend = FsStore::new(dir.path()).unwrap();
    backend
        .save(ConfigSource::with_label(
            "orders",
            "prod",
            "main",
            props(&[("v", serde_json::json!("new"))]),
        ))
        .await
        .unwrap();

    let fetched = backend
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(
        fetched.properties,
        props(&[("v", serde_json::json!("new"))])
    );
    // No shadow .json created alongside the .yaml.
    assert!(!yaml_dir.join("orders-prod.json").exists());
}

#[tokio::test]
async fn filesystem_reads_json_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("svc-default.json"),
        serde_json::to_string_pretty(&props(&[("k", serde_json::json!("j"))])).unwrap(),
    )
    .unwrap();
    let backend = FsStore::new(dir.path()).unwrap();
    let fetched = backend
        .fetch("svc", "default", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(fetched.properties, props(&[("k", serde_json::json!("j"))]));
}

#[tokio::test]
async fn filesystem_empty_yaml_is_empty_map() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("svc-default.yaml"), "").unwrap();
    let backend = FsStore::new(dir.path()).unwrap();
    let fetched = backend
        .fetch("svc", "default", "main")
        .await
        .unwrap()
        .expect("present");
    assert!(fetched.properties.is_empty());
}

#[tokio::test]
async fn filesystem_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let backend = FsStore::new(dir.path()).unwrap();
    assert!(backend
        .fetch("nope", "dev", "main")
        .await
        .unwrap()
        .is_none());
}

// ---------------------------------------------------------------------------
// Tiered overlay — port of test_tiered_overlay.py
// ---------------------------------------------------------------------------

// Port of test_domain_overrides_common.
#[tokio::test]
async fn domain_overrides_common() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    let domain = tmp.path().join("domain");
    write_yaml(
        &common,
        "orders",
        "prod",
        &props(&[
            ("host", serde_json::json!("common.db")),
            ("timeout", serde_json::json!(5)),
            ("common_only", serde_json::json!("yes")),
        ]),
    );
    write_yaml(
        &domain,
        "orders",
        "prod",
        &props(&[
            ("host", serde_json::json!("domain.db")),
            ("domain_only", serde_json::json!("yes")),
        ]),
    );

    let backend =
        FsStore::with_search_locations(&domain, [domain.clone(), common.clone()]).unwrap();
    let source = backend
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(source.properties["host"], serde_json::json!("domain.db"));
    assert_eq!(source.properties["timeout"], serde_json::json!(5));
    assert_eq!(source.properties["common_only"], serde_json::json!("yes"));
    assert_eq!(source.properties["domain_only"], serde_json::json!("yes"));
}

// Port of test_three_tier_override_chain.
#[tokio::test]
async fn three_tier_override_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    let core = tmp.path().join("core");
    let domain = tmp.path().join("domain");

    write_yaml(
        &common,
        "svc",
        "default",
        &props(&[
            ("log_level", serde_json::json!("INFO")),
            ("timeout", serde_json::json!(30)),
            ("common_key", serde_json::json!("c")),
        ]),
    );
    write_yaml(
        &core,
        "svc",
        "default",
        &props(&[
            ("log_level", serde_json::json!("WARN")),
            ("core_key", serde_json::json!("k")),
        ]),
    );
    write_yaml(
        &domain,
        "svc",
        "default",
        &props(&[
            ("log_level", serde_json::json!("DEBUG")),
            ("domain_key", serde_json::json!("d")),
        ]),
    );

    let backend =
        FsStore::with_search_locations(&domain, [domain.clone(), core.clone(), common.clone()])
            .unwrap();
    let source = backend
        .fetch("svc", "default", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(source.properties["log_level"], serde_json::json!("DEBUG"));
    assert_eq!(source.properties["timeout"], serde_json::json!(30));
    assert_eq!(source.properties["common_key"], serde_json::json!("c"));
    assert_eq!(source.properties["core_key"], serde_json::json!("k"));
    assert_eq!(source.properties["domain_key"], serde_json::json!("d"));
}

// Port of test_missing_in_all_locations_returns_none.
#[tokio::test]
async fn missing_in_all_locations_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    let domain = tmp.path().join("domain");
    let backend = FsStore::with_search_locations(&domain, [domain.clone(), common]).unwrap();
    assert!(backend
        .fetch("missing", "dev", "main")
        .await
        .unwrap()
        .is_none());
}

// Port of test_partial_presence_only_in_lower.
#[tokio::test]
async fn partial_presence_only_in_lower() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    let domain = tmp.path().join("domain");
    std::fs::create_dir_all(&domain).unwrap();
    write_yaml(
        &common,
        "shared",
        "default",
        &props(&[("base_url", serde_json::json!("http://common"))]),
    );

    let backend = FsStore::with_search_locations(&domain, [domain.clone(), common]).unwrap();
    let source = backend
        .fetch("shared", "default", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(
        source.properties["base_url"],
        serde_json::json!("http://common")
    );
}

// Port of test_save_writes_to_primary_location.
#[tokio::test]
async fn save_writes_to_primary_location() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    let domain = tmp.path().join("domain");
    std::fs::create_dir_all(&common).unwrap();
    std::fs::create_dir_all(&domain).unwrap();

    let backend =
        FsStore::with_search_locations(&domain, [domain.clone(), common.clone()]).unwrap();
    backend
        .save(ConfigSource::new(
            "svc",
            "prod",
            props(&[("key", serde_json::json!("val"))]),
        ))
        .await
        .unwrap();

    let domain_matches = glob_count(&domain, "svc-prod.");
    assert!(domain_matches > 0, "file should be written to domain");
    let common_matches = glob_count(&common, "svc-prod.");
    assert_eq!(common_matches, 0, "file must NOT be written to common");
}

// Port of test_list_uses_primary_location.
#[tokio::test]
async fn list_uses_primary_location() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("common");
    let domain = tmp.path().join("domain");
    write_yaml(
        &common,
        "shared",
        "default",
        &props(&[("x", serde_json::json!(1))]),
    );
    write_yaml(
        &domain,
        "orders",
        "prod",
        &props(&[("y", serde_json::json!(2))]),
    );

    let backend = FsStore::with_search_locations(&domain, [domain.clone(), common]).unwrap();
    let sources = backend.list().await.unwrap();
    let apps: Vec<&str> = sources.iter().map(|s| s.application.as_str()).collect();
    assert!(apps.contains(&"orders"));
    assert!(!apps.contains(&"shared"), "shared lives only in common");
}

// Port of test_single_root_unchanged.
#[tokio::test]
async fn single_root_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    write_yaml(
        tmp.path(),
        "orders",
        "dev",
        &props(&[("db", serde_json::json!("sqlite"))]),
    );
    let backend = FsStore::new(tmp.path()).unwrap();
    let source = backend
        .fetch("orders", "dev", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(source.properties["db"], serde_json::json!("sqlite"));
}

/// Counts files whose name begins with `prefix` anywhere under `dir`
/// (recursive — pyfly's tests use `Path.rglob`).
fn glob_count(dir: &Path, prefix: &str) -> usize {
    let mut count = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix))
                .unwrap_or(false)
            {
                count += 1;
            }
        }
    }
    count
}

// ---------------------------------------------------------------------------
// ConfigServer overlay — port of test_server_returns_property_sources
// ---------------------------------------------------------------------------

// Port of test_server_returns_property_sources.
#[tokio::test]
async fn server_returns_property_sources() {
    let backend = MemoryBackend::new();
    backend
        .save(ConfigSource::new(
            "orders",
            "prod",
            props(&[("a", serde_json::json!("b"))]),
        ))
        .await
        .unwrap();
    let server = ConfigServer::new(backend);
    let env = server
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(env.name, "orders");
    assert_eq!(env.profiles, vec!["prod".to_string()]);
    assert_eq!(
        env.property_sources[0].source,
        props(&[("a", serde_json::json!("b"))])
    );
    assert_eq!(env.property_sources[0].name, "orders-prod");
}

#[tokio::test]
async fn server_overlay_order_highest_first() {
    let backend = MemoryBackend::new();
    // Shared application defaults (lowest precedence).
    backend
        .save(ConfigSource::new(
            "application",
            "default",
            props(&[("shared", serde_json::json!("base"))]),
        ))
        .await
        .unwrap();
    // App-specific prod (highest precedence).
    backend
        .save(ConfigSource::new(
            "orders",
            "prod",
            props(&[("db", serde_json::json!("prod"))]),
        ))
        .await
        .unwrap();
    let server = ConfigServer::new(backend);
    let env = server
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    // Highest-precedence source comes first.
    assert_eq!(env.property_sources[0].name, "orders-prod");
    assert_eq!(
        env.property_sources.last().unwrap().name,
        "application-default"
    );
}

#[tokio::test]
async fn server_returns_none_when_no_overlay() {
    let backend = MemoryBackend::new();
    let server = ConfigServer::new(backend);
    assert!(server
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn server_default_profile_not_queried_twice() {
    // When profile == "default", (orders, default) and (orders, default)
    // collapse to one query; same for application/default.
    let backend = MemoryBackend::new();
    backend
        .save(ConfigSource::new(
            "orders",
            "default",
            props(&[("k", serde_json::json!("v"))]),
        ))
        .await
        .unwrap();
    let server = ConfigServer::new(backend);
    let env = server
        .fetch("orders", "default", "main")
        .await
        .unwrap()
        .expect("present");
    // Only one matching source — no duplicate orders-default entry.
    let count = env
        .property_sources
        .iter()
        .filter(|p| p.name == "orders-default")
        .count();
    assert_eq!(count, 1);
}

// ---------------------------------------------------------------------------
// Git backend — port of test_git_backend.py (local repo, no network)
// ---------------------------------------------------------------------------

/// Runs `git <args>` in `dir`, panicking with stderr on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Creates a local git repository with one committed config file on the
/// `main` branch, returning its path. Mirrors pyfly's `_make_repo`.
fn make_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("origin");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(
        repo.join("orders-prod.yaml"),
        serde_yaml::to_string(&props(&[
            ("db.url", serde_json::json!("postgres://prod")),
            ("workers", serde_json::json!(4)),
        ]))
        .unwrap(),
    )
    .unwrap();
    git(&repo, &["add", "orders-prod.yaml"]);
    git(&repo, &["commit", "-m", "initial config"]);
    repo
}

// Port of test_git_backend_fetch.
#[tokio::test]
async fn git_backend_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = make_repo(tmp.path());
    let clone_dir = tmp.path().join("clone");

    let backend = GitStore::new(origin.to_string_lossy().to_string())
        .label("main")
        .clone_dir(clone_dir);
    let source = backend
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(source.application, "orders");
    assert_eq!(source.profile, "prod");
    assert_eq!(
        source.properties["db.url"],
        serde_json::json!("postgres://prod")
    );
    assert_eq!(source.properties["workers"], serde_json::json!(4));
}

// Port of test_git_backend_list.
#[tokio::test]
async fn git_backend_list() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = make_repo(tmp.path());
    let clone_dir = tmp.path().join("clone");
    let backend = GitStore::new(origin.to_string_lossy().to_string())
        .label("main")
        .clone_dir(clone_dir);
    let sources = backend.list().await.unwrap();
    assert!(sources
        .iter()
        .any(|s| s.application == "orders" && s.profile == "prod"));
}

// Port of test_git_backend_save_commits.
#[tokio::test]
async fn git_backend_save_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = make_repo(tmp.path());
    let clone_dir = tmp.path().join("clone");
    let backend = GitStore::new(origin.to_string_lossy().to_string())
        .label("main")
        .clone_dir(clone_dir.clone());
    backend.fetch("orders", "prod", "main").await.unwrap();

    backend
        .save(ConfigSource::with_label(
            "payments",
            "prod",
            "main",
            props(&[
                ("gateway", serde_json::json!("stripe")),
                ("retries", serde_json::json!(3)),
            ]),
        ))
        .await
        .unwrap();

    let fetched = backend
        .fetch("payments", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(fetched.properties["gateway"], serde_json::json!("stripe"));

    // A commit must have been created.
    let log = Command::new("git")
        .arg("-C")
        .arg(&clone_dir)
        .args(["log", "-1", "--pretty=%s"])
        .output()
        .unwrap();
    let msg = String::from_utf8_lossy(&log.stdout);
    assert!(
        msg.contains("payments") || msg.contains("firefly"),
        "last commit message was {msg:?}"
    );
}

// Port of test_git_backend_save_updates_existing.
#[tokio::test]
async fn git_backend_save_updates_existing() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = make_repo(tmp.path());
    let clone_dir = tmp.path().join("clone");
    let backend = GitStore::new(origin.to_string_lossy().to_string())
        .label("main")
        .clone_dir(clone_dir);

    backend
        .save(ConfigSource::with_label(
            "orders",
            "prod",
            "main",
            props(&[
                ("db.url", serde_json::json!("postgres://prod-v2")),
                ("workers", serde_json::json!(8)),
            ]),
        ))
        .await
        .unwrap();

    let fetched = backend
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(
        fetched.properties["db.url"],
        serde_json::json!("postgres://prod-v2")
    );
    assert_eq!(fetched.properties["workers"], serde_json::json!(8));
}

// Port of test_git_backend_refresh_no_remote — refresh() is a graceful
// no-op when the clone has no remote.
#[tokio::test]
async fn git_backend_refresh_no_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = make_repo(tmp.path());
    let clone_dir = tmp.path().join("clone");
    let backend = GitStore::new(origin.to_string_lossy().to_string())
        .label("main")
        .clone_dir(clone_dir.clone());
    backend.fetch("orders", "prod", "main").await.unwrap();

    // Remove the remote so refresh() must skip gracefully.
    git(&clone_dir, &["remote", "remove", "origin"]);
    backend.refresh().await.expect("refresh must not error");
}

// Port of test_git_backend_missing_returns_none.
#[tokio::test]
async fn git_backend_missing_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = make_repo(tmp.path());
    let clone_dir = tmp.path().join("clone");
    let backend = GitStore::new(origin.to_string_lossy().to_string())
        .label("main")
        .clone_dir(clone_dir);
    assert!(backend
        .fetch("nonexistent", "dev", "main")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn git_backend_uses_tempdir_when_no_clone_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = make_repo(tmp.path());
    let backend = GitStore::new(origin.to_string_lossy().to_string()).label("main");
    let source = backend
        .fetch("orders", "prod", "main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(source.properties["workers"], serde_json::json!(4));
}

// ---------------------------------------------------------------------------
// Store trait optional save path — default unsupported.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_store_save_is_unsupported_by_default() {
    use firefly_config_server::{ConfigServerError, Environment, MemoryStore, Store};
    let store = MemoryStore::new();
    let err = store
        .save("orders", "prod", "main", Environment::default())
        .await
        .unwrap_err();
    assert!(matches!(err, ConfigServerError::Unsupported(_)), "{err:?}");
}

#[tokio::test]
async fn readonly_backend_save_rejected() {
    // A ConfigBackend with the default save() rejects writes.
    struct ReadOnly;
    #[async_trait::async_trait]
    impl ConfigBackend for ReadOnly {
        async fn fetch(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<ConfigSource>, firefly_config_server::BackendError> {
            Ok(None)
        }
        async fn list(&self) -> Result<Vec<ConfigSource>, firefly_config_server::BackendError> {
            Ok(vec![])
        }
    }
    let err = ReadOnly
        .save(ConfigSource::new("a", "b", Properties::new()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, firefly_config_server::BackendError::Unsupported(_)),
        "{err:?}"
    );
}
