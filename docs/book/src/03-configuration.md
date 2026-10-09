# Configuration

In the [Quickstart](./02-quickstart.md) Lumen named itself with two `pub const`
strings, and `FireflyApplication` pulled its bind addresses straight off the
environment (`FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`). That is the
right starting point — but a real wallet service runs in dev, in CI, and in
prod, and each environment wants different ports, log levels, and (eventually)
database URLs. Hard-coded literals do not survive that journey.

This chapter is where those literals stop being literals and start coming from
files and the environment, in a typed, layered, profile-aware way — the same
shape Spring Boot's `@ConfigurationProperties` gives a Java service, ported to
plain `serde` structs. Everything here is *additive*: the one-line `main` from
the Quickstart does not change, and the constants you wrote keep working while
you learn the machinery that will eventually replace them.

By the end of this chapter you will:

- Define a configuration as a plain `serde` struct and **bind** flat,
  dot-keyed values onto it with the type-driven binder.
- Load configuration from `application.yaml`, a profile-specific overlay, and
  the environment, and explain the **precedence chain** that decides who wins.
- Resolve `${...}` placeholders and reason about environment-beats-config
  ordering.
- Turn a config struct into an **injectable bean** with
  `#[derive(ConfigProperties)]`, optionally validated at startup.
- Stand up Lumen's datasource and security layer **from `application.yaml`**
  with one awaited auto-configure call each — no container, no builder chains.
- Mask secrets, reload at runtime, and pull configuration from a config server.

## Concepts you will meet

Before the first struct, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — configuration property.** A *configuration property* is
> a single named value your program reads at startup — `web.addr`,
> `cache.ttl`, `datasource.url`. Firefly represents the whole set as a **flat,
> dot-keyed map** of strings (`{"web.addr": "127.0.0.1:8080", ...}`) and then
> *binds* it onto your typed struct. The Spring analog is a property in
> `application.properties` / `application.yaml`.

> **Note** **Key term — source.** A *source* is anything that produces some of
> those flat entries: a YAML file, the process environment, hard-coded
> defaults, CLI flags, a remote config server. Firefly's `Source` trait has one
> job — hand back a `HashMap<String, String>`. The Spring analog is a
> `PropertySource`.

> **Note** **Key term — profile.** A *profile* names an environment —
> `dev`, `test`, `staging`, `prod` — and selects an extra YAML overlay
> (`application-prod.yaml`) layered on top of the base file. This is exactly
> Spring's notion of an active profile, down to the `dev,cloud` comma syntax.

> **Note** **Key term — binding.** *Binding* is the act of decoding the flat
> string map onto a typed struct: `"9090"` becomes a `u16`, `"alpha,beta"`
> becomes a `Vec<String>`, `"true"` becomes a `bool`. The binder is
> **type-driven** — the target field's type decides how each string is parsed.
> Spring calls the same idea relaxed binding onto a `@ConfigurationProperties`
> class.

> **Design note.** Firefly binds a profile-aware `application.yaml` → profile →
> environment hierarchy onto typed structs, and the flattening and binding
> rules are specified precisely so the same `application.yaml` produces the same
> keys deterministically. Firefly treats this determinism as a guarantee, not an
> accident — there is no general-purpose YAML engine deciding things behind your
> back.

## Step 1 — See where Lumen is today: app identity as configuration

You do not have to write any config to follow this step — you already have
config, you just spelled it as constants. Recall Lumen's bootstrap. The
Quickstart's `main` was the bare form; `src/web.rs` keeps a fuller `bootstrap`
helper that also stamps the version:

```rust,ignore
// src/web.rs — the two values that name the service
pub const APP_NAME: &str = "lumen";
pub const VERSION: &str = firefly::VERSION;

firefly::FireflyApplication::new(APP_NAME)
    .version(VERSION)
    .run()
    .await
```

What just happened: those two values become `CoreConfig.app_name` /
`CoreConfig.app_version` inside the framework — plain configuration.
`FireflyApplication::new(name)` writes `app_name`; `.version(v)` writes
`app_version`. Every other field of `CoreConfig` is a knob too, and the two
Lumen sets are exactly the values `/actuator/info` reports and the banner
prints.

> **Note** **Key term — `CoreConfig`.** `CoreConfig` is the framework's own
> configuration struct (CORS, security headers, idempotency, the app name and
> version, …). `FireflyApplication` carries one and lets you tune it with
> `.configure(|c| ...)`. The remaining fields default — an in-memory cache, an
> in-process broker, a fresh CQRS bus — which is why a bare `cargo run` needs no
> infrastructure. The Spring analog is the bundle of `server.*` / `spring.*`
> properties Spring Boot binds for you.

Promoting any of those defaults to real infrastructure is a one-field change you
will make in [Production & Deployment](./20-production.md). The config story in
*this* chapter is the general machinery underneath: how a value like an address
stops being a literal in Rust and starts arriving from a file or the
environment.

> **Tip** **Checkpoint.** You can already prove identity is configuration:
> `curl localhost:8081/actuator/info` (management port) and read back
> `"app":{"name":"lumen","version":"..."}`. Change the string passed to
> `new(...)`, re-run, and the banner and that endpoint both follow.

## Step 2 — Define a configuration struct

A configuration struct is plain `serde`. There is no special base type to
inherit and no attribute to remember — nested structs simply become nested
dot-keyed sections (`web.addr`, `web.admin_addr`). Here is the shape Lumen would
adopt as it outgrows the two constants.

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Web {
    /// Public API bind address — the typed home of FIREFLY_SERVER_ADDR.
    addr: String,
    /// Admin/management bind address — the typed home of FIREFLY_MANAGEMENT_ADDR.
    admin_addr: String,
}

