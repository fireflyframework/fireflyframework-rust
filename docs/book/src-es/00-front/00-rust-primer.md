## Introducción a Rust

Este libro construye microservicios reactivos, asíncronos y con event sourcing en Rust. Los capítulos dan por hecho que sabes *leer* Rust con comodidad; esta introducción se asegura de que así sea, incluso si tu trabajo diario es Java/Spring o Python. No es un curso completo de Rust. Enseña exactamente la porción del lenguaje en la que se apoyan los capítulos posteriores, en el orden en que se apoyan en ella, de modo que cuando el Capítulo 6 te entregue un `Mono<WalletEvent>` o el Capítulo 10 te muestre `#[derive(Command)]`, ninguna de las sintaxis te sorprenda.

Si ya has desarrollado en Rust con `async`/`await`, traits y ownership, salta hacia delante: el Capítulo 1 es donde empieza Lumen. Si vienes de un lenguaje con recolección de basura, lee esto una vez, escribe los fragmentos a mano y mantenlo como marcador.

> **Note** — cada bloque de código de esta introducción se compiló y ejecutó antes de imprimirse. Los fragmentos de programa completo se compilan y ejecutan tal y como se muestran; los pocos que omiten la configuración circundante están marcados y usan nombres reales y actuales de la API de Firefly.

### Cargo, crates, módulos y `use`

Un proyecto de Rust se construye con **Cargo**, el gestor de paquetes y herramienta de compilación, el equivalente aproximado de Maven/Gradle en el mundo Java o de pip/Poetry en Python. Una unidad de compilación es un **crate**: o bien un binario (una aplicación), o bien una biblioteca. Los crates de los que depende tu proyecto se listan en un fichero de manifiesto llamado `Cargo.toml`:

```toml
[package]
name = "lumen"
version = "0.1.0"
edition = "2021"

[dependencies]
firefly = { version = "26.6.24" }                       # the whole framework
tokio   = { version = "1", features = ["full"] }       # the async runtime
serde   = { version = "1", features = ["derive"] }      # (de)serialization
```

Cada entrada bajo `[dependencies]` es un crate obtenido de crates.io (el registro de paquetes de Rust, como Maven Central o PyPI). La lista `features` activa partes opcionales de un crate: un interruptor en tiempo de compilación, no una opción en tiempo de ejecución.

Los comandos del día a día:

```bash
cargo build      # compile the project
cargo run        # compile, then run the binary
cargo test       # compile and run the tests
cargo check      # type-check without producing a binary (fast)
```

Dentro de un crate, el código se organiza en **módulos**: espacios de nombres que agrupan elementos relacionados. Accedes al contenido de un módulo mediante una ruta de segmentos separados por `::`, y la palabra clave `use` trae una ruta al ámbito actual para que puedas referirte a ella por su nombre corto:

```rust
use std::sync::Arc;          // bring `Arc` into scope from the standard library
use std::collections::HashMap;

fn main() {
    let map: HashMap<String, i64> = HashMap::new();   // now just `HashMap`
    let _shared = Arc::new(map);
}
```

> **Spring parity.** `use std::sync::Arc;` es el `import java.util.concurrent.atomic.*;` de Rust. Un crate es un artefacto Maven; un módulo es un paquete Java; `Cargo.toml` es tu `pom.xml`. La gran diferencia es que no hay classpath en tiempo de ejecución: todo se resuelve y enlaza en tiempo de compilación en un único binario.

A lo largo del libro, todo el framework llega mediante una única importación glob que verás al principio de casi todos los listados:

```rust,ignore
use firefly::prelude::*;
```

Esa única línea trae `Mono`, `Flux`, `Bus`, las macros y el resto de la superficie de uso frecuente. Cuando un capítulo introduce un nuevo tipo del framework, la prosa nombra la ruta de fachada tras la que vive (`firefly::cqrs::Bus`, `firefly::eventsourcing::EventStore`), de modo que siempre sabes que entró a través de esa única dependencia.

### Variables y tipos básicos

Las variables se declaran con `let`. Son **inmutables por defecto**; añade `mut` para que una vinculación pueda reasignarse. Los tipos suelen inferirse, pero puedes anotarlos tras dos puntos:

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

