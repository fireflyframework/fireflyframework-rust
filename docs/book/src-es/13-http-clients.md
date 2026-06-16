# Clientes HTTP

Hasta ahora, cada tramo de una transferencia de Lumen ha sido una llamada a un
método *local*: el paso de abono es `ledger.deposit(&req.to, ...)`, en proceso e
infalible salvo por las reglas de dominio que aplica. Es algo deliberado: Lumen
es autocontenido, y mantenerlo así permitió que los capítulos anteriores
enseñaran modelado de dominio, CQRS, event sourcing y sagas sin tener la red de
por medio. Este capítulo es el momento en que la red entra en escena. Es el
*siguiente adaptador que añadirías*: cuando una transferencia tiene que liquidarse
contra una pasarela de pagos real, el tramo de abono deja de ser un
`Ledger::deposit` local y se convierte en una llamada a un servicio externo de
**Payments**, una llamada que puede expirar por timeout, fallar a medias o
aterrizar en un host sobrecargado, modos de fallo que un método local nunca tuvo.

`firefly-client` te da un cliente *tipado* para esa llamada en lugar de una sesión
`reqwest` artesanal hilvanada con lógica de reintentos y timeouts. Conocerás tres
estilos de cliente —eager, reactivo y declarativo— que comparten un mismo conjunto
de automatismos, y luego verás cómo una **capa de experiencia** se sitúa delante
de Lumen y la compone con sus vecinos en una única API con forma de recorrido
(journey). Todo es accesible a través de la única fachada `firefly` de la que has
dependido desde [Quickstart](./02-quickstart.md).

Al terminar este capítulo, serás capaz de:

- Construir un `RestClient` eager con `RestBuilder`, llamarlo y decodificar un
  documento de problema upstream en un error tipado.
- Construir el `WebClient` reactivo y elegir entre sus terminales `body_to_mono` /
  `body_to_flux` / `exchange`, y saber por qué *no* lleva reintentos integrados.
- Escribir un trait declarativo `#[http_client]` y dejar que la macro genere la
  implementación que emite las peticiones, la imagen especular de un
  `#[rest_controller]`.
- Envolver una llamada saliente en un `CircuitBreaker` para que un upstream enfermo
  no pueda arrastrar a Lumen con él.
- Comprender el patrón de capa de experiencia (BFF) y la estricta dirección de
  dependencia `channel → experience → domain → core`.

## Conceptos que conocerás

Antes del primer cliente, aquí están las ideas en las que se apoya este capítulo.
Cada una se reintroduce en contexto allí donde se usa por primera vez; esta es la
versión breve.

> **Note** **Término clave — cliente HTTP.** Un *cliente* aquí es un objeto que tu
> servicio usa para hacer llamadas HTTP *salientes* a otro servicio. Es el inverso
> de un controlador, que *recibe* llamadas entrantes. Firefly incluye un cliente
> tipado para que la forma de la petición, la decodificación de la respuesta y el
> manejo de errores los compruebe el compilador, en lugar de dejarlos a una llamada
> `reqwest` cruda.

> **Note** **Término clave — documento de problema RFC 9457.** Un cuerpo de error
> JSON estándar (tipo de medio `application/problem+json`) que transporta los campos
> `type`, `title`, `status` y `detail`. RFC 9457 es el estándar actual (deja
> obsoleto al RFC 7807). Firefly los *produce* desde handlers que fallan y los
> *consume* en el lado del cliente, decodificando un problema upstream en un
> `FireflyError` tipado, de modo que un fallo externo arrastra el estado y el
> detalle del upstream directamente a través de la propia pila de errores de Lumen.

> **Note** **Término clave — id de correlación / contexto de traza.** Un *id de
> correlación* es un identificador por petición que viaja con ella para que sus
> líneas de log y las de cada servicio al que llama puedan unirse entre sí. El
> *contexto de traza* del W3C (cabeceras `traceparent` / `tracestate`) hace lo
> mismo para el trazado distribuido. Todos los clientes de Firefly reenvían ambos
> de forma automática, así que una petición que se ramifica hacia tres upstreams
> sigue siendo una única traza coherente.

> **Note** **Término clave — publicador reactivo (`Mono` / `Flux`).** Un `Mono<T>`
> es un valor asíncrono diferido que se resuelve en, como mucho, un `T`; un
> `Flux<T>` es un *flujo* asíncrono diferido de `T`. Los conociste en
> [El modelo reactivo](./05-reactive-model.md). El cliente reactivo los devuelve
> para que una llamada saliente caiga directamente en una tubería reactiva. El
> análogo en Spring es el `Mono` / `Flux` de Project Reactor.

> **Note** **Término clave — Backend-for-Frontend (BFF).** Una aplicación ligera
> del lado del servidor que agrega varios servicios de dominio en una única API con
> *forma de recorrido* adaptada a un frontend concreto, en lugar de hacer que el
> frontend llame a cada servicio y combine los resultados por su cuenta. Se trata
> en profundidad en [La capa de experiencia](./20a-experience-tier.md); aquí se
> introduce.

El crate publica sus clientes tras una única puerta de entrada, `firefly::client`,
y las piezas declarativas también se reexportan a través de `firefly::prelude`.
Los dos clientes HTTP comparten los mismos automatismos —`Accept` / `Content-Type`
por defecto, propagación del id de correlación y del contexto de traza W3C, y
decodificación de problemas RFC 9457 en un `FireflyError` tipado—:

- el **`RestClient` eager** (construido con `RestBuilder`): una `async fn` que hace
  `await` de un `Result`, con un presupuesto de reintentos integrado;
- el **`WebClient` reactivo** (construido con `WebClientBuilder`): cuyos operadores
  terminales devuelven `Mono` / `Flux`, de modo que una llamada saliente compone de
  extremo a extremo con una tubería reactiva.

Por encima del `WebClient` se sitúa el trait **declarativo `#[http_client]`** —el
análogo del `@HttpExchange` de Spring 6—, que escribes como un trait y dejas que la
macro implemente. El crate también incluye builders y andamiajes para clientes
GraphQL, SOAP, gRPC y WebSocket, seleccionados por feature para que las
dependencias pesadas no entren en servicios que no las usan.

> **Design note.** Ambos clientes HTTP son *valores construidos con un builder
> fluido*: no hay una interfaz anotada de la que generar para las superficies eager
> y reactiva, ni reflexión. Los decoradores de resiliencia (tratados cerca del
> final) envuelven la llamada desde fuera en lugar de venir integrados. Eso mantiene
> cada cliente pequeño y convierte la política de reintentos/corte de circuito en
> una propiedad del punto de llamada, no en un valor por defecto oculto.

## Paso 1 — Construir el `RestClient` eager

El cliente eager es el que conviene usar cuando solo quieres hacer `await` de un
resultado. Lo construyes con `RestBuilder`, configurando la URL base, las cabeceras
por defecto, un timeout por petición y un presupuesto de intentos, y luego llamas a
`request` con un método, una ruta y un cuerpo opcional.

Aquí Lumen construye el cliente de Payments al que llamaría desde el tramo de abono
de una transferencia:

```rust,no_run
use std::time::Duration;
use firefly::client::RestBuilder;
use http::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct SettleTransfer { wallet_id: String, amount: i64, reference: String }
#[derive(Deserialize)]
struct Payment { id: String, status: String }

#[tokio::main]
async fn main() {
    let payments = RestBuilder::new("https://payments.internal")
        .with_header("X-Tenant", "lumen")
        .with_timeout(Duration::from_secs(5))
        .with_retries(3)
        .build();

    let req = SettleTransfer {
        wallet_id: "wlt_alice".into(),
        amount: 300,
        reference: "transfer-42".into(),
    };
    match payments.request::<_, Payment>(Method::POST, "/payments", Some(&req)).await {
        Ok(payment) => println!("settled {} ({})", payment.id, payment.status),
        Err(err) => {
            // Upstream RFC 9457 problems are decoded into a typed FireflyError.
            if let Some(fe) = err.as_firefly() {
                eprintln!("payments upstream {}: {}", fe.status, fe.detail);
            }
        }
    }
}
```

Qué acaba de ocurrir, bloque a bloque:

- `RestBuilder::new("https://payments.internal")` prepara un builder en una URL base
  (las barras finales se recortan para que la concatenación `base + path` quede
  limpia).
- `.with_header("X-Tenant", "lumen")` fija una cabecera por defecto enviada en
  *cada* petición que hace este cliente.
- `.with_timeout(Duration::from_secs(5))` limita cada intento a cinco segundos.
- `.with_retries(3)` fija el *presupuesto total de intentos* a tres. Fíjate en que
  es el número de intentos, no de reintentos adicionales: `1` significa un intento
  sin reintento, y el cliente solo reintenta ante errores de red y estados
  `429` / `5xx`, con backoff exponencial (100 ms duplicándose por intento, con tope
  en 2 s).
- `.build()` finaliza el `RestClient`.
- `payments.request::<_, Payment>(Method::POST, "/payments", Some(&req))` envía la
  petición: el turbofish nombra el tipo del cuerpo (aquí inferido) y el tipo de
  respuesta `Payment`. Codifica el cuerpo en JSON, fija `Content-Type` /
  `Accept: application/json`, reenvía el id de correlación y el contexto de traza, y
  decodifica un cuerpo 2xx en `Payment`.

Por qué importa: una respuesta `application/problem+json` distinta de 2xx se
decodifica en un `FireflyError`, de modo que un fallo upstream arrastra el estado y
el detalle del upstream directamente a través de la propia pila de errores de Lumen.
`err.as_firefly()` es el accesor tipado que recupera el problema decodificado del
upstream.

> **Tip** **Punto de control.** Puedes llamar a `request::<_, T>(method, path, body)`
> y recibir de vuelta un `Result<T, ClientError>`. En la rama de error,
> `err.as_firefly()` devuelve `Some(&FireflyError)` siempre que el fallo fuera un
> error HTTP upstream (no un fallo de transporte / codificación / decodificación), y
> `fe.status` / `fe.detail` reflejan el problema del upstream.

### Ramificar según la clase de fallo

Rara vez quieres hacer match contra códigos de estado crudos. `ClientError` ofrece
helpers de predicado para que quien llama pueda ramificar según la *clase* de fallo:

```rust,ignore
match payments.request::<_, Payment>(Method::POST, "/payments", Some(&req)).await {
    Ok(payment) => { /* settled */ }
    Err(err) if err.is_unprocessable_entity() => { /* 422 — map onto Lumen's own 422 */ }
    Err(err) if err.is_retryable()            => { /* 429 / 5xx / transport — worth a retry */ }
    Err(err) => { /* everything else */ }
}
```

Los predicados reflejan la forma en que el framework renderiza problemas en otros
sitios: `is_validation()` (400), `is_unauthorized()` (401/403), `is_not_found()`
(404), `is_conflict()` (409), `is_unprocessable_entity()` (422),
`is_rate_limited()` (429), `is_server_error()` (5xx) e `is_retryable()` (la misma
regla que el cliente aplica internamente: fallos de transporte, `429` y cualquier
`5xx`).

> **Note** **Dónde encaja esto en la saga.** En [Sagas](./12-sagas.md) el tramo de
> abono era `ledger.deposit(&req.to, amount)`. En un despliegue partido eso pasa a
> ser `payments.request::<_, Payment>(Method::POST, "/payments", …)`. La *forma* de
> la saga no cambia —sigue siendo un `#[saga_step]` con el `compensate =
> "refund_debit"` del débito—, solo que el *cuerpo* del paso de abono ahora hace
> E/S a través de la red, que es justamente por lo que la compensación (devolver el
> débito) importa más que nunca.

## Paso 2 — Mantener la llamada de liquidación idempotente

La llamada de liquidación de una transferencia debe ser idempotente: si
`POST /payments` expira por timeout y el reintento de la saga la dispara de nuevo,
Payments no debe crear *dos* pagos. Lleva una `Idempotency-Key` estable —típicamente
el id de la transferencia— para que el upstream deduplique una petición reentregada.
Fíjala como cabecera por defecto en un builder por llamada:

```rust,ignore
let payments = RestBuilder::new("https://payments.internal")
    .with_header("Idempotency-Key", &transfer_id) // stable per business op
    .with_timeout(Duration::from_secs(2))
    .build();
```

Qué acaba de ocurrir: como la clave es una cabecera por defecto, *cada* intento que
hace este cliente —incluidos los reintentos que dispara el presupuesto— lleva la
misma clave. La deduplicación en sí es tarea del upstream; la tarea del cliente es
reenviar la clave de forma *consistente* a lo largo de los reintentos.

Por qué importa: este es el espejo saliente de la idempotencia entrante que
obtuviste gratis en [Quickstart](./02-quickstart.md); allí Lumen *registra* una
`Idempotency-Key` y reproduce la respuesta almacenada; aquí Lumen *envía* una para
que el servicio al que llama pueda hacer lo mismo.

> **Tip** **Punto de control.** Un `POST` reintentado que lleva una
> `Idempotency-Key` estable llega al upstream con la *misma* clave cada vez. Si fijas
> la clave por intento en lugar de por operación de negocio, la deduplicación se
> rompe: hazla una cabecera por defecto basada en el id de negocio (el id de la
> transferencia), no en el intento.

## Paso 3 — Construir el `WebClient` reactivo

El cliente reactivo devuelve `Mono` / `Flux`, de modo que una llamada saliente cae
directamente en una tubería reactiva y compone de extremo a extremo con los
responders `NdJson` / `Sse` ([El modelo reactivo](./05-reactive-model.md)) que usa
el endpoint de streaming de Lumen. Lo construyes con `WebClientBuilder`; la cadena
fluida de la petición se lee de arriba abajo: construir, direccionar, enviar,
decodificar:

```rust,no_run
use firefly::client::WebClientBuilder;
use http::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct SettleTransfer { wallet_id: String, amount: i64 }
#[derive(Deserialize)]
struct Payment { id: String }
#[derive(Deserialize)]
struct LedgerTick { seq: u64 }

#[tokio::main]
async fn main() {
    let client = WebClientBuilder::new("https://payments.internal")
        .with_header("X-Tenant", "lumen")
        .build();

    // body_to_mono — a single value -> Mono<Payment>.
    let _payment = client
        .method(Method::POST)
        .uri("/payments")
        .body(&SettleTransfer { wallet_id: "wlt_alice".into(), amount: 300 })
        .retrieve()
        .body_to_mono::<Payment>()
        .block()
        .await;

    // body_to_flux — a streamed NDJSON OR SSE body, decoded lazily
    // element-by-element with backpressure.
    let _ticks = client
        .get()
        .uri("/ledger/ticks")
        .header("Accept", "application/x-ndjson")
        .retrieve()
        .body_to_flux::<LedgerTick>()
        .collect_list()
        .block()
        .await;

    // exchange — raw status + headers without raising on a non-2xx.
    let _resp = client.get().uri("/health").retrieve().exchange().block().await;
}
```

Qué acaba de ocurrir, leyendo una cadena a la vez:

- `client.method(Method::POST)` (o los atajos `.get()` / `.post()` / `.put()` /
  `.delete()` / `.patch()`) inicia una petición; `.uri(...)` fija la ruta;
  `.body(&...)` codifica un cuerpo en JSON; `.retrieve()` finaliza la petición en una
  *especificación de respuesta*. Aún no ha ocurrido ninguna E/S: la petición se
  envía de forma perezosa cuando se suscribe el publicador devuelto.
