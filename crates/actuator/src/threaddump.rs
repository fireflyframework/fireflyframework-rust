//! `GET /actuator/threaddump` — Spring Boot's thread-dump endpoint, adapted
//! to the async-Rust runtime model.
//!
//! Spring's `/actuator/threaddump` snapshots every live JVM thread with its
//! per-frame stack trace (`className` / `methodName` / `fileName` /
//! `lineNumber`), name, id, daemon flag, and state. pyfly reproduces the
//! shape over Python threads.
//!
//! Rust's async runtime has no per-task call stacks to walk — a tokio task is
//! a state machine multiplexed across a small pool of OS worker threads, so
//! there is no portable, stable per-task stack capture. This endpoint
//! therefore reports the **OS worker threads backing the tokio runtime** plus
//! a synthetic summary "thread" describing runtime task occupancy, all under
//! the exact Spring `{threads: [{threadName, threadId, daemon, threadState,
//! stackTrace}]}` wire shape so Spring-oriented operators and tooling that
//! call `/actuator/threaddump` get a well-formed, parseable response.
//!
//! This mirrors the deliberate runtime-model adaptation already used for
//! `/actuator/tasks` (counting alive tokio tasks where Go counts goroutines):
//! the *operational intent* — "how much concurrent work is in flight, on what
//! threads?" — is preserved even though async Rust cannot supply per-task
//! stack frames.

use serde::Serialize;
use serde_json::{json, Value};

/// One stack frame in a [`ThreadInfo::stack_trace`], matching Spring's frame
/// shape. Always present in the wire object even when empty so consumers can
/// rely on the field set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StackFrame {
    /// The declaring "class" — for Rust, the module path of the frame
    /// (best-effort; empty when unavailable).
    #[serde(rename = "className")]
    pub class_name: String,
    /// The method/function name.
    #[serde(rename = "methodName")]
    pub method_name: String,
    /// The source file, when known (empty otherwise).
    #[serde(rename = "fileName")]
    pub file_name: String,
    /// The source line, when known (`0` otherwise).
    #[serde(rename = "lineNumber")]
    pub line_number: u32,
}

/// One thread entry in the dump, matching Spring's
/// `{threadName, threadId, daemon, threadState, stackTrace}` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadInfo {
    /// Human-readable thread name.
    #[serde(rename = "threadName")]
    pub thread_name: String,
    /// Numeric thread id.
    #[serde(rename = "threadId")]
    pub thread_id: u64,
    /// Whether the thread is a daemon/background thread.
    pub daemon: bool,
    /// Thread state in Spring's vocabulary (`RUNNABLE`, `WAITING`, …).
    #[serde(rename = "threadState")]
    pub thread_state: String,
    /// Per-frame stack trace (empty in async Rust — see the module docs).
    #[serde(rename = "stackTrace")]
    pub stack_trace: Vec<StackFrame>,
}

/// Builds the `/actuator/threaddump` body — `{"threads": [ThreadInfo, …]}` —
/// from the current tokio runtime metrics. The first entry is a synthetic
/// runtime-summary "thread" reporting worker count and alive-task occupancy;
/// the remaining entries are the runtime's worker threads.
///
/// Returns at least one entry whenever called from within a tokio runtime
/// context (the synthetic summary thread is always present), keeping the
/// pyfly contract `len(threads) >= 1`.
pub fn thread_dump() -> Value {
    let metrics = tokio::runtime::Handle::current().metrics();
    let workers = metrics.num_workers();
    let alive = metrics.num_alive_tasks();

    let mut threads: Vec<ThreadInfo> = Vec::with_capacity(workers + 1);

    // Synthetic runtime summary thread — the async-Rust analog of a "main"
    // thread, carrying the occupancy numbers a `/actuator/tasks` caller would
    // otherwise read. Stable id 0 so tooling can recognise it.
    threads.push(ThreadInfo {
        thread_name: "tokio-runtime".to_string(),
        thread_id: 0,
        daemon: false,
        thread_state: if alive > 0 { "RUNNABLE" } else { "WAITING" }.to_string(),
        stack_trace: Vec::new(),
    });

    // One entry per runtime worker thread. Workers are daemon-style: they run
    // for the lifetime of the runtime and are RUNNABLE while the runtime is up.
    for i in 0..workers {
        threads.push(ThreadInfo {
            thread_name: format!("tokio-runtime-worker-{i}"),
            thread_id: (i as u64) + 1,
            daemon: true,
            thread_state: "RUNNABLE".to_string(),
            stack_trace: Vec::new(),
        });
    }

    json!({ "threads": threads })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dump_has_summary_and_worker_threads() {
        let body = thread_dump();
        let threads = body["threads"].as_array().expect("threads array");
        // pyfly contract: at least one thread.
        assert!(!threads.is_empty());
        // The synthetic summary thread always leads.
        assert_eq!(threads[0]["threadName"], "tokio-runtime");
        assert_eq!(threads[0]["threadId"], 0);
        // Worker threads are present (2 configured).
        assert!(threads
            .iter()
            .any(|t| t["threadName"] == "tokio-runtime-worker-0"));
        // Every entry carries the Spring field set.
        for t in threads {
            assert!(t.get("threadName").is_some());
            assert!(t.get("threadId").is_some());
            assert!(t.get("daemon").is_some());
            assert!(t.get("threadState").is_some());
            assert!(t["stackTrace"].is_array());
        }
    }

    #[test]
    fn frame_serializes_with_spring_field_names() {
        let frame = StackFrame {
            class_name: "firefly_actuator::threaddump".into(),
            method_name: "thread_dump".into(),
            file_name: "threaddump.rs".into(),
            line_number: 42,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(
            json,
            json!({
                "className": "firefly_actuator::threaddump",
                "methodName": "thread_dump",
                "fileName": "threaddump.rs",
                "lineNumber": 42
            })
        );
    }
}