Los tipos numéricos explicitan su tamaño y signo: `i32`/`i64` (con signo), `u32`/`u64` (sin signo), `usize` (del tamaño de un puntero, usado para longitudes e índices), `f32`/`f64` (en coma flotante). Lumen guarda el dinero en céntimos `i64`, nunca en `f64`, de modo que el redondeo nunca puede corromper un saldo.

> **Spring parity.** `let` declara una variable local como el `var` de Java, pero la inmutabilidad es la opción por defecto, lo contrario de Java, donde debes escribir `final` para optar *por* ella. En Rust escribes `mut` para optar *en contra* de la inmutabilidad. Este valor por defecto es estructural: el compilador lo usa para razonar sobre quién tiene permiso para cambiar qué.

#### `String` frente a `&str`

Esta distinción hace tropezar a todos los recién llegados, así que vamos a conocerla pronto. Hay dos tipos de cadena:

- **`String`** — una cadena en propiedad, asignada en el heap y ampliable. *Posees* el búfer; cuando la `String` sale de ámbito, su memoria se libera.
- **`&str`** — una *vista prestada* (borrowed) de datos de cadena que no posees (una "porción de cadena", o "string slice"). Los literales de cadena como `"wallet"` son `&str`.

El idioma habitual: **acepta `&str` como parámetro y devuelve `String` cuando produces un nuevo valor en propiedad.** Un `&str` puede tomar prestado de una `String`, de un literal o de cualquier sitio, así que aceptar `&str` hace que una función sea lo más flexible posible.

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

### Ownership, préstamos y referencias

Este es el concepto que hace que Rust sea *Rust*. Es también el que no tiene equivalente en Java ni en Python, así que lee esta sección despacio.

Cada valor tiene exactamente un **owner** (propietario): la variable responsable de liberarlo. Cuando el propietario sale de ámbito, el valor se descarta (su memoria y recursos se liberan). No hay ningún recolector de basura que decida *cuándo* ocurre eso; ocurre de forma determinista, al final del ámbito del propietario.

Cuando asignas un valor a otra variable o lo pasas a una función, la propiedad **se mueve** (move). La vinculación original ya no puede usarse:

```rust,ignore
let a = String::from("Lumen");
let b = a;            // ownership MOVES from `a` to `b`
println!("{a}");     // COMPILE ERROR: value borrowed here after move
```

Si quieres *usar* un valor sin tomar su propiedad, lo tomas prestado (**borrow**) con una referencia. Una referencia se escribe `&T` (un préstamo compartido/inmutable) o `&mut T` (un préstamo exclusivo/mutable). El préstamo permite a una función leer o modificar un valor y después devolverlo:

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

El compilador impone una regla, la invariante central del **borrow checker**: en cualquier momento puedes tener **o bien** cualquier número de referencias compartidas (`&T`) **o bien** exactamente una referencia exclusiva (`&mut T`) a un valor, nunca ambas. Esa única regla es lo que hace imposibles las condiciones de carrera de datos: nunca puedes tener un hilo leyendo mientras otro escribe, porque el sistema de tipos no permitirá que coexistan dos de tales referencias.

> **Spring parity.** En Java o Python, los objetos viven en el heap, cada variable es una referencia a ellos y un recolector de basura los reclama en algún momento posterior impredecible. El aliasing es libre y sin verificar: dos hilos pueden sostener alegremente el mismo `ArrayList` y corromperlo. Rust cambia esa libertad por garantías: la propiedad se rastrea en tiempo de compilación, la memoria se libera en el instante en que termina el ámbito del propietario (sin pausas del GC, sin `finalize`), y las reglas de préstamo convierten las condiciones de carrera de datos en un *error de compilación* en lugar de un incidente de producción a las 2 de la madrugada. La frase que oirás es "concurrencia sin miedo" (fearless concurrency): eso es lo que significa.

> **Tip** — cuando un fragmento hace `Arc::clone(&repo)` o `&self` en lugar de mover un valor, está tomando prestado para no ceder la propiedad. Leer "¿esto es un move o un borrow?" a partir del `&` es la mayor parte de lo que hace falta para seguir el resto del libro.

