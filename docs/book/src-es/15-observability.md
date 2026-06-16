# Observabilidad

En [Seguridad](./14-security.md) protegiste las rutas mutadoras de Lumen tras un
JWT y un filtro de roles. El servicio ahora es seguro, pero sigue siendo una caja
negra. Cuando un depósito va lento en producción, necesitas saber *dónde* se fue
el tiempo; cuando el broker se degrada, quieres un panel que se ponga rojo antes
que tu busca; cuando un auditor pregunta por qué se rechazó una transferencia,
necesitas una línea de log estructurada con contexto suficiente para reconstruir
la decisión.

La buena noticia —y el tema de todo este capítulo— es que casi nada de esto es
código que tengas que escribir. `FireflyApplication::run()` ya instaló la capa de
logging, el composite de salud, el registro de métricas, el middleware de
métricas de petición, la originación de trace-context W3C y un panel `/admin`
autohospedado vinculado a los componentes en vivo. Lumen es observable desde
[Tu primera API HTTP](./06-first-http-api.md); este capítulo te enseña a *leer*
lo que ya está ahí, y a añadir el puñado de piezas opcionales —un info
contributor, una sonda de salud, una métrica de dominio, una vista de panel
personalizada— que solo tu aplicación puede aportar.

Al terminar este capítulo, serás capaz de:

- Alcanzar la **superficie de gestión** de Lumen —`/actuator/*` en el puerto de
  gestión— y explicar por qué vive en un listener separado de la API pública.
- Registrar un **info contributor** para que `/actuator/info` informe sobre qué
  infraestructura está ejecutando esta instancia.
- Añadir un **health indicator** al composite y verlo agregarse en
  `/actuator/health`.
- Leer las **métricas de petición** que el framework ya registra, y registrar tu
  propio contador y gauge en el mismo registro.
- Comprender el **logging estructurado y enriquecido por correlación** y el
  **trace-context W3C**: cómo una petición se hilvana a través de logs, eventos y
  llamadas salientes sin enhebrado manual.
- Abrir el **panel de administración autohospedado**, leer sus quince vistas
  —incluida la vista **Beans** poblada— y añadir una vista personalizada propia.

## Conceptos que conocerás

Cada idea de las que siguen se reintroduce en contexto allí donde se usa por
primera vez; esta es la versión breve para tener el vocabulario en su sitio antes
del primer comando.

> **Note** **Término clave — superficie de gestión / actuator.** La *superficie
> de gestión* es un conjunto de endpoints HTTP operativos —comprobaciones de
> salud, información de compilación, métricas, introspección de configuración,
> control del nivel de log en ejecución— que existen para operadores y
> herramientas, no para los usuarios finales. Firefly los sirve bajo
> `/actuator/*` en un **puerto separado** de tu API de negocio. Esto refleja
> Spring Boot Actuator.

> **Note** **Término clave — info contributor.** Un *info contributor* es un
> pequeño callback que añade una sección JSON a `/actuator/info`. Lo registras en
> el builder de la aplicación; el framework lo invoca cuando un operador accede al
> endpoint. El análogo en Spring es un bean `InfoContributor`.

> **Note** **Término clave — health indicator y composite.** Un *health
> indicator* es una sonda asíncrona que informa `UP` / `DEGRADED` / `DOWN` (con un
> mensaje y detalles opcionales). Un *composite* agrega muchos indicadores en una
> única consolidación y la sirve en `/actuator/health`. Esto es el
> `HealthIndicator` de Spring Boot más su agregador de salud.

> **Note** **Término clave — id de correlación.** Un *id de correlación* es un
> único identificador adjunto a todo lo que toca una sola petición —cada línea de
> log, cada evento que publica, cada llamada saliente que hace— de modo que puedas
> reconstruir la historia completa a partir de un único valor. Firefly lo
> establece en un ámbito task-local a la entrada; el análogo en Spring es una
> entrada MDC enhebrada a través de una petición.

## Los dos puertos, y qué sirve cada uno

Antes del primer endpoint, fija el modelo mental. Lumen ejecuta **dos
listeners**:

- la **API pública** en `0.0.0.0:8080` —tus rutas de negocio y nada más—;
- la **superficie de gestión** en `0.0.0.0:8081` —`/actuator/*` más el panel
  `/admin` autohospedado más la documentación de API autogenerada.

`FireflyApplication` ensambla y sirve ambos routers; Lumen no escribe ningún
cableado de actuator ni de admin. La separación es la clave: un endpoint
operativo como `/actuator/env` (que puede revelar configuración) o `/admin` (un
panel en vivo) nunca se filtra a la red pública, porque el listener público
sencillamente no monta esas rutas.