#[derive(Debug, Deserialize)]
struct LumenConfig {
    name: String,
    web: Web,
    tags: Vec<String>,
}
```

What just happened: you declared three top-level keys — `name`, the `web`
section, and a `tags` list — purely with `serde`. The binder reaches `web.addr`
by walking `LumenConfig.web` → `Web.addr`, and it reaches each element of `tags`
by splitting a comma-joined string.

Why it matters: the binder is **type-driven**, so you rarely need
`#[serde(default)]`. A missing key produces the type's zero value — `0` for an
integer, `""` for a `String`, `false` for a `bool`, an empty `Vec` for a list —
exactly like a zero-valued struct. That is a deliberate parity choice with the
Go and pyfly ports.

> **Note** **Key term — relaxed key.** Keys are normalized at the door:
> lower-cased, with kebab-case dashes folded to snake_case underscores. So
> `admin-addr:` written in YAML binds the `admin_addr` serde field, and
> `WEB.ADDR` from the environment lands on the same `web.addr` key as a YAML
> `web.addr`. Spring calls this relaxed binding.

The full leaf catalogue the binder supports: `String`, `bool` (it accepts
`1`/`0`, `t`/`f`, and `true`/`false` forms), every integer width, `f32`/`f64`,
`char`, unit enums (matched by variant name), `Option<T>` (`None` when the key
and its whole subtree are absent), sequences of scalars (comma-separated,
trimmed), and `HashMap<String, _>` subtrees (every immediate child segment
becomes a map key). For a duration, bind an `i64`/`u64` of milliseconds and
convert: `Duration::from_millis(cfg.cache.ttl_ms)`.

## Step 3 — Bind values onto the struct

A struct alone does nothing; you bind a flat map onto it. The lowest-level
entry point is `bind`, which takes a `HashMap<String, String>` and decodes it
onto a fresh `T`.

```rust,ignore
use std::collections::HashMap;
use firefly::config::{bind, ConfigError};

let flat = HashMap::from([
    ("name".to_string(), "lumen".to_string()),
    ("web.addr".to_string(), "127.0.0.1:8080".to_string()),
    ("web.admin-addr".to_string(), "127.0.0.1:8081".to_string()),
    ("tags".to_string(), "wallet, ledger, demo".to_string()),
]);

let cfg: LumenConfig = bind(&flat)?;
assert_eq!(cfg.web.addr, "127.0.0.1:8080");
assert_eq!(cfg.tags, vec!["wallet", "ledger", "demo"]);
# Ok::<(), ConfigError>(())
```

What just happened: `bind` walked your struct's type, looked up each dotted key,
and parsed the string into the target field. Note three things the type drove on
its own — `web.admin-addr` (kebab) bound the `admin_addr` (snake) field,
`"wallet, ledger, demo"` split-and-trimmed onto a `Vec<String>`, and nothing
required `#[serde(default)]`.

> **Note** **Key term — facade import.** `firefly::config` is the
> `firefly-config` crate re-exported through the one-dependency facade, so you
> still depend only on `firefly`. Throughout this chapter `firefly::config::X`
> and `firefly_config::X` name the same item; the book prefers the facade path
> to keep the single-dependency story honest.

In real code you almost never build that map by hand — sources build it for you.
The canonical loader, `load`, takes a list of sources, merges them, resolves
placeholders, and binds in one call:

```rust,ignore
use firefly::config::{load, Source};

let cfg: LumenConfig = load(&sources)?;
```

The next step is where `sources` comes from.

> **Tip** **Checkpoint.** Drop the `bind` example into a unit test and run it.
> A green test means your struct's shape and the dotted keys line up — this is
> the fastest way to debug a binding before YAML and the environment are in the
> mix.

## Step 4 — Load with profiles

The most common bootstrap is one helper call. `load_from_profile` reads
`application.yaml`, then the profile-specific `application-{profile}.yaml`, then
`FIREFLY_*` environment variables, merges them in that order, and binds the
result:

```rust,ignore
use firefly::config::{load_from_profile, ConfigError};

fn main() -> Result<(), ConfigError> {
    // dir, app basename, fallback profile (FIREFLY_PROFILE overrides at runtime).
    let cfg: LumenConfig = load_from_profile("/etc/lumen", "application", "dev")?;
    println!("public API on {}", cfg.web.addr);
    Ok(())
}
```

What just happened, argument by argument:

- `"/etc/lumen"` is the directory the YAML files live in.
- `"application"` is the file *basename* — so it reads `application.yaml` and
  `application-{profile}.yaml`. (Pass `"lumen"` to read `lumen.yaml` instead.)
- `"dev"` is the **fallback** profile, used only when `FIREFLY_PROFILE` is unset.

Both YAML files are tolerated absent — a service that hard-codes everything in
Rust can ship no YAML at all and this call still succeeds against the
environment alone.

> **Note** **Key term — `FIREFLY_PROFILE`.** This environment variable selects
> the active profile(s) at runtime. `FIREFLY_PROFILE=prod` reads
> `application-prod.yaml`; a comma-separated value (`FIREFLY_PROFILE=dev,cloud`)
> overlays one file per profile, in order (`application-dev.yaml` then
> `application-cloud.yaml`, later wins). This is how Lumen would carry an
> in-memory event store in `dev` and a Postgres one in `prod` without a single
> `if` in the wiring code.

> **Warning** `load_from_profile` always appends `from_env("FIREFLY")` as its
> top layer, so its environment overrides are spelled `FIREFLY_*`
> (`FIREFLY_WEB_ADDR`), *not* `LUMEN_*`. If you want a `LUMEN_`-prefixed
> environment layer, build the chain yourself (Step 6) with `from_env("LUMEN")`.

## Step 5 — Understand source precedence

The whole system rests on one rule: **`Layered::new(vec![s1, s2, ...])` merges
its sources left to right, and the last write wins.** Higher rows in the table
below sit later in the list and therefore override lower ones.

