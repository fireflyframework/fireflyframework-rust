## Conventions

This page explains the typographic and structural conventions used throughout the book.

### Code Listings

Every multi-line code example carries a **language tab** in its top-left corner naming the language or file it belongs to. Inline code references within prose use `monospace` font, as in "the `#[component]` attribute registers the type with Firefly's container."

```rust
use firefly_core::{Core, CoreConfig};

#[tokio::main]
async fn main() -> Result<(), FireflyError> {
    let core = Core::new(CoreConfig::default()).await?;
    core.serve().await
}
```

Snippets marked `rust,ignore` or `rust,no_run` elide setup for brevity, but the API names, types, and method signatures are exactly what the crates expose.

### Callouts

Five callout styles appear throughout the body:

> **Note** — Notes provide supplementary context or clarify a subtlety in the main text. Worth reading, but not blocking.

> **Tip** — Tips share a shortcut, idiom, or best practice that will save you time in real projects.

> **Warning** — Warnings flag a common mistake or a sharp edge that can cause hard-to-debug problems if ignored.

> **Spring parity** — Spring parity callouts map a Firefly concept directly to its Spring Boot equivalent — ideal for developers migrating from the JVM ecosystem.

> **Reactor parity** — Reactor parity callouts map Firefly's `Mono`/`Flux` surface onto Project Reactor and WebFlux, so the reactive idioms you know transfer directly.

### Figures & Mapping Tables

Diagrams open each chapter as inline SVG, so they render crisply at any zoom in both the screen and print editions. Mapping tables line up a familiar concept (Reactor, Spring, Spring Data) against its Firefly spelling, so you translate by lookup rather than by guesswork.

### Recap & Exercises

Each chapter closes with a **Recap** of what changed in the Lumen codebase and a set of **Exercises** that push one step further. The exercises are optional but recommended for anything you intend to apply immediately.
