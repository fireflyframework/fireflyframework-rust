# Tu primera API HTTP

Hasta ahora Lumen compila, arranca, imprime un banner y sirve un actuator, pero
no tiene endpoints propios. También sabes, por
[Cableado de dependencias](./04-dependency-wiring.md), cómo el framework descubre
y cablea los beans que escanea. Este es el capítulo en el que Lumen deja de ser un
banner y empieza a ser un *servicio*: le das una superficie HTTP real, declarada
con una macro, montada por ti, y demostrada por un test que ejercita el router
completo sin llegar nunca a vincular un socket.

La capa HTTP por debajo es [axum](https://docs.rs/axum). Firefly no la oculta —
escribes handlers de axum corrientes—, pero *añade* la macro de controlador, el
renderizado de problemas y el middleware de correlación/idempotencia que conociste
en el [Inicio rápido](./02-quickstart.md). Escribes dos handlers; el framework
aporta el cableado y monta el controlador.

Al terminar este capítulo, serás capaz de:

- Declarar un controlador REST como un único bean de DI cuyos colaboradores se
  autocablean, usando `#[derive(Controller)]` y `#[rest_controller]`.
- Mapear dos verbos —`POST /api/v1/wallets` y `GET /api/v1/wallets/:id`— a métodos
  handler, y comprender cómo la macro compone las rutas.
- Devolver una vista `serde` simple (`WalletView`) y convertir errores tipados en
  documentos RFC 9457 `application/problem+json` con el estado HTTP correcto.
- Entender *por qué* nunca llamas a `mount`: que añadir el bean del controlador *es*
  montarlo.
- Ejercitar el router completamente cableado en proceso con `tower::oneshot`, sin
  servidor activo y sin ningún puerto por el que competir.

## Conceptos que conocerás

Antes de la primera línea de código, aquí están las ideas en las que se apoya este
capítulo. Cada una se reintroduce en contexto allí donde se usa por primera vez;
esta es la versión breve.

> **Note** **Término clave — controlador.** Un *controlador* es el objeto que posee
> un grupo de endpoints HTTP. Sus métodos son los *handlers*, uno por cada mapeo de
> verbo y ruta. En Firefly un controlador es simplemente un bean con un bloque
> `impl` anotado; el framework lee las anotaciones y construye la tabla de
> enrutamiento. El análogo en Spring es un `@RestController`.

> **Note** **Término clave — handler / extractor.** Un *handler* es la función
> asíncrona que se ejecuta para una ruta. Un *extractor* es un tipo de argumento que
> extrae por ti una parte de la petición: el id de la ruta, el cuerpo JSON, un objeto
> de consulta. Estos son los propios extractores de axum (`Path`, `Json`, `State`);
> Firefly los reutiliza y añade algunos propios.

> **Note** **Término clave — documento de problema RFC 9457.** El RFC 9457 (que deja
> obsoleto al RFC 7807) define `application/problem+json`: un sobre JSON pequeño y
> estándar para errores HTTP con los campos `type`, `title`, `status` y `detail`.
> Firefly renderiza así automáticamente todos los errores de los handlers, de modo
> que todos tus errores hablan una única forma legible por máquina. El análogo en
> Spring es `ProblemDetail`.

> **Note** **Término clave — bus CQRS.** Lumen enruta los **comandos** que cambian
> estado y las **consultas** de solo lectura a través de un *bus* compartido. La
> labor del controlador es únicamente traducir HTTP a un mensaje y despacharlo; la
> lógica de la wallet vive detrás del bus. Construyes esa maquinaria en
> [CQRS](./09-cqrs.md). Para este capítulo, trata `bus.send(...)` / `bus.query(...)`
> como «entrega este mensaje al handler que sabe qué hacer con él». *CQRS* es la
> sigla de Command/Query Responsibility Segregation.

## Paso 1 — Declarar el bean del controlador

Los endpoints de wallet de Lumen viven todos en un tipo, `WalletApi`. Es un bean de
DI `#[derive(Controller)]`: un struct simple cuyos colaboradores se `#[autowired]`
desde el contenedor. Declarar el struct es la primera mitad de un controlador; el
bloque `impl` anotado del [Paso 2](#step-2--map-the-verbs) es la segunda mitad.

Abre `src/web.rs` y añade los imports y el struct:

```rust,ignore
// src/web.rs
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use firefly::cqrs::QueryCache;
use firefly::prelude::*;
use firefly::web::{WebError, WebResult};

use crate::commands::{GetWallet, OpenWallet};
use crate::domain::{DomainError, WalletView};

/// The wallet HTTP surface — a `#[derive(Controller)]` DI bean. Its
/// collaborators are **autowired** from the container, and `#[rest_controller]`
/// auto-mounts it; there is no hand-built state and no manual `routes()` call.
#[derive(Clone, Controller)]
pub struct WalletApi {
    /// The command/query bus the controller dispatches through (autowired).
    #[autowired]
    pub bus: Arc<Bus>,
    /// The application service the transfer saga and event stream use (autowired).
    #[autowired]
    pub ledger: Arc<Ledger>,
    /// The query cache, invalidated after a mutation (autowired).
    #[autowired]
    pub query_cache: Arc<QueryCache>,
}
```

Lo que acaba de ocurrir, bloque a bloque:

- Los imports traen los extractores de axum (`Path`, `State`, `Json`), el
  `QueryCache` de CQRS, toda la superficie de alta frecuencia vía
  `firefly::prelude::*` (que te da `Bus`, `Controller`, `#[autowired]` y las macros
  de verbo), y los tipos de resultado/error web (`WebResult`, `WebError`). El import
  de `DomainError` lo usa el mapeador de errores del
  [Paso 5](#step-5--map-typed-errors-to-rfc-9457-problems).
- `#[derive(Controller)]` marca el struct como un bean de controlador. Es el mismo
  estereotipo que los demás beans de Firefly que ya has visto: el contenedor lo
  escanea, lo construye y gestiona su ciclo de vida.
- Cada campo `#[autowired]` es un *colaborador* que el contenedor resuelve e inyecta
  cuando construye el bean. `bus` es el bus CQRS a través del que despachan los
  handlers; `ledger` es el servicio de aplicación que usan más adelante la saga y los
  endpoints de streaming; `query_cache` se invalida tras una escritura para que una
  lectura tras escritura nunca sirva un saldo obsoleto. Nunca construyes `WalletApi`
  tú mismo: lo hace el framework.
- `Clone` es necesario porque la macro entrega un clon del controlador a axum como
  *estado* por ruta; el struct está respaldado por `Arc`, así que clonar es barato.

> **Note** **Término clave — autocableado.** El *autocableado* (autowiring) es la
> inyección por constructor del framework: un campo `#[autowired]` se resuelve desde
> el contenedor por tipo y se entrega al bean en el momento de su construcción. Es
> exactamente el `@Autowired` de Spring. Tú declaras *qué* necesita un controlador;
> el contenedor decide *cómo* suministrarlo.

> **Tip** **Punto de control.** El struct compila en cuanto existen en el crate los
> beans `Bus`, `Ledger` y `QueryCache` que autocablea (los declaras como factorías
> `#[bean]`: el `Bus` lo aporta el framework, `Ledger` y `QueryCache` son de Lumen).
> Si `cargo build` se queja de que uno de estos tipos no se resuelve, vas por delante
> de la narrativa: las factorías de beans aterrizan en [CQRS](./09-cqrs.md). Por
> ahora, céntrate en la forma del controlador.

## Paso 2 — Mapear los verbos

Un struct con campos autocableados es solo un bean. Se convierte en controlador
cuando su bloque `impl` lleva `#[rest_controller]` y sus métodos llevan atributos de
verbo. La macro lee cada uno y genera una función `WalletApi::routes(state) ->
axum::Router`, de modo que la tabla de enrutamiento *se deriva de tu código*, no se
mantiene en un fichero aparte junto a él.

Añade el bloque `impl` a `src/web.rs`:

```rust,ignore
// src/web.rs (continued)
/// `#[rest_controller(path = "...")]` generates `WalletApi::routes(state) ->
/// axum::Router`. Each method carries one verb mapping and returns
/// `WebResult<T>`, so a handler error renders as RFC 9457
/// `application/problem+json`.
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletApi {
    /// `POST /api/v1/wallets` — open a wallet. Validation failures surface as
    /// 422 problems; success answers `201 Created` with the view.
    #[post(
        "/wallets",
        summary = "Open a wallet",
        description = "Opens a new wallet for an owner with an optional opening balance.",
        status = 201
    )]
    async fn open(
        State(api): State<WalletApi>,
        Json(body): Json<OpenWallet>,
    ) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
        let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
        Ok((axum::http::StatusCode::CREATED, Json(view)))
    }

    /// `GET /api/v1/wallets/:id` — fetch the read-model view. An unknown id
    /// renders as a 404 problem.
    #[get(
        "/wallets/:id",
        summary = "Fetch a wallet",
        description = "Returns the read-model view of a wallet."
    )]
    async fn get(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<WalletView>> {
        let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
        Ok(Json(view))
    }
}
```

Aquí hay tres cosas que merece la pena leer con atención.

**La ruta se compone.** `#[rest_controller(path = "/api/v1")]` es el prefijo;
`#[post("/wallets")]` y `#[get("/wallets/:id")]` son los sufijos. La macro los une
en `/api/v1/wallets` y `/api/v1/wallets/:id`. Los atributos `tag`, `summary`,
`description` y `status` son metadatos opcionales: `tag` agrupa los endpoints en la
documentación de la API, `summary`/`description` los anotan, y `status = 201` le
indica al generador de OpenAPI el estado de éxito. Cambian la *documentación*, no el
enrutamiento.

