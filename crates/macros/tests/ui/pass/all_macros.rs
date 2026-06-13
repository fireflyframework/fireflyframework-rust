// A single compile-pass case exercising every Tier-1 + clean Tier-2 macro
// against the real facade. If any macro's expansion stops compiling, this
// fails and trybuild reports it.

use std::sync::Arc;

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

// ---- CQRS derives + handlers ----
#[derive(Clone, Serialize, Command)]
struct CreateThing {
    #[firefly(validate)]
    name: String,
}

#[derive(Clone, Serialize, Query)]
#[firefly(cache_ttl = "1m")]
struct GetThing {
    id: String,
}

#[derive(Clone)]
struct ThingView {
    id: String,
}

#[command_handler]
async fn handle_create(c: CreateThing) -> Result<ThingView, CqrsError> {
    Ok(ThingView { id: c.name })
}

#[query_handler]
async fn handle_get(q: GetThing) -> Result<ThingView, CqrsError> {
    Ok(ThingView { id: q.id })
}

// ---- Component / Service / Repository + register_all! ----
#[derive(Repository, Default)]
struct Repo {
    n: u32,
}

#[derive(Service)]
#[firefly(scope = "singleton", primary)]
struct Svc {
    #[autowired]
    repo: Arc<Repo>,
}

#[derive(Component, Default)]
#[firefly(scope = "transient", name = "plain")]
struct Plain;

// ---- Scheduled ----
#[scheduled(cron = "0 2 * * *", zone = "UTC")]
async fn nightly() -> Result<(), std::io::Error> {
    Ok(())
}

#[scheduled(fixed_rate = "30s", initial_delay = "5s")]
async fn ticker() -> Result<(), std::io::Error> {
    Ok(())
}

#[scheduled(fixed_delay = "10s")]
async fn drainer() -> Result<(), std::io::Error> {
    Ok(())
}

// ---- Event sourcing derives ----
#[derive(Clone, Serialize, Deserialize, DomainEvent)]
#[firefly(event_type = "ThingCreated")]
struct ThingCreated {
    id: String,
}

#[derive(Default, AggregateRoot)]
#[firefly(aggregate_type = "Thing")]
struct ThingAggregate {
    root: firefly::eventsourcing::AggregateRoot,
}

// ---- REST controller ----
use axum::extract::{Path, State};
use axum::Json;

#[derive(Clone)]
struct Api;

#[rest_controller(path = "/api/v1/things")]
impl Api {
    #[get("/:id")]
    async fn get(State(_a): State<Api>, Path(id): Path<String>) -> WebResult<Json<String>> {
        Ok(Json(id))
    }

    #[post("")]
    async fn create(State(_a): State<Api>) -> WebResult<Json<&'static str>> {
        Ok(Json("ok"))
    }
}

// ---- Event listener ----
#[event_listener("things.created")]
async fn on_created(_ev: firefly::eda::Event) -> firefly::kernel::FireflyResult<()> {
    Ok(())
}

fn main() {
    // Reference the generated symbols so the build proves they exist.
    let bus = Bus::new();
    register_handle_create(&bus);
    register_handle_get(&bus);

    let container = Container::new();
    firefly::register_all!(&container, [Repo, Svc, Plain]);

    let scheduler = Scheduler::new();
    schedule_nightly(&scheduler);
    schedule_ticker(&scheduler);
    schedule_drainer(&scheduler);

    let _router = Api::routes(Api);

    let _ = ThingCreated::EVENT_TYPE;
    let _ = ThingAggregate::AGGREGATE_TYPE;

    // The listener subscribe helper exists (not awaited here).
    let _f = subscribe_on_created;
}
