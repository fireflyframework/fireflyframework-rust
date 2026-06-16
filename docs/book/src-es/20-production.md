# Producción y despliegue

Lumen ha crecido a lo largo del libro desde un esqueleto desnudo hasta convertirse
en un servicio CQRS seguro, observable y con event sourcing, dotado de una saga, un
flujo de trabajo, una transferencia en dos fases y una tarea programada de
mantenimiento. Todo lo que añadiste llegó de la misma forma: *declarando un bean*
que el framework descubre, y el punto de entrada nunca cambió. Este capítulo final
del arco de construcción cierra el círculo: veremos exactamente cómo ese
**`main` de una línea** arranca y se apaga de forma fiable, activaremos el
**endpoint reactivo de streaming** opcional y recorreremos el camino desde
"funciona en mi máquina" hasta un contenedor en producción: apagado controlado, la
separación entre el puerto público y el de gestión, configuración dirigida por el
entorno, empaquetado y el reemplazo del almacén de eventos y el broker en memoria
por Postgres y Kafka duraderos.

Nada de esto reescribe Lumen. El endpoint de streaming es un bean más; el cambio a
Postgres es una factoría de bean editada en su sitio; el resto es postura
operativa. Esa es la recompensa del diseño de puertos y adaptadores que has estado
construyendo todo este tiempo.

Al terminar este capítulo, serás capaz de:

- Seguir `run()` de principio a fin: el pipeline de arranque de ocho etapas, los
  dos servidores y el drenaje controlado ante SIGINT/SIGTERM que asigna un apagado
  limpio a `Ok(())`.
- Añadir el endpoint reactivo de streaming opcional (`GET /api/v1/wallets/:id/events`
  → NDJSON o SSE) como un bean `RouteContributor` protegido por feature, y entender
  por qué el 404 se resuelve antes de que arranque el cuerpo del streaming.
- Servir el actuator en un puerto de gestión separado y protegido por firewall, y
  apuntar las sondas de liveness/readiness de tu orquestador a las sub-rutas
  correctas.
- Activar el middleware de endurecimiento para producción a través de `CoreConfig`
  y leer la cadena efectiva de filtros de fuera hacia dentro.
- Reemplazar el almacén de eventos y el broker en memoria por Postgres y Kafka
  editando una sola factoría `#[bean]`, sin que cambie nada aguas abajo.
- Empaquetar Lumen como un contenedor y verificarlo frente a una lista de
  comprobación de despliegue.

## Conceptos que conocerás

Antes del primer paso, aquí están las ideas en las que se apoya este capítulo.
Cada una se reintroduce en contexto donde se usa por primera vez; esta es la
versión corta.

> **Note** **Término clave — apagado controlado (graceful shutdown).** El *apagado
> controlado* significa que, cuando se pide al proceso que se detenga, deja de
> aceptar nuevas peticiones, permite que terminen las peticiones en curso (dentro
> de un presupuesto de tiempo) y solo entonces sale. El análogo en Spring Boot es
> el ajuste `server.shutdown=graceful` más el drenaje del servidor embebido;
> Firefly lo hace por defecto sin ninguna configuración.

> **Note** **Término clave — superficie de gestión.** La *superficie de gestión* es
> el conjunto de endpoints HTTP operativos —salud, info, métricas, entorno, control
> del nivel de log— que existen para operadores y orquestadores, no para usuarios
> finales. Firefly los sirve en un listener separado del de tu API de negocio. Esto
> refleja Spring Boot Actuator en un `management.server.port` dedicado.

> **Note** **Término clave — RouteContributor.** Un *`RouteContributor`* es un bean
> que aporta un sub-router (`axum::Router`) a la API pública. El framework descubre
> cada bean `RouteContributor` y fusiona sus rutas en la aplicación ensamblada, de
> modo que puedes añadir rutas sin tocar `main` ni ningún `#[rest_controller]`. El
> análogo en Spring es aportar un bean `RouterFunction<ServerResponse>` que el
> contexto recoge automáticamente.

> **Note** **Término clave — stream reactivo / `Flux`.** Un *`Flux<T>`* es una
> secuencia reactiva de cero o más valores `T` producidos a lo largo del tiempo con
> contrapresión (backpressure): el análogo en Rust del `Flux<T>` de Project Reactor.
> Devuelto desde un handler como `application/x-ndjson` o `text/event-stream`, hace
> streaming elemento a elemento hacia el cliente en lugar de almacenar en búfer una
> respuesta completa.