| Order | Source                                              | Beats        |
|-------|-----------------------------------------------------|--------------|
| 1     | Defaults — `StaticSource::new(name, entries)`       | nothing      |
| 2     | Base YAML — `from_optional_yaml("application.yaml")` | defaults     |
| 3     | Profile YAML — `from_optional_yaml("application-prod.yaml")` | base |
| 4     | Environment — `from_env("FIREFLY")`                 | YAML files   |
| 5     | CLI flags — `FlagSource::new().set("web.addr", "0.0.0.0:80")` | everything |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 310" role="img"
     aria-label="Configuration precedence: defaults, base YAML, profile YAML, environment and CLI flags are merged left to right with the last write winning, so a CLI flag beats environment, which beats YAML, which beats defaults"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="24.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="24.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#f3f1eb" stroke="#d8d4ca" stroke-width="1.5"/><text x="70.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">defaults</text><text x="70.0" y="109.0" text-anchor="middle" font-size="9" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">StaticSource</text>
<line x1="116.0" y1="98.0" x2="124.0" y2="98.0" stroke="#ffb34a" stroke-width="2.6" stroke-linecap="round"/><polygon points="132.0,98.0 124.0,102.5 124.0,93.5" fill="#855414"/>
<rect x="132.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="132.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#f3f1eb" stroke="#d8d4ca" stroke-width="1.5"/><text x="178.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">base YAML</text><text x="178.0" y="109.0" text-anchor="middle" font-size="9" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">application.yaml</text>
<line x1="224.0" y1="98.0" x2="232.0" y2="98.0" stroke="#ffb34a" stroke-width="2.6" stroke-linecap="round"/><polygon points="240.0,98.0 232.0,102.5 232.0,93.5" fill="#855414"/>
<rect x="240.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="240.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#f3f1eb" stroke="#d8d4ca" stroke-width="1.5"/><text x="286.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">profile YAML</text><text x="286.0" y="109.0" text-anchor="middle" font-size="9" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">application-prod.yaml</text>
<line x1="332.0" y1="98.0" x2="340.0" y2="98.0" stroke="#ffb34a" stroke-width="2.6" stroke-linecap="round"/><polygon points="348.0,98.0 340.0,102.5 340.0,93.5" fill="#855414"/>
<rect x="348.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="348.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#f3f1eb" stroke="#d8d4ca" stroke-width="1.5"/><text x="394.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">environment</text><text x="394.0" y="109.0" text-anchor="middle" font-size="9" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">FIREFLY_*</text>
<line x1="440.0" y1="98.0" x2="448.0" y2="98.0" stroke="#ffb34a" stroke-width="2.6" stroke-linecap="round"/><polygon points="456.0,98.0 448.0,102.5 448.0,93.5" fill="#855414"/>
<rect x="456.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="456.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#fff1dd" stroke="#855414" stroke-width="1.5"/><text x="502.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">CLI flags</text><text x="502.0" y="109.0" text-anchor="middle" font-size="9" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">FlagSource</text>
<text x="280.0" y="40.0" text-anchor="middle" font-size="13" font-weight="800" fill="#272820" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">merged left → right  ·  last write wins</text>
<text x="70.0" y="160.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif" font-style="italic">beats nothing</text>
<text x="490.0" y="160.0" text-anchor="middle" font-size="10.5" font-weight="700" fill="#855414" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">beats everything</text>
<text x="280.0" y="200.0" text-anchor="middle" font-size="11" font-weight="600" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">an env override beats a YAML file; a CLI flag beats both</text>
<svg x="480.0" y="254.0" width="72" height="52" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 335.52 242"><title>rsfly - Rust framework by Firefly</title><g fill="#10110f" transform="translate(20 130) scale(0.07 -0.07)"><g transform="translate(0 0)"><path d="M140 0L140 1080L299 1080L299 908Q310 927 323 945Q353 987 391 1014Q433 1048 486 1066Q539 1084 594 1087.5Q649 1091 697 1080L697 913Q641 928 574 921Q507 914 450 872Q398 835 369.5 782Q341 729 330 667.5Q319 606 319 542L319 0Z"/></g><g transform="translate(707 0)"><path d="M548 -29Q355 -29 231.5 55.5Q108 140 80 292L262 321Q284 233 363 181.5Q442 130 559 130Q670 130 733 176Q796 222 796 301Q796 347 775 375.5Q754 404 688.5 429Q623 454 491 489Q348 527 266 568Q184 609 149 663Q114 717 114 794Q114 889 166 960.5Q218 1032 311.5 1071Q405 1110 529 1110Q652 1110 749 1071Q846 1032 905.5 960.5Q965 889 976 794L794 761Q781 848 710.5 898Q640 948 529 951Q424 955 358.5 914Q293 873 293 803Q293 764 317.5 736Q342 708 408.5 683Q475 658 602 626Q745 590 827.5 547.5Q910 505 945 447.5Q980 390 980 306Q980 150 865 60.5Q750 -29 548 -29Z"/></g><g transform="translate(1768 0)"><path d="M243 0L243 933L60 933L60 1080L243 1080L243 1139Q243 1181 247 1224.5Q251 1268 265 1309.5Q279 1351 311 1386Q348 1427 392 1444.5Q436 1462 481.5 1466Q527 1470 568 1470L705 1470L705 1322L578 1322Q499 1322 460 1283.5Q421 1245 421 1167L421 1080L954 1080L954 1470L1132 1470L1132 0L954 0L954 933L421 933L421 0Z"/></g><g transform="translate(3060 0)"><g data-family-terminal="trail-y"><defs><linearGradient id="03-configuration-rsfly-family-rsfly-20-130-140" gradientUnits="userSpaceOnUse" x1="30" y1="1080" x2="554" y2="230"><stop offset="0" stop-color="#ffb34a" stop-opacity="0.2"/><stop offset="0.8" stop-color="#ffb34a" stop-opacity="1"/></linearGradient></defs><path d="M290 -480L468 4L30 1080L217 1080L554 230L870 1080L1051 1080L463 -480Z" fill="url(#03-configuration-rsfly-family-rsfly-20-130-140)"/></g></g></g><g fill="#62645b" transform="translate(25 210) scale(0.0155 -0.0155)"><path d="M625 -30Q476 -30 373 46Q314 89 271 150L271 0L139 0L139 1440L287 1440L287 953Q325 1001 375 1036Q479 1110 631 1110Q779 1110 885 1035Q991 960 1047.5 831Q1104 702 1104 541Q1104 379 1047 250Q990 121 883 45.5Q776 -30 625 -30ZM610 107Q723 107 798 164Q873 221 910.5 319.5Q948 418 948 541Q948 663 911 761Q874 859 798.5 916Q723 973 607 973Q494 973 419.5 918Q345 863 308 765.5Q271 668 271 541Q271 416 308 317.5Q345 219 420 163Q495 107 610 107Z" transform="translate(0 0)"/><path d="M294 -480L472 3L33 1080L189 1080L544 192L871 1080L1021 1080L441 -480Z" transform="translate(1131 0)"/></g><svg x="76" y="183" width="143" height="45.48" xmlns="http://www.w3.org/2000/svg" viewBox="5 -148 619.4 197"><defs><linearGradient id="03-configuration-rsfly-firefly-trail" gradientUnits="userSpaceOnUse" x1="567.4" y1="-108" x2="623.4" y2="-54"><stop offset="0" stop-color="#ffb34a" stop-opacity="0.2"/><stop offset="0.8" stop-color="#ffb34a" stop-opacity="1"/></linearGradient></defs><path d="M24.3 0L24.3 -93.3L6 -93.3L6 -108L24.3 -108L24.3 -113.9Q24.3 -118.1 24.7 -122.45Q25.1 -126.8 26.5 -130.95Q27.9 -135.1 31.1 -138.6Q34.8 -142.7 39.2 -144.45Q43.6 -146.2 48.15 -146.6Q52.7 -147 56.8 -147L70.5 -147L70.5 -132.2L57.8 -132.2Q49.9 -132.2 46 -128.35Q42.1 -124.5 42.1 -116.7L42.1 -108L70.5 -108L70.5 -93.3L42.1 -93.3L42.1 0ZM91.5 0L91.5 -108L109.3 -108L109.3 0ZM134.8 0L134.8 -108L150.7 -108L150.7 -90.8Q151.8 -92.7 153.1 -94.5Q156.1 -98.7 159.9 -101.4Q164.1 -104.8 169.4 -106.6Q174.7 -108.4 180.2 -108.75Q185.7 -109.1 190.5 -108L190.5 -91.3Q184.9 -92.8 178.2 -92.1Q171.5 -91.4 165.8 -87.2Q160.6 -83.5 157.75 -78.2Q154.9 -72.9 153.8 -66.75Q152.7 -60.6 152.7 -54.2L152.7 0ZM247.3 3Q231.4 3 219.55 -4.05Q207.7 -11.1 201.1 -23.75Q194.5 -36.4 194.5 -53.3Q194.5 -71 201 -83.95Q207.5 -96.9 219.15 -103.95Q230.8 -111 246.5 -111Q262.8 -111 274.25 -103.5Q285.7 -96 291.45 -82.1Q297.2 -68.2 296.4 -49L213.6 -49Q214.6 -33.5 222 -24.4Q230.6 -13.9 246.5 -13.9Q257.2 -13.9 265.1 -18.8Q273 -23.7 277.5 -32.9L294.6 -27Q288.3 -12.7 275.65 -4.85Q263 3 247.3 3ZM277.9 -63.3Q276.5 -77.4 270.3 -85.2Q262.5 -95 247.1 -95Q230.6 -95 222 -84.4Q215.5 -76.4 213.9 -63.3ZM324.4 0L324.4 -93.3L306.1 -93.3L306.1 -108L324.4 -108L324.4 -113.9Q324.4 -118.1 324.8 -122.45Q325.2 -126.8 326.6 -130.95Q328 -135.1 331.2 -138.6Q334.9 -142.7 339.3 -144.45Q343.7 -146.2 348.25 -146.6Q352.8 -147 356.9 -147L370.6 -147L370.6 -132.2L357.9 -132.2Q350 -132.2 346.1 -128.35Q342.2 -124.5 342.2 -116.7L342.2 -108L395.5 -108L395.5 -147L413.3 -147L413.3 0L395.5 0L395.5 -93.3L342.2 -93.3L342.2 0ZM452.8 48L470.6 -0.4L426.8 -108L445.5 -108L479.2 -23L510.8 -108L528.9 -108L470.1 48Z" fill="#10110f"/><circle cx="100.4" cy="-136.3" r="9.97" fill="#ffb34a"/><path d="M567.4 -108L623.4 -54L603.4 -54L547.4 -108Z" fill="url(#03-configuration-rsfly-firefly-trail)"/><path d="M623.4 -54L567.4 0L547.4 0L603.4 -54Z" fill="#ffb34a"/></svg></svg></svg>
<figcaption><code>Layered::new(...)</code> merges its sources left to right and the <strong>last write wins</strong>. Defaults sit earliest and beat nothing; a base YAML beats defaults; a profile overlay beats the base; environment beats YAML files; and a CLI flag beats everything — one artifact, deployable everywhere.</figcaption>
</figure>

