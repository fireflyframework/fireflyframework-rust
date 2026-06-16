# El modelo reactivo — Mono y Flux

Este es el capítulo angular. `firefly-reactive` es el **núcleo reactivo** de
Firefly, de calidad de producción: dos publishers perezosos, componibles y
conscientes de la contrapresión — `Mono` (0 o 1 valor) y `Flux` (0..N valores) —
construidos de forma nativa sobre Tokio. Cada superficie reactiva del framework
se construye a partir de estos dos tipos: endpoints HTTP reactivos, repositorios
reactivos, el `WebClient` reactivo y las caras reactivas de EDA y CQRS. Nada de
lo que hay aquí requiere infraestructura — cada ejemplo se ejecuta en el propio
proceso — de modo que puedes teclear cada uno en un test de prueba y verlo pasar.

En este capítulo no aterriza ningún archivo fuente de Lumen. En su lugar
construyes el *vocabulario* sobre el que Lumen se apoya por partida doble más
adelante: el `Flux<WalletEvent>` que hay detrás de su endpoint NDJSON / SSE de
*streaming de los eventos de un wallet* (activado en
[Producción y despliegue](./20-production.md)), y el `Mono<R>` perezoso que
devuelven `Bus::send_mono` / `Bus::query_mono` para que un comando de wallet se
componga en un pipeline reactivo. Lee esto antes de los capítulos de construcción
de servicios; todo lo posterior asume que sabes leer de un vistazo un pipeline
`Mono`/`Flux`.

Al terminar este capítulo, serás capaz de:

- Explicar qué es un *publisher reactivo*, por qué `Mono` y `Flux` son
  **perezosos** y por qué su canal de error está fijado a un único tipo.
- Construir, transformar, combinar y ejecutar pipelines sobre ambos publishers, y
  leer el `Result<Option<T>, _>` que devuelve `.block().await`.
- Recuperarte de errores con `on_error_*` / `retry_backoff`, y empujar valores de
  forma imperativa a un `Flux` con un `FluxSink`.
- Mover trabajo entre hilos con un `Scheduler` (`subscribe_on` / `publish_on`).
- Convertir un `Mono`/`Flux` en una respuesta HTTP con los responders reactivos de
  Firefly (`MonoJson`, `NdJson`, `Sse`), y rastrear cómo los usa el endpoint de
  streaming de Lumen.
- Ver cómo esos mismos dos tipos atraviesan el `WebClient` reactivo, los
  repositorios, EDA y el bus de CQRS.

## Conceptos que conocerás

Antes del primer pipeline, aquí tienes las ideas sobre las que se apoya este
capítulo. Cada una se reintroduce en contexto allí donde se usa por primera vez;
esta es la versión breve.

> **Note** **Término clave — publisher reactivo.** Un *publisher* es un valor que
> *describe* una computación que produce datos a lo largo del tiempo, sin
> ejecutarla todavía. Encadenas operadores sobre él para construir un pipeline y
> luego te *suscribes* para hacerlo ejecutar. El análogo en Spring es un
> `Publisher` de Project Reactor (`Mono` / `Flux`), el motor que hay detrás de
> Spring WebFlux. El `Mono` y el `Flux` de Firefly son la escritura en Rust de
> exactamente esos.

> **Note** **Término clave — perezoso (lazy).** Un publisher es *perezoso* cuando
> construir el pipeline no realiza ningún trabajo; el trabajo se ejecuta solo
> cuando te suscribes, bloqueas o haces await. Esto es lo contrario de un `Future`
> ejecutado con avidez que arranca en el momento en que se crea en algunos
> runtimes — un `Mono` al que nunca te suscribes nunca se ejecuta.

> **Note** **Término clave — contrapresión (backpressure).** La *contrapresión*
> es el mecanismo por el que un consumidor lento estrangula a un productor rápido
> para que los datos no se acumulen en memoria. Un `Flux` respeta la contrapresión
> de extremo a extremo: un cliente HTTP lento que consume un cuerpo `NdJson` en
> streaming realmente ralentiza al productor que lo alimenta, en lugar de
> bufferizar todo el stream por adelantado.

## Paso 1 — Conoce los dos publishers

El núcleo reactivo de Firefly son dos tipos, distinguidos por su **cardinalidad**
— cuántos valores pueden emitir:

- **`Mono<T>`** — un productor de *como mucho un* valor (0 o 1, más un error
  terminal). El análogo reactivo de «una función async que devuelve un `T`».
- **`Flux<T>`** — un productor de *0..N* valores más una finalización-o-error
  terminal. El análogo reactivo de «un stream async de `T`».

Ambos son **perezosos**: construir un pipeline no hace nada; el trabajo se ejecuta
solo cuando te suscribes, bloqueas o haces await. Ambos son `Send + 'static`, de
modo que un `Mono` o un `Flux` encaja directamente en un handler de axum sin
ningún envoltorio.

> **Note** **Término clave — señal terminal.** Un pipeline termina con exactamente
> una *señal terminal*: un `Flux` completa tras su último valor (o sin valores), y
> cualquiera de los dos publishers puede terminar antes de tiempo con un
> **error**. En `firefly-reactive` el tipo de error está fijado a
> `firefly_kernel::FireflyError`. Fijar el error mantiene ergonómica la superficie
> de operadores — no hay un parámetro de tipo de error que haya que enhebrar por
> cada `map` — y se conecta directamente con las respuestas de problema RFC 9457
> del framework, de modo que un pipeline fallido se convierte gratis en un cuerpo
> `application/problem+json`.

