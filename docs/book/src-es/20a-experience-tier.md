# La capa de experiencia (BFF)

Lumen, tal como lo ha construido el libro, es un servicio autocontenido: posee el
dominio de la cartera (wallet) *y* la API HTTP que lo expone. Las instalaciones
reales de Firefly reparten esa responsabilidad entre **tres capas de servicio**, y
Lumen se sitúa en las dos inferiores. Este capítulo introduce la única capa en la
que Lumen jamás entra — la **capa de experiencia**, la capa Backend-for-Frontend
de Firefly — y luego conecta un pequeño BFF contra el servicio que ya tienes. El
BFF compone Lumen como un SDK de dominio aguas abajo, conduce un trayecto
multietapa de "financiar una cartera y confirmar" mediante un workflow regulado
por señales, y sobrevive a una desconexión del cliente persistiendo el estado del
trayecto.

Como esta capa es terreno nuevo, el capítulo la enseña desde primeros principios:
qué son las tres capas y por qué la dirección de dependencia es unidireccional,
qué te ofrece un `ExperienceStack`, cómo registrar un SDK de dominio, cómo una
puerta de señal (signal gate) aparca un workflow hasta que llega un evento
externo, y cómo persistir y consultar un trayecto que abarca varias peticiones
HTTP. Cada API aquí procede del crate real `firefly-starter-experience` — la misma
superficie que su propio test de arranque ejercita de extremo a extremo.

Al terminar este capítulo, serás capaz de:

- Explicar el modelo de capas `channel → experience → domain → core` y por qué la
  capa de experiencia solo puede componer SDKs de *dominio* — nunca una base de
  datos, un servicio core ni un BFF hermano.
- Construir un `ExperienceStack` (el starter del BFF) y entender cómo hereda todas
  las baterías web completas a la vez que añade cinco bloques de construcción de la
  capa de experiencia.
- Registrar Lumen como un SDK de dominio con nombre a través de `DomainClients` y
  resolverlo por su nombre lógico desde cualquier handler o paso de workflow.
- Modelar un trayecto multipetición como un `Workflow` cuya puerta aparca sobre una
  señal con nombre, y reanudarlo entregando esa señal desde una petición posterior.
- Persistir el estado de un trayecto con `WorkflowState` (compatible con Redis) y
  responder "¿dónde está mi trayecto?" con `WorkflowQueryService`, de modo que un
  cliente pueda reconectarse tras una desconexión.
- Ensamblar el controlador REST atómico de tres endpoints que lo une todo.

## Conceptos que conocerás

Estas son las ideas en las que se apoya el capítulo. Cada una se reintroduce en
contexto la primera vez que se usa; esta es la versión breve.

> **Note** **Término clave — Backend-for-Frontend (BFF).** Un BFF es un servicio
> HTTP que existe para servir a *un* frontend o canal: agrega varios servicios
> aguas abajo en endpoints moldeados exactamente para las pantallas y flujos de esa
> interfaz. No posee base de datos propia. El análogo en Spring es un Spring Cloud
> Gateway / servicio de agregación situado delante de tus microservicios de dominio
> — aquí es una capa de primera clase, con baterías incluidas.

> **Note** **Término clave — SDK de dominio.** Un *SDK de dominio* no es más que un
> cliente HTTP apuntando a la API pública de un servicio de dominio aguas abajo,
> revestido con la propagación de correlación del framework, el códec JSON, la
> decodificación de errores y el reintento/backoff. Un BFF llama a sus dependencias
> a través de estos SDKs exactamente igual que lo haría cualquier cliente externo —
> nunca accede a sus interioridades.

> **Note** **Término clave — puerta de señal (signal gate).** Una *puerta de señal*
> es un paso de workflow que aparca (suspende) hasta que se entrega una *señal* con
> nombre desde fuera del workflow — normalmente mediante una petición HTTP
> posterior. Modela "esperar a que el cliente confirme" dentro de un trayecto por lo
> demás secuencial. El análogo en Java/Firefly es un paso `@WaitForSignal`; no hay
> equivalente directo en Spring Boot.

## Paso 1 — Entender las tres capas de servicio

Las instalaciones de Firefly se estructuran en tres capas de **servicio**
(distintas de las capas del grafo de crates de la documentación de arquitectura).
La dirección de dependencia es estricta y unidireccional:
`channel → experience → domain → core`. Una capa solo puede llamar a la capa
directamente inferior.