- `.body_to_mono::<Payment>()` dice «decodifica todo el cuerpo como un único
  `Payment`» y produce un `Mono<Payment>`. `.block().await` se suscribe y espera,
  devolviendo `Result<Option<Payment>, FireflyError>`: el `Result` transporta
  cualquier error terminal y el `Option` modela un cuerpo vacío (`204`).
- `.body_to_flux::<LedgerTick>()` dice «decodifica este cuerpo en streaming elemento
  a elemento» y produce un `Flux<LedgerTick>`; `.collect_list()` lo reúne en un
  `Mono<Vec<LedgerTick>>`.
- `.exchange()` devuelve la respuesta cruda (estado + cabeceras + cuerpo) *sin* lanzar
  ante un no-2xx, como un `Mono<WebClientResponse>`.

> **Note** **Término clave — operador terminal.** Un *operador terminal* es el método
> que cierra la cadena fluida y decide la forma del resultado. En la especificación
> de respuesta de un `WebClient`, los tres terminales son:
>
> | Operador                | Devuelve                  | Comportamiento                                       |
> |-------------------------|---------------------------|------------------------------------------------------|
> | `body_to_mono::<T>()`   | `Mono<T>`                 | todo el cuerpo decodificado como un único `T`        |
> | `body_to_flux::<T>()`   | `Flux<T>`                 | un cuerpo NDJSON/SSE en streaming, elemento a elemento |
> | `exchange()`            | `Mono<WebClientResponse>` | el estado + cabeceras + cuerpo crudos, sin lanzar    |

> **Tip** **Punto de control.** Una cadena de `WebClient` que termina en
> `.body_to_mono::<T>()` te da un `Mono<T>` del que puedes hacer `.block().await`
> (devolviendo `Result<Option<T>, FireflyError>`) o componer más. Nada se dispara
> hasta que te suscribes: si construyes la cadena y nunca le haces block/await, no se
> envía ninguna petición.

## Paso 4 — Hacer streaming de una respuesta con `body_to_flux`

`body_to_flux` consume el flujo de bytes trozo a trozo y decodifica un elemento por
frame, de forma perezosa y con contrapresión (backpressure): un consumidor lento
estrangula al productor, y `.take(n)` deja de tirar antes de tiempo. El decodificador
se elige a partir del `Content-Type` de la respuesta:

- `application/x-ndjson` (y cualquier tipo no-SSE) → un documento JSON por línea
  terminada en salto de línea;
- `text/event-stream` → frames SSE separados por una línea en blanco; las líneas
  `data:` se concatenan y las líneas de comentario / `event:` / `id:` se ignoran.

Un elemento mal formado termina el flujo con un `FireflyError` de decodificación: el
primer error es terminal, el contrato de reactive-streams que respeta el `Flux` de
Firefly.

Por qué importa: este es el lado *consumidor* del mismo formato de cable que
*produce* el propio endpoint `GET /api/v1/wallets/:id/events` de Lumen (el endpoint
de `streaming` con feature gate). Un servicio hace streaming del log de eventos de la
wallet; otro lo lee de vuelta elemento a elemento, la simetría exacta que te compra
el modelo reactivo.

> **Tip** **Punto de control.** Apunta un `body_to_flux::<T>()` a un endpoint
> `application/x-ndjson` y hazle `.take(5)`; solo se tiran cinco elementos y el
> upstream deja de producir. Apúntalo a un endpoint `text/event-stream` y los frames
> SSE `data:` se decodifican igual: el tipo de contenido, no la llamada, elige el
> decodificador.

## Paso 5 — Inspeccionar la respuesta cruda con `exchange`

`exchange()` devuelve un `WebClientResponse` *sin* lanzar ante un no-2xx, de modo que
puedes inspeccionar el estado y decidir qué hacer: el terminal adecuado cuando un
no-2xx es *esperado* y no debería cortocircuitar la tubería:

```rust,ignore
let resp = client.get().uri("/health").retrieve().exchange().block().await?.unwrap();
if resp.is_success() {
    let body: serde_json::Value = resp.body_json()?;
} else if let Some(problem) = resp.problem() {
    // a decoded RFC 9457 FireflyError, if the body was a problem document
}
```

Qué acaba de ocurrir: `.exchange().block().await` devuelve
`Result<Option<WebClientResponse>, FireflyError>`; el `?` desempaqueta el `Result`
(aquí solo da error un fallo a nivel de transporte) y `.unwrap()` el `Option`.
`resp.is_success()` comprueba el rango 2xx, `resp.body_json::<T>()` decodifica el
cuerpo en búfer y `resp.problem()` decodifica un cuerpo `application/problem+json`
distinto de 2xx en un `FireflyError` (devolviendo `None` para un 2xx). La diferencia
con `body_to_mono` es el comportamiento de *lanzar*: `body_to_mono` convierte un
no-2xx en el `Err` terminal del `Mono`, mientras que `exchange` te entrega la
respuesta cruda para que ramifiques sobre ella.

## Paso 6 — Componer reintentos (el `WebClient` no integra ninguno)

A diferencia de `RestBuilder::with_retries`, el `WebClient` **no** tiene presupuesto
de reintentos. Es intencionado: la política de reintentos sigue siendo una propiedad
del *punto de llamada*, no del cliente. Compón los reintentos sobre el publicador
devuelto con `Mono::retry` / `Mono::retry_backoff`:

```rust,ignore
use firefly::reactive::{Backoff, Mono};
use std::time::Duration;

let payment = Mono::retry_backoff(
    || client.get().uri("/payments/p1").retrieve().body_to_mono::<Payment>(),
    Backoff::new(3, Duration::from_millis(100)),
);
```

Qué acaba de ocurrir: `Mono::retry_backoff` toma un *closure de fábrica* (debe
reconstruir la petición en cada intento, ya que un `Mono` suscrito se consume) y una
planificación `Backoff::new(max_retries, base)`. Cada fallo vuelve a ejecutar la
fábrica tras un retardo que crece exponencialmente. `Mono::retry(factory, n)` es el
hermano de recuento fijo sin backoff.

Por qué importa: el mismo `WebClient` puede ser cauto en un endpoint y agresivo en
otro, porque la política vive en la llamada y no en el cliente. Esto refleja cómo el
modelo reactivo compone `retry` sobre un publicador en lugar de configurarlo una sola
vez de forma global.

> **Tip** **Punto de control.** Una llamada de `WebClient` envuelta en
> `Mono::retry_backoff` reintenta según su propia planificación; el `WebClient`
> desnudo nunca reintenta. Si te descubres deseando que `WebClientBuilder` tuviera un
> `.with_retries`, esa es la señal para recurrir a `Mono::retry_backoff` en su lugar.

## Paso 7 — Escribir un trait declarativo `#[http_client]`

Escribir la cadena de llamada a mano está bien para peticiones puntuales, pero un
*servicio al que llamas repetidamente* merece una interfaz tipada. `#[http_client]`
es el análogo del `@HttpExchange` de Spring 6 (el sustituto moderno de OpenFeign):
escribes un **trait** de métodos que llevan los mismos atributos de verbo que usa un
`#[rest_controller]`, y la macro genera un `<Trait>Impl` que emite las peticiones a
través de un `WebClient`. Es la imagen especular de un controlador: el mismo
vocabulario, con la petición *emitida* en vez de *recibida*.

> **Note** **Término clave — cliente declarativo.** Un *cliente declarativo* es una
> interfaz que tú *describes* (verbos, rutas, argumentos) y dejas que el framework
> *implemente*, en lugar de escribir tú mismo el código que emite las peticiones. La
> macro lee el trait y genera el cuerpo. El análogo en Spring es `@HttpExchange`
> sobre una interfaz Java (antiguamente el `@FeignClient` de Spring Cloud OpenFeign).

```rust,ignore
use firefly::prelude::*;            // #[http_client], ClientError, Mono, Flux
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct CreateOrder { pub sku: String, pub qty: u32 }
#[derive(Deserialize)]
pub struct Order { pub id: String, pub sku: String }

#[http_client(path = "/api/v1/orders", name = "orders", bean)]
pub trait OrdersClient {
    // `:id` name-matches the `id` arg → path variable (percent-encoded).
    #[get("/:id")]
    async fn get_order(&self, id: String) -> Result<Order, ClientError>;

    // `status` / `page` are neither path vars nor a body → inferred query
    // params; `Option` omits itself when `None`.
    #[get("/")]
    async fn list(&self, status: String, page: Option<u32>) -> Result<Vec<Order>, ClientError>;

    // the lone non-scalar arg is the JSON body; one explicit header.
    #[post("/")]
    async fn create(&self, #[header("X-Tenant")] tenant: String, order: CreateOrder)
        -> Result<Order, ClientError>;

    #[delete("/:id")]
    async fn cancel(&self, id: String) -> Result<(), ClientError>;   // 204 → ()

    // reactive-first: a non-async fn returning Mono/Flux, no bridging.
    #[get("/stream")]
    fn stream(&self) -> Flux<Order>;
}
```

Qué acaba de ocurrir: la macro emitió el trait (menos los atributos marcadores de
verbo / por argumento) más una struct concreta `OrdersClientImpl` que envuelve un
`WebClient` e implementa el trait traduciendo el verbo, la plantilla de ruta y los
argumentos ligados de cada método en una petición fluida de `WebClient`. El
`path = "/api/v1/orders"` a nivel de trait se une a la ruta de cada método;
`name = "orders"` nombra el bean de DI; `bean` opta por el registro (Paso 8).

Constrúyelo a partir de una URL base, o inyecta un `WebClient` afinado:

```rust,ignore
let api = OrdersClientImpl::new("https://orders.svc");      // builds a WebClient
let order = api.get_order("42".into()).await?;
// or: OrdersClientImpl::with_client(my_web_client)         // shared pool / timeouts
```

`OrdersClientImpl::new(base_url)` construye un `WebClient` nuevo enraizado en la URL
(y aplica los valores por defecto `accept` / `content_type` del trait, si los hay).
`OrdersClientImpl::with_client(web_client)` es la costura de DI: pasa un `WebClient`
ya configurado (timeouts, cabeceras por defecto, un pool de conexiones compartido),
el análogo del `HttpServiceProxyFactory` de Spring.

### Cómo se ligan los argumentos

