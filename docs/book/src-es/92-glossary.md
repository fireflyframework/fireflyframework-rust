# Glosario

Definiciones de los términos y tipos que se repiten a lo largo de este libro, en
el sentido preciso con que Firefly los emplea.

### Actuator
La superficie de gestión (`firefly-actuator`) que expone los endpoints
`/actuator/health`, `/actuator/info`, `/actuator/metrics`, `/actuator/env`,
`/actuator/tasks`, `/actuator/version`, `/actuator/loggers`,
`/actuator/httpexchanges`, `/actuator/refresh` y los informes de introspección de
la DI `/actuator/beans`, `/actuator/mappings`, `/actuator/conditions`.
Lo monta `core.actuator_router(..)`, normalmente en un puerto separado y
protegido por cortafuegos.

### Adapter
Una implementación concreta de un **port**. Se selecciona en el momento del
cableado como un `Arc<dyn Port>`, de modo que las dependencias pesadas de los SDK
quedan fuera de los servicios que no las usan. Ejemplos: `RedisAdapter` (un
`cache::Adapter`), `KafkaBroker` (un `eda::Broker`).

### Aggregate root
La frontera de consistencia en DDD. La variante no orientada a eventos mantiene
un búfer `PendingEvents<E>` (`firefly_kernel::ddd`); la variante orientada a
eventos es un `AggregateRoot` cuyo estado se reconstruye reproduciendo sus
`DomainEvent`s (`firefly-eventsourcing`).

### Autowired
Un campo de componente que el contenedor de DI resuelve e inyecta por tipo
(`#[autowired]`, `firefly-container`). El tipo del campo determina la forma:
`Arc<T>` (obligatorio), `Option<Arc<T>>` (opcional), `Vec<Arc<T>>` (todas las
implementaciones), `Provider<T>` (diferido). Resuelve e inyecta un campo de
componente por tipo.

### Backpressure
Un consumidor lento que regula a un productor rápido. Los flujos reactivos `Flux`
de Firefly (responders NDJSON/SSE, `WebClient::body_to_flux`, flujos de filas de
Postgres) respetan la contrapresión (backpressure) de extremo a extremo, de modo
que un flujo grande nunca acaba por completo en memoria.

### Bean
Cualquier valor que el `Container` de DI construye, cablea y posee. Se declara con
un derive de **stereotype** (`#[derive(Component/Service/Repository/Configuration/
Controller)]`) o lo produce un método factoría `#[bean]` en un contenedor
`#[derive(Configuration)]`. Se indexa por `TypeId`; se resuelve con
`resolve::<T>()`.

### BFF (Backend-for-Frontend)
Véase **Experience tier**.

### Bus
El despachador de comandos/consultas de CQRS (`firefly_cqrs::Bus`). Los handlers
se registran por tipo de entrada; el despacho se indexa por
`std::any::TypeId`. `send`/`query` son async; `send_mono`/`query_mono` son sus
gemelos reactivos.

### Compensation
El paso de deshacer que ejecuta una **saga** en orden inverso cuando un paso
posterior falla (`Step::with_compensation`). En la saga de transferencia de
Lumen, la compensación del adeudo es un depósito de reembolso en el monedero de
origen.

### CompensationPolicy
Cómo deshace sus efectos una saga/workflow ante un fallo: `BestEffort` (continuar
compensando aunque una compensación falle) o `StopOnError` (abortar en el primer
fallo de compensación).

### Component scanning
Descubrimiento de beans en tiempo de enlace (`Container::scan()` /
`firefly::scan`): cada derive de stereotype no genérico registra un thunk de
`inventory`, y `scan` los recopila por todo el grafo de crates, aplica las
**conditions** y los **profiles**, y registra a los supervivientes. El
descubrimiento es en tiempo de enlace, no reflexivo. Los beans genéricos usan el
recurso alternativo `register_all!`.

### Conditional bean
Un bean que se registra solo cuando el entorno coincide — `#[firefly(profile =
"…", condition_on_property = "k=v", condition_on_bean = "T",
condition_on_missing_bean = "T", condition_on_class = "label",
condition_on_single_candidate = "T")]`. Lo evalúa `scan` en dos pasadas (primero
los hechos de configuración/profile, después las comprobaciones que dependen del
registro).

### Container
El localizador de servicios de DI opcional e indexado por `TypeId`
(`firefly-container`): registra beans, resuelve por tipo o por nombre, admite
scopes, vínculos a trait objects, desambiguación con `primary`/`order`,
`Provider<T>` diferido, hooks de ciclo de vida y component scanning. Distinto de
**Core** (el paquete de infraestructura ya cableada).