### `Arc<T>` — propiedad compartida entre hilos

A veces un único propietario no basta: un servicio, un repositorio o un objeto de configuración pueden necesitar compartirse entre muchas tareas que se ejecutan de forma concurrente. `Arc<T>` — *Atomically Reference-Counted* (recuento de referencias atómico) — es la respuesta, y lo verás en casi todas las páginas de este libro. Un `Arc<T>` es un manejador compartido y seguro entre hilos a un `T`; clonarlo es barato (incrementa un recuento de referencias, no copia el `T`), y el `T` se descarta solo cuando desaparece el último manejador `Arc`.

```rust
use std::sync::Arc;

fn main() {
    let config = Arc::new(String::from("lumen-config"));
    let handle2 = Arc::clone(&config);   // a second handle to the SAME value
    println!("{config} / {handle2}");    // both point at one allocation
}
```

El framework se apoya en esto con una forma concreta: **`Arc<dyn Trait>`**, un manejador compartido a *algún* tipo que implementa un trait, donde el tipo concreto queda oculto tras la interfaz. Así es como Firefly hace circular servicios y "puertos" hexagonales, de modo que un handler de wallet puede depender de un `WalletRepository` sin saber si está respaldado por Postgres, MongoDB o un mapa en memoria:

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

`Arc` comparte acceso de *lectura*. Para compartir estado *mutable*, envuelve el valor interno en un cerrojo (lock): `Mutex<T>` (un único accesor a la vez) o `RwLock<T>` (muchos lectores o un único escritor), dando lugar al familiar `Arc<Mutex<T>>` / `Arc<RwLock<T>>`. El cerrojo impone la regla del "único escritor" en tiempo de ejecución para datos que el borrow checker no puede rastrear entre hilos:

```rust
use std::sync::{Arc, Mutex};

fn main() {
    let counter = Arc::new(Mutex::new(0_u64));
    *counter.lock().unwrap() += 1;   // lock, mutate, unlock at end of statement
    println!("{}", *counter.lock().unwrap());
}
```

### Structs, enums y `match`

Un **struct** agrupa campos con nombre: tus registros de datos y objetos de valor. Los métodos se definen en un bloque `impl`; `&self` toma prestado el receptor (lectura), `&mut self` lo toma prestado de forma exclusiva (mutación), y un método sin `self` es una función asociada (el "método estático" de Rust), usada comúnmente para constructores:

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

Un **enum** es mucho más potente que un enum de Java: cada variante puede llevar sus propios datos. Los enums modelan "una de varias formas", que es exactamente como el libro representa comandos, eventos y estados:

```rust
enum Command {
    Open { id: u64 },                 // a struct-like variant
    Deposit { id: u64, cents: i64 },  // with named fields
    Close,                            // a unit variant, no data
}
```

Descompones un enum con `match`: un switch exhaustivo y con valor de expresión. **Exhaustivo** significa que el compilador se niega a compilar a menos que gestiones cada variante, de modo que añadir un nuevo comando más adelante te obliga a actualizar cada lugar que haga match sobre él:

```rust,ignore
fn describe(cmd: &Command) -> String {
    match cmd {
        Command::Open { id }            => format!("open wallet {id}"),
        Command::Deposit { id, cents }  => format!("deposit {cents} into {id}"),
        Command::Close                  => "close".to_string(),
    }
}
```

`match` es una forma de **coincidencia de patrones** (pattern matching), que también aparece en la desestructuración con `let`, en `if let` y en los parámetros de función. Leerás patrones constantemente; reconocer que el lado izquierdo de un `=>` *vincula nombres desestructurando el valor* es la clave.

### `Option<T>`, `Result<T, E>` y el operador `?`

Rust no tiene `null` ni excepciones. La ausencia y el fallo son valores ordinarios, expresados mediante dos enums estándar.

**`Option<T>`** dice "quizá un `T`": es o bien `Some(value)` o bien `None`. Esto sustituye a `null`/`None`/`nil`, y como es un tipo distinto, no puedes usar accidentalmente un valor ausente como si estuviera presente.

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