| Capa de servicio | Posee | Habla con | Starter en Rust |
|--------------|------|----------|--------------|
| **core** | la base de datos (sqlx, migraciones, CRUD) | nada por debajo | `firefly-starter-core` / `firefly-starter-data` |
| **domain** | sagas, CQRS, event sourcing, adaptadores de terceros | SDKs de **core** | `firefly-starter-domain` |
| **experience (BFF)** | workflows dirigidos por señales, agregación sin estado, REST atómico | SDKs de **domain** *únicamente* | `firefly-starter-experience` |

Lumen — con su ledger basado en event sourcing, su bus CQRS y su saga de
transferencia — es un servicio de **dominio**. Un servicio de **experiencia** es el
BFF que lo expone: agrega uno o varios SDKs de dominio en endpoints moldeados para
un único frontend o canal. **Nunca** posee una base de datos, **nunca** llama
directamente a un servicio core y **nunca** llama a un servicio de experiencia
hermano. Compone SDKs de dominio (sobre `firefly-client`) y nada más.

Lo que acaba de ocurrir: has situado a Lumen en el mapa. El libro ha estado
construyendo un servicio de dominio todo este tiempo; este capítulo construye la
capa *superior* a él. Conocer la dirección importa porque no es una convención que
debas recordar — la imponen los propios starters.

> **Design note.** El límite entre capas es un *tipo*, no una regla de revisión de
> código. Un experience stack expone un registro que contiene SDKs de dominio y nada
> más (lo conocerás en el Paso 3), y no tiene ninguna superficie de acceso a datos
> contra la que registrar una base de datos. Un BFF que intentara poseer una tabla o
> marcar un servicio core sencillamente no tiene API para hacerlo — la dirección de
> dependencia es unidireccional por construcción.

> **Tip** **Punto de control.** Puedes enunciar, sin mirar, qué posee cada capa y a
> quién puede llamar. La capa de experiencia compone SDKs de dominio únicamente;
> Lumen es el servicio de dominio al que llamará el BFF de este capítulo.

## Paso 2 — Construir el stack del BFF con `ExperienceStack`

Un BFF vive en su propio crate. A diferencia de un servicio de dominio simple — que
depende únicamente de la fachada `firefly` única — un BFF depende directamente del
starter de la capa de experiencia, porque la fachada lleva los starters de core y
web, pero no el de experiencia.

> **Note** **Término clave — starter de experiencia.** `firefly-starter-experience`
> es el crate que convierte un servicio web en un BFF. Se construye sobre el starter
> web (de modo que obtienes todas las baterías HTTP) y añade la maquinaria de
> trayectos. El análogo en Spring es un *starter* de Spring Boot que agrupa una
> porción coherente de capacidad detrás de una sola dependencia.

```toml
# Cargo.toml of a BFF crate (e.g. lumen-bff). Note: the experience starter is a
# direct dependency — the `firefly` facade carries the core + web starters but
# not this one.
[dependencies]
firefly-starter-experience = { version = "26.6.28" }
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
uuid = { version = "1", features = ["v4"] }
```

Con la dependencia en su sitio, una sola llamada construye el stack completo:

```rust,ignore
use firefly_starter_experience::{CoreConfig, ExperienceStack};

let bff = ExperienceStack::new(CoreConfig {
    app_name: "lumen-bff".into(),
    app_version: "1.0.0".into(),
    ..CoreConfig::default()
});
```

Lo que acaba de ocurrir: `ExperienceStack::new(cfg)` construye por debajo un
`WebStack` completo — de modo que el BFF hereda todas las baterías web del
[capítulo de producción](./20-production.md): CORS, cabeceras de seguridad,
métricas de petición, el log de acceso, la propagación del correlation-id,
idempotencia y la superficie del actuator — y luego apila encima los cinco bloques
de construcción de la capa de experiencia. El `CoreConfig` es el mismo valor de
configuración tipado que ha usado el resto del libro; el starter de experiencia
estampa `starter_name` con `"starter-experience"` cuando lo dejas en su valor por
defecto, de modo que el banner y `/actuator/info` reportan la capa.

Los cinco campos de la capa de experiencia se asientan encima del `web: WebStack`
embebido:

| Campo | Tipo | Función |
|-------|------|------|
| `clients` | `DomainClients` | el registro de SDKs de dominio, resuelto por nombre lógico |
| `signals` | `Arc<SignalService>` | las puertas de señal sobre las que aparca un paso de workflow |
| `state` | `WorkflowState` | estado de trayecto persistido compatible con Redis, indexado por correlation id |
| `query` | `Arc<WorkflowQueryService>` | la superficie de consulta del estado del trayecto |
| `children` | `Arc<ChildWorkflowService>` | composición de workflows hijos para trayectos anidados |

`ExperienceStack` hace `Deref` hacia su `WebStack` (que a su vez hace deref hacia
`Core`), de modo que cada método y campo de web + core — `apply_middleware`,
`actuator_router`, `new_application`, `with_security`, y los campos `cache`, `bus`,
`scheduler` — es alcanzable directamente sobre el valor `bff`. `Bff` es un alias de
tipo de `ExperienceStack`, así que puedes escribir el tipo de la forma que se lea
mejor en el punto de llamada.

> **Note** Hay dos formas más de escribir el mismo cableado, conservadas para los
> servicios que migran desde otros ports de Firefly.
> `register_experience_stack(cfg)` es un alias de `ExperienceStack::new(cfg)`, y
> `enable_experience_stack(cfg)` toma un `CoreConfig`, le estampa los valores por
> defecto de la capa (heredando las baterías web) y te lo devuelve — luego se lo
> pasas a `ExperienceStack::new`. Echa mano de la que se lea con más naturalidad;
> ambas cablean el stack idéntico.

> **Design note.** En un servicio de dominio simple como Lumen,
> `FireflyApplication::new(name).run().await` es la vía llave en mano — hace
> component-scan de beans, automonta controladores, drena los handlers y listeners
> registrados por inventory, aplica seguridad y middleware, autoaloja el panel de
> administración y sirve ambos puertos. Un BFF echa mano directamente de los bloques
> de construcción de más bajo nivel (`apply_middleware`, `with_security`,
> `new_application`) porque su router se ensambla a mano a partir de controladores
> de trayecto regulados por señales, en lugar de automontarse. Esos métodos son los
> mismos que `FireflyApplication` conduce por ti entre bastidores, y siguen estando
> plenamente soportados.

> **Tip** **Punto de control.** `ExperienceStack::new(...)` devuelve un valor sobre
> el que `bff.app_name` reporta el nombre de tu aplicación y `bff.starter_name` es
> `"starter-experience"`. `bff.clients.is_empty()`, `bff.signals.list_active()` y
> `bff.query.active()` están todos vacíos — los bloques de construcción están
> cableados y a la espera.

## Paso 3 — Registrar Lumen como un SDK de dominio

Un BFF alcanza cada servicio de dominio aguas abajo a través de un cliente REST con
nombre — uno por dependencia. `DomainClients` es el registro: registras Lumen bajo
un nombre lógico y luego lo resuelves por ese nombre desde cualquier handler o paso
de workflow, sin enhebrar un builder a través de cada punto de llamada.

> **Note** **Término clave — `RestClient`.** El `RestClient` es el cliente HTTP de
> `firefly-client`. Lleva propagación del correlation-id, un códec JSON,
> decodificación de errores RFC 9457 `application/problem+json` (una respuesta no-2xx
> se convierte en un error tipado) y reintento/backoff — el mismo cliente que cubrió
> el [capítulo de clientes HTTP](./13-http-clients.md). `register` te devuelve un
> `Arc<RestClient>` para uso inmediato.

```rust,ignore
// Experience -> Domain only. Register Lumen as a downstream domain SDK.
bff.clients.register("wallets", "https://lumen.internal");

// Later, in a handler or workflow step, resolve it by its logical name:
let wallets = bff.clients.get("wallets").expect("wallets SDK");
// `wallets` is an Arc<RestClient> with correlation-id propagation, a JSON codec,
// RFC 9457 problem decoding, and retry/backoff — all inherited from firefly-client.
```

Lo que acaba de ocurrir: `register(name, base_url)` construye un `RestClient` por
defecto para esa URL base, lo almacena bajo el nombre lógico y devuelve el
`Arc<RestClient>` que construyó. `get(name)` lo resuelve más tarde, devolviendo
`None` cuando no hay nada registrado bajo ese nombre. Como cada cliente registrado
apunta a un servicio de dominio, este registro es exactamente donde vive la regla
"experience → domain únicamente".