> **Note** **Término clave — puerto y adaptador.** Un *puerto* es una capacidad
> abstracta de la que depende el dominio (aquí `EventStore`, `Broker`); un
> *adaptador* es una implementación concreta de ese puerto (en memoria hoy,
> Postgres/Kafka en producción). El dominio solo habla con el puerto, así que
> cambiar el adaptador no altera nada aguas abajo. Este es el patrón de arquitectura
> hexagonal que Spring expresa con interfaces y factorías `@Bean`.

## Paso 1 — Lee el `main` de una línea una vez más

Abre `src/main.rs`. Después de cada capítulo del arco de construcción, el punto de
entrada sigue siendo una única llamada:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

Lo que acaba de ocurrir: esa única llamada a `run()` hace component-scan de los
beans de Lumen —el bus CQRS, el ledger con event sourcing, la proyección del
modelo de lectura, la caché de consultas y la cadena de seguridad, todo sobre
infraestructura en memoria—, auto-monta cada `#[rest_controller]`, autodescubre la
seguridad y el middleware del bus de read-cache, drena los handlers CQRS / el
listener EDA / la tarea `#[scheduled]` registrados en el inventario, auto-hospeda el
panel de administración, imprime el banner y el informe de arranque línea por línea
y sirve ambos puertos con apagado controlado. Todo lo que has visto capítulo a
capítulo se *declara como un bean*; `main` simplemente entrega el crate al framework.

> **Note** El binario de Lumen arranca con un almacén de eventos y un broker en
> memoria, así que `cargo run --bin lumen` no necesita nada externo: ni base de
> datos ni message broker. Los tests ejercitan el mismo cableado en proceso a través
> de `build_router()`, que llama a `FireflyApplication::bootstrap()` en lugar de
> servir sobre un socket.

> **Tip** **Punto de control.** `cargo run --bin lumen` imprime el banner de
> Firefly, las dos URL de gestión y el informe de arranque, y luego se queda en
> ejecución en `:8080` (público) y `:8081` (gestión). `Ctrl-C` sale limpiamente. Si
> eso funciona, el resto de este capítulo trata de lo que sucede por debajo y de cómo
> llevarlo a producción.

## Paso 2 — Entiende el pipeline de arranque

No hay cableado de ciclo de vida escrito a mano en Lumen: `run()` lo hace todo. Por
debajo, `run()` son exactamente dos llamadas:

```rust,ignore
// firefly::FireflyApplication::run (simplified)
pub async fn run(self) -> Result<(), BoxError> {
    self.bootstrap().await?.serve().await
}
```

`bootstrap()` ensambla la aplicación completamente cableada y devuelve un valor
`Bootstrapped` (el router, el contenedor de DI, el scheduler y las dos direcciones
de bind) *sin* servir. `serve()` lo ejecuta luego sobre el `Application` del ciclo
de vida, que atrapa SIGINT/SIGTERM, da a cada tarea de servidor su propia señal de
drenaje y concede un presupuesto de drenaje antes de salir. El pipeline, en orden:

1. **Construir la pila web** y bifurcar el logging hacia el búfer de captura del
   panel de administración.
2. **Hacer component-scan del contenedor**: auto-registrar los beans de
   infraestructura del framework y luego descubrir las factorías
   `#[derive(Configuration)]` / `#[bean]` de Lumen, los controladores
   `#[derive(Controller)]` y los campos `#[autowired]`.
3. **Auto-configurar el bus CQRS**: propagación de correlación siempre; el
   middleware de read-cache porque Lumen declara un bean `QueryCache`.
4. **Autodescubrir la seguridad**: los beans `FilterChain` + `BearerLayer`
   ([Seguridad](./14-security.md)), aplicados como capas sobre la API sin ninguna
   llamada `.security(...)`.
5. **Auto-montar los controladores**: cada `#[rest_controller]` y cada bean
   `RouteContributor` (incluido el endpoint de streaming añadido en el Paso 3),
   luego aplicar la cadena de middleware y originar el contexto de traza W3C.
6. **Drenar los handlers descubiertos**: los handlers de comando/consulta CQRS, el
   listener de proyección EDA y la tarea de mantenimiento `#[scheduled]`, desde los
   registros de inventario.
7. **Auto-hospedar el panel de administración** en el puerto de gestión y servir
   automáticamente los docs de OpenAPI (Swagger UI, ReDoc, la especificación
   OpenAPI 3.1), todo en el puerto de gestión, nunca en el público.
8. **Imprimir el informe de arranque** y luego **servir ambos puertos** con
   drenaje controlado.