### Core
El paquete de infraestructura ya cableada que devuelve `Core::new(CoreConfig)`
(`firefly-starter-core`): caché, bus de CQRS, broker de eventos, composición de
health, métricas, scheduler, logging y la cadena de middleware.

### Correlation id
Un identificador por petición que viaja en la cabecera `X-Correlation-Id` y en un
scope task-local del kernel. Enriquece automáticamente cada línea de log, cada
evento publicado y cada llamada saliente de cliente, de modo que una petición se
hila a través de los servicios.

### DomainEvent
El evento orientado a eventos, versionado y con formato de cable de
`firefly-eventsourcing` (distinto del `TransientDomainEvent` transitorio de
`firefly_kernel::ddd`). Su JSON usa un formato de cable estable y versionado.

### Event (EDA)
El sobre por el que fluye todo evento de `firefly-eda` — `id`, `type`, `source`,
`topic`, `correlationId`, `time`, `headers`, `payload`, `key`. Se construye con
`Event::new`; el sobre sigue un esquema JSON estable.

### Experience tier (BFF)
La capa de servicios superior (`firefly-starter-experience`, `ExperienceStack` /
`Bff`): un Backend-for-Frontend que compone varios SDK de **domain** en endpoints
REST atómicos y específicos de cada recorrido (journey). No posee ninguna base de
datos y solo llama a servicios de dominio (`channel → experience → domain →
core`). Se construye a partir de `DomainClients` (la `ClientFactory`), las
compuertas de `SignalService`, un `WorkflowState` con soporte de Redis y
`WorkflowQueryService`.

### FireflyError
El tipo de error del framework (`firefly_kernel::FireflyError`). Se renderiza como
una respuesta `application/problem+json` conforme a RFC 7807, y es el canal de
error fijo del `Mono`/`Flux` reactivo (su señal terminal `Err`).

### FilterChain
El emparejador de autorización basado en rutas de `firefly-security` (`permit` /
`require` / glob `permit_pattern` / `require_pattern`). Es fail-closed en cuanto
se declara cualquier regla: una ruta no declarada se deniega por defecto.

### Flux
Un publisher reactivo de *0..N* valores más una finalización-o-error terminal
(`firefly_reactive::Flux`).

### Idempotency
El comportamiento de reproducción que se aplica a las peticiones `POST`/`PUT`/`PATCH`
que llevan una cabecera `Idempotency-Key`. Una repetición reproduce la respuesta
almacenada (`Idempotent-Replay: true`); reutilizarla con un cuerpo distinto
produce un 409.

### Mono
Un publisher reactivo de *como mucho un* valor más un error terminal
(`firefly_reactive::Mono`). Un `Mono` vacío es `Ok(None)`.

### NDJSON
JSON delimitado por saltos de línea (`application/x-ndjson`) — un documento JSON
compacto por línea. El responder `NdJson(Flux<T>)` lo transmite con
contrapresión.

### Outbox (transactional)
Un patrón (`TransactionalOutbox`, `firefly-eda-postgres`) que escribe los eventos
en la misma transacción que el cambio de estado y los entrega a los consumidores
después, ofreciendo entrega al-menos-una-vez sin necesidad de un broker aparte.

### Port
Un trait `async_trait` object-safe que define un punto de integración —
`cache::Adapter`, `eda::Broker`, `notifications::Channel`, `idp::Adapter`. El
código depende del port; un **adapter** lo implementa.

### Primary
El desambiguador (`#[firefly(primary)]`) que elige un bean cuando hay varias
implementaciones vinculadas al mismo port. Resolver sin ningún primary entre
varios candidatos es un error `NoUniqueBean` que nombra a cada candidato.

### Problem (RFC 7807 / 9457)
El sobre de error `application/problem+json` (`type`, `title`, `status`,
`detail`) que todo servicio Firefly renderiza para errores y panics, siguiendo el
estándar RFC 9457. RFC 9457 deja obsoleto a RFC 7807 y es compatible a nivel de
cable con él; el libro usa ambos números indistintamente.

### Profile
Un entorno con nombre (`prod`, `dev`, `test`) que controla los conditional beans
(`#[firefly(profile = "expr")]`). La gramática de la expresión admite `&`, `|`,
`!`, la coma como OR y paréntesis. Los profiles activos residen en el
`ApplicationContext` / `ConditionContext`.