**`Result<T, E>`** dice "un `T` o un error `E`": es o bien `Ok(value)` o bien `Err(error)`. Así es como cada operación que puede fallar informa del fallo; no hay excepciones lanzadas que capturar:

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

Gestionar cada `Result` a mano sería tedioso, así que Rust te ofrece el **operador `?`**. Aplicado a un `Result`, `?` desenvuelve el valor `Ok` o, en caso de `Err`, *devuelve ese error de la función actual de inmediato*. Propaga los fallos hacia arriba por la pila de llamadas sin excepciones y sin código repetitivo:

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

> **Spring parity.** Donde Java lanza y captura, y Python eleva (raise) y captura (except), Rust *devuelve* los errores como valores y los propaga con `?`. La firma de una función te dice de antemano si puede fallar (`-> Result<T, E>`) y con qué; el fallo forma parte del tipo, no es una ruta de control de flujo oculta. Firefly fija el tipo de error de sus capas reactiva y web a un único `FireflyError`, de modo que el operador `?` propaga los fallos directamente a través de un pipeline y los saca como una respuesta de problema según RFC 9457.

### Traits e `impl` — las interfaces de Rust

Un **trait** define comportamiento compartido: un conjunto de firmas de método que un tipo puede prometer proporcionar. Es lo más parecido que tiene Rust a una interfaz de Java o a un protocolo/ABC de Python. Implementas un trait para un tipo con `impl Trait for Type`, y un trait puede aportar cuerpos de método por defecto:

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

Cuando quieres un valor cuyo tipo concreto se decide en tiempo de ejecución ("cualquier `Greeter`, me da igual cuál"), usas un **trait object** (objeto de trait), escrito `dyn Trait` y siempre tras un puntero como `Box<dyn Trait>` o el `Arc<dyn Trait>` que conociste antes. Esto es despacho dinámico, el mismo mecanismo que una referencia a interfaz de Java:

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

#### `#[derive(...)]` — macros que escriben los bloques `impl` por ti

Un atributo `#[derive(...)]` es una **macro** que autogenera una implementación de trait en tiempo de compilación, ahorrándote el código repetitivo. `#[derive(Debug)]` escribe una impl de `Debug` (para que `{:?}` pueda imprimir el valor); `#[derive(Clone)]` escribe `Clone`; `#[derive(PartialEq)]` escribe `==`:

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

Esta es exactamente la maquinaria que hay detrás de las propias macros de Firefly. Cuando un capítulo posterior escribe `#[derive(Command)]` sobre un struct, la macro derive `Command` del framework genera el cableado que permite que ese struct se despache a través del `Bus` de CQRS: tú escribes los datos, la macro escribe la fontanería. Los atributos `#[rest_controller]`, `#[event_listener]` y `#[saga]` que conocerás son la misma idea: código que escribe código en tiempo de compilación.

### Genéricos y lifetimes — lo justo

Los **genéricos** permiten que una pieza de código funcione sobre muchos tipos, escritos con parámetros de tipo `<T>` y acotados por los traits que el código necesita. Ya has usado *tipos* genéricos: `Vec<T>`, `Option<T>`, `Mono<T>`. Aquí tienes una *función* genérica; la cota `T: PartialOrd + Copy` significa "cualquier `T` que puedas comparar y copiar":

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

Los **lifetimes** (tiempos de vida) son la parte del sistema de tipos que rastrea *cuánto tiempo es válida una referencia*, de modo que el compilador pueda demostrar que un préstamo nunca sobrevive a los datos a los que apunta. Se escriben con un apóstrofo inicial, como `'a`, y la mayoría de las veces el compilador los infiere y nunca escribes uno. Cuando sí ves uno —por ejemplo en una función que devuelve una referencia tomada prestada de sus entradas—, lee `'a` como "vive al menos tanto como":

```rust
fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.len() >= b.len() { a } else { b }
}

fn main() {
    println!("{}", longest("wallet", "ledger"));   // -> "wallet"
}
```

> **Note** — no necesitas *escribir* genéricos ni lifetimes para leer este libro; necesitas no alarmarte por `<T>` ni por `'a` cuando aparezcan en una firma. `'static` es el único lifetime con nombre que merece la pena recordar: significa "válido durante toda la ejecución del programa", y lo verás como una cota (`T: Send + 'static`) en valores que se entregan a tareas asíncronas.

