## Conventions

This page explains the typographic and structural conventions used throughout the book — and demonstrates each one with a live example, so the first time you meet a callout or a code caption in a chapter it already looks familiar.

### Code Listings

Every multi-line code example is real, compiling Rust drawn from the Lumen companion crate. Where it helps, a listing is introduced with the **file it lives in** so you can find it in `samples/lumen`, as in "`samples/lumen/src/money.rs`". Inline code references within prose use `monospace`, as in "the `#[rest_controller]` attribute generates the wallet router."

Here is a representative listing — the constructor and exact-arithmetic core of Lumen's `Money` value object, lifted verbatim from `samples/lumen/src/money.rs`:

```rust
/// An exact monetary amount, expressed in integer minor units (cents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money {
    cents: i64,
}

impl Money {
    /// A zero amount — the opening balance of a brand-new wallet.
    pub const ZERO: Money = Money { cents: 0 };

    /// Returns a new `Money` that is `self + other` (immutable addition).
    #[must_use]
    pub const fn add(self, other: Money) -> Money {
        Money { cents: self.cents + other.cents }
    }
}
```

A snippet annotated `rust,ignore` or `rust,no_run` elides surrounding setup for focus, but the API names, types, and method signatures are exactly what the crates expose. A listing fenced as plain `text` is shell output, a banner, or an HTTP exchange rather than Rust source:

```text
$ cargo run -p firefly-sample-lumen
:: lumen :: digital-wallet & ledger (v26.6.4)
```

### The One-Dependency Reminder

Because Lumen's defining property is its single Firefly dependency, every framework type you see is reached through the facade — `firefly::cqrs::Bus`, `firefly::eventsourcing::EventStore`, `firefly::reactive::Flux` — or, for the high-frequency surface and every macro, through one glob:

```rust
use firefly::prelude::*;
```

When a chapter introduces a new framework type, the prose names the facade path it lives behind, so you always know it came in through that one dependency.

### Callouts

Four callout styles appear throughout the body. Each is a blockquote that opens with a bold label, and the design theme styles them distinctly:

> **Note.** Notes provide supplementary context or clarify a subtlety in the main text. Worth reading, but not blocking.

> **Tip.** Tips share a shortcut, idiom, or best practice that will save you time in real projects — for example, keeping money in integer cents so floating-point drift can never corrupt a balance.

> **Warning.** Warnings flag a common mistake or a sharp edge that causes hard-to-debug problems if ignored — for example, that Lumen's free-function CQRS handlers publish their collaborators through a process-global `OnceLock`, so a second `build_app()` in the same test binary keeps the *first* wiring.

> **Design note.** Design-note callouts explain *why* Firefly does something a particular way, and point out where an idea will feel familiar if you have used an opinionated, batteries-included framework or a reactive-streams library before. They are orientation, framed as Firefly's own design choices — not a translation table for another framework. You will meet these in nearly every chapter.

### Reference Tables

When a chapter introduces a family of related APIs, a reference table collects them in one place so you can take in the whole surface at a glance:

| Declarative attribute | What it generates |
|---|---|
| `#[rest_controller]` | an axum router from the annotated handler methods |
| `#[event_listener]` | a broker subscription bound to an event type |
| `#[scheduled]` | a task registered on the scheduler |
| `#[saga]` / `Step` | an orchestrated, compensating distributed transaction |

### Recap & Exercises

Each chapter closes with two fixed sections:

- A **Recap — what changed in Lumen** that lists the files added or extended and the one-sentence "by the end of this chapter, Lumen can …" payoff.
- A set of **Exercises** that push one step further — usually a small, self-contained extension to the code the chapter just shipped. They are optional but recommended for anything you intend to apply immediately.

Turn the page to [Why Firefly for Rust](../01-why-firefly.md), where the Lumen journey begins.