Teclea lo siguiente en un test (`#[tokio::test] async fn`) para ver ambas formas
ejecutarse hasta completarse:

```rust
use firefly_reactive::{Flux, Mono};

# async fn ex() {
// Mono: one value, lazily transformed, then awaited.
let n = Mono::just(20)
    .map(|x| x + 1)
    .filter(|x| *x > 10)
    .default_if_empty(0)
    .block()
    .await
    .unwrap();
assert_eq!(n, Some(21));

// Flux: a stream of values, filtered + mapped, collected to a Vec.
let xs = Flux::range(1, 5)
    .filter(|x| x % 2 == 1)
    .map(|x| x * 10)
    .collect_list()
    .block()
    .await
    .unwrap()   // Result -> Option
    .unwrap();  // Option -> Vec (collect_list always yields a list)
assert_eq!(xs, vec![10, 30, 50]);
# }
```

Qué acaba de pasar, bloque a bloque:

- El pipeline del `Mono` empieza con `Mono::just(20)`, luego hace `map`, `filter`
  y aporta un `default_if_empty(0)` por si el filtro rechazara el valor. Nada de
  eso se ejecutó hasta `.block().await`. El resultado es `Some(21)`: sobrevivió un
  valor.
- El pipeline del `Flux` recorre `1..=5`, conserva los números impares, multiplica
  cada uno por diez, y `collect_list` pliega todo el stream en un único `Vec`.
  Como `collect_list` devuelve un `Mono<Vec<T>>`, ejecutarlo produce
  `Ok(Some(vec))`.

> **Warning** `Mono::block()` es `async`: pese al nombre, nunca aparca un worker de
> Tokio. Resuelve el publisher en el sitio y devuelve
> `Result<Option<T>, FireflyError>`, de modo que `.block().await` es la forma
> idiomática de ejecutar un pipeline hasta completarlo. Las dos capas que devuelve
> son deliberadas — el `Result` exterior es éxito-o-error, el `Option` interior es
> valor-o-vacío.

> **Tip** **Punto de control.** Mete ambos fragmentos en un `#[tokio::test]` y
> ejecuta `cargo test`. Los dos `assert_eq!` pasan: `n == Some(21)` y
> `xs == vec![10, 30, 50]`. Has ejecutado tus primeros pipelines perezosos — y has
> visto la forma `Result<Option<T>, _>` que `.block().await` devuelve siempre.

### Leer el tipo de retorno

Todo lo que ejecuta un `Mono` hasta completarlo devuelve
`Result<Option<T>, FireflyError>`. Las tres capas cargan cada una con un hecho, y
leerlas es una habilidad que usarás en todos los capítulos posteriores:

| Resultado              | Qué significa                                            |
|------------------------|----------------------------------------------------------|
| `Ok(Some(v))`          | el pipeline produjo el valor `v`                         |
| `Ok(None)`             | el pipeline completó **vacío** (`Mono::empty`, un `filter` que rechazó todo) |
| `Err(FireflyError)`    | el pipeline alcanzó un **error terminal** y cortocircuitó |

Un operador terminal de `Flux` (`collect_list`, `reduce`, `count`, …) devuelve un
`Mono`, así que sigue la misma regla — razón por la que
`collect_list().block().await` desenvuelve dos veces en el ejemplo de arriba.

## Paso 2 — Crear publishers

Un pipeline empieza en un *constructor*. Echarás mano de un puñado constantemente;
el resto están ahí para cuando un caso límite los necesite.

Constructores de `Mono`:

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 250" role="img"
     aria-label="Reactive streams: a Mono of T emits at most one item then completes; a Flux of T emits zero or more items then completes; both can short-circuit on a terminal error"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="36.0" y="44.0" text-anchor="start" font-size="15" font-weight="800" fill="#b5531f" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Mono&lt;T&gt;</text>
<text x="36.0" y="62.0" text-anchor="start" font-size="11" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">0 or 1 item, then complete</text>
<line x1="150" y1="58" x2="500" y2="58" stroke="#7a6450" stroke-width="2"/>
<polygon points="510,58 500,53 500,63" fill="#7a6450"/>
<circle cx="250" cy="58" r="13" fill="#f6a821" stroke="#d4793a" stroke-width="1.5"/>
<line x1="430" y1="50" x2="430" y2="66" stroke="#1f8a4c" stroke-width="3"/>
<text x="250.0" y="86.0" text-anchor="middle" font-size="10" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">just(v)</text>
<text x="430.0" y="86.0" text-anchor="middle" font-size="10" font-weight="600" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">complete</text>
<line x1="24" y1="120" x2="536" y2="120" stroke="#e0cda8" stroke-width="1" stroke-dasharray="4 4"/>
<text x="36.0" y="162.0" text-anchor="start" font-size="15" font-weight="800" fill="#b5531f" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Flux&lt;T&gt;</text>
<text x="36.0" y="180.0" text-anchor="start" font-size="11" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">0..N items, then complete</text>
<line x1="150" y1="176" x2="500" y2="176" stroke="#7a6450" stroke-width="2"/>
<polygon points="510,176 500,171 500,181" fill="#7a6450"/>
<circle cx="200" cy="176" r="11" fill="#f6a821" stroke="#b5531f" stroke-width="1.2"/>
<circle cx="256" cy="176" r="10" fill="#d4793a" stroke="#b5531f" stroke-width="1.2"/>
<circle cx="312" cy="176" r="9" fill="#f6a821" stroke="#b5531f" stroke-width="1.2"/>
<circle cx="368" cy="176" r="8" fill="#d4793a" stroke="#b5531f" stroke-width="1.2"/>
<circle cx="424" cy="176" r="7" fill="#f6a821" stroke="#b5531f" stroke-width="1.2"/>
<line x1="492" y1="168" x2="492" y2="184" stroke="#1f8a4c" stroke-width="3"/>
<text x="228.0" y="206.0" text-anchor="middle" font-size="10" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">map · filter · flat_map</text>
</svg>
<figcaption>Los dos tipos de retorno reactivos. Un <code>Mono&lt;T&gt;</code> emite como mucho un elemento (<code>Ok(Some)</code>), o ninguno (<code>Ok(None)</code>), y luego completa; un <code>Flux&lt;T&gt;</code> emite un stream de cero o más elementos. Ambos cortocircuitan ante un <code>Err(FireflyError)</code> terminal.</figcaption>
</figure>

