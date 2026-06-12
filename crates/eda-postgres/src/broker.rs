//! The [`PostgresBroker`] — outbox + LISTEN/NOTIFY + advisory-lock
//! drain loop, ported from pyfly's `PostgresEventBus`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firefly_eda::{EdaError, EdaResult, Event, Handler, Publisher, Subscriber};
use futures::stream::StreamExt;
use tokio::sync::{Notify, RwLock};
use tokio::task::JoinHandle;
use tokio_postgres::{AsyncMessage, Client, NoTls};

use crate::sql;

/// Connection + topology configuration for a [`PostgresBroker`], the
/// Rust analog of the keyword arguments to pyfly's `PostgresEventBus`.
///
/// `dsn` (and the optional separate `listen_dsn`) accept either a
/// `postgresql://` URL or a tokio-postgres keyword/value string
/// (`host=… user=…`); SQLAlchemy dialect markers like
/// `postgresql+asyncpg://` are stripped automatically.
#[derive(Debug, Clone)]
pub struct PostgresConfig {
    dsn: String,
    listen_dsn: Option<String>,
    channel: String,
    destinations: Option<Vec<String>>,
    group: String,
    poll_interval: Duration,
}

impl PostgresConfig {
    /// Default `NOTIFY` channel, matching the framework table prefix.
    pub const DEFAULT_CHANNEL: &'static str = "firefly_eda";
    /// Default consumer group, matching pyfly's `"default"`.
    pub const DEFAULT_GROUP: &'static str = "default";
    /// Default poll-fallback interval, matching pyfly's `5.0` seconds.
    pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

    /// Builds a config from a connection string, with pyfly's defaults:
    /// channel `firefly_eda`, group `default`, 5 s poll interval, and no
    /// destination filter (every outbox row is delivered).
    pub fn new(dsn: impl Into<String>) -> Self {
        Self {
            dsn: dsn.into(),
            listen_dsn: None,
            channel: Self::DEFAULT_CHANNEL.to_string(),
            destinations: None,
            group: Self::DEFAULT_GROUP.to_string(),
            poll_interval: Self::DEFAULT_POLL_INTERVAL,
        }
    }

    /// Sets a separate DSN for the long-lived `LISTEN` connection. Use a
    /// direct connection here (no pgbouncer in transaction-pooling mode),
    /// since the listener holds the session open. Defaults to `dsn`.
    #[must_use]
    pub fn listen_dsn(mut self, dsn: impl Into<String>) -> Self {
        self.listen_dsn = Some(dsn.into());
        self
    }

    /// Overrides the `NOTIFY` channel name. Must be a valid Postgres
    /// identifier (`[A-Za-z_][A-Za-z0-9_]*`) — `NOTIFY` cannot take bind
    /// parameters, so the name is validated before interpolation.
    #[must_use]
    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = channel.into();
        self
    }

    /// Restricts delivery to the given destination topics. When unset
    /// (the default) every outbox row is delivered to matching handlers.
    #[must_use]
    pub fn destinations<I, S>(mut self, destinations: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.destinations = Some(destinations.into_iter().map(Into::into).collect());
        self
    }

    /// Sets the consumer-group name. All replicas sharing a group
    /// coordinate through one advisory lock and share one offset cursor.
    #[must_use]
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Sets the poll-fallback interval — how often the drain loop wakes
    /// even without a `NOTIFY`, so events that arrived during a listener
    /// reconnect are never stuck.
    #[must_use]
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// The destination filter, if any (test/inspection accessor).
    #[must_use]
    pub fn destinations_ref(&self) -> Option<&[String]> {
        self.destinations.as_deref()
    }

    /// The normalised primary DSN (dialect markers stripped).
    #[must_use]
    pub fn normalised_dsn(&self) -> String {
        sql::normalise_dsn(&self.dsn)
    }

    /// The normalised `LISTEN` DSN (falls back to the primary DSN).
    #[must_use]
    pub fn normalised_listen_dsn(&self) -> String {
        self.listen_dsn
            .as_deref()
            .map_or_else(|| self.normalised_dsn(), sql::normalise_dsn)
    }
}

/// A registered `(event_type glob, handler)` pair.
struct Subscription {
    pattern: globset::GlobMatcher,
    handler: Handler,
}