> **Note** **Término clave — `bootstrap()` frente a `serve()`.** `bootstrap()` es la
> costura de pruebas (test seam): devuelve la app `Bootstrapped` cableada —incluyendo
> `Bootstrapped::api_router`, el router público completamente ensamblado— sin enlazar
> un socket, de modo que los tests ejercitan la app real en proceso. `serve()` es la
> ruta de producción que de hecho escucha. `run()` no es más que
> `bootstrap().await?.serve().await`.

Los dos servidores y el drenaje son la parte que importa para producción:

- **Dos servidores, dos drenajes.** La API pública sirve en `:8080` y la superficie
  de gestión (`/actuator/*` más el panel `/admin` auto-hospedado más los docs de la
  API) en `:8081`. Cada uno se ejecuta en su propia tarea con su propio handle de
  `shutdown`, de modo que una señal drena ambos listeners de forma independiente:
  `axum::serve(...).with_graceful_shutdown(shutdown.wait())` por servidor.
- **`run()` se bloquea hasta una señal.** Retorna cuando se recibe SIGINT/SIGTERM y
  el drenaje se completa. Un apagado limpio aflora internamente como un error
  *cancelado*, que `serve()` asigna a `Ok(())`; cualquier otro error se propaga
  fuera de `main` y el proceso sale con código distinto de cero.
- **Binds sobreescribibles por el entorno.** `FIREFLY_SERVER_ADDR` y
  `FIREFLY_MANAGEMENT_ADDR` sobreescriben los valores por defecto
  (`0.0.0.0:8080` / `0.0.0.0:8081`), de modo que un contenedor lee sus puertos del
  entorno sin cambiar código.

Merece la pena ver exactamente la asignación "cancelado es limpio", porque es la
razón por la que `Ctrl-C` no es un error:

```rust,ignore
// Bootstrapped::serve (the tail of it)
match application.run().await {
    Ok(()) => Ok(()),
    // A handle/signal-triggered stop is a clean shutdown, not a failure.
    Err(err) if err.is_cancelled() => Ok(()),
    Err(err) => Err(Box::new(err)),
}
```

Lo que acaba de ocurrir: el `Application` del ciclo de vida ejecuta ambas tareas de
servidor; un SIGINT/SIGTERM las cancela, lo que aflora como un error *cancelado*;
`serve()` captura exactamente ese caso y devuelve `Ok(())`, de modo que `main` sale
con código cero. Cualquier fallo genuino (un puerto ya enlazado, un panic en una
tarea de servidor) se propaga y el proceso sale con código distinto de cero, que es
lo que quieres que provoque un reinicio en un orquestador.

> **Tip** **Punto de control.** Ejecuta `cargo run --bin lumen` y luego pulsa
> `Ctrl-C`. El proceso sale sin traza de pila y con código cero (`echo $?` imprime
> `0`). Esa es la asignación cancelado-a-`Ok(())` en acción.

## Paso 3 — Añade el endpoint reactivo de streaming (feature `streaming`)

El último endpoint de Lumen hace streaming del historial de eventos de un wallet.
Está protegido por feature para que la base didáctica se mantenga ligera: no
necesita nada más allá de la fachada `firefly` (`firefly::reactive::Flux` más
`firefly::web::{NdJson, Sse}`). `Cargo.toml` ya declara el flag, desactivado por
defecto:

```toml
# Cargo.toml
[features]
# The reactive streaming endpoint is feature-gated so the teaching baseline
# stays lean; this chapter turns it on. It needs nothing beyond the facade.
default = []
streaming = []
```

### 3a — Declara la ruta como un bean

El endpoint se cablea **declarando un bean**, no editando un punto de entrada. Un
`#[derive(Service)]` que `provides = "dyn firefly::web::RouteContributor"` aporta el
sub-router; `FireflyApplication` lo resuelve como el puerto `dyn RouteContributor`
(Paso 2, etapa 5) y fusiona sus rutas, de modo que un endpoint protegido por feature
aparece en la API simplemente porque su crate lo compiló. Añade esto a `src/web.rs`:

```rust,ignore
// src/web.rs
/// (feature `streaming`) A `RouteContributor` bean adding the reactive
/// `GET /api/v1/wallets/:id/events` endpoint. The framework discovers it
/// (resolved as the `dyn RouteContributor` port) and merges its routes — a
/// feature-gated endpoint wired by declaring a bean, not by a composition root.
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

Lo que acaba de ocurrir, bloque a bloque:

- `#[derive(Service)]` convierte `StreamingRoutes` en un bean de DI. El atributo
  `#[firefly(provides = "dyn firefly::web::RouteContributor")]` lo registra bajo el
  *puerto* `RouteContributor`, de modo que el framework lo encuentra cuando recopila
  los contribuidores de rutas; nunca nombras `StreamingRoutes` en ningún otro lugar.