| Constructor                       | Produce                                                  |
|-----------------------------------|---------------------------------------------------------|
| `Mono::just(v)`                   | exactamente `v`                                         |
| `Mono::just_or_empty(opt)`        | `v` si es `Some`, vacío si es `None`                    |
| `Mono::empty()`                   | completa sin valor (`Ok(None)`)                        |
| `Mono::error(e)`                  | un error terminal                                      |
| `Mono::from_future(fut)`          | hace await de un `Future<Output = T>`                  |
| `Mono::from_result_future(fut)`   | hace await de un `Future<Output = Result<T, FireflyError>>` |
| `Mono::from_callable(f)`          | ejecuta un `FnOnce() -> Result<Option<T>, FireflyError>` al suscribir |
| `Mono::defer(factory)`            | construye el `Mono` de nuevo por cada suscripción       |

Constructores de `Flux`:

| Constructor                       | Produce                                        |
|-----------------------------------|------------------------------------------------|
| `Flux::just(vec)`                 | cada elemento del `Vec`                        |
| `Flux::from_iter(iter)`           | cada elemento de un iterador                   |
| `Flux::range(start, count)`       | `start, start+1, …` (count elementos)          |
| `Flux::empty()` / `Flux::never()` | completa de inmediato / nunca emite            |
| `Flux::error(e)`                  | un error terminal                              |
| `Flux::from_stream(s)`            | un `Stream<Item = Result<T, FireflyError>>`    |
| `Flux::from_value_stream(s)`      | un `Stream<Item = T>`                          |
| `Flux::create(producer)`          | push imperativo mediante un `FluxSink` (Paso 5) |
| `Flux::interval(period)`          | `0, 1, 2, …` sobre un temporizador             |
| `Flux::generate(seed, step)`      | generación con estado                          |

Qué acaba de pasar: `Mono::just` / `Flux::just` son los constructores literales
que más usarás. `from_future` / `from_result_future` son el puente desde el Rust
`async` hacia el mundo reactivo — el mismo puente que el bus de CQRS usa
internamente para envolver un dispatch en un `Mono`. `defer` y `from_callable`
importan para el **retry**, porque construyen el trabajo *de nuevo en cada
suscripción* (Paso 4).

> **Note** **Término clave — publisher frío (cold).** Todos estos son *fríos*: el
> trabajo se rehace para cada suscriptor, comenzando en el momento de la
> suscripción, como llamar a una función de nuevo. (El opuesto, un publisher
> *caliente* o *hot*, comparte una única fuente en ejecución entre los
> suscriptores — `Mono::cache` convierte un `Mono` frío en uno que recuerda su
> resultado.) El ser frío por defecto es lo que hace posible el `retry`: un retry
> es simplemente otra suscripción.

## Paso 3 — Transformar, combinar y terminar

`Mono` y `Flux` comparten la mayoría de los nombres de operadores; las diferencias
reflejan la cardinalidad. Este es el conjunto de trabajo — tenlo a mano, no lo
memorizarás de una sola lectura:

| Categoría    | Mono                                                                 | Flux                                                                                       |
|-------------|----------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| transformar | `map` `map_async` `flat_map` `flat_map_many` `filter`                | `map` `map_async` `flat_map(n)` `concat_map` `filter` `scan` `index` `flat_map_iterable`    |
| reduce/term | `then` `then_return` `zip_with`                                       | `reduce` `collect_list` `collect_map` `count` `all` `any` `then` `last` `next` `single` `element_at` |
| limit/slice | —                                                                    | `take` `take_while` `take_last` `skip` `skip_while` `distinct` `distinct_until_changed`      |
| combinar    | `when` `zip`                                                          | `merge_with` `concat_with` `zip_with` `combine_latest` `start_with` `switch_if_empty` `default_if_empty` |
| error       | `on_error_return` `on_error_resume` `on_error_map` `retry` `retry_backoff` | `on_error_resume` `on_error_continue` `retry` `retry_backoff`                          |
| tiempo      | `timeout` `delay_element`                                            | `timeout` `delay_elements` `sample` `debounce` `interval`                                   |
| backpressure| —                                                                   | `on_backpressure_buffer` `on_backpressure_drop` `on_backpressure_latest` `limit_rate`        |
| window      | —                                                                   | `buffer` `window` `group_by`                                                                 |
| side-effect | `do_on_next` `do_on_success` `do_on_error` `do_on_finally`           | `do_on_next` `do_on_complete` `do_on_error` `do_on_finally`                                  |
| schedule    | `subscribe_on` `publish_on`                                          | `subscribe_on` `publish_on`                                                                  |
| cache/view  | `cache` `as_flux`                                                    | —                                                                                           |