### Closures

Un **closure** (cierre) es una función anónima escrita con `|params| body`. Puede capturar variables del ámbito circundante. Los closures están por todas partes en los pipelines reactivos: cada `.map(...)`, `.filter(...)` y callback de paso recibe uno:

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

Por defecto, un closure *toma prestado* lo que captura. Prefíjalo con `move` para que *tome la propiedad* de sus capturas en su lugar, algo esencial cuando el closure sobrevive al ámbito actual, como cuando se envía a otro hilo o a una tarea asíncrona:

```rust
fn main() {
    let label = String::from("amount");
    let print_it = move || println!("{label}");  // `move`: closure now owns `label`
    print_it();
}
```

> **Spring parity.** Un closure es la lambda de Rust: `|x| x + 1` es el `x -> x + 1` de Java o el `lambda x: x + 1` de Python. El matiz es la captura: `move` es como dices "captura por valor", algo a lo que recurres constantemente al entregar trabajo a tareas de `tokio`, porque la tarea puede ejecutarse después de que la función que la lanzó haya retornado.

### Async, `await` y `tokio`

Todo en Firefly es **asíncrono**. Una `async fn` no se ejecuta cuando la llamas; en su lugar devuelve un **`Future`**: una descripción perezosa (lazy) de un trabajo que produce un valor más tarde. No ocurre nada hasta que ese future se *conduce* (driven), lo cual haces con `.await`. Esperar un future con `await` suspende la tarea actual hasta que el valor está listo, liberando el hilo para hacer otro trabajo entretanto: así es como un puñado de hilos puede atender miles de conexiones concurrentes.

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

Los futures necesitan un **runtime** que los conduzca. Firefly usa **`tokio`**, el runtime asíncrono de facto para Rust. Rara vez lo tocas directamente, pero verás sus macros de atributo marcando los puntos de entrada: `#[tokio::main]` convierte una `async fn main` en un programa real, y `#[tokio::test]` hace lo mismo para un test asíncrono:

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

> **Spring parity.** Si `Mono`/`Flux` y los reactive streams te resultan familiares, ya tienes la intuición correcta. El `Future` de Rust es la primitiva de más bajo nivel que produce una `async fn`, y `tokio` es el planificador que los ejecuta; piensa en él como el bucle de eventos. Firefly superpone sus propios tipos reactivos, **`Mono<T>`** (0 o 1 valor) y **`Flux<T>`** (0..N valores), *encima de* esta base de `async`/`await`: un `Mono` es el análogo reactivo de "una función asíncrona que devuelve un `T`", y un `Flux`, el de "un flujo asíncrono de `T`". Son perezosos, componibles y conscientes de la contrapresión (backpressure), y son la piedra angular del framework. El capítulo del Modelo Reactivo es donde los conocerás en su totalidad.

### Cómo leer el resto del libro

Ese es todo el conjunto de herramientas. Con ownership y préstamos, `Option`/`Result` y `?`, traits y `#[derive]`, closures, y `async`/`await` a tu disposición, todo listado posterior es legible.

Unos cuantos recordatorios válidos para todo el libro:

- **Los listados son reales.** Cada fragmento a partir del Capítulo 1 está extraído del crate complementario ejecutable de Lumen en `samples/lumen`; la compilación se rompe si un listado se desvía del fuente. Lo que lees es lo que compila y se ejecuta.
- **Una sola importación hace casi todo el trabajo.** Los listados empiezan con `use firefly::prelude::*;`, que trae toda la superficie de uso frecuente —`Mono`, `Flux`, `Bus` y las macros— a través de la única dependencia `firefly` de Lumen.
- **Un fragmento marcado con `ignore` omite la configuración por enfoque.** Cuando un listado omite el código circundante para mantener tu atención en una sola idea, se marca como tal; los nombres de la API, los tipos y las firmas que contiene son exactamente lo que exponen los crates.

Pasa a [Convenciones](./00-conventions.md) para los detalles tipográficos, y después comienza el viaje de Lumen en [Por qué Firefly para Rust](../01-why-firefly.md).