/// Shared state guarded for the drain loop and trait methods.
#[derive(Default)]
struct State {
    subscriptions: Vec<Subscription>,
    started: bool,
    closed: bool,
}

/// Postgres-backed [`Broker`](firefly_eda::Broker): durable outbox table + `LISTEN`/`NOTIFY`
/// wake-ups + a single advisory-lock-gated drain loop per consumer
/// group, ported from pyfly's `PostgresEventBus`.
///
/// Construct with [`PostgresBroker::new`], call [`start`](Self::start)
/// to create the tables and attach the listener, register handlers with
/// [`subscribe_pattern`](Self::subscribe_pattern) (or the
/// [`Subscriber`] trait method), and [`publish`](Self::publish) events.
/// [`close`](Self::close) (or the [`Publisher`]/[`Subscriber`] `close`)
/// stops the drain loop and releases connections.
pub struct PostgresBroker {
    config: PostgresConfig,
    channel: String,
    state: Arc<RwLock<State>>,
    /// Pooled-style query client (shared; tokio-postgres pipelines).
    client: RwLock<Option<Arc<Client>>>,
    /// Wakes the drain loop on NOTIFY, subscribe, and poll timeout.
    wake: Arc<Notify>,
    /// Handles for the spawned connection + drain + listener tasks.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl PostgresBroker {
    /// Builds a broker from [`PostgresConfig`]. No connection is made
    /// until [`start`](Self::start) (or the first [`publish`](Self::publish),
    /// which auto-starts, mirroring pyfly).
    ///
    /// The configured channel is validated eagerly: an invalid
    /// identifier panics here rather than failing later at NOTIFY time.
    /// Use [`PostgresBroker::try_new`] for a non-panicking constructor.
    #[must_use]
    pub fn new(config: PostgresConfig) -> Self {
        Self::try_new(config).expect("firefly/eda-postgres: invalid NOTIFY channel identifier")
    }