Una vez que tienes un cliente, llamas a la API aguas abajo a través de `request`:

```rust,ignore
use http::Method;
use serde_json::json;

// Call Lumen's public API as any external client would. A non-2xx response
// decodes into a typed ClientError (RFC 9457 problem document).
let _: serde_json::Value = wallets
    .request(Method::POST, "/wallets/w-1/reserve", Some(&json!({ "amount": 5000 })))
    .await?;
```

El registro tiene una superficie pequeña y predecible:

- `register(name, base_url)` construye y almacena un cliente por defecto (gana la
  última escritura si el nombre ya existe).
- `register_client(name, client)` almacena un `RestClient` preajustado — úsalo
  cuando un SDK de dominio necesita un timeout personalizado, cabeceras por defecto
  o una política de reintento.
- `get(name)` resuelve un cliente, o `None`.
- `names()` lista todos los SDKs registrados (ordenados), y `len()` / `is_empty()`
  reportan el tamaño del registro.

> **Design note.** Resolver un cliente por nombre lógico — `"wallets"` en lugar de
> una URL codificada a fuego — es lo que mantiene el código de trayecto del BFF
> desacoplado de dónde se ejecuta realmente Lumen. Apunta el nombre a
> `https://lumen.internal` en producción y a `http://localhost:8080` en un test, y
> ni una sola línea del workflow cambia.

> **Tip** **Punto de control.** Tras `bff.clients.register("wallets", ...)`,
> `bff.clients.get("wallets")` devuelve `Some(_)`, `bff.clients.names()` es
> `["wallets"]` y `bff.clients.len()` es `1`. Resolver un nombre no registrado
> devuelve `None`, nunca un panic.

## Paso 4 — Modelar el trayecto como un workflow regulado por señales

Un trayecto de BFF rara vez es una sola petición. "Financia una cartera, espera a
que el cliente confirme el importe y luego confirma la transferencia" son tres
interacciones repartidas en el tiempo. Un **workflow** con una **puerta de señal**
modela exactamente eso: pasos que llaman a SDKs de dominio, y una puerta que aparca
hasta que un llamador externo entrega una señal con nombre.

> **Note** **Término clave — workflow y nodo.** Un `Workflow` es un grafo dirigido
> de `Node`s, cada uno un paso asíncrono, ejecutados en orden de dependencias. Es el
> mismo motor de DAG-con-compensación del [capítulo de sagas](./12-sagas.md)
> (`firefly-orchestration`) — aquí dirigido por señales en lugar de ejecutado hasta
> completarse en una sola llamada. `Node::new(name, action)` define un paso;
> `.depends_on([...])` declara qué pasos deben terminar primero.

El trayecto es un `Workflow` de `Node`s; `Node::wait_for_signal` construye el nodo
puerta, aparcando sobre el `SignalService` del stack hasta que llega la señal con
nombre:

```rust,ignore
use std::sync::Arc;
use firefly_starter_experience::{Node, Workflow};

let signals = Arc::clone(&bff.signals);
let journey_id = "j-1".to_string();

let workflow = Workflow::new("fund-and-confirm")
    // 1. reserve: call the Lumen "wallets" SDK to open/lock the funds.
    .node(Node::new("reserve", || async { Ok(()) }))
    // 2. await-confirm: park until POST /journeys/{id}/data delivers "confirmed".
    .node(
        Node::wait_for_signal("await-confirm", &signals, journey_id.clone(), "confirmed")
            .depends_on(["reserve"]),
    )
    // 3. commit: call the Lumen "wallets" SDK to run the transfer.
    .node(Node::new("commit", || async { Ok(()) }).depends_on(["await-confirm"]));
```

Lo que acaba de ocurrir, nodo a nodo:

- `Node::new("reserve", || async { Ok(()) })` es el primer paso. En un BFF real su
  cuerpo resuelve `bff.clients.get("wallets")` y llama al endpoint de reserva de
  Lumen; aquí el cuerpo es un stub que devuelve `Ok(())` para que la forma quede
  clara. La acción de un nodo devuelve `Result<(), BoxError>`.