- `#[autowired] api: Arc<WalletApi>` inyecta el mismo bean de controlador que usa el
  `#[rest_controller]`, así que el stream lee los mismos wallets que escribe el
  resto de la API.
- `impl RouteContributor` devuelve el sub-router que construye `streaming_router`.
  `RouteContributor::routes(&self) -> axum::Router` es el único método que requiere
  el trait.

> **Note** Todo en esta sección está detrás de `#[cfg(feature = "streaming")]`, de
> modo que con la feature desactivada el archivo compila sin nada adicional y el
> endpoint no existe. Activarlo es un flag de compilación, no un cambio de código en
> `main`.

### 3b — Construye el sub-router y el handler

El sub-router mapea la única ruta al handler sobre el estado del controlador, y el
handler carga los eventos persistidos del wallet, los mapea a la forma de vista, los
envuelve en un `Flux` y devuelve NDJSON por defecto o SSE cuando se pasa
`?format=sse`:

```rust,ignore
// src/web.rs
/// Builds the streaming sub-router over the controller state.
#[cfg(feature = "streaming")]
fn streaming_router(api: WalletApi) -> axum::Router {
    axum::Router::new()
        .route(
            "/api/v1/wallets/:id/events",
            axum::routing::get(stream_events),
        )
        .with_state(api)
}

/// The reactive streaming handler: builds a `Flux<WalletEvent>` over the
/// wallet's persisted stream and returns it as NDJSON (one JSON document per
/// line) or, with `?format=sse`, as Server-Sent Events.
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

Lo que acaba de ocurrir, bloque a bloque:

- `streaming_router` devuelve un `axum::Router` simple con `GET
  /api/v1/wallets/:id/events` mapeado a `stream_events` y el estado `WalletApi`
  adjunto. Este es el sub-router que `StreamingRoutes::routes` entrega.
- `stream_events` primero llama a `api.ledger.load_events(&id)`. Si el wallet está
  ausente, el ledger devuelve `Err(NotFound)`, y el handler lo renderiza como un 404
  RFC 9457 `application/problem+json` *y retorna*, antes de que haya comenzado ningún
  cuerpo de streaming.
- En caso de éxito, mapea los eventos de dominio a la forma de vista `WalletEvent`,
  envuelve el `Vec` en `Flux::just(...)` y elige la codificación: `Sse(flux)` para
  `?format=sse`, en caso contrario `NdJson(flux)`.

> **Note** **Término clave — `NdJson` y `Sse`.** `NdJson(flux)` (`pub struct
> NdJson<T>(pub Flux<T>)`) renderiza el `Flux` como un documento JSON por línea con
> tipo de contenido `application/x-ndjson`; `Sse(flux)` renderiza Server-Sent Events
> con tipo de contenido `text/event-stream`. Ambos envuelven un `Flux<T>` e
> implementan `IntoResponse`, de modo que un handler los devuelve directamente.

> **Warning** Aquí el orden importa. El 404 para un wallet desconocido debe
> resolverse *antes* de que se confirme la cabecera de respuesta, porque una vez que
> arranca un cuerpo de streaming la línea de estado ya está en el cable y no puede
> cambiar. Por eso `load_events` se espera y se comprueba primero, y solo entonces se
> construye un `Flux`.

> **Note** `Flux::just(items)` materializa un `Vec` conocido: perfecto para un
> historial de eventos finito que ya está cargado. Un stream de producción sobre una
> fuente viva y no acotada (p. ej. una suscripción a un broker) usaría en su lugar
> `Flux::from_stream(...)`, de modo que el cuerpo se produzca de forma perezosa con
> contrapresión en lugar de almacenarse en búfer por adelantado.

### 3c — Demuestra los tres comportamientos con un test

`src/streaming_test.rs` (compilado solo bajo `#[cfg(all(test, feature =
"streaming"))]`) arranca un contexto de app, abre un wallet, hace un depósito —de
modo que el stream tiene dos eventos— y verifica el NDJSON por defecto, el cambio a
SSE y el 404. El caso por defecto:

```rust,ignore
// src/streaming_test.rs
#[tokio::test]
async fn events_stream_as_ndjson_by_default() {
    let app = build_router().await;
    let id = open_with_deposit(&app).await; // two events: WalletOpened + MoneyDeposited
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/wallets/{id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(ct.contains("ndjson"), "default stream should be NDJSON, got {ct:?}");

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected 2 NDJSON lines, got: {text:?}");
    assert!(text.contains("WalletOpened"));
    assert!(text.contains("MoneyDeposited"));
}
```

Lo que acaba de ocurrir: `build_router().await` devuelve el router público
completamente cableado en proceso (llama a `FireflyApplication::bootstrap()` por
debajo, como en [Pruebas](./18-testing.md)). El test lo ejercita con
`tower::ServiceExt::oneshot` —sin enlazar ningún socket—, abre un wallet con un
depósito, luego hace `GET` al stream de eventos y verifica que la respuesta es
`200`, es `application/x-ndjson` y lleva exactamente dos líneas JSON (el
`WalletOpened` y el `MoneyDeposited`). Los tests hermanos verifican que
`?format=sse` cambia el tipo de contenido a `text/event-stream` y que un id de
wallet desconocido es un `404`.

> **Tip** **Punto de control.** Compila y prueba con la feature activada:
>
> ```bash
> cargo test -p firefly-sample-lumen --features streaming
> ```
>
> Los tres tests de streaming pasan. Luego ejecuta el binario de la misma forma —
> `cargo run --bin lumen --features streaming`—, abre un wallet, haz un depósito y
> `curl http://127.0.0.1:8080/api/v1/wallets/<id>/events` para ver las dos líneas
> NDJSON en el puerto público.

## Paso 4 — Separa la superficie de gestión para producción

Sirve siempre el actuator en un **listener diferente** del de la API pública para
que `/actuator/*` sea alcanzable por tu orquestador pero nunca en la red pública:
exactamente la separación `:8080` / `:8081` que Lumen usa por defecto. Protege con
firewall el puerto de gestión hacia la red interna de tu clúster. Las sub-rutas de
salud alimentan las sondas de tu orquestador:

| Sonda     | Endpoint                            |
|-----------|-------------------------------------|
| liveness  | `/actuator/health/liveness`         |
| readiness | `/actuator/health/readiness`        |
| general   | `/actuator/health`                  |

Lo que acaba de ocurrir: el router de gestión (Paso 2, etapa 7) monta el árbol
completo del actuator más el panel de administración y los docs de la API. La sonda
de liveness reporta solo los indicadores etiquetados para liveness (¿está vivo el
proceso?); la de readiness reporta solo los indicadores de readiness (¿puede servir
tráfico, están las dependencias en pie?). Apunta la sonda de liveness de tu
orquestador a `/actuator/health/liveness` y su sonda de readiness a
`/actuator/health/readiness`, ambas en `:8081`.

> **Note** Las métricas para scraping viven en el mismo puerto de gestión:
> `/actuator/prometheus` sirve la exposición de Prometheus con etiquetas, y
> `/actuator/metrics` sirve la vista JSON. Apunta tu scraper a `:8081`, nunca a
> `:8080`.

> **Tip** **Punto de control.** Con Lumen en ejecución, desde una segunda terminal:
> `curl localhost:8081/actuator/health/readiness` devuelve un cuerpo JSON con un
> `"status"`, y la misma ruta en `:8080` no devuelve nada: el puerto público no
> tiene `/actuator/*`.

## Paso 5 — Activa el middleware de endurecimiento para producción

El framework ya activa las "pilas" de la capa web cuando `FireflyApplication`
construye la pila web: el renderizador de problemas RFC 9457, la propagación del
id de correlación, la originación del contexto de traza W3C, las métricas de
peticiones y el replay de idempotencia. El middleware de producción restante es
opt-in a través de `CoreConfig`, ajustado vía
`FireflyApplication::configure(|cfg| { … })`, y cada parámetro teje su capa en el
orden de filtro correcto:

```rust,ignore
// Opt-in production middleware, tuned at the entry point.
firefly::FireflyApplication::new("lumen")
    .configure(|cfg| {
        cfg.cors = Some(firefly::web::CorsConfig::default());
        cfg.security_headers = Some(firefly::web::SecurityHeadersConfig::default());
        cfg.csrf = Some(firefly::web::CsrfLayer::new()); // browser flows only
        cfg.request_log = Some(firefly::web::RequestLogLayer::default());
    })
    .run()
    .await
```

Los parámetros y lo que añade cada uno:

| Parámetro          | Añade                                                  |
|--------------------|-------------------------------------------------------|
| `cors`             | preflight CORS + decoración de simple-request          |
| `security_headers` | cabeceras de respuesta OWASP (`nosniff`, `DENY`, HSTS, …) |
| `csrf`             | CSRF de double-submit-cookie (para flujos de navegador) |
| `request_log`      | un evento estructurado de access-log por petición     |
| `request_metrics`  | `http_server_requests_seconds` + `_max` (actuator)    |
| `http_exchanges`   | registrador de intercambios recientes + `/actuator/httpexchanges` |
| `loggers`          | control en runtime del nivel de log en `/actuator/loggers` |

Lo que acaba de ocurrir: `configure(|cfg| { … })` te entrega el `CoreConfig` antes
de que se construya la pila web, de modo que las capas que activas se tejen en el
arranque. Cada parámetro opcional está OFF por defecto, salvo las métricas de
peticiones, que están activadas por defecto (auto-instrumentación al estilo Spring
Boot) y se ajustan —o se desactivan— a través de `request_metrics` /
`disable_request_metrics`.

La cadena efectiva, de la más externa (la más cercana a la red) a la más interna (la
más cercana a tu handler), es:

```text
CorsLayer            (cors)              — preflight + simple-request edge
ProblemLayer         (always)           — panic → RFC 9457 500
SecurityHeadersLayer (security_headers) — decorate every response
TraceContextLayer    (always)           — validate/originate W3C traceparent
CorrelationLayer     (always)           — X-Correlation-Id (+ request ctx)
MetricsLayer         (request_metrics)  — http_server_requests_*
HttpExchangesLayer   (http_exchanges)   — record into the recorder
RequestLogLayer      (request_log)      — one access-log event
CsrfLayer            (csrf)             — double-submit cookie
IdempotencyLayer     (always)           — replay on Idempotency-Key
        │
        ▼
     your router
```

La idempotencia se queda en la posición más interna para que una petición repetida
siga pasando por todas las preocupaciones externas (correlación, métricas, el
access-log). El `TraceContextLayer` de W3C se sitúa justo por fuera de la correlación
para poder originar un span raíz y un `traceparent` que la capa interna de
correlación luego hace eco en la respuesta.

> **Design note.** Esta es la misma postura de actuator y middleware que trae Spring
> Boot, pero activada de forma declarativa en un único punto de llamada en lugar de a
> través de una dispersión de propiedades y clases `@Configuration`. Un
> `FireflyApplication` desnudo ya te da el núcleo siempre activo de Problem →
> TraceContext → Correlation → Idempotency; `configure(...)` añade el resto.

> **Tip** **Punto de control.** Ejecuta Lumen con `security_headers` activado y
> `curl -i localhost:8080/api/v1/wallets/anything`. La respuesta lleva
> `X-Content-Type-Options: nosniff` y `X-Frame-Options: DENY` incluso en el cuerpo de
> problema del 404: prueba de que la capa decora cada respuesta, incluidos los
> errores recuperados.

## Paso 6 — Configura para producción desde el entorno

Enlaza la configuración desde fuentes en capas con las sobreescrituras del entorno
en la cima, de modo que un contenedor lea sus ajustes del entorno
([Configuración](./03-configuration.md)). Para Lumen las dos direcciones de bind ya
están dirigidas por el entorno, así que el contenedor de producción no necesita
ningún archivo de configuración solo para mover sus puertos:

```bash
FIREFLY_PROFILE=prod \
FIREFLY_SERVER_ADDR=0.0.0.0:8080 \
FIREFLY_MANAGEMENT_ADDR=0.0.0.0:8081 \
  ./lumen
```

Lo que acaba de ocurrir: `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` se leen en
tiempo de construcción (Paso 2) y sobreescriben los valores por defecto
`0.0.0.0:8080` / `0.0.0.0:8081`, mientras que `FIREFLY_PROFILE=prod` selecciona la
capa de propiedades de producción. Las variables `FIREFLY_*` ganan a los archivos
YAML, los secretos se enmascaran en `/actuator/env` y los marcadores `${...}` se
resuelven entorno-luego-config-luego-defecto.

> **Warning** La clave de firma JWT de [Seguridad](./14-security.md) es lo más
> evidente que se inyecta de esta forma —desde el entorno o un almacén de secretos—
> en lugar de la constante `DEMO_SIGNING_KEY` en línea que Lumen trae con fines
> didácticos. Nunca incrustes una clave de firma real en el binario ni la subas al
> control de versiones.

## Paso 7 — Reemplaza la infraestructura en memoria por Postgres y Kafka

Esta es la recompensa de toda la arquitectura. Lumen reemplaza sus valores por
defecto en memoria por backends reales **cambiando una factoría `#[bean]` en
`LumenBeans`**, y nada aguas abajo cambia. Recuerda la factoría en memoria de
`src/web.rs`:

```rust,ignore
// src/web.rs — today
#[bean]
impl LumenBeans {
    /// The in-memory event store (`@Bean`).
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }

    // … the ledger factory autowires the EventStore + the framework Broker port
    #[bean]
    fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

Para pasar a Postgres, devuelve el almacén de eventos respaldado por SQL del
framework (`firefly::eventsourcing::SqlEventStore`) detrás del mismo puerto
`EventStore`. Toma un puerto `Database`, así que el cambio queda contenido en la
factoría:

```rust,ignore
// src/web.rs — production: a Postgres-backed event store behind the EventStore port.
use firefly::eventsourcing::{EventStore, SqlEventStore};

#[bean]
impl LumenBeans {
    /// An async `#[bean]` factory: connect the pool, build the SQL event store,
    /// create its table once, and hand back a `dyn EventStore` for the `ledger`
    /// factory to autowire. Any error here aborts startup (fail-fast).
    #[bean]
    async fn event_store(&self, db: Arc<dyn firefly::transactional::Database>) -> SqlEventStore {
        let store = SqlEventStore::new(db);
        store.initialize().expect("create event-store table");
        store
    }
}
```

Lo que acaba de ocurrir: la factoría `ledger` depende del *puerto* `EventStore`, y
el `Ledger`, la proyección del modelo de lectura, los handlers CQRS, la saga, la
transferencia TCC y todos los tests están escritos contra los puertos `EventStore` y
`Broker`, de modo que el dominio del wallet nunca se entera de que pasó de un
`HashMap` a Postgres. La misma forma se aplica a la mensajería: donde Lumen
sobreescribe el broker, un `#[bean]` devuelve un adaptador de Kafka detrás del puerto
`Broker` del framework, y el listener de proyección EDA consume de Kafka en lugar del
bus en proceso sin cambiar una sola línea de la proyección.

> **Note** El bean `event_store` aquí es una `async fn`. El framework espera las
> factorías de bean asíncronas durante el component-scan (Paso 2, etapa 2), de modo
> que el pool se conecta y el almacén está vivo antes de que nada lo resuelva, y un
> fallo de conexión aborta el arranque en lugar de aflorar en la primera petición.
> Esa es la propiedad fail-fast que quieres en producción.

> **Design note.** "Cambia el adaptador, conserva el código" aplicado a las capas de
> almacenamiento y mensajería, con el cambio localizado en una única factoría de
> bean. El dominio, los handlers, la proyección, la saga y los tests están escritos
> contra puertos: exactamente el diseño hexagonal hacia el que ha construido este
> libro.

> **Tip** **Punto de control.** No necesitas levantar realmente Postgres para
> aprender la forma: lee la factoría `ledger` y confirma que nombra
> `Arc<dyn EventStore>`, no `MemoryEventStore`. Cualquier cosa que autowire el
> *puerto* está lista para el cambio por construcción.

## Paso 8 — Empaqueta Lumen como un contenedor

Una compilación multi-etapa típica compila el binario de release en una imagen de
Rust y luego copia solo el binario en una imagen de runtime ligera:

```dockerfile
# Dockerfile
FROM rust:1.88 AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p firefly-sample-lumen

FROM debian:bookworm-slim
COPY --from=build /app/target/release/lumen /usr/local/bin/lumen
EXPOSE 8080 8081
ENTRYPOINT ["/usr/local/bin/lumen"]
```

Lo que acaba de ocurrir: la etapa `build` compila el paquete `firefly-sample-lumen`
(su `[[bin]]` se llama `lumen`, así que el artefacto queda en
`target/release/lumen`); la etapa de runtime copia solo ese binario en una imagen
mínima de Debian y expone ambos puertos. Como `run()` atrapa SIGTERM y drena (Paso
2), el contenedor se detiene limpiamente cuando el orquestador envía una señal de
terminación: sin necesidad de un shim `--init` ni de un wrapper de reenvío de
señales.

> **Tip** **Punto de control.** `docker build -t lumen .` produce una imagen, y
> `docker run -p 8080:8080 -p 8081:8081 lumen` la arranca. `docker stop` sobre ese
> contenedor sale limpiamente (sin recurrir a SIGKILL dentro del presupuesto de
> drenaje), porque el binario gestiona SIGTERM por sí mismo.

## Una lista de comprobación de despliegue

- [ ] Actuator (`:8081`) en un puerto **separado y protegido por firewall** del de la
      API (`:8080`).
- [ ] Sondas de liveness/readiness apuntadas a
      `/actuator/health/{liveness,readiness}`.
- [ ] `security_headers`, `cors` y (para flujos de navegador) `csrf` activados a
      través de `configure(...)`.
- [ ] `request_log` + `request_metrics` activados; logs enviados como JSON, métricas
      raspadas desde `/actuator/prometheus`.
- [ ] Propagación de correlación verificada de extremo a extremo entre servicios.
- [ ] Clave de firma JWT inyectada desde el entorno / un almacén de secretos, no en
      línea.
- [ ] Almacén de eventos + broker en memoria reemplazados por Postgres + Kafka en las
      factorías `#[bean]` de `LumenBeans`.
- [ ] Presupuesto de drenaje del apagado controlado ajustado a la transferencia en
      curso más lenta.
- [ ] La puerta de verificación en verde: `cargo test -p firefly-sample-lumen` y
      `--features streaming`, más `clippy -D warnings` y `fmt --check`.

## Resumen — qué cambió en Lumen

| Antes de este capítulo | Después de este capítulo |
|---------------------|--------------------|
| un servicio cableado que ejecutabas pero no habías llevado a producción | un contenedor desplegable con la separación de gestión, el middleware de endurecimiento y un camino de cambio de puerto |
| sin endpoint de streaming | el bean `RouteContributor` `StreamingRoutes` protegido por feature sirviendo `GET /api/v1/wallets/:id/events` como NDJSON (por defecto) o SSE (`?format=sse`) |
| solo almacén de eventos / broker en memoria | el cambio de un solo `#[bean]` a `SqlEventStore` + un adaptador `Broker` de Kafka, sin que cambie nada aguas abajo |

Ahora también sabes:

- Que `run()` es `bootstrap().await?.serve().await`: un arranque de ocho etapas, dos
  servidores cada uno con su propio drenaje, y un error *cancelado* asignado a
  `Ok(())` para que un apagado por señal salga con código cero.
- Que un endpoint protegido por feature se cablea simplemente declarando un bean
  `RouteContributor` —`main` nunca cambia— y que un handler de streaming debe
  resolver su 404 antes de que se confirme la cabecera de respuesta.
- Que el endurecimiento para producción (CORS, cabeceras OWASP, CSRF, access-log) es
  opt-in a través de `CoreConfig` en un único punto de llamada `configure(...)`,
  tejiéndose en el orden de filtro correcto.
- Que el cambio de almacenamiento y mensajería es una única factoría de bean, porque
  el dominio, los handlers, la proyección, la saga y los tests dependen todos de los
  puertos `EventStore` y `Broker`.

Eso completa el arco de construcción guiado. Lumen empezó como un directorio vacío en
[Quickstart](./02-quickstart.md); ahora es un servicio CQRS seguro, observable y con
event sourcing que hace streaming de su historial y se despliega como un único
contenedor, y el `main` de una línea nunca cambió.

## Ejercicios

1. **Ejecuta y drena.** `cargo run --bin lumen`, abre un wallet, luego `Ctrl-C` y
   observa el drenaje controlado. Confirma que el proceso sale con código cero con
   `echo $?`, y que no se imprime ninguna traza de pila: la asignación
   cancelado-a-`Ok(())` del Paso 2.
2. **Sobreescribe los puertos.** Arranca Lumen con `FIREFLY_SERVER_ADDR=0.0.0.0:9000
   FIREFLY_MANAGEMENT_ADDR=0.0.0.0:9001 cargo run --bin lumen` y confirma que la API
   se movió a `:9000` y el actuator a `:9001`, de forma independiente.
3. **Haz streaming del historial.** Compila con `--features streaming`, abre un
   wallet y haz un depósito, luego `curl http://127.0.0.1:8080/api/v1/wallets/<id>/events`
   (NDJSON) y `…?format=sse` (SSE). Compara las dos cabeceras `content-type` y
   confirma que `GET /api/v1/wallets/wlt_missing/events` devuelve un documento de
   problema `404`.
4. **Endurece la cadena.** Añade `cfg.security_headers = Some(...)` y
   `cfg.request_log = Some(...)` a través de `configure(...)`, vuelve a ejecutar e
   inspecciona una respuesta con `curl -i`. Encuentra las cabeceras OWASP, luego sitúa
   cada capa en la cadena de fuera hacia dentro del Paso 5.
5. **Esboza el cambio a Postgres.** Escribe el `#[bean]` `event_store` en
   `LumenBeans` que devuelve un `SqlEventStore` sobre un puerto `Database`, y explica
   en una frase por qué la factoría `ledger`, la proyección del modelo de lectura y
   los tests no necesitan ningún cambio.

## Adónde ir después

- Revisa los macros declarativos que hicieron posible todo esto —los atributos
  `#[bean]`, `#[rest_controller]`, `#[command_handler]`, `#[saga]` y `#[scheduled]`
  como broche final— en
  **[Servicios declarativos con macros](./21-declarative-macros.md)**.
- Busca cualquier bloque de construcción por crate en el
  **[Índice de módulos](./91-appendix-modules.md)**, o cualquier término en el
  **[Glosario](./92-glossary.md)**.