La única distinción que vale la pena interiorizar ahora es `map` frente a
`flat_map`. `map` transforma cada valor con una función corriente (`T -> U`).
`flat_map` transforma cada valor en *otro publisher* y aplana el resultado — así
es como encadenas un paso reactivo dependiente sobre uno anterior.

```rust
use firefly_reactive::{Flux, Mono};

# async fn ex() {
// flat_map: chain a Mono onto the result of another (a sequential dependency).
let total = Mono::just(3)
    .flat_map(|seed| Mono::just(seed * 10))
    .map(|x| x + 1)
    .block()
    .await
    .unwrap();
assert_eq!(total, Some(31));

// flat_map on a Flux runs up to N inner publishers concurrently; the first
// argument is that concurrency bound.
let doubled = Flux::range(1, 3)
    .flat_map(2, |n| Mono::just(n * 2).as_flux())
    .collect_list()
    .block()
    .await
    .unwrap()
    .unwrap();
assert_eq!(doubled.len(), 3);
# }
```

Qué acaba de pasar:

- En el `Mono`, `flat_map(|seed| Mono::just(seed * 10))` toma el `3`, produce un
  `Mono` nuevo (`30`) y lo aplana de modo que el siguiente `map` ve `30`. Esta es
  la escritura reactiva de «haz A, luego usa el resultado de A para hacer B».
- En el `Flux`, `flat_map(2, ..)` es la misma idea desplegada en abanico: cada uno
  de los tres valores de origen se convierte en un publisher interno, y hasta
  **2** de ellos se ejecutan a la vez. `.as_flux()` eleva el `Mono` interno a un
  `Flux` para que las firmas cuadren.

Para ejecutar dos pipelines independientes y combinar sus resultados, usa `zip`
(la función libre) — ambos se ejecutan, y luego sus salidas se emparejan en una
tupla:

```rust
use firefly_reactive::{zip, Mono};

# async fn ex() {
// zip two Monos into a tuple — both run, then combine.
let pair = zip(Mono::just("alice"), Mono::just(42))
    .block()
    .await
    .unwrap();
assert_eq!(pair, Some(("alice", 42)));
# }
```

> **Tip** **Punto de control.** Ejecuta los tres fragmentos en un test. Deberías
> ver `total == Some(31)`, `doubled.len() == 3` y `pair == Some(("alice", 42))`.
> Si echas mano de `flat_map` en un `Flux` y el compilador se queja de los
> argumentos, recuerda que la forma de Flux toma primero el límite de
> concurrencia.

## Paso 4 — Manejar errores y reintentar

Un elemento `Err` es **terminal** en un `Flux`: cada operador cortocircuita en el
primer error y lo propaga aguas abajo — no hay canal de error por elemento. Una
vez que se dispara un error, no fluye ningún valor posterior. Para recuperarte,
eliges un operador de recuperación:

- `Mono::on_error_return(fallback)` — sustituye un valor.
- `Mono::on_error_resume(f)` / `Flux::on_error_resume(f)` — cambia a un publisher
  de respaldo, conservando los elementos emitidos antes del error.
- `Flux::on_error_continue(handler)` — descarta el elemento que falla y conserva
  el resto (para operadores que reseñalan por elemento).
- `Mono::on_error_map(f)` — traduce el error a un `FireflyError` distinto.

> **Note** **Término clave — factory de retry.** `retry` y `retry_backoff` no
> pueden reejecutar un publisher existente, porque un stream o un future de Rust es
> *de un solo uso* — una vez consumido, desaparece. Por eso toman un **closure
> factory** que construye el publisher *de nuevo* para cada intento. Cada retry es
> una suscripción totalmente nueva a un publisher totalmente nuevo. El análogo en
> Spring es el `Retry.backoff(..)` de Reactor.

`Backoff::new(max_retries, base_delay)` describe el calendario. Aquí una fuente
inestable falla sus dos primeros intentos y tiene éxito en el tercero:

```rust
use std::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use firefly_reactive::{Backoff, Mono};
use firefly_kernel::FireflyError;

# async fn ex() {
let calls = Arc::new(AtomicUsize::new(0));
let c = calls.clone();
let value = Mono::retry_backoff(
    move || {
        let c = c.clone();
        Mono::from_callable(move || {
            let n = c.fetch_add(1, Ordering::SeqCst);
            if n < 2 { Err(FireflyError::internal("flaky")) } else { Ok(Some(n)) }
        })
    },
    Backoff::new(5, Duration::from_millis(10)),
)
.block()
.await
.unwrap();
assert_eq!(value, Some(2));
# }
```