So an environment override (`FIREFLY_WEB_ADDR=0.0.0.0:80`) always beats a YAML
file, and a CLI flag beats both. That same precedence is exactly why
`FireflyApplication` lets `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` win
over any baked-in default bind address — the environment layer outranks the
default.

Why it matters: precedence is what makes one artifact deployable everywhere.
You commit sensible defaults and a base `application.yaml`, ship a thin
`application-prod.yaml` overlay, and let the platform inject secrets and
last-mile overrides through the environment — each layer only states what it
needs to change.

> **Tip** **Checkpoint.** You can reason about any value by reading the table
> top-down and taking the first source that defines it. If `web.addr` appears in
> both `application.yaml` and `FIREFLY_WEB_ADDR`, the environment wins because
> row 4 is later than row 2.

## Step 6 — Build the source chain explicitly

`load_from_profile` is the convenient default. When you need full control — a
different env prefix, hard-coded defaults, a remote source slotted in — assemble
the `Vec<Box<dyn Source>>` yourself and hand it to `load`:

```rust,ignore
use std::collections::HashMap;
use firefly::config::{from_env, from_optional_yaml, load, Source, StaticSource};

let sources: Vec<Box<dyn Source>> = vec![
    // 1. Defaults at the bottom — overridden by anything below.
    Box::new(StaticSource::new(
        "defaults",
        HashMap::from([("web.addr".to_string(), "127.0.0.1:8080".to_string())]),
    )),
    // 2. Base YAML beats defaults.
    Box::new(from_optional_yaml("application.yaml")),
    // 3. A LUMEN_*-prefixed environment layer beats the YAML.
    Box::new(from_env("LUMEN")),
];
let cfg: LumenConfig = load(&sources)?;
```