    /// Fallible constructor: returns the validated channel error instead
    /// of panicking when the configured channel is not a safe Postgres
    /// identifier — the Rust analog of pyfly's `_quote_ident` raising
    /// `ValueError` in `__init__`.
    pub fn try_new(config: PostgresConfig) -> Result<Self, sql::IdentError> {
        let channel = sql::quote_ident(&config.channel)?.to_string();
        Ok(Self {
            config,
            channel,
            state: Arc::new(RwLock::new(State::default())),
            client: RwLock::new(None),
            wake: Arc::new(Notify::new()),
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// Registers `handler` for every event whose `event_type` matches the
    /// `fnmatch`-style `pattern` (e.g. `"Order*"`, `"*.created"`, or an
    /// exact type) — the Rust spelling of pyfly's
    /// `subscribe(event_type_pattern, handler)`.
    ///
    /// Pokes the drain loop so a handler registered after `start()`
    /// immediately receives any already-persisted events.
    pub async fn subscribe_pattern(&self, pattern: &str, handler: Handler) -> EdaResult<()> {
        let matcher = globset::Glob::new(pattern)
            .map_err(|e| {
                EdaError::Handler(firefly_kernel::FireflyError::bad_request(format!(
                    "firefly/eda-postgres: invalid event-type pattern {pattern:?}: {e}"
                )))
            })?
            .compile_matcher();
        {
            let mut state = self.state.write().await;
            if state.closed {
                return Err(EdaError::Closed);
            }
            state.subscriptions.push(Subscription {
                pattern: matcher,
                handler,
            });
        }
        // Always poke; the drain guards on the subscription list itself,
        // so waking before start() is harmless.
        self.wake.notify_one();
        Ok(())
    }

    /// Persists `ev` in the outbox and fires a `NOTIFY` to wake the drain
    /// loop. Auto-starts the broker on first use, like pyfly. The event's
    /// `topic` becomes the outbox `destination`; its `event_type` is the
    /// glob target for subscriber dispatch; `payload` (an opaque byte
    /// blob in the canonical envelope) and `headers` are stored as JSONB.
    pub async fn publish(&self, ev: Event) -> EdaResult<()> {
        if !self.state.read().await.started {
            self.start().await?;
        }
        let client = self.query_client().await?;

        // tokio-postgres has no JSONB type feature enabled, so bind the
        // JSON as text and let the SQL `$n::jsonb` casts coerce it.
        let payload_json = payload_to_json(ev.payload.as_deref()).to_string();
        let headers_json = headers_to_json(&ev.headers).to_string();

        let row = client
            .query_one(
                sql::INSERT_OUTBOX,
                &[&ev.topic, &ev.event_type, &payload_json, &headers_json],
            )
            .await
            .map_err(pg_err)?;
        let id: i64 = row.get(0);

        // NOTIFY cannot bind parameters; the channel is a validated
        // identifier and the id is a plain integer, so interpolation is
        // safe — exactly as pyfly does.
        client
            .batch_execute(&format!("NOTIFY {}, '{}'", self.channel, id))
            .await
            .map_err(pg_err)?;
        Ok(())
    }

    /// Creates the outbox/offset tables, seeds the group's cursor,
    /// attaches the `LISTEN` connection, and spawns the drain loop.
    /// Idempotent: a second call returns immediately.
    pub async fn start(&self) -> EdaResult<()> {
        {
            let state = self.state.read().await;
            if state.started {
                return Ok(());
            }
            if state.closed {
                return Err(EdaError::Closed);
            }
        }

        // Query client (drives DDL, inserts, the drain). One shared
        // pipelined connection stands in for pyfly's asyncpg pool.
        let client = self.connect(&self.config.normalised_dsn()).await?;
        client
            .batch_execute(sql::DDL_OUTBOX)
            .await
            .map_err(pg_err)?;
        client
            .batch_execute(sql::DDL_OFFSETS)
            .await
            .map_err(pg_err)?;
        client
            .execute(sql::INSERT_OFFSET, &[&self.config.group])
            .await
            .map_err(pg_err)?;
        *self.client.write().await = Some(client.clone());

        // Listener connection: dedicated session running LISTEN, polling
        // its connection for NOTIFY messages and poking the wake.
        self.spawn_listener().await?;

        // Drain loop.
        let drain = DrainLoop {
            config: self.config.clone(),
            client,
            state: self.state.clone(),
            wake: self.wake.clone(),
        };
        let task = tokio::spawn(async move { drain.run().await });
        self.tasks.lock().expect("tasks lock poisoned").push(task);

        {
            let mut state = self.state.write().await;
            state.started = true;
        }
        self.wake.notify_one(); // initial catch-up sweep
        tracing::info!(
            channel = %self.channel,
            group = %self.config.group,
            "PostgresBroker started"
        );
        Ok(())
    }

    /// Stops the drain loop, removes the listener, and releases all
    /// connections. Subsequent publish/subscribe calls fail with
    /// [`EdaError::Closed`]. Idempotent.
    pub async fn close(&self) -> EdaResult<()> {
        {
            let mut state = self.state.write().await;
            state.closed = true;
            state.started = false;
            state.subscriptions.clear();
        }
        self.wake.notify_waiters(); // let the loop observe `closed`
        let tasks: Vec<_> = std::mem::take(&mut *self.tasks.lock().expect("tasks lock poisoned"));
        for task in tasks {
            task.abort();
        }
        *self.client.write().await = None;
        Ok(())
    }

    /// Dials a new tokio-postgres connection and drives it on a spawned
    /// task whose handle is tracked for shutdown.
    async fn connect(&self, dsn: &str) -> EdaResult<Arc<Client>> {
        let (client, connection) = tokio_postgres::connect(dsn, NoTls).await.map_err(pg_err)?;
        let task = tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!(err = %err, "postgres connection closed");
            }
        });
        self.tasks.lock().expect("tasks lock poisoned").push(task);
        Ok(Arc::new(client))
    }

