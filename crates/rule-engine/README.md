# `firefly-rule-engine`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-common-rule-engine` · **Go module:** `ruleengine` · **.NET project:** `FireflyFramework.RuleEngine.{Interfaces,Models,Core,Web,Sdk}`

## Overview

`firefly-rule-engine` is the framework's **declarative business-rule
engine**. Rules are authored as YAML documents (or programmatically via
the [`models`](src/models.rs) module), parsed into an AST, and evaluated
by a recursive walker that resolves fact-paths against a JSON-object
fact.

Sub-modules mirror the Go package split (one crate, five modules):

* `models` — AST: `Rule`, `RuleSet`, `Logic`, `Condition`, `Action`,
  `Op`.
* `interfaces` — port: `Evaluator`, `Verdict`, `Fact`.
* `core` — `AstEvaluator`, the default `Evaluator`.
* `web` — REST admin (axum router) — planned for v26.06 in Go,
  implemented here.
* `sdk` — typed admin client — planned for v26.06 in Go, implemented
  here.

## Rule shape

```yaml
name: vip-tagging
version: 1
rules:
  - id: premium
    priority: 10
    when:
      all:
        - cond: { path: user.age,     op: gte, value: 18 }
        - cond: { path: user.country, op: in,  value: [ES, FR] }
    then:
      - type: tag
        params: { name: premium }
  - id: vip
    priority: 5
    when:
      any:
        - cond: { path: user.spend,    op: gt,        value: 1000 }
        - cond: { path: user.referral, op: isNotNull }
    then:
      - type: tag
        params: { name: vip }
```

The field names and omission rules of the JSON/YAML projection match
the Go struct tags exactly, so rule files transfer across the Java,
.NET, Go, Python, and Rust runtimes verbatim.

## Operators

`eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `in`, `notIn`, `contains`,
`startsWith`, `endsWith`, `matches` (regex), `isNull`, `isNotNull`.

Like Go's open `type Op string`, an unrecognised operator survives
parsing (`Op::Other`) and is rejected at **evaluation** time with
`ruleengine: unknown op: …`.

## Public surface

```rust,ignore
// models
pub enum   Op        { Eq, Ne, Lt, Lte, Gt, Gte, In, NotIn, Contains,
                       StartsWith, EndsWith, Matches, IsNull, IsNotNull, Other(String) }
pub struct Condition { path: String, op: Op, value: Value }
pub struct Logic     { all: Vec<Logic>, any: Vec<Logic>, not: Option<Box<Logic>>, cond: Option<Condition> }
pub struct Action    { action_type: String, params: Map<String, Value> }   // serialized as `type`
pub struct Rule      { id, description: String, priority: i64, when: Logic, then: Vec<Action> }
pub struct RuleSet   { name, version: String, rules: Vec<Rule> }
impl RuleSet         { fn from_yaml(&str) -> Result<Self, DslError>; fn to_yaml(&self) -> Result<String, DslError> }

// interfaces
pub type   Fact    = serde_json::Map<String, Value>;
pub struct Verdict { matched: Vec<String>, actions: Vec<Action> }
#[async_trait]
pub trait  Evaluator: Send + Sync {
    async fn evaluate(&self, set: &RuleSet, fact: &Fact) -> Result<Verdict, EvalError>;
}

// core
pub struct AstEvaluator;             // stateless; AstEvaluator::new()
impl AstEvaluator { fn evaluate_sync(&self, &RuleSet, &Fact) -> Result<Verdict, EvalError> }

// web
pub fn rule_engine_router() -> axum::Router;
pub fn rule_engine_router_with(evaluator: Arc<dyn Evaluator>) -> axum::Router;

// sdk
pub struct RuleEngineClient;         // RuleEngineClient::new(base_url)
impl RuleEngineClient {
    async fn evaluate(&self, &RuleSet, &Fact) -> Result<Verdict, SdkError>;
    async fn evaluate_yaml(&self, &str, &Fact) -> Result<Verdict, SdkError>;
}
```

Rules fire in **descending priority order**; ties broken by document
order. The `Verdict` returns the matched rule ids and the merged action
list.

## Quick start

```rust
use firefly_rule_engine::{Action, AstEvaluator, Logic, Op, Rule, RuleSet};
use serde_json::json;

fn main() {
    let rs = RuleSet::new("orders").with_rule(
        Rule::new("high-value", Logic::cond("amount", Op::Gt, json!(1000.0)))
            .with_action(Action::new("review").with_param("queue", "manual")),
    );

    let fact = json!({"amount": 1500}).as_object().unwrap().clone();
    let verdict = AstEvaluator::new().evaluate_sync(&rs, &fact).unwrap();
    assert_eq!(verdict.matched, ["high-value"]);
    assert_eq!(verdict.actions[0].action_type, "review");
}
```

Or straight from the YAML DSL: `RuleSet::from_yaml(yaml)?` then
evaluate as above.

## REST admin (`web`)

| Method | Path                       | Body                                  | Response                              |
|--------|----------------------------|---------------------------------------|---------------------------------------|
| `POST` | `/api/rules/evaluate`      | `{"ruleset": <RuleSet>, "fact": {…}}` | `{"matched": […], "actions": […]}`     |
| `POST` | `/api/rules/evaluate/yaml` | `{"yaml": "<DSL>", "fact": {…}}`      | `{"matched": […], "actions": […]}`     |

Both answer `400 Bad Request` with `{"error": "<message>"}` when the
YAML cannot be parsed or evaluation fails (unknown operator, bad regex,
non-numeric comparison).

```rust,ignore
let app = firefly_rule_engine::rule_engine_router();
let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
axum::serve(listener, app).await?;
```

## Typed client (`sdk`)

`RuleEngineClient` maps one-for-one onto the router. HTTP goes through
the `HttpTransport` port (default: reqwest), so tests drive the client
against the in-process router via `tower::ServiceExt::oneshot` — no
sockets.

```rust,ignore
let client = firefly_rule_engine::RuleEngineClient::new("http://rules.internal:8080");
let verdict = client.evaluate_yaml(yaml, &fact).await?;
```

## Testing

```bash
cargo test -p firefly-rule-engine
```

Ports every Go test (`all` / `any` / `not` composition, regex
`matches`, priority ordering) and adds range fall-through,
unknown-operator rejection, wire-format assertions against the Go
struct tags, serde round-trips, in-process router tests, and SDK ↔
router round-trips.