**Cada handler es un handler de axum corriente.** `State`, `Path` y `Json` son los
propios extractores de axum; Firefly no los reemplaza. `State(api): State<WalletApi>`
te entrega el controlador (con sus colaboradores autocableados ya en su sitio);
`Path(id): Path<String>` vincula el segmento `:id`; `Json(body): Json<OpenWallet>`
deserializa el cuerpo de la petición. El tipo de retorno `WebResult<T>` es lo que
permite que un error del handler se renderice como un documento de problema, tratado
en el [Paso 5](#step-5--map-typed-errors-to-rfc-9457-problems).

**El controlador es delgado.** `open` y `get` traducen HTTP a un mensaje y lo
entregan al `Bus` de CQRS, para luego traducir el resultado (o el error) de vuelta a
una respuesta HTTP. La *lógica* de la wallet vive detrás del bus, donde la pone
[CQRS](./09-cqrs.md). Lee `api.bus.send(...)` (un comando) y `api.bus.query(...)`
(una consulta) como «despacha al handler que sabe qué hacer»; el bus, los comandos y
el modelo de lectura son los temas de los capítulos 7 al 11.

> **Note** **Término clave — resolutor de argumentos / extractor validante.** Más
> allá de `Json`/`Path`/`Query`, `firefly::web` (reexportado en `firefly::prelude`)
> incluye extractores que encajan en la misma firma de handler: `Valid<T>` para un
> cuerpo JSON y `ValidPath<T>` / `ValidQuery<T>` para objetos de ruta/consulta (un
> fallo de vinculación es un **400**, un fallo de restricción un problema **422**), el
> extractor de carga de formularios `Multipart` / `UploadedFile`, y el resolutor de
> argumentos `PageRequest` que vincula el `Pageable` de Spring desde
> `?page=&size=&sort=`. El ejemplo por capas en
> [Microservicios por capas](./22-layered-microservices.md) los usa todos. Aquí los
> extractores simples `Json`/`Path` son suficientes.

> **Design note.** `#[rest_controller(path = "/api/v1")]` declara un controlador y su
> prefijo de ruta; `#[get]` / `#[post]` declaran los mapeos de verbo. Más allá de
> generar el router, la macro emite un descriptor de ruta por endpoint que alimenta la
> vista `/mappings` del actuator y el generador de OpenAPI, de modo que la tabla de
> enrutamiento se deriva de tu código en lugar de mantenerse a su lado, y las
> superficies de documentación quedan automáticamente sincronizadas con los handlers.
> Si has usado antes un framework con baterías incluidas, este estilo de controlador
> declarativo te resultará familiar.

> **Tip** **Punto de control.** `WalletApi` ahora lleva un `impl` con
> `#[rest_controller]` y dos métodos anotados. La macro ha generado una función
> `WalletApi::routes(state)` (que nunca llamas a mano) y ha registrado un *mount thunk*
> en el inventario de tiempo de enlazado. Verás ambos dar fruto en el
> [Paso 6](#step-6--controllers-are-auto-mounted).

## Paso 3 — Definir la forma del cable

La vista que devuelve un handler es un struct `serde` simple. Es la proyección del
*modelo de lectura* de una wallet: plana, optimizada para consultas y desacoplada del
agregado interno.

> **Note** **Término clave — modelo de lectura / DTO.** Un *DTO* (objeto de
> transferencia de datos) es la forma que ve un cliente en el cable, deliberadamente
> separada de tus tipos de dominio internos. El `WalletView` de Lumen es el DTO del
> modelo de lectura: una proyección plana que devuelve una consulta. Mantenerlo
> separado del agregado `Wallet` significa que puedes evolucionar el modelo interno
> sin romper el contrato de la API.

```rust,ignore
// src/domain.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WalletView {
    /// The wallet id.
    pub id: String,
    /// The owner's display name.
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied) — lets a client
    /// detect staleness under eventual consistency.
    pub version: i64,
}
```

Lo que acaba de ocurrir: `WalletView` deriva `Serialize` / `Deserialize` para
cruzar el cable, y `Schema` para que el generador de OpenAPI pueda describirlo (la
derivación `Schema` es el tema de [OpenAPI](./06a-openapi.md)). El saldo viaja como
un recuento entero de *unidades menores* (céntimos), de modo que `€10.00` es el
número JSON `1000`; el dinero nunca viaja como un float.

El cuerpo de petición que Lumen acepta en `POST /api/v1/wallets` es igual de
corriente: el comando `OpenWallet`. Un `#[serde(rename)]` en su campo de saldo hace
que la clave JSON sea `openingBalance` mientras el campo Rust se queda en
snake_case, de modo que el cable se ve así:

```json
{ "owner": "alice", "openingBalance": 1000 }
```

> **Tip** **Punto de control.** `WalletView` vive en `src/domain.rs` y el controlador
> lo importa con `use crate::domain::WalletView;`. El JSON que devuelve un `GET` son
> exactamente sus cuatro campos: `id`, `owner`, `balance`, `version`.

## Paso 4 — Dejar que el cliente elija el formato (opcional)

Los handlers de Lumen responden `application/json` porque devuelven
`Json<WalletView>`: un contrato deliberado y fijado a un formato. Pero un controlador
también puede entregar al framework un DTO y dejar que el *cliente* elija el formato
del cable. Este paso es lectura opcional; puedes saltar al
[Paso 5](#step-5--map-typed-errors-to-rfc-9457-problems) sin perder nada de la
narrativa en curso.

> **Note** **Término clave — negociación de contenido.** La *negociación de
> contenido* permite que un único handler sirva varios formatos de cable: el cliente
> envía una cabecera `Accept` y el framework renderiza la respuesta con el conversor
> que coincida. El análogo en Spring es un `HttpMessageConverter` elegido por
> `produces`.

Envuelve el valor de retorno en `Negotiate(dto)` y la respuesta se renderiza con el
conversor que seleccione la cabecera `Accept` de la petición —`JsonMessageConverter`
para `application/json`, `XmlMessageConverter` para `application/xml` / `text/xml`—,
mientras que el cuerpo de la petición se lee por su `Content-Type` de la misma forma:

```rust,ignore
// a format-agnostic variant of the wallet GET
use firefly::web::Negotiate;

#[get("/wallets/:id")]
async fn get(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
) -> WebResult<Negotiate<WalletView>> {
    let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
    Ok(Negotiate(view))
}
```

El mismo handler sirve ahora ambas formas de cable a partir del único `WalletView`:

```text
GET /api/v1/wallets/wlt_1  Accept: application/json
→ { "id": "wlt_1", "owner": "alice", "balance": 1000, "version": 1 }

GET /api/v1/wallets/wlt_1  Accept: application/xml
→ <response><id>wlt_1</id><owner>alice</owner><balance>1000</balance>...</response>
```

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 380" role="img"
     aria-label="Request lifecycle: an inbound HTTP request passes the Problem, TraceContext, Correlation and ContentNegotiation layers, outermost first, before reaching the rest_controller handler, and errors unwind to an RFC 9457 problem+json response"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="280.0" y="24.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">inbound HTTP request</text>
<line x1="280.0" y1="30.0" x2="280.0" y2="44.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,52.0 275.5,44.0 284.5,44.0" fill="#b5531f"/>
<rect x="150.0" y="58.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="56.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="77.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ProblemLayer</text><text x="280.0" y="91.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">errors → problem+json</text><line x1="280.0" y1="104.0" x2="280.0" y2="112.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,120.0 275.5,112.0 284.5,112.0" fill="#b5531f"/><rect x="150.0" y="122.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="120.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="141.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">TraceContextLayer</text><text x="280.0" y="155.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">W3C traceparent in / out</text><line x1="280.0" y1="168.0" x2="280.0" y2="176.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,184.0 275.5,176.0 284.5,176.0" fill="#b5531f"/><rect x="150.0" y="186.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="184.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="205.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">CorrelationLayer</text><text x="280.0" y="219.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ensure-or-generate id</text><line x1="280.0" y1="232.0" x2="280.0" y2="240.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,248.0 275.5,240.0 284.5,240.0" fill="#b5531f"/><rect x="150.0" y="250.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="248.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="269.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ContentNegotiationLayer</text><text x="280.0" y="283.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Accept → JSON / XML</text>
<line x1="280.0" y1="296.0" x2="280.0" y2="310.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,318.0 275.5,310.0 284.5,310.0" fill="#b5531f"/>
<rect x="180.0" y="320.5" width="200.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="180.0" y="318.0" width="200.0" height="46.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="280.0" y="338.0" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[rest_controller]</text><text x="280.0" y="352.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">your handler runs</text>
<text x="540.0" y="84.0" text-anchor="end" font-size="10" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif" font-style="italic">outermost</text>
<text x="540.0" y="276.0" text-anchor="end" font-size="10" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif" font-style="italic">innermost</text>
</svg>
<figcaption>La pila de capas por defecto, de la más externa a la más interna (algunas capas opcionales —CORS, cabeceras de seguridad, métricas— se omiten). <code>ProblemLayer</code> envuelve todo, de modo que cualquier error se desenrolla hasta una respuesta RFC&nbsp;9457 <code>application/problem+json</code>; el contexto de traza y la correlación se abren antes de que se ejecute tu handler; la negociación de contenido se sitúa más cerca de las rutas.</figcaption>
</figure>

Nada de esto lo cableas tú. La `ContentNegotiationLayer` se instala por defecto: se
sitúa lo más cerca posible de tus rutas, de modo que una respuesta `Negotiate` se
vuelve a renderizar según el `Accept` del cliente antes de que se ejecute el borde
exterior del middleware, y una respuesta `Json<T>` simple (o cualquier otra) pasa sin
tocarse. Un `Accept` ausente o vacío toma JSON por defecto, y un tipo sin coincidencia
recurre al primer conversor registrado (JSON), de modo que la negociación nunca hace
fracasar la petición.

> **Design note.** `Negotiate(dto)` entrega al framework un DTO y deja que la cabecera
> `Accept` de la petición elija el formato del cable, sin código de controlador. El par
> `JsonMessageConverter` / `XmlMessageConverter` viene en el registro, y la
> `ContentNegotiationLayer` se instala por defecto, así que la negociación está
> activada de fábrica. Añade un conversor —digamos CBOR— implementando
> `MessageConverter` y registrándolo; los conversores de usuario tienen prioridad
> sobre los integrados.

Si quieres un único *estilo de casa* —cada respuesta en `camelCase`, los nulos
descartados, las mismas reglas de inclusión en todas partes— en lugar de atributos
serde por tipo, Firefly te da un único objeto para expresar esa política:
`ObjectMapper`. Es un builder que establece una convención de nombrado de
propiedades, una regla de inclusión y el formato legible:

```rust,ignore
use firefly::web::{ObjectMapper, PropertyNaming, Inclusion};

// camelCase on the wire, drop nulls, compact output.
let mapper = ObjectMapper::new()
    .naming(PropertyNaming::CamelCase)
    .inclusion(Inclusion::NonNull)
    .pretty(false);
```

Las opciones de nombrado e inclusión son:

| Opción                                       | Efecto                                              |
|----------------------------------------------|-----------------------------------------------------|
| `PropertyNaming::AsIs` *(por defecto)*       | deja los nombres de campo intactos                  |
| `PropertyNaming::CamelCase`                  | `opening_balance` → `openingBalance`                |
| `PropertyNaming::SnakeCase`                  | `openingBalance` → `opening_balance`                |
| `PropertyNaming::KebabCase`                  | `opening_balance` → `opening-balance`               |
| `PropertyNaming::PascalCase`                 | `opening_balance` → `OpeningBalance`                |
| `PropertyNaming::ScreamingSnakeCase`         | `opening_balance` → `OPENING_BALANCE`               |
| `Inclusion::Always` *(por defecto)*          | serializa todos los campos                          |
| `Inclusion::NonNull`                         | omite los campos `null`                             |
| `Inclusion::NonEmpty`                        | omite los `null`, las cadenas vacías y las colecciones vacías |

La transformación de nombrado es *reversible*: un struct Rust en `snake_case` habla
`camelCase` en el cable y lo vuelve a leer de la misma forma, de modo que el mismo
mapper se sitúa en ambos extremos de una petición/respuesta. Si necesitas la
transformación en bruto —por ejemplo para posprocesar un `serde_json::Value` que
construiste a mano—, `apply_write(value)` renombra hacia el cable y `apply_read(value)`
renombra de vuelta hacia tus structs.

Para que el *servicio entero* observe una única política sin decorar cada DTO,
envuelve un mapper en `MappingJsonConverter` y regístralo. Implementa
`MessageConverter` para `application/json`, y como se registra como conversor de
*usuario* tiene prioridad sobre el `JsonMessageConverter` integrado:

```rust,ignore
use firefly::web::{ObjectMapper, PropertyNaming, Inclusion, MappingJsonConverter};

// One mapper expresses the service-wide JSON contract.
let mapper = ObjectMapper::new()
    .naming(PropertyNaming::CamelCase)
    .inclusion(Inclusion::NonNull);

// Wrap it as the JSON converter and register it so every negotiated
// application/json exchange observes the policy.
registry.add(std::sync::Arc::new(MappingJsonConverter::new(mapper)));
```

Registrarlo una vez (como un bean conversor) aplica una política global de nombrado e
inclusión JSON a toda la superficie HTTP, en lugar de repetir
`#[serde(rename_all = ...)]` en cada DTO. Los atributos serde por tipo siguen
componiéndose por encima: recurre a ellos cuando un tipo necesita desviarse del estilo
de casa, y deja que `MappingJsonConverter` lleve el valor por defecto en todo lo demás.

> **Warning** Un mapper de renombrado reescribe *todas* las claves de objeto del
> documento: opera sobre el árbol JSON, así que no puede distinguir un campo de struct
> de una clave dentro de un `HashMap` de forma libre que lleves como datos. Usa una
> política global de nombrado sobre cargas útiles *con forma de DTO*; para un tipo cuyo
> cuerpo contiene datos arbitrarios con claves de cadena, deja la política global en
> `AsIs` y nombra ese único tipo con `#[serde(rename_all = "camelCase")]`: eso es
> consciente del tipo y nunca toca las claves de datos.

## Paso 5 — Mapear errores tipados a problemas RFC 9457

Un handler que devuelve `WebResult<T>` convierte cualquier error en la respuesta
`application/problem+json` correcta vía `?`. `WebResult<T>` es un alias cuyo brazo de
error es un `WebError`, y el framework sabe cómo renderizarlo. El controlador de Lumen
mapea el canal de error del bus a un estado HTTP preciso con un único helper.

> **Note** **Término clave — `WebResult` / `WebError`.** `WebResult<T>` es
> `Result<T, WebError>`. Un `WebError` lleva un `FireflyError`, y el renderizador de
> problemas del framework lo convierte en un cuerpo `application/problem+json` con el
> código de estado correcto. Devolver `WebResult<T>` y usar `?` es todo lo que hace
> falta: nunca escribes la respuesta tú mismo.

Añade el mapeador de errores a `src/web.rs`:

```rust,ignore
// src/web.rs (continued)
/// Maps a bus `CqrsError` onto the precise HTTP problem the domain implies:
/// a validation failure → 422, a not-found detail → 404, an
/// insufficient-funds / non-positive detail → 422, otherwise 500.
fn cqrs_to_web(err: CqrsError) -> WebError {
    match err {
        CqrsError::Validation(detail) => WebError::from(FireflyError::validation(detail)),
        CqrsError::Handler(detail) => {
            if detail.ends_with("not found") {
                WebError::from(FireflyError::not_found(detail))
            } else if detail == DomainError::InsufficientFunds.to_string()
                || detail == DomainError::NonPositiveAmount.to_string()
                || detail == DomainError::OwnerRequired.to_string()
            {
                WebError::from(FireflyError::validation(detail))
            } else {
                WebError::from(FireflyError::not_found(detail))
            }
        }
        other => WebError::from(FireflyError::internal(other.to_string())),
    }
}
```

Lo que acaba de ocurrir: `cqrs_to_web` inspecciona el `CqrsError` del bus y elige el
constructor de `FireflyError` que coincide con el fallo: un fallo de validación se
convierte en un 422, un detalle de «not found» en un 404, y un error inesperado en un
500. Los handlers lo llaman como `.map_err(cqrs_to_web)?`, de modo que el error fluye
fuera del handler como un `WebError` y el renderizador del framework hace el resto.

Los constructores de `FireflyError` mapean directamente a un estado HTTP: elige el que
coincida con el fallo y el renderizador hace el resto:

| Constructor                              | Estado | Uso                          |
|------------------------------------------|--------|------------------------------|
| `FireflyError::bad_request(detail)`      | 400    | entrada malformada           |
| `FireflyError::unauthorized(detail)`     | 401    | credenciales ausentes/inválidas |
| `FireflyError::forbidden(detail)`        | 403    | autenticado pero no autorizado |
| `FireflyError::not_found(detail)`        | 404    | recurso ausente              |
| `FireflyError::conflict(detail)`         | 409    | conflicto de estado          |
| `FireflyError::validation(detail)`       | 422    | fallo de validación semántica |
| `FireflyError::internal(detail)`         | 500    | fallo del servidor           |

Un problema renderizado para una wallet desconocida se ve así; observa el tipo de
contenido dedicado `application/problem+json`, sobre el que los tests hacen asertos:

```json
{
  "type": "https://fireflyframework.org/problems/not-found",
  "title": "Not Found",
  "status": 404,
  "detail": "wallet wlt_does_not_exist not found"
}
```

> **Design note.** Devolver `WebResult<T>` convierte cualquier `FireflyError` en la
> respuesta `application/problem+json` correcta vía `?`, con el renderizado de
> problemas integrado: nunca escribes un mapeo de error a estado para los propios
> errores del framework. El contrato RFC 9457 es estable y neutral respecto al
> lenguaje, así que un 404 de Firefly se presenta idéntico a todo cliente sin importar
> qué servicio lo produjo.

> **Tip** **Punto de control.** `src/web.rs` contiene ahora el struct `WalletApi`, su
> `impl` con `#[rest_controller]` y `cqrs_to_web`. Eso es una superficie HTTP completa
> —dos endpoints y su mapeo de errores— sin una sola línea que monte una ruta o
> construya un router a mano.

## Paso 6 — Los controladores se automontan

Nunca montas el controlador. Como `WalletApi` es un bean `#[derive(Controller)]`, la
macro `#[rest_controller]` registró un *mount thunk* en el inventario de tiempo de
enlazado junto a la función generada `routes(state)`. En el arranque,
`FireflyApplication` llama a `firefly::web::mount_controllers(&container)`, que
resuelve cada bean de controlador desde el contenedor (construyendo sus colaboradores
autocableados), llama a su `routes(state)` y fusiona el resultado; luego superpone la
seguridad y envuelve todo el conjunto en la cadena de middleware web:

```rust,ignore
// inside FireflyApplication::bootstrap — you write none of this:
let routes = firefly::web::mount_controllers(&container)         // every #[rest_controller]
    .merge(firefly::web::mount_route_contributors(&container));  // every RouteContributor bean
// security (the FilterChain + BearerLayer beans) is layered onto these routes,
// then the whole router is wrapped in the observability edge:
let api = web.apply_middleware(routes);                          // + trace, metrics, 404, problem
```

> **Note** **Término clave — inventario de tiempo de enlazado.** El *inventario* es un
> registro en el que las macros escriben en tiempo de compilación: cada
> `#[rest_controller]`, handler de comando, listener de evento y tarea `#[scheduled]`
> se registra ahí. En el arranque el framework relee el inventario y cablea todo: sin
> reflexión, sin lista de registro manual. Así es como `main` nunca cambia a medida que
> Lumen crece.

Así que añadir el controlador *es* montarlo: declara el bean, anota el impl, y la
tabla de rutas crece. La función generada `routes(state)` de la macro sigue ahí (es lo
que llama el mount thunk), y el `RouteDescriptor` que emite por endpoint alimenta la
vista `/mappings` del actuator y el generador de OpenAPI, pero nunca llamas a ninguno a
mano.

Cada petición a una ruta de wallet pasa por la cadena canónica que obtuviste gratis en
el [Inicio rápido](./02-quickstart.md) —la capa de problemas RFC 9457, la propagación
del id de correlación y la repetición por idempotencia— antes de llegar a tu handler.
Tú escribiste los dos handlers; el resto del ciclo de vida de la petición es del
framework.

> **Note** `main` nunca cambia a medida que Lumen crece. La capa de seguridad JWT se
> descubre desde un bean `FilterChain` en [Seguridad](./14-security.md); el endpoint de
> streaming se añade como un bean `RouteContributor` en
> [Producción](./20-production.md). Cada uno es un *nuevo bean que encuentra el escaneo*,
> no una línea editada en una raíz de composición: el framework absorbe cada adición.

> **Tip** **Punto de control.** Ejecuta `cargo run` y lee la línea `:: routes ::` del
> informe de arranque: `/api/v1/wallets` y `/api/v1/wallets/:id` aparecen ahora en
> ella. Las añadiste declarando un bean, no tocando un router. (Las mutaciones
> responderán `401` hasta que existan los beans de seguridad; eso es lo esperado y llega
> en [Seguridad](./14-security.md).)

## Paso 7 — Demostrarlo en proceso

Ahora demuestra que todo el conjunto hace el viaje de ida y vuelta. Los tests HTTP de
Lumen ejercitan el router *real y completamente cableado* **en proceso** con
`tower::ServiceExt::oneshot`: sin socket vinculado, sin puerto por el que competir.

> **Note** **Término clave — `bootstrap()` y `oneshot`.** `bootstrap()` es el hermano
> de `run()`: ensambla la misma app —el mismo escaneo de componentes y el mismo
> automontaje—, pero devuelve un valor `Bootstrapped` *sin servir*, exponiendo el
> `api_router` cableado. `tower::ServiceExt::oneshot` alimenta una `Request` a ese
> `Router` y devuelve la `Response`, todo en el proceso del test. Juntos ejecutan la
> ruta de petición real sin un servidor activo.

La ruta de arranque del test es un pequeño helper, `build_router()`, en `src/web.rs`.
Está limitado a las compilaciones de test y llama a `bootstrap()`, devolviendo el
`axum::Router` exacto que sirve `main`:

```rust,ignore
// src/web.rs — the in-process router the tests drive (no socket bound).
#[cfg(test)]
pub(crate) async fn build_router() -> axum::Router {
    firefly::FireflyApplication::new(APP_NAME)
        .version(VERSION)
        .bootstrap()
        .await
        .expect("lumen bootstrap")
        .api_router
}
```

Como `bootstrap()` ejecuta el *mismo* escaneo de componentes y automontaje que `run()`,
el test ejercita la pila de controladores real y completamente cableada —las rutas
generadas por la macro, el contrato JSON y el mapeo de códigos de estado—, la misma
ruta de código que toca un cliente real, menos la red. `APP_NAME` y `VERSION` son las
dos constantes que Lumen mantiene junto a su superficie HTTP (las conociste en el
Inicio rápido).

Los tests propiamente dichos viven en `src/http_test.rs`, un `mod` `#[cfg(test)]`
compilado dentro del crate para que pueda alcanzar el `build_router` interno del crate.
Cada test arranca **un** contexto de aplicación y ejercita cada petición contra él —el
modelo `@SpringBootTest` de Spring Boot—, de modo que los singletons se mantienen
consistentes a lo largo de las peticiones de un test (la wallet que abre un comando es
la wallet que lee una consulta posterior). Un par de pequeños helpers de petición
mantienen los tests legibles:

```rust,ignore
// src/http_test.rs
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::build_router;
use crate::domain::WalletView;
use crate::security::{mint_token, CUSTOMER_ROLE};

/// A bearer token for a customer — mutations require authentication, which the
/// framework auto-discovers from the security beans.
fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &[CUSTOMER_ROLE]))
}

/// Sends one request against the (cloned) shared app and returns the response.
async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.unwrap()
}

/// Decodes a JSON response body into a typed value.
async fn body_json<T: serde::de::DeserializeOwned>(res: Response) -> T {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
```

> **Note** Como la seguridad se autodescubre desde los beans `FilterChain` y
> `BearerLayer` (el tema de [Seguridad](./14-security.md)), el `POST` mutante lleva una
> cabecera `Authorization: Bearer …`. El `GET` de solo lectura no necesita ninguna. Si
> aún no has añadido los beans de seguridad, ejecuta los tests de mutación sin la
> cabecera y espera un `401`: eso *es* el framework aplicando la cadena que descubrió.

Aquí está el primer test de extremo a extremo, el viaje de ida y vuelta open-then-get.
El `axum::Router` está respaldado por `Arc` y es barato de clonar, así que cada
`oneshot` clona la app compartida:

```rust,ignore
#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    let app = build_router().await;

    // POST /api/v1/wallets → 201 Created with the opened view.
    let res = send(
        &app,
        Request::post("/api/v1/wallets")
            .header("content-type", "application/json")
            .header("authorization", bearer())
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "owner": "alice", "openingBalance": 1_000
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED, "open should 201");
    let opened: WalletView = body_json(res).await;
    assert_eq!(opened.owner, "alice");
    assert_eq!(opened.balance, 1_000);
    assert_eq!(opened.version, 1);

    // GET /api/v1/wallets/:id → 200 OK with the same view.
    let res = send(
        &app,
        Request::get(&format!("/api/v1/wallets/{}", opened.id))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let fetched: WalletView = body_json(res).await;
    assert_eq!(fetched.id, opened.id);
    assert_eq!(fetched.balance, 1_000);
}
```

Lo que acaba de ocurrir: el `POST` abre una wallet y el framework responde `201` con el
`WalletView`; el `GET` vuelve a leer la misma wallet y responde `200` con la vista
correspondiente. Ambas peticiones pasaron por la pila completa del controlador montado,
el despacho CQRS y el contrato JSON, en un único proceso y sin red.

Las rutas de error se prueban de la misma forma. Un id que nunca se abrió es un problema
`404`, y el test asevera el tipo de contenido `application/problem+json`, de modo que el
contrato RFC 9457 forma parte de la suite, no solo de la prosa:

```rust,ignore
#[tokio::test]
async fn unknown_wallet_is_404_problem() {
    let app = build_router().await;
    let res = send(
        &app,
        Request::get("/api/v1/wallets/wlt_does_not_exist")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let ct = res.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("application/problem+json"));
}
```

> **Design note.** `oneshot` contra `build_router()` ejecuta toda la pila del
> controlador en el proceso del test sin servidor activo ni socket vinculado, de modo
> que el test ejercita la ruta de petición real a plena velocidad y sin contención de
> puertos. [Testing](./18-testing.md) lo integra en una estrategia completa.

> **Tip** **Punto de control.** Ejecuta `cargo test -p firefly-sample-lumen` y observa
> cómo pasan el test del viaje de ida y vuelta y el del problema 404 contra el router
> real, ensamblado por el framework. (El ejemplo completo también prueba
> depósito/retiro, la saga de transferencia y la cadena de seguridad; esos dependen de
> maquinaria de capítulos posteriores.)

## Resumen — qué cambió en Lumen

| Antes | Después de este capítulo |
|--------|--------------------|
| un router público vacío | un controlador `WalletApi` declarado con `#[derive(Controller)]` + `#[rest_controller]` y dos endpoints reales |
| sin contrato de cliente | `POST /api/v1/wallets` → `201` + `WalletView`, `GET /api/v1/wallets/:id` → `200`/`404`, todo JSON |
| errores sin considerar | `FireflyError` tipado → RFC 9457 `application/problem+json` con el estado correcto, vía `cqrs_to_web` |
| nada que probar | un viaje de ida y vuelta con `tower::oneshot` que ejercita el router completo en proceso, asertos de tipo de contenido incluidos |

Ahora también sabes:

- Que un controlador es *solo un bean más un `impl` anotado*: colaboradores
  `#[autowired]` en el struct, atributos de verbo en los métodos, y que la macro deriva
  la tabla de enrutamiento de tu código.
- Que nunca montas un controlador: `mount_controllers(&container)` resuelve y fusiona
  cada `#[rest_controller]` en el arranque, de modo que añadir el bean *es* añadir las
  rutas, y `main` nunca cambia.
- Que `WebResult<T>` más un constructor de `FireflyError` convierte cualquier error del
  handler en el `application/problem+json` correcto, sin escribir respuestas a mano.
- Que `bootstrap()` es la costura de test: `build_router()` ejercita el router
  completamente cableado en proceso con `tower::oneshot`, sin socket vinculado.

El controlador es deliberadamente delgado: habla HTTP y delega la lógica de la wallet
al bus. Esa costura es lo que rellenan los siguientes capítulos: el modelo de lectura
que sirve el `GET`, el dominio que aplica las reglas, y los handlers CQRS a los que
despacha el `POST`.

## Ejercicios

1. **Añade una ruta.** Dale a `WalletApi` un método `list` `#[get("/wallets")]` que
   devuelva `WebResult<Json<Vec<WalletView>>>`. Ejecuta Lumen y observa cómo la nueva
   ruta aparece en la línea `:: routes ::` del informe de arranque y en
   `WalletApi::routes`: nunca tocas una tabla de enrutamiento.
2. **Da forma a un error.** Haz que `cqrs_to_web` (o un pequeño handler propio)
   devuelva `FireflyError::conflict("wallet already closed")` y confirma que la
   respuesta es un `409` con `application/problem+json`. Prueba también `bad_request` y
   `forbidden`, y lee el `type`/`title`/`status` renderizado de cada uno frente a la
   tabla del [Paso 5](#step-5--map-typed-errors-to-rfc-9457-problems).
3. **Negocia el formato.** Cambia el tipo de retorno del handler `GET` a
   `Negotiate<WalletView>` (Paso 4), ejecuta Lumen y pide la misma wallet dos veces:
   una con `Accept: application/json` y otra con `Accept: application/xml`. Confirma que
   un único handler sirve ambas formas de cable.
4. **Escribe tú mismo el viaje de ida y vuelta.** Copia
   `open_then_get_round_trips_through_cqrs`, cambia el propietario y el saldo de
   apertura, y asevera que el `balance` devuelto coincide. Ejecuta
   `cargo test -p firefly-sample-lumen` y observa cómo pasa contra el router real.
5. **Honra la idempotencia.** Haz `POST /api/v1/wallets` dos veces con la misma cabecera
   `Idempotency-Key` y un cuerpo idéntico; confirma que la segunda respuesta repite el
   resultado almacenado. Luego cambia el cuerpo bajo la misma clave y observa el `409`.
   Nada de esto lo escribiste tú: vino con la cadena de middleware.

## Adónde ir después

- Mira cómo la macro convierte tus tipos `#[rest_controller]` y `#[derive(Schema)]` en
  una especificación viva en **[OpenAPI y documentación de la API](./06a-openapi.md)**.
- Dale al endpoint `GET` un almacén de respaldo real con
  **[Persistencia y repositorios reactivos](./07-persistence.md)**.
- Pon las reglas de la wallet detrás del bus en
  **[Diseño orientado al dominio](./08-domain-driven-design.md)** y
  **[CQRS](./09-cqrs.md)**: la maquinaria a la que despachan `bus.send(...)` /
  `bus.query(...)`.