Qué acaba de pasar: el `move || { … }` exterior es la factory — `retry_backoff` la
llama una vez por intento. Dentro, `Mono::from_callable` ejecuta el trabajo falible
cuando se suscribe. El `AtomicUsize` compartido cuenta los intentos: las llamadas
`0` y `1` devuelven `Err`, así que `retry_backoff` espera (10 ms, y luego un
backoff creciente) y se vuelve a suscribir; la llamada `2` devuelve `Ok(Some(2))`,
que se convierte en el resultado. El tope `Backoff::new(5, …)` significa que se
rendiría tras cinco reintentos.

Los plazos también son errores. `Mono::timeout` / `Flux::timeout` mapean un plazo
incumplido a un `FireflyError` 504 (código `REACTIVE_TIMEOUT`), que se renderiza
como una respuesta de problema RFC 9457 — la misma ruta de respuesta que cualquier
otro error terminal.

> **Tip** **Punto de control.** Ejecuta el fragmento de retry. Afirma
> `value == Some(2)`: la fuente falló dos veces y la tercera suscripción tuvo
> éxito. Cambia el umbral de `n < 2` a `n < 9` y observa cómo el pipeline agota sus
> cinco reintentos y aflora el `Err` en su lugar.

## Paso 5 — Emitir de forma imperativa con `FluxSink`

Los constructores del Paso 2 cubren las fuentes declarativas. Cuando los valores
llegan desde un callback, un canal o un bucle imperativo, empújalos a un `Flux`
con `Flux::create` y un `FluxSink`.

> **Note** **Término clave — `FluxSink`.** Un `FluxSink` es el handle de push que
> se te entrega dentro de `Flux::create`. Llama a `sink.next(v)` para emitir un
> valor, `sink.error(e)` para terminar con un error y `sink.complete()` para
> finalizar el stream. Es el análogo en Rust del `FluxSink` de Reactor de
> `Flux.create(..)`.

```rust
use firefly_reactive::Flux;

# async fn ex() {
let flux = Flux::create(|sink| {
    for i in 1..=3 {
        sink.next(i);
    }
    sink.complete();
});
let out = flux.collect_list().block().await.unwrap();
assert_eq!(out, Some(vec![1, 2, 3]));
# }
```

Qué acaba de pasar: `Flux::create` entrega a tu closure un `sink`. El bucle emite
`1, 2, 3` con `sink.next`, luego `sink.complete()` cierra el stream para que
`collect_list` sepa que ha terminado. Así es como adaptas un productor no reactivo
—por ejemplo, un cursor de base de datos o un SDK basado en callbacks— a un `Flux`
sin reescribirlo.

> **Tip** **Punto de control.** El test afirma `out == Some(vec![1, 2, 3])`.
> Olvida el `sink.complete()` y el stream nunca termina — `collect_list` esperaría
> para siempre. Con `create`, la finalización es responsabilidad tuya.

## Paso 6 — Mover trabajo entre hilos con un `Scheduler`

Por defecto, un pipeline se ejecuta allí donde lo suscribiste. Un `Scheduler` te
permite mover el trabajo a un contexto de ejecución distinto —el pool de workers de
Tokio, un pool de bloqueo o en línea— sin reestructurar el pipeline.

> **Note** **Término clave — `Scheduler`.** Un `Scheduler` decide *dónde* se
> ejecuta el trabajo. `Scheduler::Immediate` se ejecuta en línea sobre la tarea
> actual (sin salto); `Scheduler::Parallel` se ejecuta sobre el pool de workers de
> Tokio, para trabajo limitado por CPU; `Scheduler::BoundedElastic` ejecuta
> llamadas bloqueantes en un pool aparte para que nunca dejen sin recursos al pool
> de workers. Estos reflejan los `Schedulers.immediate()`, `.parallel()` y
> `.boundedElastic()` de Reactor.

Dos operadores aplican un scheduler. `subscribe_on` salta la **fuente** a un
scheduler:

```rust
use firefly_reactive::{Flux, Scheduler};

# async fn ex() {
let out = Flux::range(1, 3)
    .subscribe_on(Scheduler::Parallel)   // run the source on the Tokio worker pool
    .map(|x| x * 2)
    .collect_list()
    .block()
    .await
    .unwrap();
assert_eq!(out, Some(vec![2, 4, 6]));
# }
```

`publish_on` cambia el hilo para todo lo que está **aguas abajo** de él, de modo
que el punto de descarga puede situarse en cualquier lugar de la cadena — una
fuente barata puede saltar a un hilo de worker justo antes de un `map` costoso:

```rust
use firefly_reactive::{Flux, Scheduler};

# async fn ex() {
let out = Flux::range(1, 3)
    .map(|x| x + 1)                   // runs wherever the subscribe happens
    .publish_on(Scheduler::Parallel)  // everything below hops to the worker pool
    .map(|x| x * 10)                  // runs on the Tokio worker pool
    .collect_list()
    .block()
    .await
    .unwrap();
assert_eq!(out, Some(vec![20, 30, 40]));
# }
```

Qué acaba de pasar: `subscribe_on` eligió dónde se ejecuta la *fuente* (toda la
cadena de arriba la siguió hasta `Parallel`); `publish_on` partió la cadena en dos
— el primer `map` se ejecutó en el sitio de la suscripción, el segundo se ejecutó
en el pool de workers. La regla práctica: echa mano de `subscribe_on` para situar
una *fuente* bloqueante o limitada por CPU, y de `publish_on` para descargar una
etapa *aguas abajo* costosa.