What just happened: you spelled out the precedence chain in list order.
`StaticSource::new` takes a name and a `HashMap` of hard-coded entries — it sits
at the bottom. `from_optional_yaml` reads a file if present (and is silently
empty if not). `from_env("LUMEN")` maps `LUMEN_WEB_ADDR` → `web.addr`. Because
the env source is *last*, `LUMEN_WEB_ADDR=0.0.0.0:80` overrides both the YAML
and the default.

> **Note** **Key term — `StaticSource` / `from_env` / `from_optional_yaml` /
> `FlagSource`.** These are the four built-in sources. `StaticSource` wraps an
> in-memory map (defaults). `from_env(prefix)` reads `PREFIX_FOO_BAR` →
> `foo.bar` from the process environment. `from_optional_yaml(path)` reads a
> YAML file, tolerating absence. `FlagSource` collects CLI overrides set with
> `.set("web.addr", "...")`. All four implement the same `Source` trait, so the
> order in the `vec!` *is* the precedence.

## Step 7 — Write the YAML and know the value rules

YAML files are parsed by a small line-by-line YAML-subset scanner — not a
general-purpose YAML engine — so the flattened output is deterministic and
stable for any given file:

```yaml
# application.yaml
name: lumen
web:
  addr: 127.0.0.1:8080
  admin-addr: 127.0.0.1:8081
tags: wallet, ledger, demo   # a comma-joined scalar binds a Vec<String>
```

The rules the scanner guarantees:

- nested mappings become **dot-joined, lower-cased** keys (`web.admin-addr` →
  the flat key `web.admin_addr` after relaxed normalization);
- scalar lexemes are **preserved verbatim** (`1.10` stays `"1.10"`) until the
  binder parses them against the target field's type;
- duplicate keys follow **last-write-wins**;
- aliases, anchors, multi-document files, tags, and flow sequences are
  **deliberately not interpreted** — bring your own parser if you need them.

What just happened: this base file states Lumen's identity and its two bind
addresses in the typed home those addresses always wanted. The `tags` line shows
the one subtlety — a sequence is written as a comma-joined scalar, and the
binder splits it back into the `Vec<String>` field.

> **Tip** **Checkpoint.** Put this file next to a test that calls
> `load_from_profile(".", "application", "dev")` and asserts
> `cfg.web.admin_addr == "127.0.0.1:8081"`. A pass proves the kebab→snake
> normalization and the comma-split list both work end to end.

## Step 8 — Resolve `${...}` placeholders

`load` (and `bind`) run a post-merge pass that resolves `${...}` placeholders
inside values — the same `${...}` syntax Spring uses. It is also exposed
standalone as `resolve_placeholders(&flat)`.

```yaml
name: lumen
datasource:
  url: ${DATABASE_URL:postgres://localhost/lumen}   # env var, else default
  pool: ${name}-pool                                 # config reference
```

The resolution order, highest priority first:

- `${ENV_VAR}` — a literal environment variable, read as written;
- the **relaxed `FIREFLY_*` form** of a config key — `${name}` also honors
  `FIREFLY_NAME` before consulting the merged map, so **environment beats
  config**;
- `${name}` — a config reference into the merged map itself, resolved
  recursively with a depth-10 guard against cycles;
- `${key:default}` — the text after the first `:` is a fallback when neither the
  environment nor the config resolves `key`.

What just happened: `datasource.url` reads `DATABASE_URL` from the environment
when present and otherwise falls back to the local default — one line that is
correct in both dev and prod. `datasource.pool` interpolates another config
value (`name` → `lumen`) to produce `lumen-pool`.

> **Warning** An unresolvable placeholder *without* a default raises
> `ConfigError::Placeholder`, and so does a circular reference (`a: ${b}` /
> `b: ${a}`) once it trips the depth-10 guard. A typo'd `${DATBASE_URL}` with no
> `:default` fails the load loudly rather than binding an empty string.

## Step 9 — Bind config straight into a bean with `#[derive(ConfigProperties)]`

Loading a struct by hand in `main` is fine, but Lumen's services want their
configuration *injected*, not threaded through every constructor.
`#[derive(ConfigProperties)]` turns a `serde` struct into a container-managed,
prefix-bound bean — the exact pattern the next chapter builds on.

```rust,ignore
use firefly::prelude::*;
use serde::Deserialize;

/// Binds the `lumen.web.*` config subtree into an injectable bean.
#[derive(Deserialize, ConfigProperties, Default)]
#[firefly(prefix = "lumen.web")]
pub struct WebProperties {
    pub addr: String,
    #[serde(default)]
    pub admin_addr: String,
}
```