> **Note** **Término clave — sobrescritura de la dirección de bind.**
> `FIREFLY_SERVER_ADDR` y `FIREFLY_MANAGEMENT_ADDR` son las dos variables de
> entorno que mueven los listeners público y de gestión de forma independiente
> (con valores por defecto `0.0.0.0:8080` / `0.0.0.0:8081`). Las conociste en
> [Inicio rápido](./02-quickstart.md); son la forma de poner el puerto de gestión
> en una interfaz privada en producción.

> **Tip** **Punto de control.** Con Lumen en ejecución (`cargo run --bin lumen`),
> la superficie de gestión responde en el `8081` y la API pública en el `8080`.
> Confirma la separación: `curl localhost:8081/actuator/health` devuelve JSON,
> mientras que `curl localhost:8080/actuator/health` devuelve un documento de
> problema 404 —el actuator no está en el puerto público.

## Paso 1 — Alcanzar el actuator

Incluso sin código de observabilidad propio, el actuator está vivo. Arranca
Lumen y, desde una segunda terminal, recorre tres endpoints.

```bash
curl localhost:8081/actuator/health
# {"status":"UP", ...}

curl localhost:8081/actuator/info
# {"app":{"name":"lumen","version":"26.6.28"}, ...}

curl localhost:8081/actuator/metrics
# {"names":[ "http_server_requests_seconds", ... ]}
```

Qué acaba de ocurrir: `/actuator/health` agregó cada health indicator registrado
en un único `status`; `/actuator/info` reflejó el nombre de la aplicación que
pasaste a `FireflyApplication::new("lumen")` más la versión del framework;
`/actuator/metrics` listó los medidores que el framework ha estado registrando
desde el arranque —incluido el temporizador de petición por ruta que leerás en el
Paso 4.

La superficie de gestión completa aparece a continuación. Lumen llega al puerto de
gestión en `http://localhost:8081/actuator/*`:

| Endpoint                       | Devuelve                                           |
|--------------------------------|----------------------------------------------------|
| `/actuator/health`             | la consolidación del composite (+ sondas de liveness/readiness) |
| `/actuator/info`               | metadatos de la app + tus info contributors        |
| `/actuator/metrics`            | el listado de medidores registrados                |
| `/actuator/metrics/:name`      | el detalle de un medidor                           |
| `/actuator/prometheus`         | el destino de scrape en exposición de texto Prometheus |
| `/actuator/env`                | fuentes de propiedades enmascaradas y atribuidas a su origen |
| `/actuator/scheduledtasks`     | descriptores de tareas programadas                 |
| `/actuator/version`            | la versión en ejecución                            |
| `/actuator/beans`              | cada bean de DI (tipo, scope, estereotipo, primary)|
| `/actuator/mappings`           | cada ruta `#[rest_controller]` (método/path)       |
| `/actuator/conditions`         | las guardas condicionales por bean                 |
| `/actuator/loggers[/:name]`    | control del nivel de log en ejecución              |
| `/actuator/threaddump`         | un volcado de hilos/tareas                         |
| `/actuator/httpexchanges`      | intercambios HTTP recientes (cuando está cableado) |
| `/actuator/caches[/:name]`     | listado de cachés + desalojo (cuando está cableado)|
| `/actuator/refresh`            | recarga la configuración (el hook `Refresher`)     |

> **Note** Los informes `beans` / `mappings` / `conditions` reflejan la
> introspección de inyección de dependencias de Spring Boot Actuator: se
> autorregistran en el framework junto con el resto, de modo que puedes
> introspeccionar el grafo de objetos cableado por HTTP sin código de aplicación.
> Viste el mismo inventario impreso en el arranque en
> [Inicio rápido](./02-quickstart.md); estos endpoints lo sirven en vivo.

> **Tip** **Punto de control.** Los tres `curl` de arriba devuelven JSON. Si
> `curl` conecta pero cada path da 404, estás accediendo al `8080` (público) en
> lugar del `8081` (gestión). El puerto público no tiene `/actuator/*`.

## Paso 2 — Describir esta instancia con un info contributor

`/actuator/info` ya informa el bloque `app` —nombre y versión—, pero no puede
saber qué *infraestructura* está ejecutando esta instancia concreta. Eso es
conocimiento de la aplicación, así que es la única pieza de código de
observabilidad que Lumen podría añadir. La aportas como un **info contributor**
registrado de forma fluida en el builder de la aplicación.

> **Note** **Término clave — `InfoContributor`.** El tipo es
> `Box<dyn Fn() -> serde_json::Map<String, Value> + Send + Sync>` —un closure
> en una caja que devuelve un objeto JSON. El mapa de cada contributor se
> convierte en una sección de `/actuator/info`. El closure se ejecuta en cada
> petición al endpoint, de modo que puede informar valores en vivo.

```rust,ignore
use firefly::starter_core::InfoContributor;

let contributor: InfoContributor = Box::new(|| {
    let mut info = serde_json::Map::new();
    info.insert(
        "sample".into(),
        serde_json::json!({ "name": "lumen", "store": "in-memory", "eventBus": "in-memory" }),
    );
    info
});

firefly::FireflyApplication::new("lumen")
    .info_contributor(contributor)   // adds a "sample" section to /actuator/info
    .run()
    .await
```