    /// Spawns the long-lived LISTEN connection. tokio-postgres surfaces
    /// `NOTIFY` only by polling the connection object, so we drive it
    /// with `poll_message` and flip the wake on every notification.
    async fn spawn_listener(&self) -> EdaResult<()> {
        let dsn = self.config.normalised_listen_dsn();
        let (client, mut connection) =
            tokio_postgres::connect(&dsn, NoTls).await.map_err(pg_err)?;
        let wake = self.wake.clone();
        let channel = self.channel.clone();
        let task = tokio::spawn(async move {
            // tokio-postgres surfaces NOTIFY only while the connection is
            // polled, so wrap it in a stream we drive ourselves.
            let mut stream = futures::stream::poll_fn(move |cx| connection.poll_message(cx));

            // Phase 1 — issue LISTEN. The simple-query future only makes
            // progress while the connection (stream) is polled, so drive
            // both concurrently. Scope the borrow of `client` so it ends
            // before we move on, keeping `client` owned (and thus the
            // session alive) for the lifetime of the task.
            {
                let listen_sql = format!("LISTEN {channel}");
                let listen = client.batch_execute(&listen_sql);
                futures::pin_mut!(listen);
                loop {
                    tokio::select! {
                        res = &mut listen => {
                            if let Err(err) = res {
                                tracing::warn!(err = %err, "postgres LISTEN failed");
                            }
                            break;
                        }
                        msg = stream.next() => {
                            if Self::handle_listener_message(msg, &wake) {
                                return;
                            }
                        }
                    }
                }
            }

            // Phase 2 — pump notifications until the connection ends.
            while let Some(msg) = stream.next().await {
                if Self::handle_listener_message(Some(msg), &wake) {
                    break;
                }
            }
            // `client` is dropped here, at task end — never before, so the
            // LISTEN session stays open while we pump.
            drop(client);
        });
        self.tasks.lock().expect("tasks lock poisoned").push(task);
        Ok(())
    }

    /// Routes one polled connection message: a `NOTIFY` pokes the drain
    /// loop; a connection error ends the listener task (returns `true`).
    fn handle_listener_message(
        msg: Option<Result<AsyncMessage, tokio_postgres::Error>>,
        wake: &Arc<Notify>,
    ) -> bool {
        match msg {
            Some(Ok(AsyncMessage::Notification(_))) => {
                wake.notify_one();
                false
            }
            Some(Ok(_)) => false,
            Some(Err(err)) => {
                tracing::debug!(err = %err, "postgres LISTEN connection ended");
                true
            }
            None => true,
        }
    }

    /// Returns the shared query client, erroring if the broker is closed.
    async fn query_client(&self) -> EdaResult<Arc<Client>> {
        self.client.read().await.clone().ok_or(EdaError::Closed)
    }
}

impl std::fmt::Debug for PostgresBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresBroker")
            .field("channel", &self.channel)
            .field("group", &self.config.group)
            .field("destinations", &self.config.destinations)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Publisher for PostgresBroker {
    async fn publish(&self, ev: Event) -> EdaResult<()> {
        PostgresBroker::publish(self, ev).await
    }

    async fn close(&self) -> EdaResult<()> {
        PostgresBroker::close(self).await
    }
}

#[async_trait]
impl Subscriber for PostgresBroker {
    /// Registers `h` for every event whose `event_type` matches `topic`
    /// as an `fnmatch` glob. (The Rust port treats the `Subscriber`
    /// `topic` argument as pyfly's `event_type_pattern`.)
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()> {
        self.subscribe_pattern(topic, h).await
    }

    async fn close(&self) -> EdaResult<()> {
        PostgresBroker::close(self).await
    }
}

/// The owned state the spawned drain task needs.
struct DrainLoop {
    config: PostgresConfig,
    client: Arc<Client>,
    state: Arc<RwLock<State>>,
    wake: Arc<Notify>,
}

impl DrainLoop {
    /// The consume loop: drain-then-wait, waking on NOTIFY/subscribe or
    /// the poll-interval timeout (pyfly's `_consume_loop`).
    async fn run(self) {
        loop {
            if self.state.read().await.closed {
                return;
            }
            let has_subs = !self.state.read().await.subscriptions.is_empty();
            if has_subs {
                if let Err(err) = self.drain().await {
                    tracing::error!(err = %err, "EDA Postgres drain loop failed");
                    // Brief back-off so a persistent failure doesn't spin.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            if self.state.read().await.closed {
                return;
            }
            // Wait for a wake or the poll timeout, whichever first.
            let notified = self.wake.notified();
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep(self.config.poll_interval) => {}
            }
        }
    }

