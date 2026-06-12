//! `GET/POST /actuator/loggers` — runtime log-level read/write over a
//! `tracing_subscriber` reload handle, Spring Boot parity.
//!
//! pyfly mutates Python's `logging` hierarchy in place; the Rust port
//! keeps a directive table (root level + per-target levels) inside
//! [`LoggersState`] and rebuilds + reloads an
//! [`EnvFilter`](tracing_subscriber::EnvFilter) through
//! [`reload::Handle`](tracing_subscriber::reload::Handle) on every
//! change. The wire shapes use Spring's level vocabulary
//! (`OFF/ERROR/WARN/INFO/DEBUG/TRACE`) so the response is drop-in
//! compatible with Spring Boot tooling.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::RwLock;

use serde_json::{json, Value};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{reload, EnvFilter};

/// Spring Boot's level vocabulary (most → least severe, plus OFF) —
/// the `levels` array of `GET /actuator/loggers`.
pub const SPRING_LEVELS: [&str; 6] = ["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"];

/// Errors from [`LoggersState::set_level`].
#[derive(Debug)]
pub enum LoggersError {
    /// The requested level is not in [`SPRING_LEVELS`] — rendered as
    /// HTTP 400.
    UnknownLevel(String),
    /// Reloading the subscriber's filter failed (e.g. the subscriber
    /// was dropped) — rendered as HTTP 500.
    Reload(String),
}

impl fmt::Display for LoggersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoggersError::UnknownLevel(msg) | LoggersError::Reload(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for LoggersError {}

type ReloadFn = Box<dyn Fn(EnvFilter) -> Result<(), String> + Send + Sync>;

/// Runtime log-level control behind `/actuator/loggers`: a directive
/// table (ROOT level + per-target levels) that rebuilds an [`EnvFilter`]
/// and pushes it through the wrapped reload handle on every change.
pub struct LoggersState {
    initial_root: LevelFilter,
    root: RwLock<LevelFilter>,
    targets: RwLock<BTreeMap<String, LevelFilter>>,
    reload: ReloadFn,
}

impl LoggersState {
    /// Wraps a `tracing_subscriber` reload handle, starting from a root
    /// level of `INFO` and no per-target directives.
    pub fn from_handle<S: 'static>(handle: reload::Handle<EnvFilter, S>) -> Self {
        Self::from_handle_with_directives(handle, "info")
    }

    /// Wraps a reload handle seeded with the given env-filter directive
    /// string (e.g. `"info,my_crate=debug"`) so `GET /actuator/loggers`
    /// reflects the levels the subscriber started with.
    pub fn from_handle_with_directives<S: 'static>(
        handle: reload::Handle<EnvFilter, S>,
        directives: &str,
    ) -> Self {
        Self::with_reload_fn(
            move |filter| handle.reload(filter).map_err(|e| e.to_string()),
            directives,
        )
    }

    /// Builds a state over an arbitrary reload function — useful for
    /// tests and for subscribers not managed by
    /// `tracing_subscriber::reload`.
    pub fn with_reload_fn<F>(reload: F, directives: &str) -> Self
    where
        F: Fn(EnvFilter) -> Result<(), String> + Send + Sync + 'static,
    {
        let mut root = LevelFilter::INFO;
        let mut targets = BTreeMap::new();
        for directive in directives.split(',') {
            let directive = directive.trim();
            if directive.is_empty() {
                continue;
            }
            match directive.split_once('=') {
                Some((target, level)) => {
                    if let Some(level) = parse_level(level) {
                        targets.insert(target.trim().to_string(), level);
                    }
                }
                None => {
                    if let Some(level) = parse_level(directive) {
                        root = level;
                    }
                }
            }
        }
        Self {
            initial_root: root,
            root: RwLock::new(root),
            targets: RwLock::new(targets),
            reload: Box::new(reload),
        }
    }

    /// The current directive string, e.g. `"info,my_crate=debug"` —
    /// what the wrapped subscriber filter was last reloaded with.
    pub fn directives(&self) -> String {
        let root = *self.root.read().expect("loggers root lock poisoned");
        let targets = self.targets.read().expect("loggers target lock poisoned");
        let mut out = level_directive(root).to_string();
        for (target, level) in targets.iter() {
            out.push(',');
            out.push_str(target);
            out.push('=');
            out.push_str(level_directive(*level));
        }
        out
    }

    /// The full `GET /actuator/loggers` body: Spring's `levels`
    /// vocabulary, the `ROOT` logger plus every configured target, and
    /// the (empty) `groups` map.
    pub fn levels_json(&self) -> Value {
        let root = *self.root.read().expect("loggers root lock poisoned");
        let targets = self.targets.read().expect("loggers target lock poisoned");

        let mut loggers = serde_json::Map::new();
        loggers.insert(
            "ROOT".into(),
            json!({
                "configuredLevel": level_name(root),
                "effectiveLevel": level_name(root),
            }),
        );
        for (target, level) in targets.iter() {
            loggers.insert(
                target.clone(),
                json!({
                    "configuredLevel": level_name(*level),
                    "effectiveLevel": level_name(*level),
                }),
            );
        }

        json!({
            "levels": SPRING_LEVELS,
            "loggers": loggers,
            "groups": {},
        })
    }

    /// The `GET /actuator/loggers/{name}` body:
    /// `{"configuredLevel": …, "effectiveLevel": …}`. A target without
    /// its own directive reports a `null` configured level and inherits
    /// its effective level from the closest configured ancestor module
    /// (`a::b::c` → `a::b` → `a`) or `ROOT`.
    pub fn logger_json(&self, name: &str) -> Value {
        if name.eq_ignore_ascii_case("ROOT") {
            let root = *self.root.read().expect("loggers root lock poisoned");
            return json!({
                "configuredLevel": level_name(root),
                "effectiveLevel": level_name(root),
            });
        }
        let targets = self.targets.read().expect("loggers target lock poisoned");
        let configured = targets.get(name).copied();
        let effective = configured.unwrap_or_else(|| {
            let mut prefix = name;
            while let Some(idx) = prefix.rfind("::") {
                prefix = &prefix[..idx];
                if let Some(level) = targets.get(prefix) {
                    return *level;
                }
            }
            *self.root.read().expect("loggers root lock poisoned")
        });
        json!({
            "configuredLevel": configured.map(level_name),
            "effectiveLevel": level_name(effective),
        })
    }

    /// Sets or resets a logger level — the `POST /actuator/loggers/{name}`
    /// operation. `level = None` (or an empty string) resets: a target
    /// directive is removed (inheriting again), `ROOT` returns to the
    /// level it was constructed with. Rebuilds the [`EnvFilter`] and
    /// reloads the subscriber.
    pub fn set_level(&self, name: &str, level: Option<&str>) -> Result<(), LoggersError> {
        let parsed = match level {
            None | Some("") => None,
            Some(raw) => Some(parse_level(raw).ok_or_else(|| {
                LoggersError::UnknownLevel(format!(
                    "Unknown level: {raw}. Valid levels: {}",
                    SPRING_LEVELS.join(", ")
                ))
            })?),
        };

        if name.eq_ignore_ascii_case("ROOT") {
            *self.root.write().expect("loggers root lock poisoned") =
                parsed.unwrap_or(self.initial_root);
        } else {
            let mut targets = self.targets.write().expect("loggers target lock poisoned");
            match parsed {
                Some(level) => {
                    targets.insert(name.to_string(), level);
                }
                None => {
                    targets.remove(name);
                }
            }
        }

        let directives = self.directives();
        let filter = EnvFilter::try_new(&directives)
            .map_err(|e| LoggersError::Reload(format!("invalid filter '{directives}': {e}")))?;
        (self.reload)(filter).map_err(LoggersError::Reload)
    }
}

