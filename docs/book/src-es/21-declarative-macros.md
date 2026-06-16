# Servicios declarativos con macros

Lumen está terminado. A lo largo de más de veinte capítulos creció desde un
andamiaje vacío hasta un servicio CQRS con event sourcing, seguro y observable,
con una saga de transferencia, un flujo de cumplimiento, una transferencia en dos
fases, un latido programado y un endpoint de streaming opcional — y depende de
exactamente **un** crate de Firefly. Este capítulo final vuelve a leer todo el
servicio a través de una única lente: las **macros declarativas**. Al terminar
serás capaz de señalar cada `#[derive(...)]` y `#[...]` de `samples/lumen` y
decir con precisión en qué cableado se colapsó al convertirse en una declaración
junto al código. Esa es la tesis que el crate en ejecución demuestra: *una fachada
más macros equivale al framework, sin el código repetitivo.*

Este capítulo no introduce una funcionalidad nueva. Es un recorrido guiado por la
capa declarativa que has estado usando todo el tiempo, ralentizado para que cada
macro se explique desde primeros principios antes de leerla en contexto. Donde una
macro se ejercita en `samples/lumen` leemos el código de Lumen tal cual; donde es
una parte de primera clase del framework que Lumen simplemente no usa, leemos un
ejemplo independiente y enfocado, y lo indicamos.

Al terminar este capítulo, serás capaz de:

- Explicar qué es una *macro declarativa* en Firefly, y por qué el cableado
  generado lo comprueba el compilador en lugar de descubrirlo mediante reflexión
  en tiempo de ejecución.
- Rastrear cada macro que Lumen usa — `#[derive(Command/Query)]`, `#[handlers]`,
  `#[derive(DomainEvent/AggregateRoot)]`, `#[event_listener]`,
  `#[rest_controller]`, `#[scheduled]`, `#[firefly::saga/workflow/tcc]` — hasta el
  `impl`, router o registro exacto que emite.
- Nombrar el conjunto declarativo de apoyo que Lumen no usa —
  `#[derive(Builder)]`, `#[derive(Mapper)]`, `#[derive(Entity)]` /
  `#[derive(SqlxRepository)]` / `#[firefly::repository]` /
  `#[firefly::transactional]`, los decoradores de seguridad de método y de
  resiliencia, `#[cacheable]` y el resto — y leer un ejemplo correcto de cada uno.
- Describir la ruta oculta del contrato `__rt` que permite a un servicio de un
  solo crate compilar todo lo que una macro expande.
- Explicar el *drenaje* de `inventory`: cómo un bean, listener, tarea o
  controlador *declarado* pasa a estar *cableado* en el arranque sin ninguna
  llamada de registro escrita a mano.
- Verificar que todo el crate compila, pasa los tests y supera el linter de forma
  limpia desde la raíz del workspace.

## Conceptos que conocerás

Antes del catálogo, aquí tienes las cuatro ideas en las que se apoya este
capítulo. Cada una se reintroduce en contexto donde se usa por primera vez; esta
es la versión breve.

> **Note** **Término clave — macro declarativa.** Una *macro declarativa* es un
> atributo (`#[...]`) o un derive (`#[derive(...)]`) que un `proc-macro` expande
> en **tiempo de compilación** a los `impl`, routers y funciones auxiliares que de
> otro modo escribirías a mano. La declaración se sitúa junto al código que
> describe; el compilador comprueba el código generado como cualquier otro
> fuente. El equivalente en Spring es una anotación (`@RestController`,
> `@Component`) — salvo que Spring descubre y procesa las anotaciones por
> reflexión al arrancar, mientras que Firefly las resuelve en tiempo de
> compilación.

> **Note** **Término clave — fachada y preludio.** La *fachada* es el único crate
> `firefly` que reexporta todo el framework y cada macro; el *preludio* es
> `firefly::prelude`, un módulo con los tipos de alta frecuencia que importas en
> bloque con `use firefly::prelude::*;`. Depender de una fachada e importar un
> preludio es toda la historia de "una dependencia, una importación". El
> equivalente en Spring es un único starter de Spring Boot más los tipos del
> framework autoimportados.

> **Note** **Término clave — bean.** Un *bean* es un objeto que el framework
> construye, gestiona y entrega a quien declare que lo necesita (con
> `#[autowired]`). Tú declaras los beans; el framework los descubre al arrancar y
> los conecta entre sí. Esto es exactamente la noción de Spring de un bean
> gestionado por el contexto de aplicación.

> **Note** **Término clave — registro de inventory.** `inventory` es un crate de
> Rust que permite a una macro registrar un valor en una tabla global del proceso
> **en tiempo de enlazado** — antes de que se ejecute `main`. Cada macro
> declarativa que produce un handler, listener, tarea o controlador envía un
> *registro* a una de estas tablas; `FireflyApplication` **drena** las tablas en
> el arranque e instala cada entrada. El efecto refleja el escaneo de componentes
> del classpath de Spring, pero el inventario lo construye el enlazador, no el
> recorrido del classpath en tiempo de ejecución.

## Paso 1 — Una dependencia, un preludio

Cada capítulo empezó del mismo modo, así que empieza ahí. Abre el `Cargo.toml` de
Lumen. Lista exactamente un crate de Firefly; todo lo declarativo llega a través
de él.

```toml
# samples/lumen/Cargo.toml
[dependencies]
# The one-dependency story: the `firefly` facade re-exports the whole framework
# AND every `#[derive(...)]` / `#[...]` macro. Generated code resolves runtime
# types through the facade, so Lumen never lists the underlying `firefly-*`
# crates. The `admin` feature pulls in the self-hosted admin dashboard.
firefly = { version = "26.6.28", features = ["admin"] }

# The two ecosystem crates a Firefly service still writes against directly:
# axum (you author the controller handlers) and serde (your messages and event
# payloads are Serialize/Deserialize). `serde_json` encodes the event payloads.
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Async runtime for `#[tokio::main]`, and the id/clock crates the domain uses.
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
async-trait = "0.1"

