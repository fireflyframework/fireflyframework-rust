# `firefly-starter-application`

> **Tier:** Starter · **Status:** Stable

## Overview

`firefly-starter-application` composes
[`firefly-starter-core`](../starter-core/) with a pre-wired
[`firefly-plugins`](../plugins/) `Registry` — the canonical
application-tier wiring used by services that load plugins
(IdP, orchestration, rule-engine integrations) at startup.

```rust,ignore
pub struct Application {
    pub core: Core,
    pub plugins: Arc<Registry>,
}

impl Application {
    pub fn new(cfg: CoreConfig) -> Self;
}

impl Deref for Application { type Target = Core; }
impl DerefMut for Application {}
```

The starter name defaults to `"starter-application"` (overriding the
`Core` default `"starter-core"`) so the actuator `/info` and the
startup banner correctly identify the tier; any other starter name set
explicitly in the config is preserved.

`Application` derefs to `Core` so every core field (`bus`, `cache`, `broker`, `health`,
`metrics`, `scheduler`, …) and convenience method (`apply_middleware`,
`actuator_router`, `new_application`, `print_banner`) is reachable
directly on the application. `Core`, `CoreConfig`, and the plugin
types (`Plugin`, `Registry`, `PluginError`, `BoxError`) are
re-exported so a service can depend on this crate alone.

## Quick start

```rust,ignore
use std::sync::Arc;

use firefly_starter_application::{Application, CoreConfig};

let app = Application::new(CoreConfig {
    app_name: "approval-engine".into(),
    ..CoreConfig::default()
});

app.plugins.register(Arc::new(IdpPlugin { adapter: keycloak_adapter }));
app.plugins.register(Arc::new(RuleEnginePlugin { loader: yaml_loader }));

app.plugins.start_all().await?;
// ... application runs (core fields reachable via deref: app.bus, app.cache, ...)
app.plugins.stop_all().await?;
```

## Testing

```bash
cargo test -p firefly-starter-application
```

The suite verifies application wiring (the wired plugin registry being
present, core dependencies wired, the `starter-application` name, and a
roundtrip plugin start + stop) plus default-name fallbacks, the
explicit-starter-name guard, the banner identifying the tier, a stub
plugin lifecycle through the wired registry, deref/deref-mut promotion
to the embedded core, the version stamp, and `Send + Sync` bounds.