What just happened: the derive registers `WebProperties` as a singleton whose
factory binds the `lumen.web.*` slice of the merged, profile-resolved,
placeholder-expanded config map. The container warms it eagerly at startup, so
any bean can then receive it by type.

> **Note** **Key term — bean / autowiring.** A *bean* is an object the framework
> constructs and manages for you; *autowiring* is the framework handing a bean
> to whoever declares a field for it. A `#[derive(Service)]` bean writes
> `#[autowired] props: Arc<WebProperties>` and receives the bound values — no
> manual `load`, no global. You will wire one in
> [Dependency Wiring](./04-dependency-wiring.md). This is Spring's
> `@ConfigurationProperties` bean injected with `@Autowired`.

For one-off scalars there is a lighter touch — inject a single resolved value
onto a field with a default:

```rust,ignore
#[firefly(value = "${lumen.web.addr:127.0.0.1:8080}")]
addr: String,
```

To *validate* a properties bean after binding — Spring's `@Validated` on a
`@ConfigurationProperties` class — add `#[firefly(validate)]` and
`#[derive(Validate)]`. The macro runs the struct's declarative constraints once
the config is bound, and a violation **fails the bean's creation** at context
refresh with the structured per-field errors, rather than letting a malformed
configuration boot:

```rust,ignore
use firefly::prelude::*;
use serde::Deserialize;

#[derive(Deserialize, ConfigProperties, Validate, Default)]
#[firefly(prefix = "lumen.web", validate)]   // @ConfigurationProperties @Validated
pub struct WebProperties {
    #[validate(not_empty)]
    pub addr: String,
    #[serde(default)]
    pub admin_addr: String,
}
```

What just happened: an empty `lumen.web.addr` now aborts startup with a clear
per-field violation (`addr: must not be empty (not_empty)`) instead of binding
`""` and failing later when something tries to bind a socket.

> **Design note.** Firefly offers two binding styles against the *same* merged,
> profile-resolved, placeholder-expanded map. A prefix-bound bean
> (`#[derive(ConfigProperties)]` + `#[firefly(prefix = "...")]`) pulls a whole
> config subtree into one injectable struct; single-value injection
> (`#[firefly(value = "${...}")]`) wires one resolved scalar onto a field. Use
> the first for a cohesive settings group, the second for a stray knob.

## Step 10 — Auto-configure the datasource and security from `application.yaml`

The properties machinery so far hands you a typed struct. Firefly's
infrastructure crates take the next step: a handful of subsystems are
**config-driven and DI-free** — you bind a plain `serde` struct from
`application.yaml`/env, then `await` a single auto-configure call at boot, and
the subsystem stands itself up. No container, no manual builder chains, no
`if scheme == "postgres"` branching in your wiring. Two subsystems Lumen leans
on this way are its datasource and its security layer.

Both feed off one YAML tree. `firefly.datasource.*` binds onto
`DataSourceProperties` and `firefly.security.*` onto `SecurityProperties`:

```yaml
firefly:
  datasource:
    url: ${DATABASE_URL:postgres://localhost/lumen}  # scheme picks the backend
    max-connections: 16
    min-connections: 2
    acquire-timeout-ms: 5000
    idle-timeout-ms: 600000
    max-lifetime-ms: 1800000
  security:
    jwt:
      jwk-set-uri: https://idp.example.com/.well-known/jwks.json
      issuer-uri: https://idp.example.com/
      audience: lumen-api
    bearer:
      header-name: Authorization
      allow-anonymous: false
```

### The datasource — `DataSourceProperties` → pool → transaction manager

`DataSourceProperties` is a plain `serde` struct with the fields `{ url,
max_connections, min_connections, acquire_timeout_ms, idle_timeout_ms,
max_lifetime_ms }`. The **URL scheme selects the backend**, each behind its own
cargo feature: `postgres://` / `postgresql://` → PostgreSQL, `mysql://` →
MySQL, `sqlite:` → SQLite. A `0` for any pool setting leaves the `sqlx` default
in place.

`firefly::data_sqlx::auto_configure(&props)` does the one thing you want at
boot: it builds the connection pool **and** registers a
`SqlxTransactionManager` over it, so `#[transactional]` resolves later with no
manual wiring. The returned `Db` is the same pool, ready to build typed
repositories. (For finer control, `Db::connect(url)` and
`Db::connect_with(&props)` build just the pool.)

```rust,ignore
use firefly::data_sqlx::{auto_configure, DataSourceProperties};

// `Db` carries the pool; auto_configure also registers the tx manager.
let db = auto_configure(&props).await?;     // Result<Db, FireflyError>
```

> **Note** **Key term — transaction manager.** A *transaction manager* opens,
> commits, and rolls back database transactions on behalf of the
> `#[transactional]` attribute. By registering one, `auto_configure` makes
> `#[transactional]` work process-wide without you constructing or threading the
> manager anywhere — the Rust analog of Spring Boot auto-configuring a
> `DataSourceTransactionManager`. You will use it in [Persistence](./07-persistence.md).

### The security layer — `SecurityProperties` → verifier → bearer layer

`SecurityProperties` nests `{ jwt: JwtProperties, bearer: BearerProperties }`.
`JwtProperties` holds `{ jwk_set_uri, issuer_uri, audience, secret, algorithm,
expiration_seconds }`; `BearerProperties` holds `{ header_name, allow_anonymous }`.
Two functions turn that into running middleware:

- `verifier_from_config(&props.jwt)` returns
  `Result<Option<Arc<dyn Verifier>>, SecurityError>`. A non-empty `jwk_set_uri`
  builds a JWKS (RS256) resource-server verifier; otherwise a non-empty `secret`
  builds an HMAC verifier (`HS256`/`HS384`/`HS512`); otherwise `None`.