[features]
# The reactive streaming endpoint is feature-gated so the teaching baseline
# stays lean; the production chapter turns it on. It needs nothing beyond the
# `firefly` facade.
default = []
streaming = []
```

Y cada módulo se abre con una única importación en bloque:

```rust,ignore
use firefly::prelude::*;
```

Lo que acaba de pasar: ese glob trae al ámbito toda la superficie de alta
frecuencia — `Bus`, `Container`, `Scheduler`, `Saga` / `Step`, `Application` /
`ShutdownHandle`, `Core` / `CoreConfig`, `WebResult` / `WebError`, `FireflyError`
/ `FireflyResult`, `Mono` / `Flux` — **y** cada macro. Para los tipos que nombras
explícitamente hay alias por crate (`firefly::cqrs::Bus`,
`firefly::eventsourcing::EventStore`, `firefly::security::JwtService`, …), por lo
que varios módulos de Lumen también escriben `use firefly::cqrs::QueryCache;` o
`use firefly::eda::{Broker, Event};` junto al glob del preludio. `axum` y `serde`
son los únicos dos crates del ecosistema contra los que Lumen escribe
directamente.

> **Note** Un crate `proc-macro` no puede reexportar por sí mismo tipos de tiempo
> de ejecución, así que el código generado por la macro referencia cada tipo de
> tiempo de ejecución a través de la ruta oculta del contrato `__rt` de la fachada
> — por ejemplo `::firefly::__rt::firefly_cqrs::Bus`. Por eso Lumen, dependiendo
> solo de `firefly`, compila todo lo que una macro expanda sin listar nunca los
> crates `firefly-*` subyacentes. Nunca escribes `__rt` tú mismo; si renombras o
> intercalas un shim sobre la fachada, pasa `#[firefly(crate = "my_firefly")]` a
> cualquier macro para sobrescribir el segmento inicial. Volvemos a este contrato
> en el [Paso 11](#step-11--how-the-wiring-actually-lands-the-__rt-contract-and-the-inventory-drain).

### Mantenerse ligero

La compilación por defecto de `firefly` incorpora solo los crates de *port*
ligeros del framework — sin drivers de terceros pesados. Lumen no necesita
ninguno, así que su compilación es mínima. Los adaptadores pesados son features
opt-in de cargo (la ruta de intercambio a la que apuntó cada capítulo):

| Feature | Incorpora |
|---------|-----------|
| `data-sqlx` | adaptador de repositorio relacional (Postgres / MySQL / SQLite) |
| `data-mongodb` | adaptador de repositorio documental (MongoDB) |
| `eda-kafka` / `eda-rabbitmq` / `eda-redis` / `eda-postgres` | transportes de broker de eventos |
| `cache-redis` / `cache-postgres` | backends de caché |
| `admin` | el panel de administración autoalojado |
| `full` | todo lo anterior |

> **Tip** **Punto de control.** Abre `samples/lumen/Cargo.toml` y confirma que hay
> exactamente una línea `firefly = { … }` bajo `[dependencies]`, y que cada
> archivo fuente bajo `samples/lumen/src/` se abre con `use firefly::prelude::*;`.
> Esa única dependencia y esa única importación son toda la premisa que este
> capítulo desgrana.

## Paso 2 — El catálogo de macros, mapeado a los archivos de Lumen

Lumen ejercita el conjunto declarativo central. Antes de leer cualquier macro en
profundidad, aquí está el mapa: cada macro, el archivo exacto de Lumen en el que
aterriza y lo que genera.

| Macro | Archivo de Lumen | Genera |
|-------|-----------|-----------|
| `#[derive(Command)]` / `#[derive(Query)]` | `commands.rs` | el `impl` de `Message` (`#[firefly(validate)]`, `#[firefly(cache_ttl = "…")]`) |
| `#[derive(Schema)]` | `commands.rs`, `domain.rs`, `web.rs`, … | un esquema OpenAPI para el tipo, de modo que aparece en `/v3/api-docs` |
| `#[handlers]` (sobre un `impl` de bean-handler) | `commands.rs`, `ledger.rs` | registra en el bus / broker cada método `#[command_handler]` / `#[query_handler]` / `#[event_listener]` de un bean de DI |
| `#[command_handler]` / `#[query_handler]` (marcadores de método) | `commands.rs` | marcan un método handler de CQRS dentro de un `impl` con `#[handlers]` |
| `#[derive(DomainEvent)]` | `domain.rs` | discriminador `EVENT_TYPE` + conversión `to_domain_event` |
| `#[derive(AggregateRoot)]` | `domain.rs` | `AGGREGATE_TYPE` + `aggregate()` / `aggregate_mut()` |
| `#[derive(Service)]` / `#[derive(Repository)]` | `commands.rs`, `ledger.rs` | un bean `@Component` / `@Repository` escaneado con campos `#[autowired]` |
| `#[event_listener(topic = "…")]` (marcador de método) | `ledger.rs` | marca un método listener de EDA dentro de un `impl` con `#[handlers]` (el bean de proyección) |
| `#[derive(Configuration)]` + `#[bean]` | `web.rs` | un contenedor `@Configuration` cuyas factorías `#[bean]` declaran beans de infraestructura |
| `#[derive(Controller)]` + `#[rest_controller]` + `#[get/post]` | `web.rs` | un bean controlador autowired y su `WalletApi::routes(state) -> axum::Router` |
| `#[scheduled(fixed_rate = "…")]` | `housekeeping.rs` | un auxiliar `schedule_<fn>(scheduler)` más un registro drenado |
| `#[firefly::saga]` + `#[saga_step]` | `transfer.rs` | `TransferSaga::run` / `::saga()` — un grafo de pasos con compensación |
| `#[firefly::workflow]` + `#[workflow_step]` | `compliance.rs` | un `run` de workflow sobre el DAG de pasos |
| `#[firefly::tcc]` + `#[participant]` | `tcc_transfer.rs` | un `run` de TCC que conduce el try / confirm / cancel de cada participante |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 300" role="img"
     aria-label="Declarative macros mapped to generated code: derive Command emits a Message impl, derive Schema emits an OpenAPI schema, derive DomainEvent emits EVENT_TYPE and to_domain_event, rest_controller emits a controller bean and routes builder, derive Service emits a scanned bean with autowired fields"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="150.0" y="24.0" text-anchor="middle" font-size="11.5" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">you write</text>
<text x="430.0" y="24.0" text-anchor="middle" font-size="11.5" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">the macro generates</text>
<rect x="24.0" y="42.5" width="240.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="40.0" width="240.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="144.0" y="64.5" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[derive(Command)]</text>
<line x1="264.0" y1="60.0" x2="296.0" y2="60.0" stroke="#d4793a" stroke-width="2.4" stroke-linecap="round"/><polygon points="304.0,60.0 296.0,64.5 296.0,55.5" fill="#b5531f"/>
<rect x="304.0" y="42.5" width="232.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="304.0" y="40.0" width="232.0" height="40.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="420.0" y="64.5" text-anchor="middle" font-size="11" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">the Message impl  (kind, validate, cache_ttl)</text>
<rect x="24.0" y="90.5" width="240.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="88.0" width="240.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="144.0" y="112.5" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[derive(Schema)]</text>
<line x1="264.0" y1="108.0" x2="296.0" y2="108.0" stroke="#d4793a" stroke-width="2.4" stroke-linecap="round"/><polygon points="304.0,108.0 296.0,112.5 296.0,103.5" fill="#b5531f"/>
<rect x="304.0" y="90.5" width="232.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="304.0" y="88.0" width="232.0" height="40.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="420.0" y="112.5" text-anchor="middle" font-size="11" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">an OpenAPI schema  (appears in /v3/api-docs)</text>
<rect x="24.0" y="138.5" width="240.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="136.0" width="240.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="144.0" y="160.5" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[derive(DomainEvent)]</text>
<line x1="264.0" y1="156.0" x2="296.0" y2="156.0" stroke="#d4793a" stroke-width="2.4" stroke-linecap="round"/><polygon points="304.0,156.0 296.0,160.5 296.0,151.5" fill="#b5531f"/>
<rect x="304.0" y="138.5" width="232.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="304.0" y="136.0" width="232.0" height="40.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="420.0" y="160.5" text-anchor="middle" font-size="11" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">EVENT_TYPE + to_domain_event</text>
<rect x="24.0" y="186.5" width="240.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="184.0" width="240.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="144.0" y="208.5" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[rest_controller]</text>
<line x1="264.0" y1="204.0" x2="296.0" y2="204.0" stroke="#d4793a" stroke-width="2.4" stroke-linecap="round"/><polygon points="304.0,204.0 296.0,208.5 296.0,199.5" fill="#b5531f"/>
<rect x="304.0" y="186.5" width="232.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="304.0" y="184.0" width="232.0" height="40.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="420.0" y="208.5" text-anchor="middle" font-size="11" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">a Controller bean + WalletApi::routes(state)</text>
<rect x="24.0" y="234.5" width="240.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="232.0" width="240.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="144.0" y="256.5" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[derive(Service)]</text>
<line x1="264.0" y1="252.0" x2="296.0" y2="252.0" stroke="#d4793a" stroke-width="2.4" stroke-linecap="round"/><polygon points="304.0,252.0 296.0,256.5 296.0,247.5" fill="#b5531f"/>
<rect x="304.0" y="234.5" width="232.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="304.0" y="232.0" width="232.0" height="40.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="420.0" y="256.5" text-anchor="middle" font-size="11" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">a scanned bean with #[autowired] fields</text>
</svg>
<figcaption>Declarativo, en tiempo de compilación. Cada atributo o derive se expande al cableado que de otro modo escribirías a mano: un <code>impl</code> de <code>Message</code>, un esquema OpenAPI, un discriminador de evento, un constructor <code>routes()</code> de controlador o un bean escaneado con campos autowired — generado por <code>firefly-macros</code>, no por un paso de codegen que ejecutes.</figcaption>
</figure>

Los siguientes pasos leen cada uno de estos en su archivo de Lumen, en el orden en
que el propio crate está estratificado. Después de eso, el
[Paso 10](#step-10--the-rest-of-the-declarative-set-not-used-by-lumen) cataloga las
macros que Lumen *no* ejercita — porque usa event sourcing y gestiona sus
preocupaciones transversales por otros medios — cada una con un ejemplo
independiente y correcto.

> **Tip** **Punto de control.** Mantén esta tabla abierta en un segundo panel. A
> medida que leas cada paso, encuentra la fila que le corresponde y confirma que la
> columna "Genera" coincide con la explicación. La tabla es el esqueleto; los pasos
> son el músculo.

## Paso 3 — Los mensajes de CQRS y su bean handler (`commands.rs`)

> **Note** **Término clave — CQRS.** *Command/Query Responsibility Segregation* es
> un patrón que enruta los **comandos** que cambian el estado y las **consultas**
> de solo lectura a través de handlers separados en un *bus* compartido. Un comando
> muta; una consulta lee; nunca comparten un handler. El equivalente en Spring es
> un gateway de comandos/consultas sobre métodos anotados con `@CommandHandler` /
> `@QueryHandler`.

`#[derive(Command)]` y `#[derive(Query)]` generan el `impl` de `Message` que
permite al bus enrutar una struct. `#[firefly(validate)]` sobre un campo hace que
un valor vacío o cero falle la validación *antes* de que el handler se ejecute;
`#[firefly(cache_ttl = "…")]` sobre una consulta alimenta la caché de lectura.
Aquí está la declaración de Lumen tal cual, incluyendo el `#[derive(Builder)]` y el
`#[derive(Schema)]` que también lleva:

```rust,ignore
// samples/lumen/src/commands.rs
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command, Builder, Schema)]
#[serde(default)]
pub struct OpenWallet {
    #[firefly(validate)]
    #[builder(into)]                 // accept &str, String, …
    pub owner: String,
    #[serde(rename = "openingBalance")]
    #[builder(default)]              // unset → 0
    pub opening_balance: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    pub id: String,
}
```

Lo que acaba de pasar, derive a derive:

- `Command` / `Query` emiten el `impl` de `firefly::cqrs::Message` — el trait sobre
  el que despacha el bus. `#[firefly(validate)]` registra `owner` como un campo
  obligatorio, así que `OpenWallet::default().validate()` es un `Err`.
  `#[firefly(cache_ttl = "30s")]` lo lee la caché de consultas a través del
  `Message::cache_ttl` generado.
- `Schema` emite un esquema OpenAPI para el tipo, de modo que `OpenWallet` aparece
  en la especificación servida en `/v3/api-docs` en el puerto de gestión — sin
  esquema escrito a mano.
- `Builder` (el equivalente de `@Builder` de Lombok) se trata en el
  [Paso 10](#construction--the-fluent-builder-derivebuilder); ignóralo por ahora.

> **Note** **Término clave — bean handler.** Un *bean handler* es un componente de
> DI cuyos métodos son los handlers de comandos/consultas. Sus colaboradores se
> obtienen mediante `#[autowired]` desde el contenedor, así que cada handler los
> alcanza a través de `self` — no hay variable global de proceso ni raíz de
> composición. El equivalente en Spring es un `@Component` cuyos métodos
> `@CommandHandler` / `@QueryHandler` se escanean y registran.

En Lumen los handlers viven en uno de esos beans — `WalletHandlers`, un
`#[derive(Service)]` cuyo `Ledger` del lado de escritura y `ReadModel` del lado de
lectura son `#[autowired]` — y `#[handlers]` registra cada método en el bus. Esto
es Lumen tal cual:

```rust,ignore
// samples/lumen/src/commands.rs
#[derive(Service)]
struct WalletHandlers {
    #[autowired]
    ledger: Arc<Ledger>,
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletHandlers {
    #[command_handler]
    async fn open_wallet(&self, cmd: OpenWallet) -> Result<WalletView, CqrsError> {
        if cmd.opening_balance < 0 {
            return Err(CqrsError::validation("openingBalance must be >= 0"));
        }
        self.ledger
            .open(&cmd.owner, Money::cents(cmd.opening_balance))
            .await
            .map_err(to_cqrs)
    }

    #[query_handler]
    async fn get_wallet(&self, q: GetWallet) -> Result<WalletView, CqrsError> {
        if let Some(view) = self.read_model.find(&q.id) {
            return Ok(view);
        }
        let events = self.ledger.load_events(&q.id).await.map_err(to_cqrs)?;
        Ok(Wallet::rehydrate(&q.id, &events).view())
    }
    // … deposit / withdraw
}
```

Lo que acaba de pasar: `#[handlers]` es un atributo a **nivel de impl** (como
`#[rest_controller]`) aplicado al bloque `impl` de un bean registrado. Cada método
marcado con `#[command_handler]` / `#[query_handler]` recibe `&self` más un
argumento de mensaje y devuelve `Result<R, CqrsError>`. Por cada marcador la macro
envía un `BeanHandlerRegistration` a un registro de inventory de tiempo de
compilación que, en el arranque, resuelve el bean desde el contenedor e instala un
cierre que lo captura. `FireflyApplication` drena esos registros durante
`register_discovered_handlers`. Así Lumen instala los cuatro handlers *declarando*
el bean y sus métodos — no hay ninguna llamada `register(&bus)` escrita a mano ni
ningún cableado de estado publicado con `OnceLock`.

Por qué importa: el handler alcanza `self.ledger` y `self.read_model` a través de
la inyección del contenedor, así que el mismo handler que conduce un test HTTP es
el mismo handler al que despacha el bus en vivo — un cableado, ejercitado de dos
formas.

> **Note** `#[command_handler]` / `#[query_handler]` también funcionan sobre una
> `async fn(Msg) -> Result<R, CqrsError>` **libre**, en cuyo caso la macro genera
> un auxiliar `register_<fn>(bus)` para un handler simple y sin colaboradores — la
> forma que usa el sample `macro-quickstart`. `#[handlers]` es la forma de **bean**
> para un handler que conecta colaboradores por autowiring, que es el cableado real
> de Lumen. Mismos marcadores, dos formas; Lumen usa la forma de bean.

> **Tip** **Punto de control.** Busca `get_wallet_carries_cache_ttl` en
> `samples/lumen/src/commands.rs`. Afirma que `GetWallet::default().cache_ttl()`
> es `Some(_)` — prueba directa de que `#[firefly(cache_ttl = "30s")]` llegó al
> `Message::cache_ttl` generado. Ejecuta `cargo test -p firefly-sample-lumen
> get_wallet_carries_cache_ttl` y míralo pasar.

## Paso 4 — Los eventos de dominio y el agregado (`domain.rs`)

> **Note** **Término clave — event sourcing.** En *event sourcing* el estado de un
> agregado no se almacena como una fila; es el plegado de un flujo ordenado de
> **eventos de dominio** inmutables. Para cargar una wallet reproduces sus eventos;
> para cambiarla añades un evento nuevo. Cada evento necesita una identidad estable
> para que un flujo persistido siga siendo legible a medida que el esquema
> evoluciona. El equivalente en Spring es un agregado `@EventSourcingHandler` de
> Axon.

`#[derive(DomainEvent)]` estampa cada struct de payload con un discriminador
`EVENT_TYPE` estable (el nombre de su struct) y una conversión `to_domain_event` al
evento de cable del framework. `#[derive(AggregateRoot)]` encuentra el campo
`AggregateRoot` incrustado y genera `Wallet::AGGREGATE_TYPE` más los accesores
`aggregate()` / `aggregate_mut()`:

```rust,ignore
// samples/lumen/src/domain.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct WalletOpened {
    pub wallet_id: String,
    pub owner: String,
    pub opening_balance: i64,
}

#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    pub root: AggregateRoot,   // the framework root — uncommitted-event buffer + version
    pub owner: String,
    pub balance: Money,
    pub opened: bool,
}
```

Lo que acaba de pasar: el único cableado de event sourcing que Lumen escribe a mano
es el plegado `apply` que proyecta un evento en el estado en memoria. Los
discriminadores (`WalletOpened::EVENT_TYPE`, usado cuando el agregado emite (`raise`)
un evento) y la conversión de cable se generan. El argumento
`#[firefly(aggregate_type = "Wallet")]` fija la cadena de tipo del agregado, que la
const generada `Wallet::AGGREGATE_TYPE` expone y que el event store estampa en cada
evento persistido.

Por qué importa: el discriminador da a cada evento una identidad JSON estable y
versionada, así que un flujo persistido hoy sigue siendo decodificable después de
que la struct de payload crezca con campos nuevos mañana — la propiedad de la que
depende el event sourcing.

> **Tip** **Punto de control.** En `domain.rs`, `rehydrate_folds_the_full_stream`
> afirma que `Wallet::AGGREGATE_TYPE == "Wallet"` y pliega un flujo de
> apertura + depósito + retirada de vuelta al balance y la versión correctos. Ese
> único test ejercita ambos derives a la vez.

## Paso 5 — El listener de proyección (`ledger.rs`)

> **Note** **Término clave — proyección.** Una *proyección* es un constructor de
> modelo de lectura: consume eventos de dominio publicados y escribe una vista
> optimizada para consultas. Como reconstruye la vista a partir del flujo de
> eventos (en lugar de mutar una fila a partir de una única entrega), es
> **idempotente** — una reentrega at-least-once converge en la misma vista. El
> equivalente en Spring es un `@Component @EventListener` que actualiza una tabla
> de lectura.

Primero, el propio modelo de lectura es un bean. El `ReadModel` de Lumen es un
componente de acceso a datos `#[derive(Repository)]` (el `@Repository` de Spring) —
un mapa en memoria de id de wallet a `WalletView`, mantenido sin dependencias para
la base de enseñanza:

```rust,ignore
// samples/lumen/src/ledger.rs
#[derive(Debug, Default, Repository)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}
```

`container.scan()` lo registra como un bean singleton, de modo que puede conectarse
por autowiring (como `Arc<ReadModel>`) en los beans handler y de proyección. Un
servicio en producción respaldaría esto con el repositorio reactivo de `firefly`
sobre Postgres; el mapa en memoria mantiene la base sin infraestructura.

La proyección es un **bean listener de EDA** — `WalletProjection`, un
`#[derive(Service)]` que conecta por `#[autowired]` el `Ledger` (para el event
store que reproduce) y el `ReadModel` que alimenta. Dentro de un `impl` con
`#[handlers]`, un método `#[event_listener(topic = "…")]` marca la proyección —
exactamente como el bean de CQRS de arriba, pero el marcador suscribe el método a
un topic de EDA en lugar de al bus:

```rust,ignore
// samples/lumen/src/ledger.rs
#[derive(Service)]
struct WalletProjection {
    #[autowired]
    ledger: Arc<Ledger>,
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletProjection {
    #[event_listener(topic = "wallets.events")]
    async fn project(&self, ev: Event) -> FireflyResult<()> {
        let Some(wallet_id) = ev.headers.get("aggregateId") else {
            return Ok(());
        };
        // reload the wallet's stream, fold to a WalletView, upsert — idempotent.
        if let Ok(events) = self.ledger.store().load(wallet_id).await {
            let view = Wallet::rehydrate(wallet_id, &events).view();
            self.read_model.upsert(view);
        }
        Ok(())
    }
}
```

Lo que acaba de pasar: `#[handlers]` envía un `BeanListenerRegistration` a inventory
que, en el arranque, resuelve el bean desde el contenedor y suscribe su método a
`wallets.events` en el mismo broker al que publica el ledger. `FireflyApplication`
lo drena durante `subscribe_discovered_listeners`. La suscripción que cierra el
bucle de CQRS — el lado de escritura añade y publica, el lado de lectura proyecta —
queda por tanto cableada enteramente a través del contenedor de DI, sin ninguna
llamada `subscribe(&broker)` en ninguna raíz de composición.

> **Note** Como los marcadores de CQRS, `#[event_listener(topic = "…")]` también
> funciona sobre una `async fn(Event) -> FireflyResult<()>` **libre**, generando un
> auxiliar `subscribe_<fn>(broker)` para un listener simple y sin colaboradores.
> `#[handlers]` es la forma de **bean** para una proyección que conecta
> colaboradores por autowiring — el cableado real de Lumen.

> **Tip** **Punto de control.** El test HTTP `open_then_get_round_trips_through_cqrs`
> (en `http_test.rs`) abre una wallet por `POST /api/v1/wallets`, luego la lee por
> `GET /api/v1/wallets/:id` y ve el balance proyectado — prueba de que el bean
> listener se suscribió y el bucle se cerró. Arranca el `FireflyApplication`
> completo, así que ejercita el drenaje del inventory de extremo a extremo.

## Paso 6 — El controlador (`web.rs`)

> **Note** **Término clave — controlador REST.** Un *controlador REST* es un bean
> cuyos métodos mapean verbos y rutas HTTP a funciones handler. En Firefly los
> cuerpos de los handlers son handlers de `axum` ordinarios; la macro genera el
> router y lo monta. El equivalente en Spring es `@RestController` con
> `@GetMapping` / `@PostMapping`.

`#[rest_controller(path = "…")]` convierte un bloque `impl` en un
`WalletApi::routes(state) -> axum::Router` generado. El propio tipo del controlador
es un bean `#[derive(Controller)]` cuyos colaboradores son `#[autowired]`:

```rust,ignore
// samples/lumen/src/web.rs
#[derive(Clone, Controller)]
pub struct WalletApi {
    #[autowired]
    pub bus: Arc<Bus>,
    #[autowired]
    pub ledger: Arc<Ledger>,
    #[autowired]
    pub query_cache: Arc<QueryCache>,
}
```

Cada método lleva un mapeo de un verbo, usa extractores ordinarios de axum y
devuelve `WebResult<T>` para que un error del handler se renderice como
`application/problem+json` según RFC 9457. Los atributos de verbo también llevan
los metadatos OpenAPI (`summary`, `description`, `status`, `tags`) que lee el
generador de documentación:

```rust,ignore
// samples/lumen/src/web.rs
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletApi {
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

    #[get("/wallets/:id", summary = "Fetch a wallet")]
    async fn get(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<WalletView>> {
        let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
        Ok(Json(view))
    }
    // … deposit / withdraw / transfer / compliance / 2pc
}
// generated: WalletApi::routes(state) -> axum::Router
```

Lo que acaba de pasar: la macro emite `WalletApi::routes(state)` **y** envía un
`ControllerMount` más un descriptor por ruta a tablas de tiempo de enlazado. Así
`FireflyApplication` **monta automáticamente** el controlador (resolviendo su
estado autowired desde el contenedor a través de
`firefly::web::mount_controllers`), y el generador de OpenAPI y el endpoint
`/mappings` del actuator pueden enumerar las rutas de Lumen sin volver a parsear el
fuente. Lumen nunca entrega `WalletApi::routes(state)` a la pila web — declarar el
bean controlador es todo el cableado.

Por qué importa: `WebResult<T>` renderiza cualquier error del handler como un cuerpo
de problema RFC 9457 de manera uniforme, así que el moldeado de errores es el mismo
en cada endpoint sin código por handler, y el parámetro de ruta `:id` es el extractor
`Path` ordinario de axum — Firefly no inventa su propio enrutamiento.

> **Tip** **Punto de control.** Ejecuta Lumen (`cargo run -p firefly-sample-lumen`)
> y abre `http://localhost:8081/swagger-ui` en el puerto de **gestión**. El resumen
> "Open a wallet", la respuesta `201` y la etiqueta `Wallets` provienen todos de los
> atributos de verbo de arriba — sin un archivo de especificación aparte.

## Paso 7 — El latido programado (`housekeeping.rs`)

> **Note** **Término clave — tarea programada.** Una *tarea programada* es una
> `async fn` sin argumentos que el framework ejecuta con una cadencia — una
> frecuencia fija, un retardo fijo o una expresión cron. El equivalente en Spring
> es `@Scheduled`.

`#[scheduled(...)]` genera un auxiliar `schedule_<fn>(scheduler)` que registra la
función en un `Scheduler`, y también envía un `ScheduledRegistration` que el
framework drena:

```rust,ignore
// samples/lumen/src/housekeeping.rs
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> {
    HEARTBEAT_TICKS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
// generated: schedule_ledger_heartbeat(&scheduler)
```

Lo que acaba de pasar: `#[scheduled]` emite el auxiliar `schedule_<fn>(scheduler)`
*y* el registro. En el arranque el framework llama a
`register_discovered_scheduled(&scheduler)`, que drena el inventory e instala cada
tarea `#[scheduled]` — así Lumen nunca llama a `schedule_<fn>` a mano. Usa
`fixed_rate = "60s"` para una cadencia fija (con un `initial_delay` opcional), o
`cron = "…"` para una expresión cron.

> **Tip** **Punto de control.** `scheduled_task_registers` (en `housekeeping.rs`)
> construye un scheduler nuevo, llama a `register_discovered_scheduled` y afirma que
> `scheduler.tasks()` contiene `"ledger_heartbeat"` — prueba de que el registro se
> drenó del inventory sin ninguna llamada manual a `schedule_<fn>`.

## Paso 8 — El trío de orquestación (`transfer.rs`, `compliance.rs`, `tcc_transfer.rs`)

Tres macros declarativas de orquestación completan el propio fuente de Lumen. Cada
una convierte un bloque `impl` anotado en un coordinador ejecutable. Conocemos
primero los términos clave, luego leemos una declaración de cada una.

> **Note** **Término clave — saga.** Una *saga* es una transacción distribuida
> compuesta de pasos, cada uno con una **compensación** que lo deshace si un paso
> posterior falla. No hay bloqueo compartido; la consistencia se restaura
> ejecutando las compensaciones en orden inverso. El equivalente en Spring/Axon es
> una `@Saga`.

Una transferencia *no* es un único comando atómico: debita el origen, luego
acredita el destino, y si el abono falla el débito debe reembolsarse. Ese es el
patrón saga, declarado como métodos anotados en `TransferSaga`:

```rust,ignore
// samples/lumen/src/transfer.rs
#[firefly::saga(name = "money-transfer")]
impl TransferSaga {
    #[saga_step(id = "debit", compensate = "refund_debit")]
    async fn debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.withdraw(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    async fn refund_debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    #[saga_step(id = "credit", depends_on = ["debit"])]
    async fn credit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.to, Money::cents(req.amount)).await?;
        Ok(())
    }
}
// generated: TransferSaga::run(req) and TransferSaga::saga()
```

Lo que acaba de pasar: `#[firefly::saga]` reduce estos métodos sobre el motor `Saga`
de `firefly-orchestration`. `depends_on` ordena los pasos, `compensate` nombra el
método de reversión, y cada parámetro se inyecta desde el contexto de la saga —
aquí la petición, vía `#[input]`. La macro genera `TransferSaga::run`, al que llama
`run_transfer`. Cuando la pata de abono falla, el motor ejecuta `refund_debit`, así
que el flujo del origen muestra un débito real *y* su reembolso compensatorio.

> **Note** **Término clave — workflow.** Un *workflow* es un grafo acíclico dirigido
> (DAG) de pasos: los pasos independientes se ejecutan en paralelo; un paso con
> `depends_on` se ejecuta solo después de sus prerrequisitos y lee sus resultados
> vía `#[from_step]`. Donde una saga es una cadena lineal con compensación, un
> workflow es un fan-in paralelo. El equivalente en Spring es un proceso basado en
> DAG como el grafo de tareas de Spring Cloud Data Flow.

La comprobación de cumplimiento de Lumen ejecuta `balance-check` y `limit-check` en
paralelo, luego `approve` tras ambas:

```rust,ignore
// samples/lumen/src/compliance.rs
#[firefly::workflow(name = "transfer-compliance")]
impl ComplianceCheck {
    #[workflow_step(id = "balance-check")]
    async fn balance_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> { /* … */ }

    #[workflow_step(id = "limit-check")]
    async fn limit_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> { /* … */ }

    #[workflow_step(id = "approve", depends_on = ["balance-check", "limit-check"])]
    async fn approve(
        &self,
        #[from_step("balance-check")] funds_ok: bool,
        #[from_step("limit-check")] within_limit: bool,
    ) -> Result<(), ComplianceError> { /* … */ }
}
// generated: ComplianceCheck::run(req)
```

> **Note** **Término clave — TCC (Try / Confirm / Cancel).** *TCC* es una
> transacción distribuida en dos fases: cada participante primero **reserva**
> (try); solo cuando todas las reservas tienen éxito el coordinador las
> **confirma** (confirm); de lo contrario **cancela** (cancel) las ya intentadas.
> Donde una saga deshace una pata ya confirmada, TCC reserva primero y confirma al
> final. El equivalente en Spring/Seata es el modo de transacción TCC.

La transferencia en dos fases de Lumen retiene el origen, verifica el destino y
luego captura en ambos lados:

```rust,ignore
// samples/lumen/src/tcc_transfer.rs
#[firefly::tcc(name = "transfer-2pc")]
impl TwoPhaseTransfer {
    #[participant(name = "source", confirm = "capture_source", cancel = "release_source")]
    async fn hold_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* withdraw (hold) */ }
    async fn capture_source(&self) -> Result<(), DomainError> { Ok(()) }           // the debit was the capture
    async fn release_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* deposit (release) */ }

    #[participant(name = "dest", confirm = "capture_dest")]
    async fn hold_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* verify exists */ }
    async fn capture_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* deposit (capture) */ }
}
// generated: TwoPhaseTransfer::run(req)
```

Lo que acaba de pasar en las tres: cada macro lee sus métodos anotados y genera un
método `run` sobre el motor de orquestación, cableando el grafo de
pasos/participantes, la inyección de parámetros (`#[input]` / `#[from_step]`) y la
ruta de compensación o cancelación. Tú escribes los cuerpos; la macro escribe el
coordinador.

> **Tip** **Punto de control.** Los tests HTTP
> `transfer_saga_overdraft_compensates_and_is_422`,
> `compliance_workflow_rejects_overdraft_with_422` y
> `tcc_transfer_overdraft_releases_the_hold_and_is_422` (en `http_test.rs`)
> ejercitan la ruta de fallo de cada macro de extremo a extremo. Ejecuta
> `cargo test -p firefly-sample-lumen overdraft` y mira pasar las tres.

## Paso 9 — El contenedor de configuración y el contribuidor de streaming (`web.rs`)

Lumen *sí* usa el contenedor de DI directamente. `web.rs` lleva un contenedor
`#[derive(Configuration)]` cuyos métodos factoría `#[bean]` **declaran** los beans
de infraestructura:

> **Note** **Término clave — contenedor de configuración y factoría de bean.** Un
> *contenedor de configuración* es un tipo `#[derive(Configuration)]` cuyos métodos
> `#[bean]` son *factorías*: cada uno devuelve un valor construido que el contenedor
> registra como un bean y puede conectar por autowiring en otra parte. El
> equivalente en Spring es una clase `@Configuration` cuyos métodos `@Bean`
> producen beans.

```rust,ignore
// samples/lumen/src/web.rs
#[derive(Configuration)]
struct LumenBeans;

#[bean]
impl LumenBeans {
    #[bean]
    fn event_store(&self) -> MemoryEventStore { MemoryEventStore::new() }

    #[bean]
    fn query_cache(&self) -> QueryCache { QueryCache::new() }

    #[bean]
    fn jwt_service(&self) -> JwtService { JwtService::new(crate::security::DEMO_SIGNING_KEY) }

    #[bean]
    fn security_filter_chain(&self) -> FilterChain { crate::security::security_layers().1 }

    #[bean]
    fn bearer_layer(&self) -> BearerLayer { crate::security::security_layers().0 }

    #[bean]
    fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

Lo que acaba de pasar: `container.scan()` descubre y registra cada método `#[bean]`,
así que `build_app` no llama a ningún `register_arc` — el **framework** hace el
registro. La factoría `ledger` incluso *conecta sus propios argumentos por
autowiring* (`store` y el port `Broker` provisto por el framework), así que una
factoría de bean es en sí misma un punto de cableado. Los beans
`security_filter_chain` y `bearer_layer` se descubren automáticamente y se
estratifican sobre la API sin ninguna llamada `.security(...)` — el patrón
`SecurityFilterChain` de Spring. Toda la mecánica de DI está en el
[análisis profundo de inyección de dependencias](./04a-dependency-injection.md).

El endpoint de streaming opcional muestra una costura declarativa más — un bean
`RouteContributor`:

> **Note** **Término clave — contribuidor de rutas.** Un *contribuidor de rutas* es
> un bean que entrega al framework un `axum::Router` extra para fusionarlo en la API
> pública. Es la forma de añadir rutas que no encajan en la forma de
> `#[rest_controller]` (aquí, un stream reactivo con feature gate) *declarando un
> bean* en lugar de tocar una raíz de composición.

```rust,ignore
// samples/lumen/src/web.rs  (feature `streaming`)
#[cfg(feature = "streaming")]
#[derive(Service)]
#[firefly(provides = "dyn firefly::web::RouteContributor")]
struct StreamingRoutes {
    #[autowired]
    api: Arc<WalletApi>,
}

#[cfg(feature = "streaming")]
impl firefly::web::RouteContributor for StreamingRoutes {
    fn routes(&self) -> axum::Router {
        streaming_router((*self.api).clone())
    }
}
```

Lo que acaba de pasar: `#[firefly(provides = "dyn firefly::web::RouteContributor")]`
indica al contenedor que registre este `#[derive(Service)]` bajo el port
`RouteContributor`. El framework lo descubre y fusiona sus rutas — un endpoint
`GET /api/v1/wallets/:id/events` con feature gate cableado declarando un bean, no
mediante una raíz de composición.

> **Tip** **Punto de control.** Abre `samples/lumen/src/web.rs` y confirma que
> `build_router()` (la costura de test) es simplemente
> `FireflyApplication::new(APP_NAME).version(VERSION).bootstrap().await…
> .api_router` — sin un constructor escrito a mano. Cada bean de este paso lo
> autorregistra `container.scan()`; el controlador se monta automáticamente; la
> seguridad y el middleware de caché de lectura se descubren automáticamente.

## Paso 10 — El resto del conjunto declarativo (no usado por Lumen)

Varias macros más son partes de primera clase del framework que Lumen **no**
ejercita en su propio fuente — usa event sourcing (así que el `#[firefly::repository]`
/ `#[firefly::transactional]` relacionales nunca aparecen) y gestiona las
preocupaciones transversales restantes por otros medios. Cada una se muestra aquí
como un ejemplo independiente, enfocado y correcto, para que el catálogo esté
completo.

| Macro | Propósito | Genera |
|-------|---------|-----------|
| `#[derive(Builder)]` | un constructor fluido con campos obligatorios/por defecto | `T::builder()` → setters fluidos → `build() -> Result<T, String>` |
| `#[derive(Mapper)]` | conversión struct-a-struct en tiempo de compilación | un `From<Source>` por cada `#[firefly(from = "…")]` |
| `#[derive(Entity)]` | el mapeo `@Entity` a partir de campos de struct anotados | un `impl` de `SqlxEntity` (`@Table` / `@Id` / `@Version` / `@Column`) |
| `#[derive(SqlxRepository)]` | un bean `@Repository` de sqlx totalmente cableado | impls de `ReactiveCrudRepository` **y** `ReactiveSpecificationRepository` más el accesor `repository()` |
| `#[firefly::repository]` | cuerpos de método de consulta derivada y consulta personalizada | cuerpos de método sobre un `impl` de `SqlxReactiveRepository` a partir de nombres de método o `#[query(…)]` |
| `#[firefly::transactional]` | un límite de transacción declarado | un límite commit-en-`Ok` / rollback-en-`Err` alrededor de una `async fn` |
| `#[firefly::pre_authorize]` / `#[firefly::post_authorize]` | control de acceso a nivel de método | una comprobación de acceso antes del cuerpo, o una comprobación de returnObject después |
| `#[derive(Validate)]` (+ `Valid<T>`) | validación de bean JSR-380 | un `impl Validate`; el extractor `Valid<T>` rechaza un fallo de restricción con 422 |
| `#[cacheable]` / `#[cache_put]` / `#[cache_evict]` | caché declarativa | un cuerpo read-through / write-through / evict alrededor del adaptador de caché registrado |
| `#[retry]` / `#[circuit_breaker]` / `#[rate_limit]` / `#[bulkhead]` / `#[timeout]` | decoradores de resiliencia | el cuerpo envuelto en la primitiva `firefly_resilience` correspondiente |
| `#[async_method]` | async fire-and-forget | una `async fn(self: Arc<Self>, …) -> R` reescrita a una `fn … -> TaskHandle<R>` no async |
| `#[application_event_listener]` / `#[transactional_event_listener]` | eventos en proceso | un `@EventListener` / `@TransactionalEventListener` descubierto vía inventory |
| `#[aspect]` (+ `#[before]`/`#[after]`/`#[around]`) | consejo orientado a aspectos | `impl firefly_aop::Aspect` + un registro de inventory |

Los derives de estereotipo de DI restantes completan el conjunto:
`#[derive(Component/Service/Repository/Configuration/AutoConfiguration/Controller)]`,
`#[bean]`, `#[autowired]`, `register_all!` y `#[derive(ConfigProperties)]`.
`#[derive(AutoConfiguration)]` es el contenedor de autoconfiguración cuyos `#[bean]`
se retiran tras un `condition_on_missing_bean`, de modo que una aplicación puede
sobrescribir cualquier valor por defecto declarando su propio bean del mismo tipo;
`Container::scan()` autorregistra cada método `#[bean]`, y
`Container::scan_packages([..])` restringe el descubrimiento a rutas de módulo
nombradas.

### Construcción — el builder fluido (`#[derive(Builder)]`)

Los derives de la stdlib de Rust ya cubren el código repetitivo de los objetos de
valor — `Debug`, `Clone`, `PartialEq`, `Default`. El único hueco ergonómico que
dejan es un *builder fluido*, y eso es lo que rellena `#[derive(Builder)]` (el
`@Builder` de Lombok). Genera `T::builder()` que devuelve un `TBuilder` con un
setter por campo y un `build() -> Result<T, String>`. Por defecto cada campo es
**obligatorio**: `build` devuelve un `Err` nombrando el primer campo sin asignar.
`#[builder(default)]` recurre a `Default::default()`, `#[builder(default = "expr")]`
a una expresión personalizada y `#[builder(into)]` hace que el setter acepte
`impl Into<FieldTy>`. El `OpenWallet` de Lumen (del
[Paso 3](#step-3--cqrs-messages-and-their-handler-bean-commandsrs)) lo lleva:

```rust,ignore
let cmd = OpenWallet::builder()
    .owner("ada")            // impl Into<String>
    .opening_balance(10_000)
    .build()?;               // Result<OpenWallet, String>
```

Devolver un `Result` mantiene la gestión de campos faltantes en la ruta normal de
`?` en lugar de un panic. Recurre a `#[derive(Builder)]` cuando una struct tiene
muchos campos opcionales/por defecto; mantén un literal simple cuando todos los
campos son obligatorios y están presentes.

### Conversión — el mapper de tiempo de compilación (`#[derive(Mapper)]`)

`#[derive(Mapper)]` genera un `From<Source>` de tiempo de compilación, comprobado en
tipos, que mapea una struct origen a un destino campo a campo. Un
`#[firefly(from = "Source")]` produce un `impl` de `From`, y el atributo es
**repetible** para mapear desde varios orígenes. Los atributos por campo ajustan el
mapeo: `#[firefly(rename = "src")]` lee un campo origen con nombre distinto,
`#[firefly(into)]` aplica `.into()`, `#[firefly(with = "fn")]` ejecuta una función
de conversión, y `#[firefly(default)]` / `#[firefly(default_expr = "expr")]`
rellenan un campo destino sin lectura de origen:

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize, Mapper)]
#[firefly(from = "Wallet")]
pub struct WalletView {
    #[firefly(rename = "root", with = "aggregate_id")]  // read src.root, run aggregate_id(..)
    pub id: String,
    pub owner: String,                        // same name on both ends: a plain move
    #[firefly(with = "Money::cents_value")]   // src.balance: Money -> i64 via a fn
    pub balance: i64,
    #[firefly(default)]                       // version set by the projector, not the fold
    pub version: i64,
}
// generates: impl From<Wallet> for WalletView { fn from(src: Wallet) -> Self { … } }
```

Como el código generado es un `impl` de `From` corriente, cada campo lo comprueba el
compilador sin coste en tiempo de ejecución — esa garantía de tiempo de compilación
es todo el sentido. Contrástalo con el `firefly_data::Mapper` de **tiempo de
ejecución**, que convierte mediante dos pasadas de serde: usa el mapper de tiempo de
ejecución cuando el tipo origen no se conoce hasta tiempo de ejecución (mapeo de JSON
arbitrario), y prefiere `#[derive(Mapper)]` siempre que ambos extremos sean tipos
concretos.

> **Note** El `WalletView` real de Lumen lo construye un método `Wallet::view`
> escrito a mano (en `domain.rs`) en lugar de `#[derive(Mapper)]`; el listado de
> arriba es la forma declarativa equivalente, mostrada para ilustrar la macro.

### Persistencia — entidades, repositorios y transacciones (relacional)

Estas macros se sitúan en la ruta de persistencia relacional. El modelo de lectura
de Lumen es una proyección en memoria sobre un flujo de eventos, así que no usa
ninguna de ellas — pero en un servicio relacional son las herramientas del día a
día. La referencia completa es el [capítulo de Persistencia](./07-persistence.md).

`#[derive(Entity)]` genera el mapeo `SqlxEntity` (`@Table` / `@Id` / `@Version` /
`@Column`) a partir de campos anotados. Los campos escalares se mapean
automáticamente; un campo no escalar usa
`#[firefly(with(read = "path", write = "path"))]`:

```rust,ignore
#[derive(Debug, Clone, Entity)]
#[firefly(table = "accounts")]
pub struct Account {
    #[firefly(id)]
    pub id: String,
    pub owner: String,
    pub status: String,
    #[firefly(version)]
    pub version: i64,
}
```

`#[derive(SqlxRepository)]` construye un bean `@Repository` totalmente cableado a
partir del datasource `Db` inyectado (vía `repository_for`). Implementa tanto
`ReactiveCrudRepository` (la superficie `save` / `find_by_id` / `delete_by_id` /
`count`) **como** `ReactiveSpecificationRepository` (`find_by_spec`, el equivalente
de `JpaSpecificationExecutor`) por delegación, y expone el accesor `repository()`
sobre el que se construye `#[firefly::repository]`:

```rust,ignore
#[derive(SqlxRepository)]
#[firefly(entity = "Account")]
pub struct AccountRepo {
    db: Arc<Db>,
}
```

`#[firefly::repository]` convierte un nombre de método `find_by_…` / `count_by_…` /
`exists_by_…` / `delete_by_…` en un cuerpo de consulta funcional. El método de
tiempo de ejecución se elige a partir del **tipo de retorno** (`Vec<T>` / `Option<T>`
→ find, `i64` → count, `bool` → exists, `u64` → delete); los cuerpos placeholder
`unimplemented!()` se descartan:

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    async fn find_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }
    async fn find_by_iban(&self, iban: &str)     -> Result<Option<Account>, DataError> { unimplemented!() }
    async fn count_by_owner(&self, owner: &str)  -> Result<i64, DataError>          { unimplemented!() }
    async fn exists_by_email(&self, email: &str) -> Result<bool, DataError>         { unimplemented!() }
}
```

Da a un método `find_by_…` un argumento `Pageable` final (y un retorno
`Result<Vec<T>, DataError>`) y el cuerpo generado añade la ordenación y la ventana
de la página, delegando en `find_by_derived_paged`. Ten en cuenta que `Pageable::of`
devuelve un `Result`:

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    async fn find_by_owner(&self, owner: &str, page: Pageable)
        -> Result<Vec<Account>, DataError> { unimplemented!() }
}

// Build the page (1-based index) with sort + window — `of` returns a Result:
let page = Pageable::of(1, 20, RequestSort::of([Order::desc("id")])).unwrap();
let rows = repo.find_by_owner("ada", page).await?;
```

Cuando una consulta derivada del nombre no basta, anota un stub con `#[query(...)]` y
escribe la sentencia directamente. El SQL nativo enlaza cada placeholder `:name` al
argumento llamado `name`; el **tipo de retorno** sigue seleccionando la operación —
`Vec<T>` / `Option<T>` es una lista, `i64` un conteo, `bool` una comprobación de
existencia y `u64` una sentencia modificadora (que devuelve las filas afectadas):

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    #[query("SELECT id, owner FROM accounts WHERE status = :status ORDER BY id DESC")]
    async fn active_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }

    #[query("UPDATE accounts SET status = :status WHERE id = :id")]
    async fn set_status(&self, id: &str, status: &str) -> Result<u64, DataError> { unimplemented!() }
}
```

`#[query(sql = "…")]` es la grafía explícita de la forma nativa, y
`#[query(jpql = "…", entity = "Account")]` escribe la sentencia contra nombres de
entidad.