Qué acaba de ocurrir, bloque a bloque:

- `InfoContributor` se reexporta a través de la fachada en
  `firefly::starter_core::InfoContributor`, de modo que Lumen sigue dependiendo
  únicamente del crate `firefly`.
- El closure construye un `serde_json::Map` con una única clave, `sample`, cuyo
  valor describe el store y el tipo de event-bus que ejecuta esta instancia.
- `.info_contributor(contributor)` lo registra en el builder. El framework hilvana
  cada contributor registrado en el handler de `/actuator/info` cuando construye el
  router de gestión: sin llamada a `actuator_router(..)` ni gestión del segundo
  listener en tu código.

Tras esto, `/actuator/info` informa ambos bloques:

```jsonc
// GET /actuator/info
{
  "app": { "name": "lumen", "version": "26.6.28" },
  "sample": { "name": "lumen", "store": "in-memory", "eventBus": "in-memory" }
}
```

El bloque `app` se rellena con `app_name` / `app_version` que Lumen estableció en
[Configuración](./03-configuration.md); el bloque `sample` es el contributor de
arriba. Un operador que acceda a `/actuator/info` ve ahora de un vistazo que esta
instancia está sobre la infraestructura en memoria, no sobre Postgres + Kafka.

> **Tip** **Punto de control.** Tras añadir el contributor y volver a ejecutar,
> `curl localhost:8081/actuator/info` muestra un objeto `sample` de nivel superior
> que informa `"store":"in-memory"`. Puedes registrar más de un contributor; sus
> mapas se fusionan en el mismo documento JSON.

## Paso 3 — Añadir un health indicator

El composite que respalda `/actuator/health` parte de los indicadores propios del
framework. Un despliegue real de Lumen añadiría los suyos —una sonda de liveness
del broker, una comprobación de alcanzabilidad del store— para que un orquestador
pueda distinguir una instancia degradada de una sana.

> **Note** **Término clave — `IndicatorFn`.** `IndicatorFn::new(name, closure)`
> adapta un simple closure asíncrono a un `Indicator` de salud. El closure
> devuelve un `HealthResult` —`HealthResult::up()`,
> `HealthResult::degraded(msg)` o `HealthResult::down(msg)`, cada uno
> opcionalmente enriquecido con `.with_detail(..)`. El composite consolida los
> resultados: `DOWN` si algún indicador está `DOWN`, si no `DEGRADED` si alguno
> está `DEGRADED`, y en caso contrario `UP`.

El `Core` del framework ya lleva el `HealthComposite`. Puenteas un indicador hacia
él con `Core::add_observability_indicator(..)`. Hay dos lugares limpios para
hacerlo: declarar el indicador como un `#[bean]` (el framework lo descubre durante
el escaneo de componentes), o alcanzar el composite desde un hook
`FireflyApplication::on_ready` una vez escaneado el contenedor. La forma con hook
tiene este aspecto:

```rust,ignore
use firefly::observability::{HealthResult, IndicatorFn};

// `core` is the wired Core (it owns the HealthComposite). Registering an
// indicator on it makes the probe appear under /actuator/health.
core.add_observability_indicator(IndicatorFn::new("event-bus", || async {
    HealthResult::up() // a real probe would ping the broker and return down() on failure
}));
```

Qué acaba de ocurrir: `IndicatorFn::new("event-bus", ..)` envuelve el closure
asíncrono como un `Indicator` llamado `event-bus`;
`add_observability_indicator` lo registra en el composite. La siguiente llamada a
`/actuator/health` ejecuta cada indicador de forma concurrente y pliega los
resultados en el `status` global, listando tu sonda por nombre en el desglose por
componente.

> **Note** Health expone dos subrutas a las que acceden las sondas de tu
> orquestador: `/actuator/health/liveness` y `/actuator/health/readiness`. Están
> separadas para que una migración en curso que falle la *readiness* (todavía no
> me mandes tráfico) no tenga por qué hacer saltar la *liveness* (mátame y
> reiníciame). Devolver `degraded` mantiene una sonda `UP` mientras señala el
> problema en la consolidación.

> **Tip** **Punto de control.** Tras cablear un indicador, `curl
> localhost:8081/actuator/health` muestra el nombre de tu sonda junto al
> `status`. Haz que el closure devuelva `HealthResult::down("broker unreachable")`
> una vez y observa cómo el `status` global pasa a `DOWN` —esa es la regla de
> precedencia en acción.

## Paso 4 — Leer las métricas de petición que ya tienes

No tuviste que pedir la latencia por ruta: las métricas de petición se
auto-instrumentan **activadas por defecto**, tanto en la capa `Core` (de modo que
incluso un `Core` desnudo las emite) como a través del stack web, que rellena una
`RequestMetricsConfig` por defecto si no fijaste ninguna.