> **Tip** **Punto de control.** Ambos fragmentos afirman sus resultados
> recolectados (`[2, 4, 6]` y `[20, 30, 40]`). Los valores son idénticos a
> ejecutar sin un scheduler — los schedulers cambian *dónde* se ejecuta el trabajo,
> nunca *qué* computa.

## Paso 7 — Convertir un publisher en una respuesta HTTP

Aquí es donde el núcleo reactivo se encuentra con la capa web, y donde Lumen lo
usará. `firefly-web` incluye responders que convierten un `Mono`/`Flux` en una
respuesta de axum: un handler reactivo simplemente devuelve uno de ellos, y el
responder conduce el publisher y escribe la respuesta. Usan el formato de cable
estable de `firefly-sse`, de modo que cualquier cliente que hable NDJSON o SSE los
consume directamente.

| Responder                | Comportamiento                                            |
|--------------------------|-----------------------------------------------------------|
| `MonoJson(Mono<T>)`      | `Ok(Some)` → 200 JSON; `Ok(None)` → 404 problem+json; `Err` → la respuesta RFC 9457 de ese error |
| `NdJson(Flux<T>)`        | `application/x-ndjson`, un elemento por línea, con contrapresión |
| `Sse(Flux<T>)`           | `text/event-stream`, un frame `data:` por elemento        |
| `SseEvents(Flux<Event>)` | `text/event-stream` con control completo de `id` / `event` / `retry` |

```rust,no_run
use axum::{routing::get, response::IntoResponse, Router};
use firefly_reactive::{Flux, Mono};
use firefly_web::{MonoJson, NdJson, Sse};

async fn one_order() -> impl IntoResponse {
    // Ok(Some) -> 200 application/json; Ok(None) -> 404 problem+json;
    // Err -> that error's problem response.
    MonoJson(Mono::just(serde_json::json!({ "id": "o1" })))
}

async fn stream_orders() -> impl IntoResponse {
    // application/x-ndjson, one line per element, backpressured.
    NdJson(Flux::just(vec![1, 2, 3]))
}

async fn live_orders() -> impl IntoResponse {
    // text/event-stream, one `data:` frame per element.
    Sse(Flux::just(vec![1, 2, 3]))
}

let app: Router = Router::new()
    .route("/orders/one", get(one_order))
    .route("/orders", get(stream_orders))
    .route("/orders/live", get(live_orders));
```

Qué acaba de pasar, responder a responder:

- **`MonoJson(Mono<T>)`** resuelve el `Mono`: `Ok(Some)` → `200`
  `application/json`; `Ok(None)` → `404` `application/problem+json`; `Err` → la
  respuesta de problema de ese error. El `Mono` vacío convirtiéndose en un 404
  limpio es exactamente el `Result<Option<T>, _>` de tres capas del Paso 1 mapeado
  sobre HTTP.
- **`NdJson(Flux<T>)`** transmite `application/x-ndjson` — un documento JSON
  compacto más `'\n'` por elemento, vaciado de forma incremental con contrapresión
  real. El `Stream` del `Flux` se enlaza directamente con un cuerpo de streaming de
  axum; el stream completo **nunca** se bufferiza. Un elemento `Err` a mitad del
  stream termina el cuerpo de forma limpia.
- **`Sse(Flux<T>)`** transmite `text/event-stream` — cada elemento serializado en
  un frame `data: <json>\n\n` pelado, idéntico byte a byte al writer de
  `firefly-sse`.
- **`SseEvents(Flux<Event>)`** transmite valores `firefly_sse::Event` preconstruidos
  — úsalo cuando necesites control sobre los campos `id` / `event` / `retry`.

> **Warning** Aquí la contrapresión es real, no cosmética. Un cliente lento
> estrangula al productor; nada se bufferiza por adelantado. Esto es lo que permite
> a un endpoint `NdJson` transmitir un millón de filas sin que la respuesta aterrice
> nunca por completo en memoria.

### Cómo lo usa Lumen

El endpoint opcional `GET /api/v1/wallets/:id/events` de Lumen tiene exactamente
esta forma. Reproduce el stream de eventos persistidos de un wallet como un
`Flux<WalletEvent>` y se lo entrega a `NdJson` (o a `Sse` con `?format=sse`). El
handler completo —tomado verbatim de `samples/lumen/src/web.rs`, protegido por la
feature `streaming`— son los responders de arriba aplicados al dominio de wallets:

```rust,ignore
// samples/lumen/src/web.rs — the reactive streaming handler (feature `streaming`).
#[cfg(feature = "streaming")]
async fn stream_events(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<StreamParams>,
) -> Response {
    use crate::domain::WalletEvent;
    use axum::response::IntoResponse;
    use firefly::reactive::Flux;
    use firefly::web::{NdJson, Sse};

    // `load_events` returns `Err(NotFound)` for an absent wallet, so the 404 is
    // decided before the streaming response head is committed.
    let events = match api.ledger.load_events(&id).await {
        Ok(events) => events,
        Err(e) => return WebError::from(domain_to_web(e)).into_response(),
    };
    let items: Vec<WalletEvent> = events.iter().map(WalletEvent::from_domain).collect();
    let flux = Flux::just(items);
    if params.format.as_deref() == Some("sse") {
        Sse(flux).into_response()
    } else {
        NdJson(flux).into_response()
    }
}
```