La sintaxis de ruta es la `:id` del framework (la misma que `#[rest_controller]`), no
la `{id}` de Spring, así que un controlador y su cliente especular se leen idénticos,
y escribir `{id}` es un error de compilación que te apunta hacia `:id`. La ligadura de
argumentos no necesita atributos en el caso común:

- un argumento sin anotar cuyo nombre coincide con un segmento `:var` es la
  **variable de ruta** (codificada como percent-encoding);
- el único argumento no anotado y no escalar en un `POST` / `PUT` / `PATCH` es el
  **cuerpo JSON**;
- todo lo demás es un **parámetro de consulta** (`Option` se omite cuando es `None`;
  `Vec` / `&[_]` repite la clave).

Anula cualquiera de estos con `#[path]` / `#[query("k")]` / `#[header("X")]` /
`#[body]`. Cada `:var` debe ligarse a exactamente un argumento o la macro se niega a
compilar, así que un renombrado sale a la luz de forma ruidosa en vez de descartar el
valor en silencio.

### Formas de retorno

Una `async fn` que devuelve `Result<T, ClientError>` es el valor por defecto
ergonómico; `Result<T, E>` funciona para cualquier `E: From<ClientError>`; una `fn`
*no asíncrona* que devuelve `Mono<T>` / `Flux<T>` entrega el valor reactivo diferido
directamente (un `Flux` usa por defecto `Accept: application/x-ndjson`); y
`WebClientResponse` es la vía de escape cruda de `.exchange()`.

> **Note** **Fidelidad de errores.** En un método con `await` de
> `Result<T, ClientError>`, cada fallo llega como `ClientError::Problem` con un
> `FireflyError` que lleva el estado y el código originales —así que `is_not_found()`
> / `is_server_error()` / `is_retryable()` siguen clasificando correctamente— en lugar
> de las variantes estructuradas `Transport` / `Decode` / `Encode`. Esas variantes
> estructuradas sobreviven solo en las formas de retorno `Mono` / `Flux` (donde el
> terminal `FireflyError` *es* el canal de error reactivo). Haz match sobre la forma
> reactiva cuando necesites variantes exactas byte a byte.

> **Tip** **Punto de control.** Un trait bajo `#[http_client]` produce un
> `<Trait>Impl` que puedes construir con `::new(url)`. Llamar a
> `get_order("42".into())` emite `GET /api/v1/orders/42` y decodifica el cuerpo en
> `Order`. Si tienes una errata en un `:var` de modo que no liga ningún argumento, la
> compilación falla: eso es la macro haciendo su trabajo.

## Paso 8 — Autowire del cliente como bean

Con `#[http_client(... bean)]`, el `OrdersClientImpl` generado se registra como un
bean al estilo `@Service` y se liga a `dyn OrdersClient`, así que un colaborador solo
declara `#[autowired] orders: Arc<dyn OrdersClient>` y el contenedor lo resuelve: el
beneficio del autowire de cliente Feign que conociste en
[Cableado de dependencias](./04-dependency-wiring.md). El registro toma un bean
`WebClient` compartido del contenedor (uno con nombre cuando escribes `client = "…"`),
así que todo cliente declarativo sobre el mismo upstream puede compartir un único pool
de conexiones afinado.

Qué acaba de ocurrir: `bean` ata el cliente declarativo al mismo grafo de DI que
cablea los controladores y handlers de Lumen. El trait debe ser object-safe para la
ligadura `dyn` (la macro lo comprueba de antemano y añade los supertraits
`Send + Sync`), de modo que una forma no object-safe falla con un mensaje claro en
lugar de un error `dyn Trait` aguas abajo.

> **Tip** **Punto de control.** Un trait `#[http_client(... bean)]` convierte
> `Arc<dyn OrdersClient>` en una dependencia inyectable. Añade `#[autowired] orders:
> Arc<dyn OrdersClient>` a cualquier bean y el contenedor te entrega la implementación
> generada: sin construcción manual en el punto de llamada.

## Paso 9 — Envolver la llamada en un circuit breaker

Ambos clientes son deliberadamente pequeños. Para corte de circuito, limitación de
tasa (rate limiting) o bulkheads, envuelve las llamadas en decoradores de
`firefly-resilience` (los mismos que [Caché](./17-caching.md) aplica al trabajo
entrante, aplicados de la misma forma a las llamadas salientes). El circuit breaker es
lo que evita que un servicio de Payments enfermo arrastre a Lumen con él.

> **Note** **Término clave — circuit breaker.** Un *circuit breaker* vigila los
> fallos recientes de una dependencia. Tras suficientes fallos se *abre* y rechaza de
> inmediato las siguientes llamadas durante un enfriamiento, en lugar de dejar que
> cada llamante espere en un timeout condenado; luego pasa a semiabierto para sondear
> la recuperación. El análogo en Spring/Java es el `CircuitBreaker` de Resilience4j.

```rust,ignore
use firefly::resilience::{CircuitBreaker, CircuitConfig};

// CircuitBreaker::execute returns the operation's value (Result<T, _>), so the
// guarded call still yields the Payment.
let breaker = CircuitBreaker::new(CircuitConfig::default());

let payment = breaker.execute(|| async {
    payments.request::<_, Payment>(Method::POST, "/payments", Some(&req)).await
}).await?;
```

