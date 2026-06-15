## A Rust Primer

This book builds reactive, asynchronous, event-sourced microservices in Rust. The chapters assume you can *read* Rust comfortably; this primer makes sure you can — even if your day job is Java/Spring or Python. It is not a complete Rust course. It teaches exactly the slice of the language the later chapters lean on, in the order they lean on it, so that when Chapter 6 hands you a `Mono<WalletEvent>` or Chapter 10 shows you `#[derive(Command)]`, none of the syntax is a surprise.

If you have already shipped Rust with `async`/`await`, traits, and ownership, skip ahead — Chapter 1 is where Lumen begins. If you are coming from a garbage-collected language, read this once, type the snippets, and keep it bookmarked.

> **Note** — every code block in this primer was compiled and run before it was printed. The complete-program snippets build and execute as shown; the few that elide surrounding setup are marked and use real, current Firefly API names.

### Cargo, Crates, Modules, and `use`

A Rust project is built by **Cargo**, the package manager and build tool — the rough equivalent of Maven/Gradle in the Java world or pip/Poetry in Python. A unit of compilation is a **crate**: either a binary (an application) or a library. The crates your project depends on are listed in a manifest file called `Cargo.toml`:

```toml
[package]
name = "lumen"
version = "0.1.0"
edition = "2021"

[dependencies]
firefly = { version = "26.6.5" }                       # the whole framework
tokio   = { version = "1", features = ["full"] }       # the async runtime
serde   = { version = "1", features = ["derive"] }      # (de)serialization
```