### Projection
Un handler del lado de lectura que construye un modelo de lectura a partir de los
eventos (`firefly_eventsourcing::Projection`), dirigido por agregado (`replay`) o
sobre el flujo global (`drive_once` / `replay_all`).

### Qualifier
Un nombre que se usa para seleccionar un bean concreto cuando varios comparten el
mismo tipo (`#[firefly(qualifier = "replica")]` → `resolve_named`).

### Reactive
El modelo de programación `Mono`/`Flux` (`firefly-reactive`) y todo lo construido
sobre él — endpoints reactivos, repositorios, el `WebClient`, EDA/CQRS reactivos.
Si has usado alguna biblioteca de reactive-streams, el modelo de publisher te
resultará familiar.

### Saga
Un motor de transacciones distribuidas secuencial (`firefly_orchestration::Saga`)
con compensación en orden inverso ante un fallo. Véanse también `Workflow` (DAG) y
`Tcc`.

### Scheduler
El ejecutor de tareas (`firefly_scheduling::Scheduler`) que gestiona disparadores
Cron, FixedRate y FixedDelay, cada uno en su propia tarea de tokio con
recuperación ante panics.

### Scope
El ciclo de vida de un bean (`#[firefly(scope = "…")]`): `singleton` (una única
instancia cacheada, el valor por defecto), `transient` (una nueva en cada
resolución), `request` o `session` (ambos gestionados por un `ScopeHandler`).

### Signal
Un evento externo que satisface una compuerta de workflow aparcada en la
**experience tier** (`SignalService::deliver` / `Node::wait_for_signal`). La
entrega se almacena en búfer, de modo que una señal que llega antes de que la
compuerta se aparque no se pierde.

### SSE (Server-Sent Events)
Un protocolo de streaming unidireccional (`text/event-stream`). El responder
`Sse(Flux<T>)` y el `SseWriter` de `firefly-sse` lo emiten;
`WebClient::body_to_flux` lo decodifica.

### Specification
Un predicado de regla de negocio componible
(`firefly_kernel::ddd::Specification<T>`) que se combina con `.and()`, `.or()`,
`.not()`. Cualquier `Fn(&T) -> bool` lo es.

### Starter
Un crate que agrupa una pila por defecto razonable, de modo que un servicio
depende de un único crate. `firefly-starter-core` es el punto de partida común;
`firefly-starter-domain` y `firefly-starter-experience` añaden las capas de
domain y BFF.

### Stereotype
La etiqueta de rol arquitectónico que lleva un bean de DI (`component`,
`service`, `repository`, `configuration`, `controller`, `bean`), establecida por
el derive que lo declaró. Son funcionalmente equivalentes; las diferencias están
en la intención documentada y en la agrupación que muestra la vista `/beans` del
admin.

### TCC (Try-Confirm-Cancel)
Un motor de transacciones distribuidas en dos fases
(`firefly_orchestration::Tcc`): hace Try a todos los participantes, Confirm a
todos en caso de éxito y Cancel a los participantes ya intentados ante cualquier
fallo de Try.

### Value object
Un tipo de dominio definido por completo por sus atributos (sin identidad) e
**inmutable**: cada operación devuelve un valor nuevo. El `Money` de Lumen es el
ejemplo canónico — aritmética exacta de céntimos enteros, cerrada bajo
`add`/`subtract`. La contraparte en DDD de un **aggregate root** (que sí tiene
identidad).

### Verifier
El port asíncrono (`firefly_security::Verifier`) que valida un bearer token y
devuelve una `Authentication`. `JwksVerifier`, los adapters de IDP y los closures
`VerifierFn` lo satisfacen todos.

### WebClient
El cliente HTTP reactivo (`firefly_client::WebClient`) cuyos operadores
terminales devuelven `Mono`/`Flux` (`body_to_mono`, `body_to_flux`, `exchange`).

### Workflow
Un motor de transacciones distribuidas en forma de DAG
(`firefly_orchestration::Workflow`): los nodos independientes se ejecutan de
forma concurrente dentro de una oleada, con compensación en orden inverso bajo
una `CompensationPolicy` configurable. En la **experience tier**, un nodo puede
aparcarse en una compuerta de **signal**.

### WorkflowState
Estado de recorrido (journey) persistido y con soporte de Redis en la
**experience tier** (`firefly_starter_experience::WorkflowState`): hace ida y
vuelta del snapshot `StepContext` de una ejecución de workflow a través del
`Adapter` de caché, indexado por correlation id, de modo que un recorrido
aparcado sobrevive a una desconexión del cliente (`save` / `load` / `delete`).