    /// Acquires the per-group advisory lock; only the holder advances the
    /// cursor. Everyone else returns and retries on the next wake. The
    /// lock is session-scoped on the shared query connection — a crashed
    /// process drops the connection and Postgres auto-releases it.
    async fn drain(&self) -> EdaResult<()> {
        if self.state.read().await.subscriptions.is_empty() {
            return Ok(());
        }
        let lock_key = sql::group_lock_key(&self.config.group);
        let got_lock: bool = self
            .client
            .query_one("SELECT pg_try_advisory_lock($1)", &[&lock_key])
            .await
            .map_err(pg_err)?
            .get(0);
        if !got_lock {
            return Ok(());
        }
        let result = self.drain_with_lock().await;
        // Always attempt unlock; on failure the lock releases when the
        // connection closes.
        if let Err(err) = self
            .client
            .query_one("SELECT pg_advisory_unlock($1)", &[&lock_key])
            .await
        {
            tracing::debug!(err = %err, "pg_advisory_unlock raised; releases on conn close");
        }
        result
    }

    /// The drain body, invoked while holding the group's advisory lock:
    /// fetch a batch, dispatch in order, advance the cursor only past
    /// successfully dispatched ids (at-least-once).
    async fn drain_with_lock(&self) -> EdaResult<()> {
        loop {
            if self.state.read().await.closed {
                return Ok(());
            }
            let offset: i64 = self
                .client
                .query_one(sql::SELECT_OFFSET, &[&self.config.group])
                .await
                .map_err(pg_err)?
                .get(0);

            let rows = if let Some(dests) = self.config.destinations_ref() {
                let dests: Vec<String> = dests.to_vec();
                self.client
                    .query(sql::SELECT_BATCH_FILTERED, &[&offset, &dests])
                    .await
            } else {
                self.client.query(sql::SELECT_BATCH_ALL, &[&offset]).await
            }
            .map_err(pg_err)?;

            if rows.is_empty() {
                return Ok(());
            }

            // Dispatch BEFORE advancing the cursor. A handler error stops
            // the batch early so redelivery resumes from the same point.
            let mut last_dispatched = offset;
            let last_row_id: i64 = rows[rows.len() - 1].get(0);
            for row in &rows {
                let event = row_to_event(row)?;
                match self.dispatch(event).await {
                    Ok(()) => last_dispatched = row.get(0),
                    Err(err) => {
                        tracing::error!(
                            err = %err,
                            id = last_dispatched,
                            "Handler raised; deferring redelivery"
                        );
                        break;
                    }
                }
            }

            if last_dispatched > offset {
                self.client
                    .execute(sql::UPDATE_OFFSET, &[&last_dispatched, &self.config.group])
                    .await
                    .map_err(pg_err)?;
            }

            // If a handler crashed before finishing the batch, back off
            // briefly so we don't spin, and yield the loop.
            if last_dispatched != last_row_id {
                tokio::time::sleep(Duration::from_millis(500)).await;
                return Ok(());
            }
        }
    }

    /// Dispatches one event to every subscription whose glob matches the
    /// event's `event_type`. Propagates the first handler error so the
    /// caller can leave the cursor at the last successful id.
    async fn dispatch(&self, event: Event) -> EdaResult<()> {
        // Snapshot matching handlers, then invoke without holding the lock.
        let matched: Vec<Handler> = {
            let state = self.state.read().await;
            state
                .subscriptions
                .iter()
                .filter(|s| s.pattern.is_match(&event.event_type))
                .map(|s| s.handler.clone())
                .collect()
        };
        for handler in matched {
            handler(event.clone()).await.map_err(EdaError::Handler)?;
        }
        Ok(())
    }
}

/// Maps an opaque payload blob to a JSONB-storable value. The canonical
/// [`Event`] payload is a byte blob; when it is valid JSON we store it as
/// the JSON document, otherwise as a base64-ish string wrapper so the
/// `JSONB` column always holds well-formed JSON.
fn payload_to_json(payload: Option<&[u8]>) -> serde_json::Value {
    match payload {
        None => serde_json::Value::Null,
        Some(bytes) => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => value,
            Err(_) => serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned()),
        },
    }
}

/// Encodes the event headers as a JSON object for the `headers` JSONB
/// column.
fn headers_to_json(headers: &BTreeMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        headers
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
}