> **Note** **Término clave — métricas de petición.** Por cada petición el
> middleware registra el temporizador etiquetado `http_server_requests_seconds`
> más un gauge acompañante `…_max`, etiquetados con `method` / `uri` plantillado
> (la ruta coincidente, de modo que `/api/v1/wallets/:id` y no el id concreto) /
> `status` / `outcome` / `exception`. Una petición limpia lleva
> `exception="None"`. Esta es la convención de Micrometer/Spring Boot, así que los
> scrapers listos para usar la leen sin cambios.

Como el medidor ha estado registrando desde el momento en que Lumen arrancó en
[Tu primera API HTTP](./06-first-http-api.md), este capítulo solo lo *expone*.
Accede a una ruta unas cuantas veces y luego lee el medidor:

```bash
curl localhost:8080/api/v1/wallets/$ID            # generate some traffic
curl localhost:8081/actuator/metrics/http_server_requests_seconds
```

El nombre del medidor con puntos y guiones bajos se mapea directamente a
Prometheus, de modo que apuntar un `scrape_config` de Prometheus a
`/actuator/prometheus` enciende Grafana sin código extra:

```bash
curl localhost:8081/actuator/prometheus | grep http_server_requests_seconds
```

> **Note** Para desactivar la auto-instrumentación, establece
> `CoreConfig { disable_request_metrics: true, .. }`. Para ajustar la ventana de
> máximo móvil o las exclusiones de path en lugar de desactivarla, proporciona
> `request_metrics: Some(RequestMetricsConfig { .. })`. Ambos se configuran de la
> misma forma en que configuraste todo lo demás en
> [Configuración](./03-configuration.md).

### Registrar tus propios medidores

Más allá del temporizador de petición, registras medidores de dominio en el
**mismo** registro, de modo que afloran en `/actuator/metrics` y
`/actuator/prometheus` de inmediato. Obtén el registro del `Core` con
`metric_registry()` (también es un bean de DI resoluble que puedes
`#[autowired]`), y luego crea un contador o un gauge:

```rust,ignore
let metrics = core.metric_registry();

// A domain counter, bumped each time the transfer saga completes.
let transfers = metrics.counter("lumen_transfers_total");
transfers.inc();              // or transfers.add(3) for an explicit count

// A gauge sampling a live value (e.g. wallets currently held in the read model).
let active = metrics.gauge("lumen_wallets_active");
active.set(wallet_count as f64);
```

Qué acaba de ocurrir: `counter(name)` y `gauge(name)` devuelven un
`Arc<Counter>` / `Arc<Gauge>` registrado bajo ese nombre. `Counter::inc()` suma
uno (`add(n)` añade un recuento explícito); `Gauge::set(v)` registra un valor
muestreado. Ambos medidores aparecen ahora en el listado y en el scrape de
Prometheus sin ningún cableado de exporter.

> **Tip** **Punto de control.** Tras incrementar `lumen_transfers_total` y leer
> `/actuator/metrics`, el listado de medidores incluye `lumen_transfers_total`; el
> scrape de Prometheus muestra su recuento actual. El registro es compartido, así
> que tus medidores de dominio y el temporizador de petición del framework
> conviven uno al lado del otro.

## Paso 5 — Logging estructurado y correlación

`FireflyApplication` instala una capa de `tracing` que formatea cada evento como
una línea estructurada y la enriquece con el id de correlación de la petición
(establecido por el middleware de correlación, activado por defecto). Llama a
`init_logging` por ti en el arranque —en modo best-effort, de modo que un arnés de
test que ya posee el subscriber global no entra en pánico— y, con la feature
`admin` activada, duplica los registros hacia el búfer de logs en vivo del panel.

> **Note** **Término clave — `init_logging`.** `init_logging(LogConfig)` instala
> el subscriber estructurado de `tracing` como el global por defecto. Su hermano
> `init_logging_with_layers([..])` hace lo mismo pero apila capas de `tracing`
> adicionales sobre la capa de correlación —el hook que usa el panel de
> administración para duplicar cada registro de log hacia su búfer en memoria
> mientras el flujo JSON de consola sigue fluyendo. Nunca llamas a ninguno de los
> dos tú mismo; el framework lo hace en el arranque.

```rust,ignore
// What FireflyApplication does at boot — Lumen writes none of this:
let _ = web.init_logging();
// (or web.init_logging_with_layers(vec![log_buffer]) when the admin tail is on)
```

Después de eso, las macros simples de `tracing` producen líneas enriquecidas, y
los campos registrados en un span envolvente se fusionan en cada evento:

```rust,ignore
tracing::info!(wallet_id = %id, amount = %money, "deposit accepted");
```

Los nombres de campo (`time`, `level`, `msg`, `service`, `correlationId`) siguen
un esquema estable y documentado, de modo que un único pipeline de logs parsea
cada servicio Firefly de forma consistente.