- `Node::wait_for_signal("await-confirm", &signals, journey_id.clone(),
  "confirmed")` construye el nodo **puerta**. Toma el nombre del nodo, una
  referencia al `Arc<SignalService>` del stack, el correlation id del trayecto y el
  nombre de la señal que se espera. `.depends_on(["reserve"])` hace que se ejecute
  después de `reserve`.
- `Node::new("commit", ...).depends_on(["await-confirm"])` es el paso final, que se
  ejecuta solo una vez que la puerta se libera.

`workflow.run().await` ejecuta los nodos en orden de dependencias y devuelve
`Result<(), WorkflowError>`. Cuando la ejecución alcanza `await-confirm` se
**aparca** — el future se suspende dentro del nodo puerta y no avanza. Normalmente
lanzas la ejecución en una task para que el handler HTTP que la inició pueda
retornar de inmediato:

```rust,ignore
// Run the journey on a task; it parks on the `await-confirm` gate.
tokio::spawn(async move {
    let _ = workflow.run().await;
});
```

Más tarde, un endpoint atómico entrega la señal y el nodo aparcado se reanuda:

```rust,ignore
// From a later request (POST /journeys/{id}/data):
bff.signals.deliver(&journey_id, "confirmed", serde_json::json!({ "ok": true }));
```

> **Note** **Término clave — entrega de señal (con buffer).** `signals.deliver(id,
> signal, payload)` despierta la puerta aparcada. La entrega tiene **buffer**: si la
> señal llega *antes* de que la puerta haya aparcado, el payload se retiene y el
> siguiente `wait_for_signal` para ese par se resuelve de inmediato — de modo que no
> hay despertar perdido en una condición de carrera. `deliver` devuelve `true`
> cuando un waiter vivo consumió la señal y `false` cuando se almacenó en buffer (no
> trates `false` como un error). `signals.list_active()` lista todos los trayectos
> actualmente aparcados sobre una puerta, o que retienen una señal con buffer para
> ella.

> **Tip** **Punto de control.** Lanza `workflow.run()`, luego sondea
> `bff.signals.list_active()` — una vez que la ejecución alcanza la puerta, contiene
> `journey_id`. Llama a `bff.signals.deliver(&journey_id, "confirmed", payload)` y el
> workflow se completa; `list_active()` ya no lista el id.

## Paso 5 — Persistir el trayecto con `WorkflowState`

Un trayecto abarca varias peticiones, así que su estado debe sobrevivir a cualquier
petición individual. Si el cliente cierra la pestaña entre "reserve" y "confirm", un
waiter puramente en memoria se perdería. `WorkflowState` resuelve esto haciendo
round-trip de la instantánea de `StepContext` de una ejecución de workflow a través
del `Adapter` de caché del stack, indexada por correlation id.

> **Note** **Término clave — `StepContext`.** Un `StepContext` es la bolsa de datos
> por ejecución que un workflow transporta: su correlation id, las entradas, el
> resultado de cada paso y variables de forma libre. Se serializa hacia y desde una
> instantánea JSON, que es lo que almacena `WorkflowState`. Vive en
> `firefly-orchestration`, así que lo importas desde ahí.

> **Note** **Término clave — `Adapter` de caché.** El `Adapter` de caché es el
> backend clave/valor enchufable de Firefly. El adaptador en memoria es el valor por
> defecto; apúntalo al `RedisAdapter` de `firefly-cache-redis` para durabilidad
> entre reinicios — la convención en torno a la que se construye la capa de
> experiencia. `ExperienceStack` cablea `state` sobre el mismo adaptador que sostiene
> el `Core`, de modo que cambiar a Redis es un cambio de configuración, no un cambio
> de código.

```rust,ignore
use firefly_orchestration::StepContext;

// Save when a journey parks:
let ctx = StepContext::new();
ctx.set_correlation_id("j-1");
ctx.set_variable("phase", serde_json::json!("AWAITING_CONFIRM"));
bff.state.save(&ctx).await?;

// Rehydrate from a later request to advance it:
if let Some(ctx) = bff.state.load("j-1").await? {
    // ... advance the journey using ctx ...
    let _ = ctx;
}

// Discard when the journey completes:
bff.state.delete("j-1").await?;
```

Lo que acaba de ocurrir:

- `StepContext::new()` crea un contexto vacío; `set_correlation_id` lo indexa (este
  es el id de trayecto bajo el que `WorkflowState` almacena), y
  `set_variable("phase", …)` registra dónde está el trayecto. Lo lees de vuelta con
  `ctx.variable("phase")`.
- `bff.state.save(&ctx).await?` persiste la instantánea bajo el correlation id del
  contexto, devolviendo `Result<(), CacheError>`.
- `bff.state.load("j-1").await?` devuelve `Result<Option<StepContext>,
  CacheError>`. Un **fallo sobre un trayecto desconocido es `Ok(None)`, no un
  error** — de modo que una comprobación de estado sobre un trayecto que nunca
  existió se representa como un 404 limpio, nunca un 500.
- `bff.state.delete("j-1").await?` desaloja el estado cuando el trayecto finaliza,
  de modo que las ejecuciones completadas no se quedan rondando.

> **Design note.** Esta es la costura que hace a un BFF resiliente a las
> desconexiones del cliente. Un trayecto aparcado guarda su `StepContext` antes de
> suspenderse; una petición posterior — posiblemente desde una sesión de navegador
> nueva, posiblemente después de que el BFF se reiniciara (con el adaptador Redis) —
> lo carga de vuelta y lo reanuda. El waiter en memoria por sí solo no podría
> sobrevivir a ese hueco; el estado persistido sí.

> **Tip** **Punto de control.** Haz `save` de un `StepContext` que lleve una
> variable `phase`, luego haz `load` desde un handle nuevo: la variable sobrevive al
> round-trip. Haz `delete`, y `load` devuelve `Ok(None)`.

## Paso 6 — Responder a los sondeos de estado con `WorkflowQueryService`

Mientras el cliente espera en la pantalla de confirmación, el frontend sondea
"¿dónde está mi trayecto?". `WorkflowQueryService` sostiene el `StepContext` vivo
por ejecución y responde consultas *con nombre* contra él — el principal mecanismo
de recuperación cuando un cliente se reconecta.

```rust,ignore
let journey_id = "j-1".to_string();

// On start: register the run's live context.
bff.query.register(&journey_id, ctx.clone());

// Register a named query that projects a value out of the context.
bff.query.register_query(&journey_id, "phase", |ctx| {
    ctx.variable("phase").unwrap_or(serde_json::json!("UNKNOWN"))
});

// On GET /journeys/{id}: run the named query.
let phase = bff.query.query(&journey_id, "phase")?;

// On completion: drop the run.
bff.query.unregister(&journey_id);
```

Lo que acaba de ocurrir: `register(id, ctx)` inscribe una ejecución por correlation
id con su `StepContext` vivo. `register_query(id, name, |ctx| value)` adjunta una
proyección con nombre — un closure que deriva un valor JSON del contexto (aquí, la
variable `phase`). `query(id, name)` ejecuta esa proyección y devuelve
`Result<Value, WorkflowQueryError>` — una ejecución desconocida o un nombre de
consulta desconocido es un error tipado, que el controlador mapea a un 404.
`unregister(id)` elimina la ejecución cuando el trayecto termina; `active()` lista
todas las ejecuciones registradas.

> **Design note.** Dos superficies responden "¿dónde está mi trayecto?" y se
> complementan. `WorkflowState` (Paso 5) es el registro *duradero* — sobrevive a un
> reinicio y respalda la decisión de 404-o-fase. `WorkflowQueryService` es la
> proyección *viva* sobre la ejecución en proceso — más rica, más barata de
> consultar y el lugar natural para derivar un DTO de "siguiente paso" mientras el
> proceso está en marcha. Un endpoint de estado de producción lee la consulta viva
> cuando la ejecución está en memoria y recurre al estado persistido en caso
> contrario.

> **Tip** **Punto de control.** Haz `register` de una ejecución, `register_query` de
> una proyección `"phase"`, y `query(id, "phase")` devuelve el valor de la fase.
> Consultar un id no registrado o un nombre de consulta desconocido devuelve un
> `Err`, no un panic.

## Paso 7 — Ensamblar el controlador de endpoints atómicos

Junta las piezas y un controlador de experiencia expone una petición HTTP por cada
fase del trayecto — la forma **REST atómico**.