> **Note** **Término clave — límite de transacción.** Un *límite de transacción* es
> una región de código cuyo trabajo de base de datos se confirma junto o revierte
> junto. `#[firefly::transactional]` convierte ese límite en una declaración sobre
> una `async fn`. El equivalente en Spring es `@Transactional`.

`#[firefly::transactional]` envuelve el cuerpo de una `async fn` en una transacción
gobernada por el `TransactionManager` registrado — commit en `Ok`, rollback en
`Err`. La función debe ser `async`, debe devolver `Result<T, E>`, y su tipo de error
debe implementar `From<firefly_transactional::TxError>` para que los fallos de
begin/commit afloren a través de `?`. Pelada, o con opciones:

```rust,ignore
#[firefly::transactional]
async fn open_account(repo: &AccountRepo, acct: Account) -> Result<(), DataError> {
    repo.insert(&acct).await?;        // committed together on Ok,
    repo.insert_audit(&acct).await?;  // rolled back together on Err
    Ok(())
}

#[firefly::transactional(propagation = "requires_new", isolation = "serializable", read_only = false, timeout_ms = 5000)]
async fn reconcile(repo: &LedgerRepo) -> Result<(), DataError> { /* … */ }
```

Por defecto el límite se ejecuta a través del `TransactionManager` registrado
**global del proceso**. `manager = "<expr>"` (el `@Transactional("txManager")` de
Spring) lo enlaza en cambio a un manager **explícito** que el servicio posee — la
expresión produce un valor `m` con `&m: &Arc<dyn TransactionManager>`. Úsalo para un
servicio multi-datasource, o para mantener el aislamiento por instancia/por test. El
caso de uso `transfer` del sample `lumen-ledger` está cableado exactamente así:

```rust,ignore
// samples/lumen-ledger — core/src/services/wallet/v1/wallet_service_impl.rs
#[firefly::transactional(manager = "self.tx_manager()")]   // self owns the manager
async fn transfer_tx(&self, from: Uuid, to: Uuid, amount: i64) -> Result<WalletResponse, ServiceError> {
    let mut src = self.load_active(from).await?;            // debit + credit commit
    let mut dst = self.load_active(to).await?;              // together, or roll back
    src.balance -= amount; let saved = self.persist(src).await?;
    dst.balance += amount; self.persist(dst).await?;
    Ok(saved)
}
```

Dos opciones adicionales controlan qué errores provocan rollback, ambas
deliberadamente *no* llamadas `rollback_for` (el `rollbackFor` de Spring es una
trampa porque su caso límite de ya-marcado-como-rollback-only sorprende a la gente):

- `no_rollback_for = "<pat>"` — el `@Transactional(noRollbackFor = …)` de Spring:
  cuando el `Err` coincide con el patrón, el límite **confirma** en lugar de
  revertir.
- `rollback_only_for = "<pat>"` — revierte **solo** cuando el `Err` coincide con el
  patrón, confirmando ante cualquier otro error. El patrón es un patrón estilo match
  sobre el tipo de error de la función, con alternativas permitidas:
  `no_rollback_for = "Error::A | Error::B"`. Con ambos, `no_rollback_for` gana en
  caso de solapamiento.