Dos detalles que vale la pena llevarse. Primero, la decisión de *no encontrado*
ocurre **antes** de construir el `Flux`, de modo que un 404 sigue renderizándose
como una respuesta de problema limpia en vez de un stream entreabierto. Segundo,
Lumen alcanza los tipos reactivos a través de la fachada de una sola dependencia —
`firefly::reactive::Flux` y `firefly::web::{NdJson, Sse}`, nunca los crates
subyacentes `firefly-reactive` / `firefly-web`. El endpoint completo, incluido el
cableado de rutas, vuelve en [Producción y despliegue](./20-production.md).

> **Note** A lo largo del resto del libro, Lumen alcanza los tipos reactivos a
> través de la fachada — `firefly::reactive::*` para `Mono`/`Flux` y
> `firefly::web::*` para los responders. Los ejemplos de *este* capítulo importan
> `firefly_reactive` / `firefly_web` directamente para que cada fragmento se
> sostenga por sí solo, pero las dos rutas nombran tipos idénticos:
> `firefly::reactive` reexporta `firefly_reactive`, y `firefly::web` reexporta
> `firefly_web`.

## Paso 8 — Rastrea esos mismos dos tipos por el resto del framework

`Mono` y `Flux` no son una comodidad solo para la web; son la columna vertebral
de la que cuelga todo el framework. Encontrarás cada uno de estos en su propio
capítulo, pero ver el hilo conductor ahora hace que esos capítulos encajen.

**El `WebClient` reactivo.** El cliente HTTP reactivo de Firefly devuelve sus
operadores terminales como `Mono` / `Flux`, de modo que una llamada saliente entra
directamente en un pipeline reactivo y se compone de extremo a extremo con los
responders `NdJson` / `Sse` de arriba. Tratamiento completo en
[Clientes HTTP](./13-http-clients.md); la forma:

```rust,no_run
use firefly_client::WebClientBuilder;
use serde::Deserialize;

#[derive(Deserialize)]
struct Order { id: String }
#[derive(Deserialize)]
struct Tick { seq: u64 }

# async fn ex() {
let client = WebClientBuilder::new("https://api.example.com").build();

// body_to_mono — the whole body decoded as one T.
let _order: firefly_reactive::Mono<Order> =
    client.get().uri("/orders/o1").retrieve().body_to_mono::<Order>();

// body_to_flux — a streamed NDJSON/SSE body decoded element-by-element,
// lazily and with backpressure.
let _ticks: firefly_reactive::Flux<Tick> = client
    .get()
    .uri("/ticks")
    .header("Accept", "application/x-ndjson")
    .retrieve()
    .body_to_flux::<Tick>();
# }
```

> **Note** El cliente **no** trae retry incorporado. Compón `Mono::retry` /
> `Mono::retry_backoff` (Paso 4) sobre el publisher devuelto, de modo que la
> política de retry viva en el sitio de la llamada, que es donde corresponde, en
> lugar de oculta dentro del cliente.

**Repositorios.** `ReactiveCrudRepository<T, ID>` devuelve `Mono`/`Flux`; los
adaptadores SQL transmiten las filas fuera de `find_all()` como un `Flux` para que
una tabla enorme nunca aterrice por completo en memoria. Véase
[Persistencia](./07-persistence.md).

**EDA.** `InMemoryBroker::subscribe_reactive(topic)` produce un `Flux<Event>`
(dentro de un `EdaResult`), y `publish_mono(event)` es una publicación reactiva fría
que devuelve `Mono<()>`. El ledger de Lumen publica cada evento de wallet en un
`Broker`; véase [EDA](./10-eda-messaging.md).

**CQRS.** `Bus::send_mono` / `Bus::query_mono` envuelven el dispatch en un
`Mono<R>` perezoso, ejecutando *la misma* búsqueda de handler y cadena de
middleware que el `Bus::send` síncrono. Los comandos de wallet de Lumen viajan por
este bus; véase [CQRS](./09-cqrs.md). Un aperitivo — la forma que toma una consulta
`GetWallet` compuesta reactivamente (ambos métodos toman `&Arc<Bus>` para que el
`Mono` perezoso pueda ser dueño del bus):

```rust,ignore
use std::sync::Arc;
use firefly::cqrs::Bus;

// `send_mono` / `query_mono` take `&Arc<Bus>` so the lazy Mono can own the bus.
let bus: Arc<Bus> = /* the WebStack's bus */;
let balance = bus
    .query_mono::<_, WalletView>(GetWallet { id: wallet_id })
    .map(|view| view.balance)
    .block()
    .await?;            // Ok(Some(<cents>))
```

> **Note** Como `firefly-reactive` fija su canal de error a `FireflyError`, un
> dispatch fallido se mapea desde el `CqrsError` del bus a un `FireflyError` fiel
> al estado (validación → 422, autorización → 403, handler ausente → 500) con el
> error original preservado como `source()` — de modo que un comando reactivo fluye
> directamente hacia la pila de problemas RFC 9457 sin traducción adicional.

## Paso 9 — Interoperar con `Stream` / `Future` en crudo

Los tipos reactivos no son un jardín amurallado. Convierte hacia dentro y hacia
fuera en los bordes para que un `Mono`/`Flux` pueda envolver (o ser envuelto por)
Rust async corriente:

- **Hacia dentro:** `Flux::from_stream` (un `Stream<Item = Result<T, FireflyError>>`),
  `Flux::from_value_stream` (un `Stream<Item = T>`), `Mono::from_future`,
  `Mono::from_result_future`.