> **Note** **Término clave — endpoint atómico.** Un *endpoint atómico* ejecuta
> exactamente una fase de un trayecto y retorna. El estado vive en la caché
> (compatible con Redis) entre llamadas, de modo que el cliente conduce el trayecto
> una petición cada vez y puede reanudar tras una desconexión — en lugar de mantener
> una única conexión de larga duración abierta a lo largo de todo el flujo.

| Método y ruta | Hace |
|---------------|------|
| `POST /journeys` | inicia el workflow (llama al SDK `"wallets"` para reservar), persiste `WorkflowState`, aparca en la puerta, devuelve el id del trayecto |
| `POST /journeys/:id/data` | entrega la señal `confirmed` — el workflow aparcado se reanuda y confirma la transferencia vía el SDK `"wallets"` |
| `GET  /journeys/:id` | reporta la fase persistida (o 404 si el trayecto es desconocido) |

Construyes estas como rutas `axum` ordinarias, y luego pasas el router por el
middleware heredado del BFF para que cada respuesta lleve las baterías web:

```rust,ignore
use axum::routing::{get, post};
use axum::Router;

// `routes` is an axum Router with the three handlers and the BFF as state.
let routes = Router::new()
    .route("/journeys", post(start_journey))
    .route("/journeys/:id/data", post(deliver_journey_signal))
    .route("/journeys/:id", get(journey_status))
    .with_state(app_state);

// Inherit the web batteries: CORS, security headers, correlation, metrics, …
let api = bff.apply_middleware(routes);
```

Lo que acaba de ocurrir: cada fase es una petición HTTP, y el estado del workflow
vive en la caché entre ellas, de modo que un cliente puede reanudar el trayecto tras
una desconexión. Como el router pasa por `bff.apply_middleware(routes)`, cada
respuesta hereda las baterías web — la respuesta de inicio lleva
`X-Frame-Options: DENY` y un `X-Correlation-Id` igual que las propias respuestas de
Lumen — y la misma cadena de filtros `ExperienceStack::with_security(chain)` del
[capítulo de seguridad](./14-security.md) protege las rutas mutadoras.

Este es exactamente el contrato que el propio test de arranque del crate demuestra
de extremo a extremo. (Su trayecto de checkout usa una fase `AWAITING_PAYMENT` en
lugar del `AWAITING_CONFIRM` de Lumen, pero la forma es idéntica.) Dos SDKs de
dominio simulados (mock) se componen a través de un workflow regulado por señales y
se conducen con `tower::oneshot`: iniciar el trayecto reserva a través del primer
SDK y aparca en la puerta; el endpoint de estado reporta la fase persistida;
entregar la señal hace avanzar el workflow fuera de la task y lo envía a través del
segundo SDK; y el estado final pasa a `COMPLETED`.

> **Tip** **Punto de control.** Conducir el router con tres llamadas — `POST
> /journeys`, luego `GET /journeys/:id` (reporta `AWAITING_CONFIRM`), luego `POST
> /journeys/:id/data`, luego `GET /journeys/:id` de nuevo (reporta `COMPLETED`) —
> recorre el trayecto completo. La primera respuesta lleva la cabecera heredada
> `X-Frame-Options: DENY`.

## Paso 8 — Ver dónde encaja Lumen

Dibujado como la instalación completa, Lumen es uno de los servicios de dominio que
compone un BFF. La capa de canal (una app web o móvil) llama a la capa de
experiencia (`lumen-bff`), que llama a la capa de dominio (`lumen`), que llama a la
capa core (`accounts`, que posee la base de datos). Cada flecha apunta
estrictamente hacia abajo, y la capa de experiencia solo alcanza siempre a la capa
de dominio:

```text
  web / mobile app          (channel tier)
        │
        ▼   Experience → Domain only
  lumen-bff                 (experience: DomainClients + signals + state)
        │
        ▼
  lumen                     (domain: ledger, CQRS, transfer saga)
        │
        ▼
  accounts                  (core: owns the database)
```

Lo que acaba de ocurrir: has situado tu BFF en la instalación. El BFF nunca accede
al event store de Lumen ni a su bus CQRS — habla con la API HTTP pública de Lumen a
través del SDK `"wallets"` registrado, exactamente igual que lo haría cualquier
cliente externo, y añade únicamente la orquestación de trayecto que necesita un
frontend. Lumen, el servicio de dominio, posee su propia lógica; el servicio core
por debajo de él posee la base de datos.

