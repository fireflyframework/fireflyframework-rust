## Convenciones

Esta página explica las convenciones tipográficas y estructurales que se usan a lo largo del libro, y demuestra cada una con un ejemplo real, de modo que la primera vez que te encuentres con un aviso o con un pie de código en un capítulo ya te resulte familiar.

### Listados de código

Cada ejemplo de código de varias líneas es Rust real y compilable, extraído del crate complementario Lumen. Cuando resulta útil, un listado se introduce con el **fichero en el que vive** para que puedas localizarlo en `samples/lumen`, como en "`samples/lumen/src/money.rs`". Las referencias a código en línea dentro de la prosa usan `monospace`, como en "el atributo `#[rest_controller]` genera el router del monedero."

He aquí un listado representativo: el constructor y el núcleo de aritmética exacta del objeto de valor `Money` de Lumen, copiado literalmente de `samples/lumen/src/money.rs`:

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

Un fragmento anotado con `rust,ignore` o `rust,no_run` omite la configuración circundante para ganar foco, pero los nombres de la API, los tipos y las firmas de los métodos son exactamente lo que exponen los crates. Un listado delimitado como `text` simple es salida de la shell, un banner o un intercambio HTTP en lugar de código fuente Rust:

```text
$ cargo run -p firefly-sample-lumen
:: lumen :: digital-wallet & ledger (v26.6.24)
```

### El recordatorio de la dependencia única

Como la propiedad que define a Lumen es su única dependencia de Firefly, cada tipo del framework que veas se alcanza a través de la fachada — `firefly::cqrs::Bus`, `firefly::eventsourcing::EventStore`, `firefly::reactive::Flux` — o, para la superficie de alta frecuencia y cada macro, a través de un único glob:

```rust
use firefly::prelude::*;
```

Cuando un capítulo introduce un tipo nuevo del framework, la prosa nombra la ruta de la fachada tras la que vive, de modo que siempre sepas que llegó a través de esa única dependencia.

### Avisos

A lo largo del cuerpo del texto aparecen cuatro estilos de aviso. Cada uno es una cita en bloque que se abre con una etiqueta en negrita, y el tema de diseño les da un estilo diferenciado:

> **Note.** Las notas aportan contexto complementario o aclaran una sutileza del texto principal. Vale la pena leerlas, pero no son bloqueantes.

> **Tip.** Los consejos comparten un atajo, un idiom o una buena práctica que te ahorrará tiempo en proyectos reales; por ejemplo, mantener el dinero en céntimos enteros para que la deriva de coma flotante nunca pueda corromper un saldo.

> **Warning.** Las advertencias señalan un error común o una arista afilada que provoca problemas difíciles de depurar si se ignora; por ejemplo, que los manejadores CQRS de función libre de Lumen publican a sus colaboradores a través de un `OnceLock` global del proceso, de modo que un segundo arranque de `build_router()` en el mismo binario de prueba conserva el *primer* cableado.

> **Design note.** Los avisos de nota de diseño explican *por qué* Firefly hace algo de una manera concreta y señalan dónde una idea te resultará familiar si antes has usado un framework opinionado con todo incluido o una biblioteca de reactive-streams. Son orientación, planteada como las propias decisiones de diseño de Firefly, no una tabla de traducción para otro framework. Te encontrarás con ellos en casi todos los capítulos.

### Tablas de referencia

Cuando un capítulo introduce una familia de APIs relacionadas, una tabla de referencia las reúne en un solo lugar para que puedas asimilar toda la superficie de un vistazo:

| Atributo declarativo | Lo que genera |
|---|---|
| `#[rest_controller]` | un router de axum a partir de los métodos manejadores anotados |
| `#[event_listener]` | una suscripción al broker ligada a un tipo de evento |
| `#[scheduled]` | una tarea registrada en el planificador |
| `#[saga]` / `Step` | una transacción distribuida orquestada y compensable |

### Resumen y ejercicios

Cada capítulo se cierra con dos secciones fijas:

- Un **Resumen — qué cambió en Lumen** que enumera los ficheros añadidos o ampliados y la recompensa de una sola frase: "al terminar este capítulo, Lumen puede …".
- Un conjunto de **Ejercicios** que dan un paso más allá: por lo general, una extensión pequeña y autocontenida del código que el capítulo acaba de entregar. Son opcionales pero recomendables para cualquier cosa que pienses aplicar de inmediato.

Pasa la página a [Por qué Firefly para Rust](../01-why-firefly.md), donde comienza el viaje de Lumen.
