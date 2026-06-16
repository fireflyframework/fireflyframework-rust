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

//! Fully-wired demo of the embedded admin dashboard.
//!
//! ```sh
//! cargo run -p firefly-admin --example dashboard
//! # then open http://127.0.0.1:8099/admin/ in a browser
//! ```
//!
//! Unlike a bare [`AdminDeps::new`], this wires **every** optional
//! collaborator so all dashboard panels show real data:
//!
//! - **Health** — three indicators (db / cache / disk).
//! - **Beans / Bean Graph / Overview stereotypes** — a DI [`Container`] with
//!   service / repository / configuration / controller beans.
//! - **CQRS** — a [`Bus`] with command + query handlers.
//! - **Scheduled** — a [`Scheduler`] with fixed-delay + fixed-rate tasks.
//! - **Transactions** — an [`OrchestrationRegistry`] with a saga, a workflow
//!   and a TCC definition.
//! - **Caches** — a [`CacheOps`] adapter reporting two caches.
//! - **Loggers** — a [`LoggersState`] with root + per-target levels.
//! - **Metrics / Traces / Log Viewer** — counters and gauges that move, plus
//!   a background loop recording HTTP traces and emitting log records live.
//!
//! Everything here is real framework wiring — the same calls a production
//! service makes — so the dashboard is exercised end to end.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use firefly_actuator::{
    CacheDescriptor, CacheOps, HealthComposite, HealthResult, IndicatorFn, LoggersState,
    MetricRegistry,
};
use firefly_admin::{mount, AdminConfig, AdminDeps, LogBuffer, LogRecord, TraceBuffer, TraceEntry};
use firefly_container::Container;
use firefly_cqrs::{Bus, CqrsError};
use firefly_orchestration::{
    Node, OrchestrationRegistry, Saga, Step, Tcc, TccParticipant, Workflow,
};
use firefly_scheduling::{FixedDelayTrigger, FixedRateTrigger, Scheduler, Task};
use serde::Serialize;

// ── DI beans (registered with stereotypes) ──────────────────────────────────

#[derive(Clone)]
struct AccountService;
#[derive(Clone)]
struct LedgerService;
#[derive(Clone)]
struct AccountRepository;
#[derive(Clone)]
struct LedgerRepository;
#[derive(Clone)]
struct WalletController;
#[derive(Clone)]
struct LedgerConfig;

// ── CQRS messages ────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
struct OpenAccount {
    owner: String,
}
#[derive(Clone, Serialize)]
struct Deposit {
    account: String,
    cents: u64,
}
#[derive(Clone, Serialize)]
struct GetBalance {
    account: String,
}

impl firefly_cqrs::Message for OpenAccount {}
impl firefly_cqrs::Message for Deposit {}
impl firefly_cqrs::Message for GetBalance {}

// ── Cache adapter ────────────────────────────────────────────────────────────

struct DemoCache;

#[async_trait]
impl CacheOps for DemoCache {
    fn caches(&self) -> Vec<CacheDescriptor> {
        vec![
            CacheDescriptor {
                name: "accounts".into(),
                target: "firefly_cache::MemoryAdapter".into(),
            },
            CacheDescriptor {
                name: "fx-rates".into(),
                target: "firefly_cache_redis::RedisAdapter".into(),
            },
        ]
    }