- **Hacia fuera:** `Flux::to_stream` / `Flux::into_stream`, `Mono::into_future` (o
  simplemente haz `.await` del `Mono` directamente — un `Mono<T>` es en sí mismo
  awaitable).

Qué acaba de pasar: estas son las costuras que te permiten adoptar el núcleo
reactivo de forma incremental. Un `Stream` existente se convierte en un `Flux` al
que puedes aplicar operadores de contrapresión y recuperación; un `Mono` se
convierte en un `Future` corriente en el momento en que alguna otra API quiere uno.

## Resumen

Ya manejas el vocabulario sobre el que se construye el resto del libro:

- **Dos publishers, por cardinalidad.** `Mono<T>` produce 0 o 1 valor; `Flux<T>`
  produce 0..N. Ambos son **perezosos** y **fríos**: nada se ejecuta hasta que te
  suscribes, bloqueas o haces await, y cada suscripción rehace el trabajo.
- **Un único canal de error fijo.** Cada error terminal es un
  `firefly_kernel::FireflyError`, razón por la que los pipelines se conectan
  directamente con las respuestas de problema RFC 9457 sin fontanería de tipos de
  error.
- **`.block().await` devuelve `Result<Option<T>, FireflyError>`** — éxito/error en
  el exterior, valor/vacío en el interior. Un operador terminal de `Flux` devuelve
  un `Mono`, así que se lee igual.
- **La recuperación es explícita.** `on_error_return` / `on_error_resume` /
  `on_error_continue` / `on_error_map` recuperan; `retry` / `retry_backoff` toman
  una **factory** porque los publishers son de un solo uso; `timeout` mapea un
  plazo a un `FireflyError` 504.
- **`Flux::create` + `FluxSink`** empujan valores de forma imperativa; un
  `Scheduler` (`subscribe_on` / `publish_on`) mueve el trabajo entre en línea, el
  pool de workers y el pool de bloqueo.
- **Los responders web** `MonoJson`, `NdJson`, `Sse` y `SseEvents` convierten un
  publisher en una respuesta HTTP, con contrapresión real en los de streaming.
- **Esos mismos dos tipos atraviesan todo** — el `WebClient` reactivo,
  `ReactiveCrudRepository`, el broker de EDA y `Bus::send_mono` / `query_mono`.

Qué significa esto para Lumen: en este capítulo no aterrizó ningún archivo fuente,
pero Lumen ya tiene los dos publishers a partir de los cuales se construye cada
superficie reactiva que toca — el `Flux<WalletEvent>` que hay detrás de su endpoint
de streaming, y el `Mono<R>` que hay detrás de su bus de comandos/consultas.

## Ejercicios

1. **Mapea un saldo.** Construye `Mono::just(1_250_i64)` (un saldo en céntimos),
   `map`éalo a un `f64` en unidad mayor (`cents as f64 / 100.0`) y hazle
   `block().await`. Confirma que obtienes `Some(12.5)`.

2. **Transmite eventos de wallet como un `Flux`.** Crea un `Vec<i64>` de deltas de
   saldo con signo (`[1000, 50, -25]`), envuélvelo con `Flux::just`, haz `scan` de
   un saldo acumulado y `collect_list`. Verifica que los saldos acumulados son
   `[1000, 1050, 1025]` — una versión hecha a mano de lo que el endpoint de
   streaming de Lumen emite por evento.

3. **Recupérate de una fuente inestable.** Escribe un `Mono::from_callable` que
   devuelva `Err(FireflyError::internal("flaky"))` las dos primeras veces y
   `Ok(Some(n))` después, luego envuélvelo en `Mono::retry_backoff(factory,
   Backoff::new(5, Duration::from_millis(10)))`. Afirma que se resuelve a un valor —
   el patrón de retry-factory que el cliente HTTP de Lumen usaría contra un
   proveedor externo de FX.

4. **Elige un responder.** Dado un `Flux<WalletEvent>`, decide qué responder quiere
   un dashboard en tiempo real (`Sse`) frente a una exportación masiva (`NdJson`),
   y explica en una frase por qué la contrapresión importa en el caso de la
   exportación.

5. **Empuja y luego completa.** Usa `Flux::create` para emitir `1..=5` con
   `sink.next`, pero *omite* `sink.complete()`. Ejecútalo bajo un `Mono::timeout`
   de unos pocos cientos de milisegundos y observa el `FireflyError` 504 — luego
   añade el `complete()` y míralo pasar limpiamente. Por esto la finalización es
   responsabilidad tuya con `create`.

## Adónde ir después

- Pon estos publishers a trabajar detrás de rutas reales en
  **[Tu primera API HTTP](./06-first-http-api.md)** — el primer capítulo que
  devuelve un `Mono`/`Flux` desde un handler de Lumen.
- Ve cómo `Flux` transmite filas fuera de la base de datos en
  **[Persistencia](./07-persistence.md)** mediante `ReactiveCrudRepository`.
- Compón `Bus::send_mono` / `Bus::query_mono` en pipelines de wallet en
  **[CQRS](./09-cqrs.md)**.
- Suscríbete a un `Flux<Event>` y haz `publish_mono` de eventos de wallet en
  **[EDA y mensajería](./10-eda-messaging.md)**.