Qué acaba de ocurrir: `CircuitBreaker::new(CircuitConfig::default())` construye un
breaker; `breaker.execute(|| async { ... })` ejecuta el closure bajo supervisión,
registrando cada resultado y propagando el `Result<T, _>` de la operación (de modo que
la llamada protegida sigue produciendo el `Payment`).

Por qué importa: cuando llamadas repetidas fallan, el breaker se abre y rechaza las
llamadas siguientes de inmediato con `ResilienceError::CircuitOpen` en lugar de
esperar en un timeout, así que un único upstream lento no puede agotar el pool de
tareas de Lumen. La resiliencia pertenece a la capa de *cliente*, configurada una sola
vez, no dispersa por cada handler.

> **Tip** **Punto de control.** Lleva al upstream a fallar suficientes veces y la
> siguiente `breaker.execute(...)` devuelve `Err(ResilienceError::CircuitOpen)` *de
> inmediato*, sin espera de timeout. `err.is_circuit_open()` lo confirma.

## Paso 10 — Conocer la capa de experiencia (un BFF de Lumen)

Un frontend móvil o web rara vez quiere la forma cruda de un único servicio de
dominio; quiere un *recorrido* (journey): «muéstrame el saldo de esta wallet **y** sus
pagos pendientes, en una sola llamada». Llamar a Lumen por el saldo y a Payments por
la lista de pendientes y combinarlos en el cliente significa dos viajes de ida y
vuelta, dos dominios de fallo, y que el frontend filtre conocimiento de las
interioridades de ambos servicios. El patrón Backend-for-Frontend (BFF) mueve esa
composición al lado del servidor.

Firefly incluye un starter dedicado para esta capa, `firefly-starter-experience`. Se
construye sobre el mismo `WebStack` que usa Lumen (así que hereda CORS, cabeceras de
seguridad, métricas de petición, correlación y la superficie del actuator) y añade los
bloques de construcción del BFF:

- `DomainClients`: un registro de `RestClient`s con nombre para los servicios de
  dominio aguas abajo;
- `SignalService`: regula un paso de workflow de larga duración, dirigido por señales,
  en el que se detiene hasta que un llamante entrega una señal con nombre (el
  `Workflow` de capa de experiencia de [Sagas](./12-sagas.md));
- un `WorkflowState` con capacidad para Redis indexado por id de correlación, más un
  `WorkflowQueryService` para lecturas de estado del recorrido.

Un servicio de experiencia de Lumen registra sus clientes aguas abajo de antemano y
luego los compone:

```rust,ignore
use firefly::starter_experience::{ExperienceStack, CoreConfig};

let bff = ExperienceStack::new(CoreConfig {
    app_name: "lumen-mobile-bff".into(),
    ..Default::default()
});

// Register the domain SDKs this BFF composes. `register` returns an
// Arc<RestClient> already wired with correlation + trace propagation.
let wallets = bff.clients.register("wallets", "https://lumen.internal");
let payments = bff.clients.register("payments", "https://payments.internal");
```

Qué acaba de ocurrir: `ExperienceStack::new(CoreConfig { app_name, .. })` cablea la
capa web más los bloques de construcción del BFF; `bff.clients` es el registro
`DomainClients`, y `register(name, base_url)` devuelve un `Arc<RestClient>` ya cableado
con propagación de correlación + traza. La raíz de composición se ramifica entonces a
través de los clientes registrados —ambas llamadas salen de forma concurrente, así que
la latencia compuesta queda acotada por el upstream más lento en vez de por su suma— y
se degrada con elegancia si un upstream tiene el circuito abierto (muestra el saldo,
deja la lista de pendientes vacía) en lugar de hacer fallar toda la respuesta.

> **Note** La capa de experiencia tiene un capítulo propio —
> [La capa de experiencia](./20a-experience-tier.md)— que cubre en profundidad
> `SignalService`, `WorkflowState`, el fan-out concurrente (`Mono::zip_with`) y los
> handlers de degradación parcial. Esta sección es la introducción; el tratamiento
> completo vive allí.

> **Design note.** El límite de capa es estricto: la dirección de dependencia es
> `channel → experience → domain → core`. Un servicio de experiencia *nunca* posee una
> base de datos, *nunca* llama a un servicio core directamente y *nunca* llama a un
> servicio de experiencia hermano: solo compone SDKs de *dominio*. Lumen es un servicio
> de estilo dominio/core construido sobre la fachada `firefly`; el BFF es un crate
> aparte que depende de `firefly-starter-experience` y del cliente publicado de Lumen.
> Esa separación es la razón por la que el starter de experiencia *no* va incluido en
> la fachada de dependencia única: un servicio de dominio no lo necesita.

## Otros protocolos

Más allá de REST, el crate incluye builders y andamiajes para los protocolos que
necesita una plataforma de back-office, seleccionados por feature para que las
dependencias pesadas no entren en servicios que no las usan:

- `GraphQlBuilder` / `GraphQlClient`: hacen POST de un `{ query, variables?,
  operationName? }`, lanzan `ClientError::GraphQl` ante un array `errors` no vacío y
  decodifican `data` en un `T` tipado. Siempre disponibles (sin dependencias extra).