Como el id de correlación vive en un ámbito task-local, fluye automáticamente
hacia cada línea de log, cada evento que el ledger de Lumen publica
(`Event::new` lo estampa) y cada llamada de cliente saliente (el `traceparent`
W3C se propaga). Una petición que abre una wallet, publica `WalletOpened` y lo
proyecta en el read model se hilvana bajo un único id sin enhebrado manual —el id
de correlación task-local sustituye a la fontanería MDC thread-local que
escribirías a mano en otros stacks.

### Configurar el logging

El logging se configura igual que configuras todo lo demás: desde el único
fichero de configuración principal. Vincula la sección `firefly.logging.*` a un
`LogConfig` con
`firefly::observability::log_config_from_properties(props, base)`:

```yaml
firefly:
  logging:
    format: json                # json (default) | text (logfmt) | console
    level: info                 # root level
    level.firefly_web: warn     # per-logger levels (Spring's logging.level.<logger>)
    level.app::ledger: trace
    service: lumen              # the `service` field stamped on every line
    file:
      name: lumen.log           # enable the rolling file appender
      max-size: 10MB
      max-history: 7
```

Qué hacen estas claves: `format` elige el renderizador de salida; el `level`
escueto es el nivel raíz, y `level.<target>` sobrescribe un logger concreto
(coincidiendo con `logging.level.<logger>` de Spring); `service` se estampa en
cada línea; el bloque `file` activa el rolling file appender y ajusta su rotación.
Por tanto, los niveles por logger, el formato de salida y el rolling file appender
provienen todos de la configuración. Un fichero de logging externo puede además
incorporarse con `apply_external_config`.

Y cada nivel puede cambiarse **sin reiniciar** mediante
`POST /actuator/loggers/{name}` —el control de loggers en ejecución del actuator.
El endpoint informa el `configuredLevel` / `effectiveLevel` de cada logger, la
forma convencional que esperan las herramientas de gestión:

```bash
# Raise app::ledger to TRACE on a running instance, no redeploy.
curl -X POST localhost:8081/actuator/loggers/app::ledger \
  -H 'content-type: application/json' \
  -d '{"configuredLevel":"TRACE"}'
```

> **Tip** **Punto de control.** `curl localhost:8081/actuator/loggers` lista cada
> logger con su `configuredLevel` / `effectiveLevel`. Haz POST de un nuevo nivel a
> un logger, vuelve a hacer GET y confirma que el nivel cambió en el proceso en
> vivo.

## Paso 6 — Trace context y OpenTelemetry

La cadena de middleware por defecto que `FireflyApplication` aplica incluye la
`TraceContextLayer`, que **origina** el contexto de traza distribuida en cada
petición.

> **Note** **Término clave — trace context W3C.** `traceparent` / `tracestate`
> son las cabeceras HTTP estándar que transportan una traza distribuida a través
> de las fronteras de servicio: un trace-id de 32 hex y un span-id de 16 hex
> identifican dónde se sitúa la petición en un árbol de llamadas mayor. *Originar*
> significa: cuando una petición entrante no lleva `traceparent`, la capa acuña un
> span raíz nuevo para que la petición salga de Lumen como cabecera de una traza
> bien formada.

Así, la capa valida un `traceparent` / `tracestate` entrante cuando está presente
y acuña un span raíz W3C cuando está ausente, lo inserta en la petición y
enriquece cada línea de log con `trace_id` / `span_id`. Una petición que llega sin
cabecera de traza pasa a ser igualmente la cabecera de una traza distribuida, y el
`traceparent` que Lumen propaga en las llamadas salientes se convierte en la
arista padre/hijo hacia el siguiente servicio.

El cableado del SDK de OpenTelemetry —exporters, sampling, atributos de recurso—
se deja a tu aplicación, donde añades tu capa OTel de `tracing` preferida junto a
la capa de correlación. Lumen se distribuye sin exporter (es código didáctico sin
colector externo), pero la originación y propagación de trace-context ya están en
los bordes. Cuando sí quieras spans fluyendo hacia un colector, construye un
tracer OTLP y añade la capa de `tracing-opentelemetry` al subscriber que Firefly
instaló —la capa de correlación sigue funcionando junto a ella:

```rust,ignore
use opentelemetry_otlp::WithExportConfig;
use tracing_subscriber::prelude::*;

// Build an OTLP pipeline pointing at your collector.
let tracer = opentelemetry_otlp::new_pipeline()
    .tracing()
    .with_exporter(
        opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint("http://otel-collector:4317"),
    )
    .install_batch(opentelemetry_sdk::runtime::Tokio)?;

// Register the OTel layer alongside Firefly's structured-log + correlation layers.
tracing_subscriber::registry()
    .with(tracing_opentelemetry::layer().with_tracer(tracer))
    .init();
```