- `bearer_layer_from_config(&props)` returns
  `Result<Option<BearerLayer>, SecurityError>` — the ready-to-mount layer with
  the configured header name and anonymous policy already applied, or `None`
  when no verifier is configured.

> **Note** **Key term — verifier / bearer layer.** A *verifier* checks an
> incoming JWT's signature and claims; a *bearer layer* is the HTTP middleware
> that pulls the token off the request header and runs the verifier. Together
> they are the Rust analog of a Spring Security resource-server filter chain.
> The full security story is [Security](./14-security.md); here you are only
> learning that both can be *configured*, not hand-built.

### The one-call startup wiring

Bind one config struct, then drive both subsystems from it. The whole wiring is
a load plus two awaited calls:

```rust,ignore
use firefly::config::{load_from_profile, ConfigError};
use firefly::data_sqlx::{auto_configure, DataSourceProperties};
use firefly::security::{bearer_layer_from_config, SecurityProperties};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Firefly {
    datasource: DataSourceProperties,
    security: SecurityProperties,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LumenConfig {
    firefly: Firefly,   // binds the `firefly.datasource.*` / `firefly.security.*` subtree
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load + merge + profile-resolve + placeholder-expand, then bind.
    let cfg: LumenConfig = load_from_profile("/etc/lumen", "application", "dev")?;

    // 2. Build the pool AND register the transaction manager in one await.
    let db = auto_configure(&cfg.firefly.datasource).await?;

    // 3. Build the ready-to-mount bearer layer (None if no JWT settings).
    let bearer = bearer_layer_from_config(&cfg.firefly.security)?;

    // `db` builds typed repositories; mount `bearer` on the web stack.
    // ...
    let _ = (db, bearer);
    Ok(())
}
```

What just happened: this is the precedence chain from Step 5 doing real work.
`DATABASE_URL` in the environment overrides the YAML default for the pool, and
the JWKS endpoint can be re-pointed per profile without touching code. The
`#[transactional]` machinery and the bearer middleware both pick up what
`auto_configure` and `bearer_layer_from_config` registered — no globals threaded
through your constructors.

> **Design note.** Firefly deliberately keeps this path DI-free: the config
> structs are ordinary `serde` types and the auto-configure calls are ordinary
> `async fn`s you `await` at boot. You can adopt the full
> `#[derive(ConfigProperties)]` container style later without rewriting any of
> it — the same bound values flow either way.

> **Tip** **Checkpoint.** Even without a real database, this `main` compiles:
> `auto_configure` against `sqlite::memory:` (set `firefly.datasource.url` to
> `sqlite::memory:`) returns a live `Db` you can hold, and an empty
> `firefly.security.*` makes `bearer_layer_from_config` return `Ok(None)`.

## Step 11 — Gate beans by profile expression

Sometimes a value is not enough — you want a whole bean to exist only in some
environments. `accepts_profiles(&active, &exprs)` evaluates a
profile-expression grammar against an active-profile list: AND (`&`), OR (`|`),
negation (`!`), and grouping with parentheses.

```rust,ignore
use firefly::config::{accepts_profiles, active_profiles};

let active = active_profiles("dev");                  // e.g. ["prod", "cloud"]
accepts_profiles(&active, &["prod & cloud"]);         // AND
accepts_profiles(&active, &["prod | qa"]);            // OR
accepts_profiles(&active, &["!test"]);                // negation
accepts_profiles(&active, &["(prod & cloud) | qa"]);  // grouping
```

What just happened: `active_profiles("dev")` reads the comma-separated
`FIREFLY_PROFILE` (falling back to `"dev"`), and `accepts_profiles` answers
whether *any* of the given expressions matches that active set. It returns
`true` on a match; a malformed expression evaluates to `false` and never panics.

Why it matters: the next chapter shows a bean declaring
`#[firefly(profile = "prod")]`, and the container applies exactly this rule at
scan time — so a Postgres-only bean simply does not exist in the `dev` profile.

## Step 12 — Reload at runtime and mask secrets

Two operational concerns round out the picture.

**Runtime reload.** `ReloadableConfig<T>` keeps the source chain alive after the
first bind. `reload()` replays the full merge → placeholder-resolution → bind
pipeline and atomically swaps the snapshot; a failed reload keeps the previous
one. This is the hook a `POST /actuator/refresh` endpoint wires up — so an
operator could re-point Lumen's datasource without a restart.

```rust,ignore
use firefly::config::{ReloadableConfig, Source};

let cfg: ReloadableConfig<LumenConfig> = ReloadableConfig::load(sources)?;
let snapshot = cfg.get();                  // Arc<LumenConfig> — read per use
let mut rx = cfg.subscribe();              // tokio watch receiver
let changed: Vec<String> = cfg.reload()?;  // sorted, changed top-level keys
```

`Arc<ReloadableConfig<T>>` coerces to `Arc<dyn Refresher>` — the object-safe
trait the actuator refresh endpoint depends on.

> **Note** **Key term — refresh scope.** A *refresh-scoped* reader calls
> `cfg.get()` per use instead of caching the inner value, so it always sees the
> latest snapshot after a reload. This is the Rust analog of Spring Cloud's
> `@RefreshScope` plus its `POST /actuator/refresh` contract.

**Masking secrets.** `Layered::property_sources()` returns ordered,
origin-attributed `PropertySourceView`s (highest precedence first) — the data
Firefly's `/actuator/env` view renders, with secrets masked. Keys naming secrets
(`password`, `secret`, `token`, `credential`, `*key`, …) mask as `******`, and a
password embedded in a URI's userinfo is redacted
(`postgresql://user:******@host`). The `mask` module exposes `mask_value`,
`is_sensitive_key`, and `sanitize_uri` directly.

Why it matters for Lumen: the moment it holds a JWT signing key (chapter 14) and
a datasource URL, neither should ever appear in plaintext on `/actuator/env` —
and with masking on by default, neither does.