- `SoapBuilder` / `SoapClient`: envuelven un cuerpo en un sobre SOAP 1.1, hacen POST de
  `text/xml` con una cabecera `SOAPAction` opcional y devuelven el XML crudo de la
  respuesta. Siempre disponibles.
- `GrpcBuilder`: construye un canal `tonic` para un stub generado que proporciona el
  llamante. Tras la feature `grpc` (`grpc-tls` para TLS).
- `WsBuilder` / `WsClient`: conectan y hacen streaming sobre `tokio-tungstenite`. Tras
  la feature `websocket`.

Las superficies REST, GraphQL y SOAP están totalmente cableadas; los protocolos de
streaming (gRPC y WebSocket) están tras feature gate. Igual que con los clientes HTTP,
cada llamada saliente hereda automáticamente el id de correlación del llamante, así que
una petición que se ramifica hacia tres upstreams se cose entre sí en tus trazas.

## Resumen — qué cambió en Lumen

| Antes | Después de este capítulo |
|--------|--------------------|
| cada tramo de transferencia es un `ledger.deposit(...)` local | el tramo de abono puede convertirse en un `payments.request(...)` saliente y resiliente a través de la red |
| sin modos de fallo salientes | los problemas RFC 9457 upstream se decodifican en un `FireflyError` tipado, clasificado por predicados `is_*` |
| sin idempotencia de red | una `Idempotency-Key` estable reenviada de forma consistente a lo largo de los reintentos |
| — | el `WebClient` reactivo (`body_to_mono` / `body_to_flux` / `exchange`), con reintentos *compuestos* vía `Mono::retry_backoff`, no integrados |
| — | traits declarativos `#[http_client]` autowired como beans `Arc<dyn …>`, más una llamada protegida por `CircuitBreaker` |

Ahora también sabes:

- Que el `RestClient` eager, el `WebClient` reactivo y el `#[http_client]`
  declarativo comparten un mismo conjunto de automatismos: cabeceras por defecto,
  propagación de correlación / traza y decodificación de problemas RFC 9457.
- Por qué el `WebClient` no tiene presupuesto de reintentos: la política de reintentos
  es una propiedad del punto de llamada, expresada con `Mono::retry` /
  `Mono::retry_backoff`.
- Que un cliente declarativo refleja un `#[rest_controller]` (misma sintaxis de ruta
  `:id`, mismos atributos de verbo), y que `bean` lo ata al grafo de DI.
- El patrón de capa de experiencia (BFF) y su estricto límite `channel → experience →
  domain → core`, cuyo tratamiento completo vive en
  [La capa de experiencia](./20a-experience-tier.md).

## Ejercicios

1. **Decodificar un problema upstream.** Levanta un stub que responda a
   `POST /payments` con un cuerpo `422 application/problem+json`. Llámalo a través de
   un `RestClient` y comprueba que `err.as_firefly()` devuelve `Some`, que
   `fe.status == 422` y que `err.is_unprocessable_entity()` es `true`, de modo que la
   saga pueda mapear el rechazo upstream sobre el propio `422` de Lumen en lugar de un
   `500`.

2. **Componer un reintento.** Envuelve una llamada de `WebClient` a un stub
   inestable en `Mono::retry_backoff(|| …, Backoff::new(3, Duration::from_millis(50)))`.
   Haz que el stub falle dos veces y luego tenga éxito, y comprueba que la llamada se
   resuelve finalmente; después confirma que el `WebClient` desnudo (sin envoltorio) se
   rinde tras un único intento.

3. **Envolver el tramo de abono en un circuit breaker.** Toma el paso de abono de la
   saga de transferencia y reemplaza `ledger.deposit(...)` por un
   `payments.request(...)` protegido por `CircuitBreaker`. Lleva al stub a fallar
   suficientes veces para disparar el breaker, y comprueba que la siguiente llamada
   devuelve `ResilienceError::CircuitOpen` *de inmediato* (sin timeout), y que la saga
   aún compensa el débito.

4. **Escribir un cliente declarativo.** Define un trait
   `#[http_client(path = "/api/v1/orders")]` con un método
   `get_order(&self, id: String) -> Result<Order, ClientError>`, constrúyelo con
   `::new("http://localhost:PORT")` contra un stub local, y comprueba que emite
   `GET /api/v1/orders/42`. Luego cambia la ruta para usar la `{id}` de Spring y
   confirma que la compilación falla con la pista de `:id`.

5. **Componer un resumen de BFF.** Construye un `ExperienceStack` diminuto, registra un
   cliente `wallets` y otro `payments` contra dos stubs locales, y escribe un handler
   que obtenga el saldo y la lista de pendientes de forma concurrente. Haz que el stub
   de payments devuelva un error y comprueba que el handler aún devuelve el saldo con
   una lista de pendientes vacía, demostrando degradación parcial en vez de un `500`.

## Adónde ir después

- Asegura el lado *entrante* —autenticación bearer JWT y RBAC basado en rutas sobre las
  rutas mutadoras de Lumen— en **[Seguridad](./14-security.md)**.
- Profundiza en componer servicios de dominio en una API con forma de recorrido en
  **[La capa de experiencia](./20a-experience-tier.md)**.
- Revisita los decoradores de resiliencia (circuit breaker, rate limiter, bulkhead,
  timeout) aplicados al trabajo entrante en **[Caché](./17-caching.md)**.