/// Parses a Spring level name (case-insensitive) into a [`LevelFilter`].
fn parse_level(raw: &str) -> Option<LevelFilter> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "OFF" => Some(LevelFilter::OFF),
        "ERROR" => Some(LevelFilter::ERROR),
        "WARN" => Some(LevelFilter::WARN),
        "INFO" => Some(LevelFilter::INFO),
        "DEBUG" => Some(LevelFilter::DEBUG),
        "TRACE" => Some(LevelFilter::TRACE),
        _ => None,
    }
}

/// Spring's name for a level (`WARN`, never `WARNING`).
fn level_name(level: LevelFilter) -> &'static str {
    if level == LevelFilter::OFF {
        "OFF"
    } else if level == LevelFilter::ERROR {
        "ERROR"
    } else if level == LevelFilter::WARN {
        "WARN"
    } else if level == LevelFilter::INFO {
        "INFO"
    } else if level == LevelFilter::DEBUG {
        "DEBUG"
    } else {
        "TRACE"
    }
}

/// The env-filter directive token for a level (lowercase).
fn level_directive(level: LevelFilter) -> &'static str {
    if level == LevelFilter::OFF {
        "off"
    } else if level == LevelFilter::ERROR {
        "error"
    } else if level == LevelFilter::WARN {
        "warn"
    } else if level == LevelFilter::INFO {
        "info"
    } else if level == LevelFilter::DEBUG {
        "debug"
    } else {
        "trace"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn recording_state(directives: &str) -> (LoggersState, Arc<Mutex<Vec<String>>>) {
        let reloads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&reloads);
        let state = LoggersState::with_reload_fn(
            move |filter| {
                sink.lock().unwrap().push(filter.to_string());
                Ok(())
            },
            directives,
        );
        (state, reloads)
    }

    // pyfly: test_get_lists_loggers_levels_and_groups
    #[test]
    fn levels_json_lists_root_levels_and_groups() {
        let (state, _) = recording_state("info,my_crate=debug");
        let body = state.levels_json();
        assert_eq!(
            body["levels"],
            json!(["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"])
        );
        assert_eq!(body["loggers"]["ROOT"]["configuredLevel"], "INFO");
        assert_eq!(body["loggers"]["my_crate"]["configuredLevel"], "DEBUG");
        assert!(body["groups"].is_object());
    }

    // pyfly: test_levels_use_spring_names_not_python
    #[test]
    fn levels_use_spring_names() {
        let (state, _) = recording_state("warn");
        assert_eq!(
            state.levels_json()["loggers"]["ROOT"]["configuredLevel"],
            "WARN"
        );
    }

    // pyfly: test_get_single_logger_by_name
    #[test]
    fn logger_json_reports_configured_and_effective() {
        let (state, _) = recording_state("info,app::db=debug");
        let body = state.logger_json("app::db");
        assert_eq!(body["configuredLevel"], "DEBUG");
        assert_eq!(body["effectiveLevel"], "DEBUG");
    }

    #[test]
    fn logger_json_inherits_from_ancestor_then_root() {
        let (state, _) = recording_state("warn,app=debug");
        let child = state.logger_json("app::db::pool");
        assert_eq!(child["configuredLevel"], Value::Null);
        assert_eq!(child["effectiveLevel"], "DEBUG");
        let orphan = state.logger_json("other");
        assert_eq!(orphan["configuredLevel"], Value::Null);
        assert_eq!(orphan["effectiveLevel"], "WARN");
    }

    // pyfly: test_set_logger_level_direct_returns_none_on_success
    #[test]
    fn set_level_updates_directives_and_reloads() {
        let (state, reloads) = recording_state("info");
        state.set_level("my_crate", Some("DEBUG")).unwrap();
        assert_eq!(state.directives(), "info,my_crate=debug");
        let seen = reloads.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("my_crate=debug"), "{seen:?}");
    }

    // pyfly: test_post_null_resets_level
    #[test]
    fn set_level_none_resets_target_and_root() {
        let (state, _) = recording_state("warn");
        state.set_level("my_crate", Some("TRACE")).unwrap();
        state.set_level("my_crate", None).unwrap();
        assert_eq!(state.directives(), "warn");

        state.set_level("ROOT", Some("ERROR")).unwrap();
        assert_eq!(state.directives(), "error");
        state.set_level("root", None).unwrap();
        assert_eq!(state.directives(), "warn", "ROOT resets to initial");
    }

    // pyfly: test_post_accepts_trace_and_off
    #[test]
    fn set_level_accepts_trace_and_off() {
        let (state, _) = recording_state("info");
        state.set_level("a", Some("TRACE")).unwrap();
        state.set_level("b", Some("OFF")).unwrap();
        assert_eq!(state.directives(), "info,a=trace,b=off");
    }

    // pyfly: test_post_invalid_level_returns_400
    #[test]
    fn set_level_rejects_unknown_level() {
        let (state, reloads) = recording_state("info");
        let err = state.set_level("ROOT", Some("BANANA")).unwrap_err();
        match err {
            LoggersError::UnknownLevel(msg) => {
                assert!(msg.contains("BANANA"), "{msg}");
                assert!(msg.contains("TRACE"), "{msg}");
            }
            other => panic!("expected UnknownLevel, got {other:?}"),
        }
        assert!(reloads.lock().unwrap().is_empty(), "no reload on error");
    }

    #[test]
    fn reload_failure_is_surfaced() {
        let state = LoggersState::with_reload_fn(|_| Err("subscriber gone".into()), "info");
        let err = state.set_level("ROOT", Some("DEBUG")).unwrap_err();
        assert!(matches!(err, LoggersError::Reload(_)));
        assert_eq!(err.to_string(), "subscriber gone");
    }

    #[test]
    fn from_handle_reloads_the_subscriber_filter() {
        let (layer, handle) =
            reload::Layer::<EnvFilter, tracing_subscriber::Registry>::new(EnvFilter::new("info"));
        let state: LoggersState = LoggersState::from_handle_with_directives(handle.clone(), "info");
        state.set_level("my_crate", Some("DEBUG")).unwrap();
        let current = handle.with_current(|f| f.to_string()).unwrap();
        assert!(current.contains("my_crate=debug"), "{current}");
        drop(layer);
    }
}