> **Tip** **Checkpoint.** Add a `datasource.password` key to a `StaticSource`,
> call `Layered::new(sources).property_sources()`, and confirm the rendered
> value is `******`, not the secret.

## Step 13 — Pull configuration from a config server (optional)

For a fleet of services, you may centralize configuration. `ConfigClient`
fetches a remote document (compatible with the Spring Cloud Config server wire
format) and flattens it into a `StaticSource` you slot into the chain above the
defaults:

```rust,ignore
use firefly::config::ConfigClient;

let remote = ConfigClient::new("http://config:8888", "lumen")
    .with_profile("prod")
    .with_label("main")
    .with_basic_auth("user", "pass")
    .fetch_source()           // fail-fast; .fetch_source_or_empty() = soft fallback
    .await?;
sources.insert(1, Box::new(remote)); // above defaults, below env/flags
```

What just happened: `ConfigClient::new(url, app)` builds a client (profile
defaults to `default`, label to `main`); the builder methods set the rest;
`fetch_source().await` queries `{url}/{app}/{profile}/{label}` and returns a
`StaticSource`. A non-2xx response logs a warning and yields an empty map (a
soft miss); transport or decode failures raise `ConfigError::Remote`. The
standalone server lives in [`firefly-config-server`](./91-appendix-modules.md).

## In-process application events

One more piece of the config crate is worth naming, because you will meet it at
lifecycle boundaries. `ApplicationEventBus` is an **in-process,
`TypeId`-dispatched, order-sorted, synchronous** pub/sub for lifecycle and local
notification events — distinct from the asynchronous `firefly-eda` broker Lumen
uses for domain events (no transport, no topics; listeners run on the publishing
thread):

```rust,ignore
use firefly::config::{ApplicationEventBus, ApplicationReadyEvent};

let bus = ApplicationEventBus::new();
bus.subscribe::<ApplicationReadyEvent, _>(|_e| { /* on ready */ });
bus.publish(&ApplicationReadyEvent);
```

Lifecycle events ship: `ContextRefreshedEvent`, `ApplicationReadyEvent`,
`ContextClosedEvent`, and `RefreshScopeRefreshedEvent` (fired after a successful
reload). Any `'static` type can be published as a local domain event.

> **Note** Do not confuse this with [Event-Driven Architecture](./10-eda-messaging.md):
> the `ApplicationEventBus` is a *local* lifecycle/notification channel; Lumen's
> wallet domain events ride the `firefly-eda` `Broker` over a topic, with a real
> Kafka/RabbitMQ adapter waiting behind the in-memory default.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| identity hard-coded in two `pub const` strings | the same values understood as `CoreConfig` knobs that feed the banner and `/actuator/info` |
| bind addresses read by `FireflyApplication` from `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` | the typed home for those addresses, sitting at the top of a documented precedence chain |
| no path to per-environment settings | profiles, placeholders, and `#[derive(ConfigProperties)]` ready for injection in the next chapter |
| datasource and security would be hand-built | both stand up from `application.yaml` with one awaited auto-configure call each |
| secrets unconsidered | masking + `/actuator/env` redaction in place before Lumen ever holds a signing key |

You also now know:

- That configuration is a **flat, dot-keyed string map** bound onto a typed
  `serde` struct, with the *target type* driving every parse.
- The **precedence chain** — defaults → base YAML → profile YAML → environment →
  CLI flags — and that the last source wins.
- That `load_from_profile` is the convenient default (with a `FIREFLY_*` env
  layer), while an explicit `Vec<Box<dyn Source>>` + `load` gives full control.
- How `${...}` placeholders resolve (environment beats config, with `:default`
  fallbacks and a cycle guard), how `#[derive(ConfigProperties)]` injects a
  bound subtree, and how `auto_configure` / `bearer_layer_from_config` stand up
  whole subsystems from YAML.

## Exercises

1. **Promote the ports to YAML.** Write an `application.yaml` with `web.addr` /
   `web.admin-addr`, load it with `load_from_profile(".", "application", "dev")`,
   and confirm a `FIREFLY_WEB_ADDR` environment variable still wins (precedence
   row 4 beats row 2). Then rebuild the chain by hand with `from_env("LUMEN")`
   and show `LUMEN_WEB_ADDR` winning instead.
2. **Add a profile.** Create `application-prod.yaml` that overrides `web.addr`
   to `0.0.0.0:80`, run with `FIREFLY_PROFILE=prod`, and verify the prod value
   takes effect while plain `dev` keeps the localhost binding.
3. **Resolve a placeholder.** Set `datasource.url:
   ${DATABASE_URL:postgres://localhost/lumen}` in YAML, load once with
   `DATABASE_URL` unset (assert the default) and once with it set (assert the
   override). Then delete the `:default` and confirm the unset case now raises
   `ConfigError::Placeholder`.
4. **Bind a `ConfigProperties` bean.** Define the `WebProperties` struct from
   Step 9, set `lumen.web.addr` via a `ConditionContext::new().with_property(...)`,
   and resolve `WebProperties` from a `Container` — you will recognize this
   pattern in the next chapter's DI tests.
5. **Mask a secret.** Add a `datasource.password` key to a `StaticSource`, call
   `Layered::new(sources).property_sources()`, and confirm the value renders as
   `******` rather than in plaintext.

## Where to go next

- See how Lumen's composition root resolves its collaborators — and how the
  best-in-class container scans and wires the beans (including the
  `#[derive(ConfigProperties)]` ones you just met) — in
  **[Dependency Wiring](./04-dependency-wiring.md)**.
- Turn the configured datasource into typed repositories in
  **[Persistence](./07-persistence.md)**.
- Promote the in-process defaults to real Postgres and Kafka in
  **[Production & Deployment](./20-production.md)**.