Las cabeceras `traceparent` que Firefly ya propaga se convierten en las aristas
padre/hijo entre spans, de modo que una petición que se ramifica hacia una llamada
saliente aparece como una única traza distribuida en tu backend.

## Paso 7 — Advice global de excepciones (opcional)

Los errores de Lumen ya se renderizan como `application/problem+json` (RFC 9457)
en la frontera del handler —lo viste desde el primerísimo endpoint en
[Tu primera API HTTP](./06-first-http-api.md). Para una reescritura
*transversal* —mapear toda una clase de errores a un status o cuerpo
personalizado sin tocar cada handler— el framework ofrece una capa de advice
global transparente.

> **Note** **Término clave — advice global de excepciones.** Un registro de
> transformaciones que post-procesa cada respuesta `application/problem+json`
> después de que el handler la produce —el análogo en Rust del
> `@ControllerAdvice` de Spring. Registras el registro como un `#[bean]`; el
> framework instala una `ExceptionAdviceLayer` como la capa más externa solo
> cuando el registro no está vacío, de modo que un servicio que no declara tal
> bean conserva la vía RFC 9457 simple.

Registra un bean `ExceptionHandlerRegistry` e indexa las transformaciones por
tipo de problema:

```rust,ignore
use firefly::web::ExceptionHandlerRegistry;
use firefly::kernel::{ProblemDetail, TYPE_NOT_FOUND};

// A #[bean] returning a registry: every "not found" becomes a friendlier 410.
#[bean]
fn exception_advice(&self) -> ExceptionHandlerRegistry {
    ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd: &ProblemDetail| {
        let mut out = pd.clone();
        out.status = 410;
        out
    })
}
```

Qué acaba de ocurrir: `on_type(TYPE_NOT_FOUND, transform)` registra un closure que
recibe el `ProblemDetail` producido y devuelve uno reescrito —aquí cambiando el
status de 404 a 410 (`Gone`). El framework ejecuta la transformación coincidente
en cada documento de problema saliente. Las sobrescrituras locales del controlador
siguen prevaleciendo sobre las reglas globales, de modo que un handler puede
quedar fuera de la reescritura transversal.

> **Tip** **Punto de control.** Con el bean registrado, solicita una wallet
> inexistente y confirma que el status de la respuesta es ahora `410` mientras el
> cuerpo sigue siendo un documento `application/problem+json` válido. Elimina el
> bean y el status vuelve al `404` por defecto —prueba de que la capa solo se
> instala cuando el registro no está vacío.

## Paso 8 — El panel de administración autohospedado

La superficie del actuator es JSON para máquinas. `firefly-admin` monta un panel
de administración de una sola página —empaquetado, sin build de `npm`— que une
salud, métricas, loggers, beans, mappings, cachés, handlers CQRS, trazas y una
cola de logs en vivo en un único panel de control con flujos Server-Sent-Event.

> **Note** **Término clave — panel autohospedado.** El panel es una SPA en
> JavaScript puro servida por el propio framework en el puerto de gestión: no hay
> un servicio de frontend aparte que desplegar ni paso de build. Con la feature
> `admin` de la fachada habilitada, `FireflyApplication` lo monta en `/admin/` y lo
> vincula a los componentes en vivo.

Con la feature `admin` activada, **`FireflyApplication` lo autohospeda en el
puerto de gestión** y lo vincula a los colaboradores reales: el composite de
salud, el registro de métricas, el bus CQRS, el scheduler, el contenedor de DI
(que respalda la vista Beans), una instantánea de entorno construida a partir de
los perfiles activos y el entorno de proceso `FIREFLY_*`, un búfer de trazas
alimentado por el recorder de intercambios HTTP, y un búfer de logs alimentado por
la capa de logging duplicada. Los paneles `env` / `config` / `mappings` muestran
**datos reales**, no stubs. Lumen no escribe nada de este cableado —distribuye el
panel en `/admin/` simplemente por ser una `FireflyApplication`.

```bash
cargo run --bin lumen --features admin
# then open http://localhost:8081/admin/ in a browser
```

El panel renderiza quince vistas integradas, agrupadas en la barra lateral:

| Sección        | Vistas                                                         |
|----------------|----------------------------------------------------------------|
| Dashboard      | Overview, Health                                               |
| Application    | **Beans**, Environment, Configuration, Loggers                 |
| Monitoring     | Metrics, Scheduled Tasks, HTTP Traces, Log Viewer              |
| Infrastructure | Mappings, Caches, CQRS, Transactions                           |
| Fleet          | Instances (server mode)                                        |

Cada vista está respaldada por un endpoint JSON `/admin/api/*`; los flujos SSE
(`/admin/api/sse/{health,metrics,traces,logfile,beans,runtime,server}`) empujan
actualizaciones sin que tu código haga polling. Los paths de admin y actuator se
excluyen de la captura de trazas, de modo que nunca contaminan el panel HTTP
Traces.

### La vista Beans