```rust,ignore
#[firefly::transactional(no_rollback_for = "DataError::NotFound(_)")]
async fn upsert(repo: &AccountRepo, acct: Account) -> Result<(), DataError> { /* … */ }
```

Estas dos macros relacionales son la contraparte de cómo Lumen logra la consistencia
*sin* un gestor de transacciones: añade eventos al `EventStore` bajo concurrencia
optimista y los proyecta, en lugar de mutar filas dentro de un límite
`#[transactional]`. Mismo objetivo — escrituras atómicas y consistentes — alcanzado
mediante dos arquitecturas diferentes.

### Seguridad de método — `#[pre_authorize]` / `#[post_authorize]`

Dos macros imponen el control de acceso en el límite del método, leyendo la
identidad del llamante del contexto de seguridad ambiental en lugar de de una
`Request`. El tratamiento completo está en el
[capítulo de Seguridad](./14-security.md).

`#[firefly::pre_authorize(...)]` ejecuta una comprobación de acceso **antes** del
cuerpo. Aplícala a una `fn` que devuelva `Result<T, E>` cuyo error implemente
`From<firefly_security::SecurityError>`, de modo que una denegación viaje por la ruta
de `?`:

```rust,ignore
#[firefly::pre_authorize]                              // `authenticated` — any caller in scope
async fn whoami() -> Result<Profile, AppError> { /* … */ }

#[firefly::pre_authorize(role = "ADMIN")]              // a single role
async fn close_books(&self) -> Result<(), AppError> { /* … */ }

#[firefly::pre_authorize(any_role = ["TELLER", "ADMIN"])]
async fn open_account(&self, req: OpenAccount) -> Result<Account, AppError> { /* … */ }

#[firefly::pre_authorize(authority = "wallet:write")]  // a single fine-grained authority
async fn deposit(&self, id: &str, cents: i64) -> Result<(), AppError> { /* … */ }
```

