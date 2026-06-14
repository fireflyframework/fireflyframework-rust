# `firefly-aop`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-aop` provides aspect-oriented advice for arbitrary service methods,
in the style of mature AOP frameworks. It provides

| Surface | Role |
|---------|------|
| `matches_pointcut` / `Pointcut` | dot-segmented glob matcher over qualified names |
| `JoinPoint` | the intercepted-call context (`type_name`, `method_name`, `args`, `result`, `error`) |
| `Aspect` | trait of five advice hooks, each with a default no-op |
| `AspectRegistry` / `AdviceBinding` | ordered pointcut→aspect bindings |
| `intercept` / `intercept_with_bindings` | the advice-chain executor |
| `Invocation` / `Proceed` | the captured original call + the `around` continuation |

## Pointcut language

Patterns match dot-segmented **qualified names** of the form
`stereotype.ClassName.method` (e.g. `service.OrderService.create`):

* `*` matches exactly one segment (never crosses a dot);
* `**` matches one or more segments (any depth);
* partial globs inside a segment use fnmatch rules — `*` → `[^.]*`, `?` →
  `[^.]`, everything else literal (`get_*`, `*Service`).

`matches_pointcut(pattern, name)` is the one-shot matcher; `Pointcut::compile`
gives a reusable compiled form (the registry holds one per binding so patterns
are never recompiled per dispatch). The translation
(`_segment_to_regex`/`_pattern_to_regex`) is exercised by an extensive suite of
pointcut test cases.

## Advice ordering

For every binding whose pointcut matches the qualified name, in registry order
(lowest `order` first):

```
1. before            (each matching binding, in order)
2. around            (first-registered outermost; proceed() runs the next link)
        │
        ▼ original call (Invocation, at the innermost link)
3a. after_returning  (on success — jp.result populated)
3b. after_throwing   (on error   — jp.error populated, then error re-propagated)
4. after             (always — pyfly's `finally`)
```

`before` / `after_returning` / `after_throwing` / `after` observe the
`JoinPoint` but cannot change the outcome; only `around` can, by transforming
what `Proceed::proceed()` yields (or by not proceeding at all to short-circuit).

## Quick start

```rust
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use firefly_aop::{intercept, invocation, ok, AspectRegistry, Aspect, JoinPoint};

struct Audit(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Aspect for Audit {
    async fn before(&self, jp: &JoinPoint) {
        self.0.lock().unwrap().push(format!("calling {}", jp.qualified_name()));
    }
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let log = Arc::new(Mutex::new(Vec::new()));
let mut registry = AspectRegistry::new();
registry.register(Arc::new(Audit(log.clone())), "service.*.*", 0);

let out = intercept(
    &registry,
    "service.OrderService",
    "create",
    Arc::new((42u32,)),
    invocation(|| async { ok("order-42".to_string()) }),
)
.await
.unwrap();

assert_eq!(out.downcast_ref::<String>().unwrap(), "order-42");
# }
```

## Design notes

The crate leans into Rust idioms rather than runtime reflection or monkey-patching:

* **Explicit advice registration.** An aspect is a single `Aspect` impl (its five
  hooks are the five advice kinds) and its pointcut + order are supplied
  **explicitly** at `register(aspect, pointcut, order)`. Each `register` call
  produces exactly one `AdviceBinding`; bindings stay globally sorted by `order`
  (lower first; equal orders preserve registration sequence via a stable sort).

* **Weaving is explicit.** There is no runtime method mutation or DI-container
  post-processing. Instead the **call site** wraps the original call in an
  `Invocation` and routes it through `intercept` at construction time.
  "Non-matching methods untouched" falls out for free — if no binding matches the
  qualified name, `intercept` runs the invocation with zero advice overhead.

`args` and return values are type-erased to `Arc<dyn Any + Send + Sync>` (advice
downcasts when it needs the concrete type) and errors to
`Box<dyn Error + Send + Sync>`. The `AdviceKind` string names (`"before"`,
`"after_returning"`, …) identify each advice hook.

For **HTTP-edge** and **bus-dispatch** cross-cutting concerns, keep using
`firefly-web`'s tower layers and `firefly-cqrs`'s `Middleware` respectively;
`firefly-aop` targets pattern-matched advice over arbitrary service methods.