La vista **Beans** es la ventana del panel hacia el contenedor de inyección de
dependencias. Como `FireflyApplication` siempre pasa el contenedor escaneado, el
panel sirve:

| Endpoint                     | Devuelve                                             |
|------------------------------|------------------------------------------------------|
| `GET /admin/api/beans`       | cada bean registrado con su estereotipo y scope      |
| `GET /admin/api/beans/graph` | el grafo de dependencias entre beans                 |
| `GET /admin/api/beans/:name` | el detalle de un bean (tipo, scope, dependencias)    |
| `GET /admin/api/sse/beans`   | una instantánea en vivo en cada intervalo de refresco|

La vista Overview también consolida un bloque `beans` (`{ total, stereotypes }`) y
un bloque `wiring` (recuentos en vivo de handlers CQRS y tareas programadas)
extraídos del mismo contenedor, de modo que la página de inicio muestra cuánto
está cableado el servicio sin abrir la vista Beans completa.

La vista Beans de Lumen está **poblada**, no escasa: el framework escanea por
componentes la configuración que declara los beans de Lumen, de modo que el event
store, el read model, la query cache, el servicio JWT, el `FilterChain` /
`BearerLayer`, el servicio de aplicación del ledger y el controlador `WalletApi`
aparecen todos como beans con sus estereotipos y las dependencias autowired entre
ellos. (Si hospedaras el panel de forma autónoma sin un contenedor, estos
endpoints se degradan con elegancia a un bloque vacío `{ "total": 0 }`.)

> **Note** `firefly-admin` también funciona en *modo servidor*: las instancias se
> autorregistran mediante un cliente admin, y un servidor central agrega una flota
> de servicios en la vista Instances. El panel es la misma SPA en JavaScript puro
> impulsada por completo por los endpoints JSON + SSE de `/admin/api` —no hay paso
> de build de frontend en ninguno de los dos modos.

> **Tip** **Punto de control.** Con `--features admin`, abre
> `http://localhost:8081/admin/`, selecciona **Beans** y encuentra el controlador
> `WalletApi`. Sus dependencias autowired `bus` / `ledger` / `query_cache`
> deberían aparecer como aristas en el grafo de beans —prueba de que la vista lee
> el contenedor real, no un stub.

### Una vista personalizada

Para añadir tu propia vista en la barra lateral, implementa el trait `AdminView`
y empújala a `AdminDeps::views`; el panel la lista bajo `/admin/api/views[/:id]`.
Una vista "Treasury" de Lumen aflora el balance de custodia total de todas las
wallets, consultado desde el read model:

```rust,ignore
use std::sync::Arc;
use firefly::admin::AdminView;

struct TreasuryView {
    read_model: Arc<WalletReadModel>,
}

#[async_trait::async_trait]
impl AdminView for TreasuryView {
    fn view_id(&self) -> &str { "treasury" }
    fn display_name(&self) -> &str { "Treasury" }
    fn icon(&self) -> &str { "wallet" }

    // Backs GET /admin/api/views/treasury (keyed by view_id).
    async fn data(&self) -> serde_json::Value {
        let total: i64 = self.read_model.all().iter().map(|w| w.balance).sum();
        serde_json::json!({ "custodyTotal": total, "wallets": self.read_model.len() })
    }
}
```

Los cuatro métodos del trait son: `view_id` (la clave del registro y el segmento
de path `/views/{id}`), `display_name` e `icon` (lo que la barra lateral
renderiza), y el `data()` asíncrono que produce el payload JSON de la vista.
Registra la vista antes de montar empujándola a `AdminDeps::views`:

```rust,ignore
let mut deps = AdminDeps::new(/* required collaborators … */);
deps.views.push(Arc::new(TreasuryView { read_model: Arc::clone(&read_model) }));
```

> **Note** Cuando dejas que `FireflyApplication` autohospede el panel, nunca
> construyes `AdminDeps` tú mismo —el framework obtiene cada colaborador del stack
> web en vivo y del contenedor escaneado. Solo construyes `AdminDeps`
> directamente en el caso avanzado de más abajo, donde hospedas el panel fuera de
> una `FireflyApplication`.

> **Design note.** El router del panel es accesible directamente cuando quieres
> hospedarlo fuera de `FireflyApplication` —un servidor personalizado, o un test.
> `mount(AdminConfig, AdminDeps)` devuelve el router; `AdminDeps::new` toma los
> colaboradores requeridos y el resto son campos opcionales que se rellenan con la
> sintaxis de actualización de struct:
>
> ```rust,ignore
> use std::sync::Arc;
> use firefly::admin::{mount, AdminConfig, AdminDeps, LogBuffer, TraceBuffer};
>
> let deps = AdminDeps {
>     scheduler: Some(scheduler),    // → Scheduled Tasks view
>     bus: Some(bus),                // → CQRS view
>     container: Some(container),    // → Beans view
>     ..AdminDeps::new(
>         "lumen",
>         firefly::VERSION,
>         health_composite,          // Arc<HealthComposite>
>         metric_registry,           // Arc<MetricRegistry>
>         Arc::new(TraceBuffer::new()),
>         LogBuffer::new(),
>     )
> };
> let dashboard = mount(AdminConfig::default(), deps);
> ```
>
> `FireflyApplication` realiza exactamente este mount por ti, que es la razón por
> la que Lumen distribuye el panel sin código de admin propio.