Cuando no hay ningún llamante en el ámbito el cuerpo se omite y la macro devuelve
`Err(SecurityError::Unauthenticated.into())`; cuando hay un llamante presente pero le
falta el rol/autoridad requerido devuelve `Err(SecurityError::Forbidden.into())`.

`#[firefly::post_authorize(<bool expr>)]` se ejecuta **después** de que una
`async fn` retorne y filtra el valor según una expresión booleana que ve `result`
(una `&T` al valor devuelto) y `auth` (una `&Authentication`); si es `false` el
valor se descarta y la llamada devuelve `Forbidden`:

```rust,ignore
// Only return the wallet if the caller owns it.
#[firefly::post_authorize(result.owner == auth.subject())]
async fn get_wallet(&self, id: &str) -> Result<WalletView, AppError> { /* … */ }
```

Como `BearerLayer` delimita el ámbito de la autenticación para toda la llamada
descendente, estas comprobaciones funcionan sobre un método de servicio que nunca ve
la `Request` — la macro lee del ámbito, no de un argumento del handler.

### Validación — `#[derive(Validate)]` y `Valid<T>`

`#[derive(Validate)]` genera un `impl Validate` que ejecuta la restricción
`#[validate(email/url/not_empty/length/range/pattern/custom)]` de cada campo, y el
extractor web `Valid<T>` rechaza un fallo de restricción con `422`:

```rust,ignore
#[derive(Debug, Deserialize, Validate)]
struct CreateUser {
    #[validate(not_empty, length(min = 2, max = 64))]
    name: String,
    #[validate(email)]
    email: String,
}

// In a controller, `Valid<CreateUser>` returns 422 if any constraint fails:
async fn create(Valid(body): Valid<CreateUser>) -> WebResult<Json<UserView>> { /* … */ }
```

### Caché, async, eventos en proceso y aspectos

`#[cacheable]` / `#[cache_put]` / `#[cache_evict]` envuelven el cuerpo de un método
en una ruta read-through / write-through / evict alrededor del adaptador de caché
registrado en el proceso. `#[cacheable]` también acepta `condition = "<bool expr>"`
(saltar la caché cuando la expresión del parámetro es `false`) y
`unless = "<bool expr>"` (no almacenar cuando la expresión del resultado — enlazada
como `result: &V` — es `true`):

```rust,ignore
#[cacheable(key = "format!(\"order:{}\", id)", unless = "result.is_empty()")]
async fn load_order(&self, id: &str) -> Result<Order, DataError> { /* … */ }
```

`#[async_method]` reescribe una `async fn(self: Arc<Self>, …) -> R` en una
`fn … -> TaskHandle<R>` no async que lanza el cuerpo sobre el ejecutor registrado —
fire-and-forget, con un handle para esperarlo después.

`#[application_event_listener]` / `#[transactional_event_listener]` son los listeners
de eventos en proceso (el `@EventListener` / `@TransactionalEventListener` de
Spring): cada uno se descubre vía inventory y lo dispara `publish_event`, el
transaccional enlazado a una fase de commit.

`#[aspect]` (con consejos `#[before]` / `#[after]` / `#[around]`) genera un
`impl firefly_aop::Aspect` más un registro de inventory; el consejo se ejecuta
alrededor del punto de tejido explícito `advised(…)`.

### Decoradores de resiliencia

Donde las primitivas de `firefly_resilience` son la superficie de construirlo-tú-mismo
(`Retry::new().max_attempts(3).execute(op)`), cinco macros **decoradoras** ponen las
mismas guardas sobre un método — los equivalentes de Resilience4j / Spring-Retry:

```rust,ignore
#[firefly::retry(max_attempts = 4, delay = "100ms", backoff = 2.0, max_delay = "2s")]
async fn fetch_quote(&self) -> Result<Quote, IntegrationError> { /* … */ }

#[firefly::circuit_breaker(failure_threshold = 5, open_duration = "30s")]
async fn call_upstream(&self) -> Result<Reply, IntegrationError> { /* … */ }

#[firefly::rate_limit(rate = 100.0, burst = 20)]    // 100/s, bucket of 20
async fn search(&self, q: &str) -> Result<Hits, SearchError> { /* … */ }

#[firefly::bulkhead(20)]                              // ≤ 20 calls in flight
async fn render(&self, doc: &Doc) -> Result<Pdf, RenderError> { /* … */ }

#[firefly::timeout("2s")]
async fn slow_report(&self) -> Result<Report, ReportError> { /* … */ }
```

Aplícalas a una `async fn` que devuelva `Result<T, E>` cuyo error implemente
`std::error::Error + Send + Sync + 'static + From<firefly_resilience::ResilienceError>`.
El decorador conduce el propio fallo del cuerpo a través de la primitiva y recupera
la **`E` original** a la salida, mientras que el cortocircuito propio de una guarda
(un timeout, un circuito abierto, un rechazo) aflora a través de
`E::from(ResilienceError)`. Los atributos se **apilan**, el más externo primero:

```rust,ignore
#[firefly::retry(max_attempts = 3, delay = "50ms")]   // outer: re-runs the call
#[firefly::circuit_breaker(failure_threshold = 5)]    // inner: trips on a failing dep
async fn call_upstream(&self) -> Result<Reply, IntegrationError> { /* … */ }
```

Las guardas con estado (`#[circuit_breaker]`, `#[rate_limit]`, `#[bulkhead]`)
mantienen su estado en un `static` por método, compartido entre cada llamada — la
semántica de bean de registro de Resilience4j; `#[retry]` y `#[timeout]` son sin
estado y se reconstruyen en cada llamada. Las duraciones aceptan una cadena con
sufijo de unidad (`"100ms"`, `"2s"`, `"1m"`) o un entero pelado de milisegundos.

### El cliente HTTP saliente — `#[http_client]`

`#[http_client]` es el cliente declarativo de interfaz HTTP (el `@HttpExchange` de
Spring). Aplicado a un `trait`, emite el trait tal cual **y** una struct
`<Trait>Impl` que envuelve un `WebClient` e implementa el trait traduciendo el
atributo de verbo de cada método y la ruta estilo `:id` en una llamada. La forma
esperada `async fn -> Result<T, ClientError>` decodifica el cuerpo, aflora un 404
como `ClientError::Problem` y admite un error personalizado vía
`E: From<ClientError>`; los retornos no esperados `Mono<T>` / `Flux<T>` afloran el
`ClientError` crudo sin cambios:

```rust,ignore
#[http_client(path = "/api/v1/orders")]
trait OrderClient {
    #[get("/:id")]
    async fn get_order(&self, id: String) -> Result<Order, ClientError>;

    #[get("/")]
    async fn list(&self, status: String, page: Option<u32>) -> Result<Vec<Order>, ClientError>;

    #[post("/")]
    async fn create(&self, body: NewOrder) -> Result<Order, ClientError>;

    #[get("/opt/:id")]
    async fn find_opt(&self, id: String) -> Result<Option<Order>, ClientError>;
}
// generated: struct OrderClientImpl { … }  impl OrderClient for OrderClientImpl { … }
```

> **Tip** **Punto de control.** No ejecutarás ninguno de los ejemplos del Paso 10
> contra Lumen — son entradas de catálogo, no fuente de Lumen. La prueba de fuego es
> el siguiente paso: confirmar que todas las macros de *Lumen* compilan y pasan.

## Paso 11 — Cómo aterriza realmente el cableado: el contrato `__rt` y el drenaje del inventory

Ya has visto cada macro que Lumen usa. La última pieza es *por qué declarar un bean
es todo el cableado*. Dos mecanismos lo hacen funcionar.