    async fn evict(&self, name: &str) -> bool {
        matches!(name, "accounts" | "fx-rates")
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[tokio::main]
async fn main() {
    // Make the Environment panel show active profiles.
    std::env::set_var("FIREFLY_PROFILES_ACTIVE", "demo,local");

    // ── Health: three indicators ─────────────────────────────────────────
    let health = Arc::new(HealthComposite::new());
    health.add(IndicatorFn::new("db", || async { HealthResult::up() }));
    health.add(IndicatorFn::new("cache", || async { HealthResult::up() }));
    health.add(IndicatorFn::new("diskSpace", || async {
        HealthResult::up()
    }));

    // ── Metrics: a handful of meters that we keep moving below ────────────
    let metrics = Arc::new(MetricRegistry::new());
    metrics.counter("http_server_requests_total").add(128);
    metrics.counter("orders_placed_total").add(37);
    metrics.counter("payments_captured_total").add(31);
    metrics.gauge("accounts_active").set(412.0);
    metrics.gauge("queue_depth").set(3.0);

    // ── DI container: stereotyped beans ──────────────────────────────────
    let container = Arc::new(Container::new());
    container.register_instance(AccountService);
    container.set_stereotype::<AccountService>("service");
    container.register_instance(LedgerService);
    container.set_stereotype::<LedgerService>("service");
    container.register_instance(AccountRepository);
    container.set_stereotype::<AccountRepository>("repository");
    container.register_instance(LedgerRepository);
    container.set_stereotype::<LedgerRepository>("repository");
    container.register_instance(WalletController);
    container.set_stereotype::<WalletController>("controller");
    container.register_instance(LedgerConfig);
    container.set_stereotype::<LedgerConfig>("configuration");
    // Resolve a few so the resolution counts are non-zero.
    let _ = container.resolve::<AccountService>();
    let _ = container.resolve::<AccountService>();
    let _ = container.resolve::<LedgerRepository>();

    // ── CQRS bus: command + query handlers ───────────────────────────────
    let bus = Arc::new(Bus::new());
    bus.register::<OpenAccount, String, _, _>(|cmd: OpenAccount| async move {
        Ok::<_, CqrsError>(format!("account:{}", cmd.owner))
    });
    bus.register::<Deposit, u64, _, _>(|cmd: Deposit| async move { Ok::<_, CqrsError>(cmd.cents) });
    bus.register::<GetBalance, u64, _, _>(|_q: GetBalance| async move { Ok::<_, CqrsError>(0) });

    // ── Scheduler: fixed-delay + fixed-rate tasks ────────────────────────
    let scheduler = Arc::new(Scheduler::new());
    scheduler.register(Task::new(
        "settlement-sweep",
        FixedDelayTrigger::new(Duration::from_secs(30)),
        || async { Ok(()) },
    ));
    scheduler.register(Task::new(
        "fx-rate-refresh",
        FixedRateTrigger {
            start: chrono::Local::now(),
            period: Duration::from_secs(60),
        },
        || async { Ok(()) },
    ));
    scheduler.register(Task::new(
        "stale-session-reaper",
        FixedDelayTrigger::new(Duration::from_secs(120)),
        || async { Ok(()) },
    ));

    // ── Orchestration: saga + workflow + TCC ─────────────────────────────
    let orchestration = Arc::new(OrchestrationRegistry::new());
    orchestration.register_saga(
        Saga::new("transferFunds")
            .step(Step::new("reserve", || async { Ok(()) }))
            .step(Step::new("debit", || async { Ok(()) }))
            .step(Step::new("credit", || async { Ok(()) })),
    );
    orchestration.register_workflow(
        Workflow::new("onboardCustomer")
            .node(Node::new("submitKyc", || async { Ok(()) }))
            .node(Node::new("approve", || async { Ok(()) }).depends_on(["submitKyc"]))
            .node(Node::new("openAccount", || async { Ok(()) }).depends_on(["approve"])),
    );
    orchestration.register_tcc(
        Tcc::new("reserveInventory").participant(TccParticipant::new(
            "warehouse",
            || async { Ok(()) },
            || async { Ok(()) },
        )),
    );

    // ── Loggers: root + per-target levels ────────────────────────────────
    let loggers = Arc::new(LoggersState::with_reload_fn(
        |_filter| Ok(()),
        "info,firefly_admin=debug,demo_service=info,tokio=warn,sqlx=warn",
    ));

    // ── Traces + logs: seed a few, then keep them live below ─────────────
    let traces = Arc::new(TraceBuffer::new());
    let logs = LogBuffer::new();
    seed_traces(&traces);
    seed_logs(&logs);

    let deps = AdminDeps {
        scheduler: Some(Arc::clone(&scheduler)),
        bus: Some(bus),
        orchestration: Some(orchestration),
        cache: Some(Arc::new(DemoCache)),
        loggers: Some(loggers),
        container: Some(container),
        ..AdminDeps::new(
            "demo-service",
            "26.6.15",
            health,
            Arc::clone(&metrics),
            Arc::clone(&traces),
            logs.clone(),
        )
    };

    // Background loop: move metrics, record traces, emit log records — so the
    // live SSE streams (metrics / traces / logfile / runtime) have something
    // to push and the dashboard feels like a running service.
    tokio::spawn(live_activity(metrics, traces, logs));

    let app = mount(AdminConfig::default(), deps);
    let addr = std::env::var("ADMIN_ADDR").unwrap_or_else(|_| "127.0.0.1:8099".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    println!("admin dashboard on http://{addr}/admin/");
    axum::serve(listener, app).await.expect("serve");
}

/// Seeds a handful of representative HTTP traces.
fn seed_traces(traces: &TraceBuffer) {
    let samples = [
        ("GET", "/api/accounts", 200u16, 4.2f64),
        ("POST", "/api/accounts", 201, 11.8),
        ("GET", "/api/accounts/42/balance", 200, 2.1),
        ("POST", "/api/transfers", 202, 18.6),
        ("GET", "/api/fx-rates", 200, 1.4),
        ("DELETE", "/api/sessions/abc", 204, 3.0),
        ("GET", "/api/accounts/999", 404, 0.9),
    ];
    for (method, path, status, ms) in samples {
        traces.record(TraceEntry {
            timestamp: now_rfc3339(),
            method: method.into(),
            path: path.into(),
            query_string: String::new(),
            status,
            duration_ms: ms,
            client_host: Some("127.0.0.1".into()),
            content_type: Some("application/json".into()),
            user_agent: "firefly-admin-demo/26.6.15".into(),
            content_length: Some(256),
        });
    }
}

/// Seeds a handful of representative log records.
fn seed_logs(logs: &LogBuffer) {
    let samples = [
        ("INFO", "demo_service::boot", "service started on :8099"),
        ("INFO", "firefly_scheduling", "registered 3 scheduled tasks"),
        (
            "DEBUG",
            "firefly_cqrs::bus",
            "registered handler OpenAccount",
        ),
        ("WARN", "demo_service::fx", "rate provider latency 412ms"),
        ("INFO", "demo_service::ledger", "settlement sweep complete"),
    ];
    for (level, logger, message) in samples {
        logs.push(LogRecord {
            id: 0,
            timestamp: now_rfc3339(),
            level: level.into(),
            logger: logger.into(),
            message: message.into(),
            context: String::new(),
            thread: Some("main".into()),
        });
    }
}

/// Keeps the dashboard alive: every two seconds bump counters, nudge a gauge,
/// record one trace and emit one log record.
async fn live_activity(metrics: Arc<MetricRegistry>, traces: Arc<TraceBuffer>, logs: LogBuffer) {
    let paths = [
        ("GET", "/api/accounts", 200u16),
        ("POST", "/api/transfers", 202),
        ("GET", "/api/fx-rates", 200),
        ("GET", "/api/accounts/42/balance", 200),
    ];
    let mut tick: u64 = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        tick += 1;

        metrics.counter("http_server_requests_total").inc();
        if tick.is_multiple_of(3) {
            metrics.counter("orders_placed_total").inc();
            metrics.counter("payments_captured_total").inc();
        }
        // A gentle oscillation so the gauge line chart visibly moves.
        let depth = 3.0 + ((tick % 7) as f64);
        metrics.gauge("queue_depth").set(depth);
        metrics
            .gauge("accounts_active")
            .set(412.0 + (tick % 11) as f64);

        let (method, path, status) = paths[(tick as usize) % paths.len()];
        traces.record(TraceEntry {
            timestamp: now_rfc3339(),
            method: method.into(),
            path: path.into(),
            query_string: String::new(),
            status,
            duration_ms: 1.0 + (tick % 9) as f64,
            client_host: Some("127.0.0.1".into()),
            content_type: Some("application/json".into()),
            user_agent: "firefly-admin-demo/26.6.15".into(),
            content_length: Some(256),
        });

        logs.push(LogRecord {
            id: 0,
            timestamp: now_rfc3339(),
            level: if tick.is_multiple_of(10) {
                "WARN"
            } else {
                "INFO"
            }
            .into(),
            logger: "demo_service::traffic".into(),
            message: format!("handled {method} {path} -> {status}"),
            context: format!("tick={tick}"),
            thread: Some("tokio-worker".into()),
        });
    }
}