## Resumen — qué cambió en Lumen

Este capítulo te enseñó a leer y extender la superficie de observabilidad
siempre-activa de Lumen —toda ella cableada por `FireflyApplication`, sin código
de observabilidad en `main.rs`:

| Aspecto | Quién lo cableó | Dónde lo lees / extiendes |
|---------|-----------------|----------------------------|
| Superficie de gestión en `:8081` | el framework | `curl /actuator/*`; sobrescribe con `FIREFLY_MANAGEMENT_ADDR` |
| Metadatos de instancia en `/actuator/info` | bloque `app` del framework + tu contributor | `.info_contributor(..)` en el builder |
| Consolidación de salud | composite del framework + tus indicadores | `add_observability_indicator(IndicatorFn::new(..))` |
| Métricas de petición (`http_server_requests_seconds`) | el framework, activadas por defecto | `/actuator/metrics`, `/actuator/prometheus` |
| Tus medidores de dominio | tú, en el registro compartido | `core.metric_registry().counter(..)` / `.gauge(..)` |
| Logs estructurados y enriquecidos por correlación | `init_logging` en el arranque | macros simples de `tracing`; ajusta vía `firefly.logging.*` |
| Trace context W3C | la `TraceContextLayer` | originado/propagado en los bordes automáticamente |
| Panel de administración autohospedado | `FireflyApplication` + la feature `admin` | `/admin/` —quince vistas incluida la **Beans** poblada |

Ahora también sabes que el id de correlación fluye automáticamente hacia cada
línea de log, evento publicado y llamada saliente; que la `TraceContextLayer`
origina un `traceparent` W3C cuando falta; y que el advice global de excepciones
es un `#[bean]` opcional que el framework instala solo cuando está presente.

## Ejercicios

1. **Alcanza el actuator.** Ejecuta `cargo run --bin lumen`, luego `curl
   localhost:8081/actuator/info` y confirma que el bloque `sample` informa el
   store en memoria. Accede a `/actuator/health` y `/actuator/metrics`, y luego
   confirma que `curl localhost:8080/actuator/health` devuelve un problema 404 —el
   actuator no está en el puerto público.
2. **Añade un health indicator.** Cablea un `IndicatorFn::new("read-model", ..)`
   en el composite con `add_observability_indicator` (desde un hook `on_ready`, o
   declarándolo como un `#[bean]`) que devuelva `UP` cuando el read model contenga
   al menos una vista de wallet y `DEGRADED` en caso contrario, y luego obsérvalo
   aparecer bajo `/actuator/health`.
3. **Una métrica de Lumen.** Registra un contador —p. ej. `lumen_transfers_total`—
   en `core.metric_registry()` cada vez que la saga de transferencia se complete,
   y verifica que aparece en `/actuator/metrics` y en el scrape de
   `/actuator/prometheus`.
4. **Cambia un nivel de log en vivo.** `curl localhost:8081/actuator/loggers` para
   listar los loggers, luego haz `POST` de un nuevo `configuredLevel` a uno de
   ellos y vuelve a hacer GET para confirmar que el cambio surtió efecto en el
   proceso en ejecución —sin reinicio.
5. **Explora la vista Beans.** Ejecuta `cargo run --bin lumen --features admin`,
   abre `http://localhost:8081/admin/` y encuentra la vista Beans —observa que
   está *poblada*. Localiza el controlador `WalletApi` y confirma que sus
   dependencias autowired `bus` / `ledger` / `query_cache` aparecen en el grafo de
   beans.

## Adónde ir después

Un servicio que puedes ver es un servicio que puedes operar. El siguiente capítulo
le da a Lumen trabajo que hacer por su cuenta —y una forma de llegar a los
clientes.

- Añade trabajos en segundo plano y notificaciones salientes en
  **[Programación y notificaciones](./16-scheduling-notifications.md)** —y observa
  cómo las nuevas tareas `#[scheduled]` aparecen bajo `/actuator/scheduledtasks` y
  en la vista Scheduled Tasks del panel.
- Repasa cómo el framework descubre y cablea los beans que muestra la vista
  **Beans** en **[Cableado de dependencias](./04-dependency-wiring.md)**.
- Conduce el router cableado —y haz aserciones sobre salud y métricas— en tests
  con `bootstrap()` en **[Testing](./18-testing.md)**.
- Mueve el puerto de gestión a una interfaz privada y activa infraestructura real
  en **[Producción y despliegue](./20-production.md)**.