/// Reconstructs an [`Event`] from a drained outbox row. The row columns
/// are `(id, destination, event_type, payload::text, headers::text,
/// created_at_rfc3339)`.
fn row_to_event(row: &tokio_postgres::Row) -> EdaResult<Event> {
    let id: i64 = row.get(0);
    let destination: String = row.get(1);
    let event_type: String = row.get(2);
    let payload_text: String = row.get(3);
    let headers_text: Option<String> = row.get(4);
    let created_at_text: Option<String> = row.get(5);

    let payload = json_text_to_payload(&payload_text);
    let headers = parse_headers(headers_text.as_deref());
    let time = created_at_text
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc));

    Ok(Event {
        id: id.to_string(),
        event_type,
        source: String::new(),
        topic: destination,
        correlation_id: String::new(),
        time,
        headers,
        payload,
        key: None,
    })
}

/// Converts the JSONB-as-text payload back to the envelope byte blob: a
/// JSON string column round-trips to its raw bytes, anything else to its
/// JSON text.
fn json_text_to_payload(text: &str) -> Option<Vec<u8>> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Null) => None,
        Ok(serde_json::Value::String(s)) => Some(s.into_bytes()),
        Ok(_) | Err(_) => Some(text.as_bytes().to_vec()),
    }
}

/// Parses the JSONB-as-text headers object into the envelope header map.
fn parse_headers(text: Option<&str>) -> BTreeMap<String, String> {
    let Some(text) = text else {
        return BTreeMap::new();
    };
    match serde_json::from_str::<BTreeMap<String, serde_json::Value>>(text) {
        Ok(map) => map
            .into_iter()
            .map(|(k, v)| {
                let value = match v {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                (k, value)
            })
            .collect(),
        Err(_) => BTreeMap::new(),
    }
}

/// Maps a tokio-postgres error into the EDA error family — a transport
/// failure surfaces as an internal [`FireflyError`] carried by
/// [`EdaError::Handler`].
fn pg_err(err: tokio_postgres::Error) -> EdaError {
    EdaError::Handler(firefly_kernel::FireflyError::internal(format!(
        "firefly/eda-postgres: {err}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_eda::Broker;

    #[test]
    fn config_defaults_match_pyfly() {
        let cfg = PostgresConfig::new("postgresql://x/y");
        assert_eq!(cfg.channel, "firefly_eda");
        assert_eq!(cfg.group, "default");
        assert_eq!(cfg.poll_interval, Duration::from_secs(5));
        assert!(cfg.destinations_ref().is_none());
    }

    // pyfly: test_destinations_default_to_all.
    #[test]
    fn destinations_default_to_all() {
        let cfg = PostgresConfig::new("postgresql://x/y");
        assert!(cfg.destinations_ref().is_none());
    }

    // pyfly: test_destinations_filter_preserved.
    #[test]
    fn destinations_filter_preserved() {
        let cfg = PostgresConfig::new("postgresql://x/y")
            .destinations(["flydesk.idp.jobs", "flydesk.idp.completions"]);
        assert_eq!(
            cfg.destinations_ref().unwrap(),
            ["flydesk.idp.jobs", "flydesk.idp.completions"]
        );
    }

    // pyfly: test_normalise_dsn_applied_in_constructor.
    #[test]
    fn normalise_dsn_applied() {
        let cfg = PostgresConfig::new("postgresql+asyncpg://idp:idp@pg:5432/flydesk_idp")
            .listen_dsn("postgresql+asyncpg://idp:idp@pg:5432/flydesk_idp");
        assert_eq!(
            cfg.normalised_dsn(),
            "postgresql://idp:idp@pg:5432/flydesk_idp"
        );
        assert_eq!(
            cfg.normalised_listen_dsn(),
            "postgresql://idp:idp@pg:5432/flydesk_idp"
        );
    }

    #[test]
    fn listen_dsn_defaults_to_primary() {
        let cfg = PostgresConfig::new("postgresql://u:p@h/db");
        assert_eq!(cfg.normalised_listen_dsn(), "postgresql://u:p@h/db");
    }

    // pyfly: test_channel_identifier_validated — invalid channel rejected.
    #[test]
    fn invalid_channel_rejected() {
        assert!(PostgresBroker::try_new(
            PostgresConfig::new("postgresql://x/y").channel("bad channel")
        )
        .is_err());
        assert!(PostgresBroker::try_new(
            PostgresConfig::new("postgresql://x/y").channel("x;DROP TABLE")
        )
        .is_err());
    }

    #[test]
    fn valid_channel_accepted() {
        let broker =
            PostgresBroker::try_new(PostgresConfig::new("postgresql://x/y").channel("firefly_eda"))
                .unwrap();
        assert_eq!(broker.channel, "firefly_eda");
    }

    // pyfly: test_protocol_compliance — the broker is a usable Broker.
    #[test]
    fn is_a_broker_trait_object() {
        let broker = PostgresBroker::new(PostgresConfig::new("postgresql://x/y"));
        // Debug surface does not leak the DSN.
        let dbg = format!("{broker:?}");
        assert!(dbg.contains("PostgresBroker"));
        assert!(!dbg.contains("postgresql://"));
        // Coerces to the shared port object used by starters.
        let _broker: Box<dyn Broker> = Box::new(broker);
    }

    #[test]
    fn payload_round_trip_json_document() {
        let json = br#"{"id":"o1"}"#.to_vec();
        let stored = payload_to_json(Some(&json));
        assert_eq!(stored, serde_json::json!({"id": "o1"}));
        // Round-trip back through the drained-text path.
        let back = json_text_to_payload(&stored.to_string());
        assert_eq!(back, Some(json));
    }

    #[test]
    fn payload_round_trip_non_json_bytes() {
        let raw = b"not-json".to_vec();
        let stored = payload_to_json(Some(&raw));
        assert_eq!(stored, serde_json::Value::String("not-json".into()));
        let back = json_text_to_payload(&stored.to_string());
        assert_eq!(back, Some(raw));
    }

    #[test]
    fn payload_none_is_null() {
        assert_eq!(payload_to_json(None), serde_json::Value::Null);
        assert_eq!(json_text_to_payload("null"), None);
    }

    #[test]
    fn headers_round_trip() {
        let mut headers = BTreeMap::new();
        headers.insert("tenant".to_string(), "t1".to_string());
        headers.insert("trace".to_string(), "abc".to_string());
        let json = headers_to_json(&headers);
        let back = parse_headers(Some(&json.to_string()));
        assert_eq!(back, headers);
    }

    #[test]
    fn empty_headers_round_trip() {
        let headers = BTreeMap::new();
        let json = headers_to_json(&headers);
        assert_eq!(json, serde_json::json!({}));
        assert_eq!(parse_headers(Some("{}")), BTreeMap::new());
        assert_eq!(parse_headers(None), BTreeMap::new());
    }

    // Real-server round-trip: outbox INSERT + NOTIFY-driven drain +
    // cursor advance + at-least-once redelivery. Needs a live Postgres;
    // set FIREFLY_TEST_POSTGRES_DSN.
    #[tokio::test]
    #[ignore = "requires postgres"]
    async fn publish_drain_round_trip_against_real_postgres() {
        use firefly_eda::handler;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dsn = std::env::var("FIREFLY_TEST_POSTGRES_DSN")
            .unwrap_or_else(|_| "host=localhost user=postgres dbname=postgres".to_string());
        let group = format!("firefly-test-{}", std::process::id());
        let broker = PostgresBroker::new(
            PostgresConfig::new(&dsn)
                .group(group)
                .channel("firefly_eda_test")
                .poll_interval(Duration::from_millis(200)),
        );
        broker.start().await.unwrap();

        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        broker
            .subscribe_pattern(
                "OrderCreated",
                handler(move |ev: Event| {
                    let seen = seen2.clone();
                    async move {
                        assert_eq!(ev.event_type, "OrderCreated");
                        seen.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            )
            .await
            .unwrap();

        let ev = Event::new(
            "orders.created",
            "OrderCreated",
            "orders-svc",
            Some(br#"{"id":"o1"}"#.to_vec()),
        );
        broker.publish(ev).await.unwrap();

        // Poll for delivery (no fixed sleep > 200ms).
        for _ in 0..25 {
            if seen.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        broker.close().await.unwrap();
    }
}