Each entry under `[dependencies]` is a crate pulled from crates.io (Rust's package registry, like Maven Central or PyPI). The `features` list turns on optional parts of a crate — a compile-time switch, not a runtime flag.

The everyday commands:

```bash
cargo build      # compile the project
cargo run        # compile, then run the binary
cargo test       # compile and run the tests
cargo check      # type-check without producing a binary (fast)
```

Within a crate, code is organized into **modules** — namespaces that group related items. You reach into a module's contents with a path of `::`-separated segments, and the `use` keyword brings a path into scope so you can refer to it by its short name:

```rust
use std::sync::Arc;          // bring `Arc` into scope from the standard library
use std::collections::HashMap;

fn main() {
    let map: HashMap<String, i64> = HashMap::new();   // now just `HashMap`
    let _shared = Arc::new(map);
}
```

> **Spring parity.** `use std::sync::Arc;` is Rust's `import java.util.concurrent.atomic.*;`. A crate is a Maven artifact; a module is a Java package; `Cargo.toml` is your `pom.xml`. The big difference is that there is no classpath at runtime — everything is resolved and linked at compile time into one binary.

Throughout the book, the entire framework arrives through a single glob import you will see at the top of nearly every listing:

```rust,ignore
use firefly::prelude::*;
```

That one line pulls in `Mono`, `Flux`, `Bus`, the macros, and the rest of the high-frequency surface. When a chapter introduces a new framework type, the prose names the facade path it lives behind — `firefly::cqrs::Bus`, `firefly::eventsourcing::EventStore` — so you always know it came in through that one dependency.

### Variables and Basic Types

Variables are declared with `let`. They are **immutable by default**; add `mut` to make a binding reassignable. Types are usually inferred, but you can annotate them after a colon:

```rust
fn main() {
    let count = 3;          // immutable; inferred as i32
    let mut total = 0_i64;  // `mut` makes it reassignable; i64
    total += count as i64;  // `as` is an explicit numeric cast
    println!("total = {total}");

    let active: bool = true;
    let ratio: f64 = 0.5;   // 64-bit float
    let initial: char = 'L';
}
```

The numeric types spell out their size and signedness: `i32`/`i64` (signed), `u32`/`u64` (unsigned), `usize` (pointer-sized, used for lengths and indices), `f32`/`f64` (floats). Lumen keeps money in `i64` cents, never `f64`, so rounding can never corrupt a balance.

> **Spring parity.** `let` declares a local like Java's `var`, but immutability is the default — the opposite of Java, where you must write `final` to opt *in*. In Rust you write `mut` to opt *out* of immutability. This default is load-bearing: the compiler uses it to reason about who is allowed to change what.

#### `String` vs `&str`

This distinction trips up every newcomer, so meet it early. There are two string types:

- **`String`** — an owned, heap-allocated, growable string. You *own* the buffer; when the `String` goes out of scope, its memory is freed.
- **`&str`** — a *borrowed view* into string data you do not own (a "string slice"). String literals like `"wallet"` are `&str`.

The idiom: **take `&str` as a parameter, return `String` when you produce a new owned value.** A `&str` can borrow from a `String`, a literal, or anywhere — so accepting `&str` makes a function maximally flexible.

```rust
fn greet(name: &str) -> String {     // borrow input, return a fresh owned String
    format!("hello, {name}")
}

fn main() {
    let owned: String = String::from("Lumen");
    let literal: &str = "wallet";
    println!("{}", greet(&owned));   // pass a &str borrowed from the String
    println!("{}", greet(literal));  // or a literal directly
}
```

### Ownership, Borrowing, and References

This is the concept that makes Rust *Rust*. It is also the one with no equivalent in Java or Python, so read this section slowly.

Every value has exactly one **owner** — the variable that is responsible for freeing it. When the owner goes out of scope, the value is dropped (its memory and resources released). There is no garbage collector deciding *when* that happens; it happens deterministically, at the end of the owner's scope.

When you assign a value to another variable or pass it to a function, ownership **moves**. The original binding can no longer be used:

```rust,ignore
let a = String::from("Lumen");
let b = a;            // ownership MOVES from `a` to `b`
println!("{a}");     // COMPILE ERROR: value borrowed here after move
```

If you want to *use* a value without taking ownership of it, you **borrow** it with a reference. A reference is written `&T` (a shared/immutable borrow) or `&mut T` (an exclusive/mutable borrow). Borrowing lets a function read or modify a value and then hand it back:

```rust
fn len_of(s: &String) -> usize {   // shared borrow: may read, not modify
    s.len()
}

fn push_bang(s: &mut String) {     // exclusive borrow: may modify
    s.push('!');
}

fn main() {
    let mut name = String::from("Lumen");
    let n = len_of(&name);   // &name  — a shared borrow
    push_bang(&mut name);    // &mut name — an exclusive borrow
    println!("{name} now has length {}", n + 1);
}
```

The compiler enforces one rule, the **borrow checker's** core invariant: at any moment you may have **either** any number of shared (`&T`) references **or** exactly one exclusive (`&mut T`) reference to a value — never both. That single rule is what makes data races impossible: you can never have one thread reading while another writes, because the type system will not let two such references coexist.

> **Spring parity.** In Java or Python, objects live on the heap, every variable is a reference to them, and a garbage collector reclaims them at some unpredictable later time. Aliasing is free and unchecked — two threads can happily hold the same `ArrayList` and corrupt it. Rust trades that freedom for guarantees: ownership is tracked at compile time, memory is freed the instant the owner's scope ends (no GC pauses, no `finalize`), and the borrow rules make data races a *compile error* rather than a 2 a.m. production incident. The phrase you will hear is "fearless concurrency" — that is what it means.

> **Tip** — when a snippet does `Arc::clone(&repo)` or `&self` instead of moving a value, it is borrowing to avoid giving up ownership. Reading "is this a move or a borrow?" off the `&` is most of what it takes to follow the rest of the book.

### `Arc<T>` — Shared Ownership Across Threads

Sometimes one owner is not enough: a service, a repository, a config object may need to be shared by many tasks running concurrently. `Arc<T>` — *Atomically Reference-Counted* — is the answer, and you will see it on nearly every page of this book. An `Arc<T>` is a thread-safe shared handle to a `T`; cloning it is cheap (it bumps a reference count, it does not copy the `T`), and the `T` is dropped only when the last `Arc` handle goes away.

```rust
use std::sync::Arc;

fn main() {
    let config = Arc::new(String::from("lumen-config"));
    let handle2 = Arc::clone(&config);   // a second handle to the SAME value
    println!("{config} / {handle2}");    // both point at one allocation
}
```

The framework leans on this in a particular shape: **`Arc<dyn Trait>`** — a shared handle to *some* type that implements a trait, where the concrete type is hidden behind the interface. This is how Firefly passes around services and hexagonal "ports" so a wallet handler can depend on a `WalletRepository` without knowing whether it is backed by Postgres, MongoDB, or an in-memory map:

```rust
use std::sync::Arc;

trait WalletRepository: Send + Sync {       // a "port"
    fn balance(&self, id: u64) -> i64;
}

struct InMemoryRepo;                         // one "adapter"
impl WalletRepository for InMemoryRepo {
    fn balance(&self, _id: u64) -> i64 { 0 }
}

fn main() {
    // The caller holds the interface, not the concrete type:
    let repo: Arc<dyn WalletRepository> = Arc::new(InMemoryRepo);
    let also_repo = Arc::clone(&repo);       // share it with another task
    println!("{}", repo.balance(1));
    println!("{}", also_repo.balance(2));
}
```

`Arc` shares *read* access. To share *mutable* state, wrap the inner value in a lock — `Mutex<T>` (one accessor at a time) or `RwLock<T>` (many readers or one writer) — giving the familiar `Arc<Mutex<T>>` / `Arc<RwLock<T>>`. The lock enforces the "one writer" rule at run time for data the borrow checker cannot track across threads:

```rust
use std::sync::{Arc, Mutex};

fn main() {
    let counter = Arc::new(Mutex::new(0_u64));
    *counter.lock().unwrap() += 1;   // lock, mutate, unlock at end of statement
    println!("{}", *counter.lock().unwrap());
}
```

### Structs, Enums, and `match`

A **struct** groups named fields — your data records and value objects. Methods are defined in an `impl` block; `&self` borrows the receiver (read), `&mut self` borrows it exclusively (mutate), and a method without `self` is an associated function (Rust's "static method"), commonly used for constructors:

```rust
struct Wallet {
    id: u64,
    balance: i64,
}

impl Wallet {
    fn new(id: u64) -> Self {            // associated fn (constructor by convention)
        Wallet { id, balance: 0 }
    }
    fn deposit(&mut self, cents: i64) {  // mutating method
        self.balance += cents;
    }
}

fn main() {
    let mut w = Wallet::new(1);
    w.deposit(500);
    println!("wallet {} has {} cents", w.id, w.balance);
}
```

An **enum** is far more powerful than a Java enum: each variant can carry its own data. Enums model "one of several shapes," which is exactly how the book represents commands, events, and states:

```rust
enum Command {
    Open { id: u64 },                 // a struct-like variant
    Deposit { id: u64, cents: i64 },  // with named fields
    Close,                            // a unit variant, no data
}
```

You take an enum apart with `match` — an exhaustive, expression-valued switch. **Exhaustive** means the compiler refuses to compile unless you handle every variant, so adding a new command later forces you to update every place that matches on it:

```rust,ignore
fn describe(cmd: &Command) -> String {
    match cmd {
        Command::Open { id }            => format!("open wallet {id}"),
        Command::Deposit { id, cents }  => format!("deposit {cents} into {id}"),
        Command::Close                  => "close".to_string(),
    }
}
```

`match` is one form of **pattern matching**, which also appears in `let` destructuring, `if let`, and function parameters. You will read patterns constantly; recognizing that the left side of a `=>` *binds names by destructuring the value* is the key.

### `Option<T>`, `Result<T, E>`, and the `?` Operator

Rust has no `null` and no exceptions. Absence and failure are ordinary values, expressed by two standard enums.

**`Option<T>`** says "maybe a `T`": it is either `Some(value)` or `None`. This replaces `null`/`None`/`nil` — and because it is a distinct type, you cannot accidentally use a missing value as if it were present.

```rust
fn first_char(s: &str) -> Option<char> {
    s.chars().next()      // None if the string is empty
}

fn main() {
    match first_char("Lumen") {
        Some(c) => println!("first char is {c}"),
        None    => println!("string was empty"),
    }
}
```

**`Result<T, E>`** says "a `T` or an error `E`": it is either `Ok(value)` or `Err(error)`. This is how every fallible operation reports failure — there are no thrown exceptions to catch:

```rust
#[derive(Debug)]
struct ParseError;

fn parse_amount(s: &str) -> Result<i64, ParseError> {
    s.parse::<i64>().map_err(|_| ParseError)   // convert the std error into ours
}

fn main() {
    println!("{:?}", parse_amount("500"));   // Ok(500)
    println!("{:?}", parse_amount("oops"));  // Err(ParseError)
}
```

Handling every `Result` by hand would be tedious, so Rust gives you the **`?` operator**. Applied to a `Result`, `?` unwraps the `Ok` value or, on `Err`, *returns that error from the current function immediately*. It threads failures up the call stack without exceptions and without boilerplate:

```rust
#[derive(Debug)]
struct ParseError;

fn parse_amount(s: &str) -> Result<i64, ParseError> {
    s.parse::<i64>().map_err(|_| ParseError)
}

fn double_amount(s: &str) -> Result<i64, ParseError> {
    let n = parse_amount(s)?;   // on Err, return early; on Ok, bind n
    Ok(n * 2)
}

fn main() {
    println!("{:?}", double_amount("21"));    // Ok(42)
    println!("{:?}", double_amount("nope"));  // Err(ParseError)
}
```

> **Spring parity.** Where Java throws and catches, and Python raises and excepts, Rust *returns* errors as values and propagates them with `?`. A function's signature tells you up front whether it can fail (`-> Result<T, E>`) and with what — failure is part of the type, not a hidden control-flow path. Firefly fixes the error type for its reactive and web layers to one `FireflyError`, so the `?` operator threads failures straight through a pipeline and out as an RFC 9457 problem response.

### Traits and `impl` — Rust's Interfaces

A **trait** defines shared behavior — a set of method signatures a type can promise to provide. It is the closest thing Rust has to a Java interface or a Python protocol/ABC. You implement a trait for a type with `impl Trait for Type`, and a trait may supply default method bodies:

```rust
trait Greeter {
    fn greet(&self) -> String;        // required method
    fn shout(&self) -> String {       // default method, built on the required one
        self.greet().to_uppercase()
    }
}

struct English;
impl Greeter for English {
    fn greet(&self) -> String {
        "hello".to_string()
    }
}

fn main() {
    let e = English;
    println!("{} / {}", e.greet(), e.shout());  // hello / HELLO
}
```

When you want a value whose concrete type is decided at run time — "any `Greeter`, I do not care which" — you use a **trait object**, written `dyn Trait` and always behind a pointer like `Box<dyn Trait>` or the `Arc<dyn Trait>` you met earlier. This is dynamic dispatch, the same mechanism as a Java interface reference:

```rust
trait Greeter {
    fn greet(&self) -> String;
}
struct English;
impl Greeter for English {
    fn greet(&self) -> String { "hello".to_string() }
}

fn main() {
    let greeters: Vec<Box<dyn Greeter>> = vec![Box::new(English)];
    for g in &greeters {
        println!("{}", g.greet());
    }
}
```

#### `#[derive(...)]` — Macros That Write `impl` Blocks for You

A `#[derive(...)]` attribute is a **macro** that auto-generates a trait implementation at compile time, saving you the boilerplate. `#[derive(Debug)]` writes a `Debug` impl (so `{:?}` can print the value); `#[derive(Clone)]` writes `Clone`; `#[derive(PartialEq)]` writes `==`:

```rust
#[derive(Debug, Clone, PartialEq)]
struct Money {
    cents: i64,
}

fn main() {
    let a = Money { cents: 500 };
    let b = a.clone();     // Clone derived
    assert_eq!(a, b);      // PartialEq derived
    println!("{a:?}");     // Debug derived -> Money { cents: 500 }
}
```

This is exactly the machinery behind Firefly's own macros. When a later chapter writes `#[derive(Command)]` on a struct, the framework's `Command` derive macro generates the wiring that lets that struct be dispatched through the CQRS `Bus` — you write the data, the macro writes the plumbing. The `#[rest_controller]`, `#[event_listener]`, and `#[saga]` attributes you will meet are the same idea: code that writes code at compile time.

### Generics and Lifetimes — Just Enough

**Generics** let one piece of code work over many types, written with `<T>` type parameters and bounded by the traits the code needs. You have already used generic *types* — `Vec<T>`, `Option<T>`, `Mono<T>`. Here is a generic *function*; the bound `T: PartialOrd + Copy` means "any `T` you can compare and copy":

```rust
fn largest<T: PartialOrd + Copy>(items: &[T]) -> T {
    let mut max = items[0];
    for &x in items {
        if x > max { max = x; }
    }
    max
}

fn main() {
    println!("{}", largest(&[3, 7, 2]));      // works on i32
    println!("{}", largest(&[1.0, 9.5]));     // and on f64
}
```

**Lifetimes** are the part of the type system that tracks *how long a reference is valid*, so the compiler can prove a borrow never outlives the data it points to. They are written with a leading apostrophe, like `'a`, and most of the time the compiler infers them and you never write one. When you do see one — for instance on a function that returns a reference borrowed from its inputs — read `'a` as "lives at least as long as":

```rust
fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.len() >= b.len() { a } else { b }
}

fn main() {
    println!("{}", longest("wallet", "ledger"));   // -> "wallet"
}
```

> **Note** — you do not need to *write* generics or lifetimes to read this book; you need to not be alarmed by `<T>` and `'a` when they appear in a signature. `'static` is the one named lifetime worth remembering: it means "valid for the entire run of the program," and you will see it as a bound (`T: Send + 'static`) on values handed to async tasks.

### Closures

A **closure** is an anonymous function written with `|params| body`. It can capture variables from the surrounding scope. Closures are everywhere in reactive pipelines — every `.map(...)`, `.filter(...)`, and step callback takes one:

```rust
fn main() {
    let add_one = |x: i32| x + 1;       // a closure bound to a name
    println!("{}", add_one(41));         // 42

    let xs = vec![1, 2, 3, 4];
    let evens: Vec<i32> = xs.iter()
        .copied()
        .filter(|x| x % 2 == 0)          // a closure passed to filter
        .collect();
    println!("{evens:?}");               // [2, 4]
}
```

By default a closure *borrows* what it captures. Prefix it with `move` to make it *take ownership* of its captures instead — essential when the closure outlives the current scope, as when it is shipped to another thread or an async task:

```rust
fn main() {
    let label = String::from("amount");
    let print_it = move || println!("{label}");  // `move`: closure now owns `label`
    print_it();
}
```

> **Spring parity.** A closure is Rust's lambda — `|x| x + 1` is Java's `x -> x + 1` or Python's `lambda x: x + 1`. The wrinkle is capture: `move` is how you say "capture by value," which you reach for constantly when handing work to `tokio` tasks, because the task may run after the spawning function has returned.

### Async, `await`, and `tokio`

Everything in Firefly is **asynchronous**. An `async fn` does not run when you call it; instead it returns a **`Future`** — a lazy description of work that produces a value later. Nothing happens until that future is *driven*, which you do with `.await`. Awaiting a future suspends the current task until the value is ready, freeing the thread to do other work in the meantime — that is how a handful of threads can serve thousands of concurrent connections.

```rust,ignore
async fn fetch_balance(id: u64) -> i64 {   // returns a Future<Output = i64>
    // ... awaits a database, an HTTP call, etc. ...
    id as i64 * 100
}

async fn report(id: u64) {
    let bal = fetch_balance(id).await;      // suspend until the value is ready
    println!("balance: {bal}");
}
```

Futures need a **runtime** to drive them. Firefly uses **`tokio`**, the de-facto async runtime for Rust. You rarely touch it directly, but you will see its attribute macros mark the entry points: `#[tokio::main]` turns an `async fn main` into a real program, and `#[tokio::test]` does the same for an async test:

```rust
async fn fetch_balance(id: u64) -> i64 {
    id as i64 * 100
}

#[tokio::main]
async fn main() {
    let bal = fetch_balance(7).await;
    println!("balance: {bal}");   // balance: 700
}
```

> **Spring parity.** If `Mono`/`Flux` and reactive streams are familiar to you, you already have the right intuition. Rust's `Future` is the lower-level primitive that an `async fn` produces, and `tokio` is the scheduler that runs them — think of it as the event loop. Firefly layers its own reactive types, **`Mono<T>`** (0-or-1 value) and **`Flux<T>`** (0..N values), *on top of* this `async`/`await` foundation: a `Mono` is the reactive analog of "an async function that returns a `T`," and a `Flux` of "an async stream of `T`." They are lazy, composable, and backpressure-aware, and they are the keystone of the framework. The Reactive Model chapter is where you meet them in full.

### Reading the Rest of the Book

That is the whole toolkit. With ownership and borrowing, `Option`/`Result` and `?`, traits and `#[derive]`, closures, and `async`/`await` in hand, every later listing is readable.

A few reminders that hold for the entire book:

- **The listings are real.** Every snippet from Chapter 1 onward is lifted from the runnable Lumen companion crate in `samples/lumen`; the build breaks if a listing drifts from the source. What you read is what compiles and runs.
- **One import does most of the work.** Listings begin with `use firefly::prelude::*;`, which brings in the whole high-frequency surface — `Mono`, `Flux`, `Bus`, and the macros — through Lumen's single `firefly` dependency.
- **A snippet marked `ignore` elides setup for focus.** When a listing omits surrounding code to keep your eye on one idea, it is flagged as such; the API names, types, and signatures in it are exactly what the crates expose.

Turn to [Conventions](./00-conventions.md) for the typographic details, then begin the Lumen journey in [Why Firefly for Rust](../01-why-firefly.md).