## Resumen — qué construyó este capítulo

No cambiaste Lumen — es un servicio de *dominio*, y este capítulo trata sobre la
capa *superior* a él. Lo que construiste es el modelo mental y el cableado para un
BFF de cara al frontend:

- El modelo de capas `channel → experience → domain → core`, y por qué la capa de
  experiencia compone SDKs de *dominio* únicamente — nunca una base de datos, un
  servicio core ni un BFF hermano.
- Un `ExperienceStack` (`Bff`) que hereda las baterías web de Lumen y añade cinco
  bloques de construcción: `clients`, `signals`, `state`, `query` y `children`.
- `DomainClients` registrando Lumen como el SDK `"wallets"` y resolviéndolo por
  nombre lógico, sobre un `RestClient` con propagación de correlación y
  decodificación de problemas RFC 9457.
- Un `Workflow` regulado por señales cuya puerta `Node::wait_for_signal` aparca el
  trayecto "financiar y confirmar" hasta que llega `signals.deliver(...)` — con
  entrega con buffer para que no haya despertar perdido.
- `WorkflowState` persistiendo ese trayecto a través de un adaptador de caché
  compatible con Redis (un fallo es `Ok(None)`, no un error), de modo que un cliente
  pueda reanudar tras una desconexión.
- `WorkflowQueryService` respondiendo a los sondeos de estado "¿dónde está mi
  trayecto?" desde el `StepContext` vivo.
- El controlador REST atómico de tres endpoints, pasado por
  `bff.apply_middleware(...)` para que cada respuesta herede las baterías web.

Cada API aquí procede de la superficie real de `firefly-starter-experience` — la
misma que su test de arranque ejercita de extremo a extremo.

## Ejercicios

1. **Registra Lumen como un SDK de dominio.** Construye un `ExperienceStack`, llama
   a `bff.clients.register("wallets", "http://localhost:8080")` y confirma que
   `bff.clients.get("wallets")` devuelve `Some(_)` y que `bff.clients.names()` lista
   `"wallets"`. Luego vuelve a registrar el mismo nombre con una URL diferente y
   confirma que `bff.clients.len()` se mantiene en `1` (gana la última escritura).
2. **Aparcar y reanudar.** Construye un `Workflow` de dos nodos cuyo segundo nodo
   sea una puerta `Node::wait_for_signal`. Lanza `workflow.run()` en una task,
   sondea hasta que `bff.signals.list_active()` contenga el id del trayecto, luego
   `bff.signals.deliver(&id, "confirmed", json!({}))` y confirma que el workflow se
   completa y que `list_active()` ya no lista el id.
3. **Carrera con la puerta.** Repite el ejercicio 2 pero llama a `deliver` *antes*
   de lanzar la ejecución. Confirma que el workflow se completa igualmente — la
   entrega con buffer significa que la señal no se pierde cuando se adelanta a la
   puerta.
4. **Persistir un trayecto.** Haz `save` de un `StepContext` que lleve una variable
   `phase` vía `bff.state.save`, hazle `load` de vuelta desde un handle nuevo y
   confirma que la variable sobrevive. Luego hazle `delete` y confirma que
   `bff.state.load(...)` devuelve `Ok(None)`.
5. **Endpoints atómicos.** Cablea el controlador de tres rutas del Paso 7 con
   `bff.apply_middleware(routes)` y condúcelo con `tower::oneshot`: inicia, sondea el
   estado (`AWAITING_CONFIRM`), entrega la señal, sondea de nuevo (`COMPLETED`).
   Asegura que la respuesta de inicio lleva la cabecera heredada
   `X-Frame-Options: DENY` y un `X-Correlation-Id`.

## Adónde ir después

- Repasa cómo se lleva a producción un servicio de dominio como Lumen — Postgres
  real, Kafka y la superficie de gestión — en
  **[Producción y despliegue](./20-production.md)**.
- Mira cómo el motor de workflows sobre el que cabalga el trayecto del BFF también
  impulsa las transferencias compensatorias propias de Lumen en
  **[Sagas, workflows y TCC](./12-sagas.md)**.
- Con el lugar de Lumen en la instalación por capas ya claro, el capítulo final
  vuelve a leer todo el servicio a través de la lente de las macros declarativas.
  Continúa en **[Servicios declarativos con macros](./21-declarative-macros.md)**.