Primero, la **ruta del contrato `__rt`** del
[Paso 1](#step-1--one-dependency-one-prelude). Un crate `proc-macro` no puede
reexportar tipos de tiempo de ejecución, así que el código generado por la macro
nombra cada tipo de tiempo de ejecución a través de `::firefly::__rt::firefly_cqrs::Bus`
y compañía. Esa es la razón por la que un servicio de un solo crate compila todo lo
que una macro expanda sin listar los crates `firefly-*` subyacentes.

Segundo, el **drenaje del inventory**. La capa declarativa hace más que generar
auxiliares: cada bean handler, bean listener, tarea programada y controlador también
envía un registro a un registro de inventory de tiempo de compilación, y
`FireflyApplication` drena esos registros en el arranque. Así Lumen no llama a
*ninguno* de los cableados a mano:

- sin `register(&bus)` — drenado por `register_discovered_handlers`,
- sin `subscribe(&broker)` — drenado por `subscribe_discovered_listeners`,
- sin `schedule_<fn>(scheduler)` — drenado por `register_discovered_scheduled`,
- sin `WalletApi::routes(state)` entregado a la pila web — drenado por
  `mount_controllers`,
- y sin `OnceLock` publicando los colaboradores de los handlers — se conectan por
  autowiring desde el contenedor.

Lumen declara los beans `WalletHandlers` / `WalletProjection`, la tarea de latido,
las factorías `LumenBeans` y el controlador `WalletApi`, y el framework resuelve cada
bean desde el contenedor y lo instala. La forma de `fn` libre de
`#[command_handler]` / `#[query_handler]` / `#[event_listener]` / `#[scheduled]`
sigue generando un auxiliar `register_<fn>` / `subscribe_<fn>` / `schedule_<fn>`
para el caso sin colaboradores; pero como los handlers de Lumen conectan
colaboradores por autowiring, usa la forma de bean, y el servicio en ejecución queda
cableado enteramente por el drenaje del inventory.

> **Tip** **Punto de control.** Ejecuta Lumen y lee el informe de arranque. La línea
> `:: cqrs handlers: … | event listeners: … | scheduled tasks: … | controllers:
> … ::` es el inventario que el framework drenó — el conteo es exactamente los
> beans, listeners, tareas y controladores que declaraste, sin ninguna llamada de
> registro en ningún sitio del fuente.

## Paso 12 — Todo el crate, declarativamente

Leídas de arriba abajo, las macros cuentan la historia de Lumen:

```text
  money.rs        (no macros — a pure value object; the no-thiserror promise)
  domain.rs       #[derive(DomainEvent)] x3   #[derive(AggregateRoot)]   #[derive(Schema)]
  ledger.rs       #[derive(Repository)] ReadModel   #[derive(Service)] WalletProjection
                  #[handlers] + #[event_listener(topic = "wallets.events")]
  commands.rs     #[derive(Command)] x3   #[derive(Query)]   #[derive(Builder/Schema)]
                  #[derive(Service)] WalletHandlers
                  #[handlers] + #[command_handler] x3 + #[query_handler]
  transfer.rs     #[firefly::saga] + #[saga_step] x2
  compliance.rs   #[firefly::workflow] + #[workflow_step] x3
  tcc_transfer.rs #[firefly::tcc] + #[participant] x2
  security.rs     (JwtService / BearerLayer / FilterChain — runtime APIs)
  web.rs          #[derive(Configuration)] + #[bean] x6   #[derive(Controller)]
                  #[rest_controller] + #[get] / #[post] x7
  housekeeping.rs #[scheduled(fixed_rate = "60s", initial_delay = "5s")]
```

Lo que *no* es una macro es igual de revelador: la cadena de filtros de seguridad se
construye con un builder de tiempo de ejecución (`FilterChain::new().require(...)`),
porque su forma es datos, no una declaración fija — y Lumen la mantiene explícita
para que el flujo de control quede visible. La saga, el workflow y el TCC *sí* son
macros declarativas; solo la cadena de filtros sigue siendo un builder de tiempo de
ejecución. Declarativo donde colapsa código repetitivo, explícito donde el grafo es
el punto: ese equilibrio es todo el diseño.

## Paso 13 — Verifica el crate

Todo lo anterior compila y está probado. Desde la raíz del workspace:

```bash
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                       # 42 unit + 12 HTTP = 54 tests
cargo test   -p firefly-sample-lumen --features streaming  # 57 tests (+3 streaming)
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
```

Los tests HTTP conducen en proceso el router ensamblado por el framework:
`build_router()` arranca un `FireflyApplication` (montando automáticamente el
controlador, drenando los handlers/listener, estratificando la seguridad) y devuelve
su router público, ejercitado a través de `tower::ServiceExt::oneshot` sin ningún
socket enlazado. Demuestran que las rutas automontadas, los handlers de CQRS, la
validación (422), la ruta de no encontrado (404), el límite de autenticación (401),
la saga de transferencia (camino feliz + compensación), el workflow de cumplimiento,
la transferencia TCC y la convergencia de la proyección funcionan todos de extremo a
extremo — cada listado en prosa de este libro es una rebanada de ese crate en
ejecución.

> **Tip** **Punto de control.** Los tres comandos tienen éxito: la compilación es
> limpia, la ejecución de tests por defecto informa de `54 passed`, la ejecución con
> streaming informa de `57 passed`, y clippy está en silencio bajo `-D warnings`.
> Eso es todo el crate declarativo, verificado.

## Resumen

- Una **macro declarativa** en Firefly se expande en tiempo de compilación a los
  `impl`, routers y registros que de otro modo escribirías a mano — comprobados por
  el compilador, nunca descubiertos por reflexión en tiempo de ejecución.
- Las macros que Lumen usa, archivo por archivo: `#[derive(Command/Query/Schema)]` y
  `#[handlers]` (`commands.rs`); `#[derive(DomainEvent/AggregateRoot)]`
  (`domain.rs`); `#[derive(Repository/Service)]` + `#[event_listener]`
  (`ledger.rs`); `#[derive(Configuration)]` + `#[bean]` y `#[derive(Controller)]` +
  `#[rest_controller]` (`web.rs`); `#[scheduled]` (`housekeeping.rs`); y el trío de
  orquestación `#[firefly::saga]` / `#[firefly::workflow]` / `#[firefly::tcc]`.
- El conjunto de apoyo que Lumen no usa sigue siendo de primera clase:
  `#[derive(Builder/Mapper/Validate)]`, los relacionales
  `#[derive(Entity/SqlxRepository)]` / `#[firefly::repository]` /
  `#[firefly::transactional]` (con `propagation` / `isolation` / `read_only` /
  `timeout_ms` / `manager`, más `no_rollback_for` / `rollback_only_for`), los
  decoradores de seguridad de método y de resiliencia, `#[cacheable]`,
  `#[async_method]`, los listeners de eventos en proceso, `#[aspect]` y
  `#[http_client]`.
- El código generado por las macros nombra los tipos de tiempo de ejecución a través
  de la ruta oculta del contrato `__rt`, que es la razón por la que un servicio de un
  solo crate compila todo lo que una macro expanda.
- El **drenaje del inventory** es lo que convierte un bean, listener, tarea o
  controlador declarado en comportamiento cableado en el arranque — así Lumen no
  escribe a mano ninguna llamada `register`, `subscribe`, `schedule` o `routes`.

Este capítulo no añadió ninguna funcionalidad; releyó Lumen como un catálogo. Cada
macro reemplazó un trozo de cableado escrito a mano con una declaración junto al
código, y todo ello llegó a través de una dependencia y un glob de preludio — la
tesis que el crate en ejecución demuestra.

## Ejercicios

1. **Rastrea una macro de extremo a extremo.** Elige `#[derive(Query)]` sobre
   `GetWallet`. Encuentra dónde se lee su `cache_ttl()` generado (la invalidación de
   `QueryCache` en `web.rs`) y el test que lo afirma (`get_wallet_carries_cache_ttl`
   en `commands.rs`). Cambia el TTL a `"5s"` y vuelve a ejecutar
   `cargo test -p firefly-sample-lumen get_wallet_carries_cache_ttl`.
2. **Añade un verbo.** Añade un método de lectura estilo
   `#[get("/wallets/:id/balance")]` al `impl` con `#[rest_controller]` en `web.rs`
   (devuelve el balance como JSON, despachando `GetWallet` a través del bus) y
   confirma que el controlador automontado lo sirve sin ningún otro cambio — sin
   editar `routes()`, sin llamada de registro.
3. **Añade una tarea programada.** Escribe una segunda función
   `#[scheduled(cron = "0 0 * * * *")]` en `housekeeping.rs` y afirma que aparece en
   `scheduler.tasks()` junto a `ledger_heartbeat` — el framework drena el nuevo
   `ScheduledRegistration` del inventory, así que no añades ninguna llamada de
   registro.
4. **Lee el inventario en el informe de arranque.** Ejecuta Lumen y encuentra la
   línea `:: cqrs handlers … | event listeners … | scheduled tasks … | controllers
   … ::`. Cuenta cada uno frente a los beans que leíste en este capítulo, luego
   añade el verbo del ejercicio 2 y mira cómo el conteo de rutas del controlador lo
   sigue.
5. **Cuenta el cableado que no escribiste.** Por cada macro de la tabla del
   [Paso 2](#step-2--the-macro-catalogue-mapped-to-lumen-files), nombra el auxiliar o
   el `impl` que generó (`register_*`, `subscribe_*`, `schedule_*`, `routes`,
   `EVENT_TYPE`, `AGGREGATE_TYPE`, el `impl` de `Message`). Esa lista es el código
   repetitivo que la capa declarativa escribió por ti.

## Adónde ir después

- Compón el crate único de Lumen en un servicio multi-crate y estratificado en
  **[Microservicios estratificados](./22-layered-microservices.md)** — donde el
  sample `lumen-ledger` (con el caso de uso `#[firefly::transactional]` del Paso 10)
  divide dominio, núcleo, web y modelos en crates separados.
- Revisa cómo el framework escanea y conecta los beans que este capítulo declaró en
  el **[análisis profundo de inyección de dependencias](./04a-dependency-injection.md)**.
- Los apéndices son referencia: un **[Índice de módulos](./91-appendix-modules.md)**
  de cada crate `firefly-*` y un **[Glosario](./92-glossary.md)** de los términos
  usados a lo largo del libro.
