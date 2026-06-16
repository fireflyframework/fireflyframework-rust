# Arquitectura orientada a eventos y mensajería

Al final del [capítulo de CQRS](./09-cqrs.md), Lumen ya sabía abrir una cartera,
ingresar, retirar y leer un saldo, pero el lado de comandos y el lado de
consultas hacían un poco de trampa en silencio. El agregado `Wallet` emitía
eventos de dominio bien definidos (`WalletOpened`, `MoneyDeposited`,
`MoneyWithdrawn`), el `Ledger` los persistía y, después, nada los llevaba a
ninguna parte. El modelo de lectura al que sirve la consulta `GetWallet` había
que repararlo sobre la marcha replegando el flujo de eventos cada vez que se leía.

Al final de *este* capítulo, Lumen cierra ese bucle. Cada evento que el ledger
persiste se **publica** además en un broker, y una **proyección** del modelo de
lectura —un bean cuyo método consume esos eventos publicados— mantiene el lado de
consultas al día sin que el lado de escritura sepa siquiera que existe. Eso es la
arquitectura orientada a eventos: un hecho se publica una vez, y cualquier número
de reacciones independientes se suscriben a él. El rastro de auditoría, la
notificación de bienvenida, el modelo de lectura del saldo: cada uno se convierte
en un suscriptor que puedes añadir meses después sin tocar un solo manejador de
comandos.

Construiremos el bucle tal como lo construye el propio código fuente de Lumen: un
puente de una sola función que convierte un evento de dominio persistido en un
sobre de transporte, una llamada de publicación al final del commit del ledger y
un bean de proyección que el framework descubre y conecta por ti. Luego haremos
un recorrido por la maquinaria de mensajería que lo rodea —topics con globs,
grupos de consumidores, reintento/dead-letter, filtros, la superficie reactiva,
los eventos en proceso y los transportes de producción— para que sepas qué
herramienta usar en cada caso.

Al terminar este capítulo, serás capaz de:

- Distinguir un **evento de dominio** (el hecho duradero del event-sourcing) de
  un **evento de mensajería** (el sobre de transporte), y tender un puente del uno
  al otro con una sola función de mapeo.
- Publicar cada evento confirmado del `Ledger` a un `Broker`, en el orden que
  garantiza que un suscriptor nunca vea un hecho sin confirmar.
- Escribir la **proyección** del modelo de lectura como un bean `#[derive(Service)]`
  cuyo método `#[event_listener]` el framework descubre y suscribe por ti, y
  entender por qué reconstruir desde el flujo lo hace idempotente.
- Aprovechar el alcance del broker: patrones de topic con globs, grupos de
  consumidores, reintento con dead-lettering, filtros por sobre y la superficie
  reactiva `Flux`.
- Diferenciar los tres roles de evento del broker —`#[event_listener]`,
  `#[application_event_listener]` / `#[transactional_event_listener]` y
  `externalize_after_commit`— y cambiar el broker en memoria por Kafka, RabbitMQ,
  Postgres o Redis sin modificar ningún manejador.

## Conceptos que conocerás

Antes de la primera línea de código, aquí están las ideas en las que se apoya
este capítulo. Cada una se reintroduce en contexto donde se usa por primera vez;
esta es la versión breve.

> **Note** **Término clave — arquitectura orientada a eventos (EDA).** Un estilo
> en el que los componentes se comunican *publicando hechos* en lugar de llamarse
> entre sí. Un productor anuncia que algo ocurrió; cualquier número de
> *suscriptores* reacciona, y al productor ni le importa ni sabe quiénes son. El
> análogo en Spring es Spring Cloud Stream / Spring for Apache Kafka: una capa de
> publicación/suscripción sobre un broker de mensajes.

> **Note** **Término clave — broker.** Un *broker* es el transporte que lleva un
> evento publicado a sus suscriptores. `firefly-eda` define un *port* `Broker`
> agnóstico al transporte; el `InMemoryBroker` en proceso es el predeterminado, y
> Kafka, RabbitMQ, Postgres y Redis implementan ese mismo port. Este es el papel
> que desempeñan el `MessageChannel` / `KafkaTemplate` + listener container de
> Spring.

> **Note** **Término clave — proyección.** Una *proyección* consume un flujo de
> eventos y mantiene una vista derivada y optimizada para consultas (el *modelo de
> lectura*). Es el lado de lectura de la segregación de responsabilidades de
> comandos y consultas (CQRS), que se mantiene al día reaccionando a los eventos
> del lado de escritura. Los desarrolladores de Spring las construyen con métodos
> `@KafkaListener` / `@EventListener` que escriben en un almacén de consultas.

> **Note** **Término clave — idempotente.** Una operación es *idempotente* cuando
> aplicarla más de una vez tiene el mismo efecto que aplicarla una sola vez. Bajo
> una entrega al-menos-una-vez, un broker puede entregar el mismo evento a un
> suscriptor dos veces; una proyección idempotente absorbe la reentrega sin
> corromper su vista.

`firefly-eda` es el **port de arquitectura orientada a eventos** del framework.
Define el sobre `Event` por el que fluye todo evento de Firefly, los ports
`Publisher` / `Subscriber` / `Broker`, un `InMemoryBroker` en proceso y la
maquinaria de mensajería: topics con globs, grupos de consumidores, reintento/DLQ,
filtros de eventos y una superficie de suscripción reactiva `Flux`. Los
transportes de producción (Kafka, RabbitMQ, outbox de Postgres, Redis Streams)
implementan los mismos ports y encajan en el momento del cableado, de modo que la
proyección de Lumen nunca cambia cuando cambia el broker.

> **Design note.** El `Broker` es el port de mensajería de Firefly agnóstico al
> transporte: publicas un `Event` en él y le suscribes manejadores.
> `wrap_listener` añade reintento y dead-lettering; las suscripciones aceptan
> patrones de topic con globs. Como todo transporte de producción implementa el
> mismo port, un manejador nunca cambia cuando cambia el broker: el cableado elige
> el adaptador, el código se queda donde está.

## Paso 1 — Diferenciar los dos tipos de «evento»

Antes de cablear nada, conviene ser preciso con la palabra *evento*, porque Lumen
acaba usándola para dos cosas distintas, y confundirlas lleva a echar mano del
port equivocado.

> **Note** **Término clave — evento de dominio frente a evento de mensajería.** Un
> *evento de dominio* (el del event-sourcing) es el hecho duradero y versionado
> que emite un agregado; vive en el *store* de eventos y es el tema del
> [próximo capítulo](./11-event-sourcing.md). Un *evento de mensajería* es el
> sobre de transporte que lleva un hecho *a los suscriptores*; vive en el *broker*.
> En términos de Spring: un evento de dominio es un registro persistido con JPA en
> tu tabla de eventos; un evento de mensajería es la carga útil que entregas a
> `KafkaTemplate.send(...)`.

Un **evento de dominio** en el sentido del event-sourcing —el `DomainEvent` de
`firefly::eventsourcing`— es el hecho duradero y versionado que emite el agregado
`Wallet` y que el próximo capítulo convierte en la fuente de la verdad. Vive en el
*store* de eventos.

Un **evento de mensajería** —el `Event` de `firefly::eda`— es el sobre de
transporte que lleva un hecho *a los suscriptores*. Vive en el *broker*. Lumen
tiende un puente entre ambos con una sola función (`to_envelope`, construida en el
Paso 3): el ledger persiste un `DomainEvent`, luego lo mapea a un `Event` y lo
publica. Este capítulo trata del segundo tipo: poner el hecho en el cable y
reaccionar a él. El primer tipo es el tema del próximo capítulo.

> **Tip** **Punto de control.** Puedes enunciar, en una frase cada uno, qué es un
> evento de dominio (un hecho duradero en el store) y qué es un evento de
> mensajería (un sobre de transporte en el broker), y qué capítulo es dueño de
> cada uno. Ten presente esa distinción: cada ruta de código de abajo se sitúa con
> firmeza en uno de los dos lados.

## Paso 2 — Leer el sobre `Event`

`Event` es el sobre de transporte canónico de Firefly. Tiene una *forma JSON
estable* —nombres de campo fijos y reglas de omisión— para que productores y
consumidores coincidan en los bytes con independencia del broker o el servicio.
Cualquier sistema que respete el contrato interopera: el mismo sobre es compatible
a nivel de cable entre los ports de Firefly en Java, .NET, Go y Python.

Construye uno con `Event::new`, que además estampa el `correlation_id` a partir
del ámbito de correlación task-local del kernel (de modo que un evento publicado
lleva el mismo id de correlación que la petición que lo produjo):

```rust
use firefly_eda::Event;

let ev = Event::new(
    "orders.created",   // topic — where it is published
    "OrderCreated",     // event type — the logical name
    "orders-svc",       // source — the producing service
    Some(br#"{"id":"o1"}"#.to_vec()), // payload (base64 on the wire)
)
.with_header("x-tenant", "acme")     // arbitrary routing/metadata header
.with_key(b"customer-42".to_vec());  // partition / routing key
```

Qué acaba de pasar, campo a campo. `Event::new` toma cuatro argumentos
posicionales —`topic`, `event_type`, `source` y un `payload` opcional— y rellena
el resto: un `id` nuevo, el `time` actual (UTC) y el `correlation_id` ambiente.
Los dos métodos del builder son aditivos: `with_header` inserta una cabecera
string→string en un mapa ordenado (para que la codificación sea determinista), y
`with_key` establece la clave opcional de partición/enrutamiento.

La `key` lleva la clave de partición/enrutamiento prevista según el contrato de
`Event`; se *omite* del cable cuando está ausente, de modo que los eventos
producidos antes de que existiera el campo siguen siendo idénticos byte a byte.
Una salvedad honesta: los adaptadores actuales todavía no enrutan a partir de
`key`. El adaptador de Kafka deriva la clave de registro de `correlation_id` (con
respaldo en el id del evento), y el adaptador de RabbitMQ enruta por el topic.
Tratar `key` como la clave de partición/enrutamiento es la *intención de diseño*
del contrato, no una garantía de los adaptadores de hoy.

> **Tip** **Punto de control.** Puedes construir un `Event` y volver a leer sus
> campos: `Event::new("t", "T", "s", None).with_header("k", "v").headers.get("k")`
> devuelve `Some("v")`, y `.with_key(b"abc".to_vec()).key` es
> `Some(b"abc".to_vec())`. Una clave ausente nunca aparece en el cable.

## Paso 3 — Tender un puente de un evento de dominio al sobre

Lumen nunca construye un `Event` a mano dentro de un manejador. El ledger es dueño
de una función de mapeo que convierte un `DomainEvent` persistido en el sobre
canónico, llevando el evento de dominio codificado en JSON como carga útil y el id
de la cartera como clave de partición prevista. Colócala en
`samples/lumen/src/ledger.rs` junto a las dos constantes compartidas a las que
hacen referencia tanto el publicador como la proyección:

```rust
use firefly::eda::Event;
use firefly::eventsourcing::DomainEvent;

use crate::domain::AGGREGATE_TYPE; // the const "Wallet"

/// The EDA topic every wallet domain event is published to. The projection
/// and any external subscriber key off it.
pub const EVENTS_TOPIC: &str = "wallets.events";

/// The logical EDA source stamped on published events.
pub const EVENT_SOURCE: &str = "lumen";

/// Maps a persisted `DomainEvent` onto the canonical EDA `Event` envelope,
/// carrying the JSON-encoded domain event as the payload and the wallet id as
/// the partition key (so per-wallet events stay ordered on a real broker).
pub fn to_envelope(event: &DomainEvent) -> Event {
    let payload = serde_json::to_vec(event).expect("domain event serialises");
    Event::new(
        EVENTS_TOPIC,
        event.event_type.clone(),
        EVENT_SOURCE,
        Some(payload),
    )
    .with_key(event.aggregate_id.clone().into_bytes())
    .with_header("aggregateType", AGGREGATE_TYPE)
    .with_header("aggregateId", event.aggregate_id.clone())
    .with_header("version", event.version.to_string())
}
```

Qué acaba de pasar, con tres decisiones de diseño en las que merece la pena
detenerse:

- El **topic** (`wallets.events`) es una constante compartida. El publicador y la
  proyección hacen referencia al *mismo* valor `EVENTS_TOPIC`, de modo que el
  nombre del canal nunca puede divergir entre ellos: renombrar la constante mueve
  ambos lados a la vez.
- La **key** es el id de la cartera. Es la clave de partición prevista para que,
  una vez que un broker enrute a partir de ella, todos los eventos de una cartera
  caigan en la misma partición y se mantengan en orden. (El adaptador de Kafka de
  hoy basa la clave de los registros en `correlation_id` y RabbitMQ enruta por el
  topic, así que esto es la intención de diseño del contrato, no una garantía
  actual, exactamente como explicaba el Paso 2.)
- Las **cabeceras** (`aggregateType`, `aggregateId`, `version`) llevan justo los
  metadatos de enrutamiento suficientes para que un suscriptor encuentre y vuelva
  a replegar el agregado afectado *sin decodificar la carga útil*. Eso es
  precisamente lo que hace la proyección de Lumen en el Paso 5: lee `aggregateId`
  de una cabecera y nunca toca el cuerpo.

> **Tip** **Punto de control.** Un test unitario sobre `to_envelope` (Lumen
> incluye uno) afirma que `env.topic == EVENTS_TOPIC`,
> `env.event_type == "WalletOpened"`, `env.key == Some(b"wlt_x".to_vec())` y que
> las cabeceras `aggregateId` / `version` están establecidas. Si eso se cumple, el
> puente es fiel.

## Paso 4 — Publicar desde el ledger (guarda antes de publicar)

El `Ledger` es la única ruta de escritura que llaman todos los comandos y la saga
de transferencia. Después de añadir al store los eventos sin confirmar de un
agregado con concurrencia optimista, publica cada uno —`to_envelope` y luego
`broker.publish`— para que la proyección aguas abajo pueda reaccionar. Aquí está
el método `commit` de `Ledger`:

```rust
use firefly::eda::Broker;
use firefly::eventsourcing::EventSourcingError;

use crate::domain::{DomainError, Wallet};

/// Appends the aggregate's uncommitted events at `expected_version`
/// (optimistic concurrency) then publishes each to the EDA broker.
async fn commit(&self, wallet: &mut Wallet, expected: i64) -> Result<(), DomainError> {
    let events = wallet.take_uncommitted();
    if events.is_empty() {
        return Ok(());
    }
    self.store
        .append(&wallet.root.id, expected, events.clone())
        .await
        .map_err(|e| match e {
            EventSourcingError::Concurrency => {
                DomainError::NotFound(format!("{}: concurrent modification", wallet.root.id))
            }
            other => DomainError::NotFound(format!("{}: {other}", wallet.root.id)),
        })?;
    for event in &events {
        self.broker
            .publish(to_envelope(event))
            .await
            .map_err(|e| DomainError::NotFound(format!("publish failed: {e}")))?;
    }
    Ok(())
}
```

Qué acaba de pasar. `take_uncommitted()` drena los eventos que produjo el comando
de dominio; si no hay ninguno, no hay nada que hacer. Luego `store.append(...)`
los persiste en la versión `expected` —la comprobación de concurrencia optimista—.
Solo *después* de que eso tiene éxito, el bucle convierte cada evento en un sobre
y lo publica. El `broker` aquí es un `Arc<dyn Broker>`: el ledger programa contra
el *port*, nunca contra un transporte concreto.

Fíjate en el orden: **añade antes de publicar.** Un suscriptor nunca debe ver un
hecho que no se persistió. Si el append falla —incluida la carrera de concurrencia
optimista—, nunca se alcanza el bucle, así que no se difunde ningún evento. El
store que respalda este ledger es el store de eventos en memoria; el
[próximo capítulo](./11-event-sourcing.md) es donde ese store se gana el nombre de
*event-sourced*.

> **Note** Añade antes de publicar: un suscriptor nunca debe ver un hecho que no
> se persistió. El hueco entre el append y el publish —donde un fallo podría
> persistir un hecho pero perder la difusión— es exactamente lo que elimina el
> outbox transaccional del [próximo capítulo](./11-event-sourcing.md).

> **Tip** **Punto de control.** `cargo test -p lumen` sigue pasando: el ida y
> vuelta de abrir/ingresar/retirar del ledger persiste tres eventos y publica tres
> sobres, en ese orden, sin ningún suscriptor todavía conectado.

## Paso 5 — Observar el fan-out del broker en proceso

`InMemoryBroker` es el transporte predeterminado: entrega por fan-out, coincidencia
de topics con globs y round-robin por `(topic, group)`, sin dependencias externas.
Es el broker que expone el stack web del framework (y que registra en el contenedor
de DI como el port `Arc<dyn Broker>`), y es todo lo que necesitan la build
didáctica y la suite de tests. Antes de cablear la proyección de Lumen, observa el
broker de forma aislada: suscribe un manejador, publica un evento:

```rust
use firefly_eda::{handler, Event, InMemoryBroker};

#[tokio::main]
async fn main() {
    let broker = InMemoryBroker::new();

    broker
        .subscribe(
            "wallets.events",
            handler(|ev: Event| async move {
                println!(
                    "observed {} for {}",
                    ev.event_type,
                    ev.headers.get("aggregateId").map(String::as_str).unwrap_or("?")
                );
                Ok(())
            }),
        )
        .unwrap();

    let ev = Event::new(
        "wallets.events",
        "WalletOpened",
        "lumen",
        Some(br#"{"wallet_id":"wlt_1"}"#.to_vec()),
    );
    broker.publish(ev).await.unwrap();
    broker.close().unwrap();
}
```

Qué acaba de pasar. `handler(closure)` envuelve una clausura asíncrona como un
callback de entrega con conteo de referencias (el tipo que el broker almacena por
suscripción). `subscribe(topic, handler)` lo registra para el topic; el método
inherente del `InMemoryBroker` concreto es síncrono, así que devuelve un `Result`
al que aplicas `.unwrap()` en vez de `.await`. `publish(ev).await` ejecuta luego
cada manejador coincidente de forma secuencial en la tarea del publicador.
`close()` libera el broker.

> **Note** `InMemoryBroker::publish` espera cada manejador suscrito de forma
> secuencial en la tarea del publicador; el primer error de un manejador
> cortocircuita y se devuelve al publicador (envuelto en `EdaError::Handler`).
> Tras `close()`, tanto publish como subscribe fallan con `EdaError::Closed`.
> (Cuando echas mano del *port* `dyn Broker` en lugar del tipo concreto —como hace
> el ledger de Lumen— los métodos del trait son `async`, así que también aplicas
> `.await` a `subscribe`. Los métodos inherentes del concreto son síncronos; los
> métodos del port son asíncronos. El mismo broker, dos superficies.)

> **Tip** **Punto de control.** Ejecuta ese `main`. Imprime
> `observed WalletOpened for wlt_1`. Si cambias el topic de la suscripción por un
> glob como `wallets.*`, sigue coincidiendo: eso es el Paso 6.

## Paso 6 — Cerrar el bucle con un bean de proyección

Aquí es donde Lumen cierra el bucle de CQRS. La **proyección** es un bean de DI:
el análogo en Rust de un `@Component` de Spring con un método `@EventListener`.
`WalletProjection` es un `#[derive(Service)]` cuyos colaboradores se obtienen con
`#[autowired]` del contenedor: el `Ledger` (por el store de eventos que reproduce)
y el `ReadModel` al que alimenta —el *mismo* `ReadModel` que lee la consulta
`GetWallet`—. Un impl `#[handlers]` marca su método con
`#[event_listener(topic = "wallets.events")]`, de modo que por cada evento
entregado el framework lo llama; alcanza sus colaboradores a través de `self`,
recarga el flujo de la cartera afectada, lo repliega en una `WalletView` y lo hace
upsert.

> **Note** **Término clave — `#[derive(Service)]` / `#[handlers]` / `#[event_listener]`.**
> `#[derive(Service)]` marca una estructura como un bean de DI singleton cuyos
> campos `#[autowired]` rellena el contenedor (el `@Service`/`@Component` de
> Spring). `#[handlers]` sobre el impl le indica al framework que escanee sus
> métodos en busca de atributos de manejador. `#[event_listener(topic = …)]`
> suscribe un método a un topic del broker —el análogo de `@KafkaListener`—.
> Escribes la reacción; el framework hace la suscripción.

Añade esto a `samples/lumen/src/ledger.rs`:

```rust
use std::sync::Arc;

use firefly::eda::Event;
use firefly::prelude::*;

use crate::domain::Wallet;
// `Ledger` and `ReadModel` are defined earlier in this same module.

/// The read-model **projection bean** — Spring's `@Component @EventListener`. It
/// `#[autowired]`s the `Ledger` (for the event store it replays) and the
/// `ReadModel` it feeds; `#[handlers]` subscribes its `project` method to
/// `EVENTS_TOPIC`. The idempotent rebuild-from-stream projection that closes the
/// CQRS loop, wired entirely through the DI container with no process-global.
#[derive(Service)]
struct WalletProjection {
    /// The application service whose event store the projection replays
    /// (autowired).
    #[autowired]
    ledger: Arc<Ledger>,
    /// The read model the projection upserts (autowired) — the same instance the
    /// `GetWallet` query reads.
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletProjection {
    /// Projects one delivered wallet event into the read model.
    #[event_listener(topic = "wallets.events")]
    async fn project(&self, ev: Event) -> FireflyResult<()> {
        let Some(wallet_id) = ev.headers.get("aggregateId") else {
            return Ok(());
        };
        // A transient store miss is swallowed so one poison message never stalls
        // the projection — the EDA at-least-once contract.
        if let Ok(events) = self.ledger.store().load(wallet_id).await {
            let view = Wallet::rehydrate(wallet_id, &events).view();
            self.read_model.upsert(view);
        }
        Ok(())
    }
}
```

Qué acaba de pasar, línea a línea. La estructura tiene dos campos `#[autowired]`,
así que el contenedor construye `WalletProjection` entregándole los singletons
`Ledger` y `ReadModel` existentes: sin `new`, sin código de cableado. El método
`project` lee `aggregateId` de una cabecera (los metadatos de enrutamiento que
estampó el Paso 3); si falta, el evento no es para nosotros y devolvemos `Ok(())`.
En caso contrario hacemos `load` del flujo de eventos completo de la cartera,
`rehydrate` del agregado, tomamos su `.view()` y la hacemos `upsert` en el modelo
de lectura. El método devuelve `FireflyResult<()>`: el `Result<(), FireflyError>`
del framework.

Dos propiedades hacen de esta una *buena* proyección, no meramente una que
funciona.

Es **idempotente.** En lugar de mutar la fila del modelo de lectura a partir del
único evento entregado (`balance += amount`), recarga el flujo completo de la
cartera y reconstruye la vista desde cero. Bajo la entrega al-menos-una-vez de la
EDA, un `MoneyDeposited` reentregado contaría dos veces si aplicaras el delta,
pero volver a replegar el mismo flujo converge en la misma `WalletView` sin
importar cuántas veces llegue el evento. La cabecera lleva el `aggregateId`; eso
es todo lo que la proyección necesita para encontrar el flujo.

Está **desacoplada.** `WalletProjection` no importa ningún comando, no llama a
ningún manejador y no tiene ni idea de que se procesó un ingreso. Reacciona
puramente al hecho publicado. Puedes añadir un suscriptor `FraudDetector` o
`WelcomeNotifier` a su lado sin tocar una línea de la ruta de comandos, que es
exactamente el Ejercicio 1.

> **Note** `#[event_listener(topic = "wallets.events")]` sobre un método de un bean
> `#[handlers]` envía un `BeanListenerRegistration` al registro `inventory` que el
> framework drena. En el arranque, `FireflyApplication` resuelve `WalletProjection`
> desde el contenedor —autowireando su `Ledger` + `ReadModel`— y suscribe su método
> `project` al topic mediante
> `subscribe_discovered_listener_beans(broker, container)`. La suscripción se
> cablea por ti; tú solo escribes la reacción.

> **Tip** **Punto de control.** `cargo test -p lumen` pasa el bucle HTTP completo:
> un `POST /api/v1/wallets/:id/deposit` fluye comando → ledger → store → broker →
> proyección → modelo de lectura, y el siguiente `GET /api/v1/wallets/:id` se sirve
> desde la vista proyectada, sin reparación manual.

## Paso 7 — Entender cómo se cablea la proyección (sin raíz de composición)

Como la proyección es un bean ordinario del contenedor, sus colaboradores llegan
por **inyección por constructor** a través de campos `#[autowired]`: sin
proceso-global que sembrar, sin paso `bind`. El contenedor entrega a
`WalletProjection` el mismo `Ledger` (y por tanto el mismo store de eventos) y el
mismo `ReadModel` que entrega a los manejadores de CQRS, así que los eventos que
publican los manejadores son exactamente los eventos que la proyección consume y
proyecta en la lectura que sirve la consulta `GetWallet`.

Por eso la fábrica `#[bean]` de `ledger` en `samples/lumen/src/web.rs` es ahora
una **fábrica pura**: construye el `Ledger` y lo devuelve, sin efecto secundario
de siembra de proyección:

```rust,ignore
// samples/lumen/src/web.rs — the `ledger` #[bean] factory.
#[bean]
fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
    let store: Arc<dyn EventStore> = store;
    Ledger::new(store, broker)
}
```

Qué acaba de pasar. Los parámetros de la fábrica se autowirean ellos mismos: el
contenedor proporciona el bean `MemoryEventStore` y el port `Arc<dyn Broker>` (que
el stack web registra, con `InMemoryBroker` por defecto). La fábrica hace upcast
del store concreto al port `dyn EventStore` y construye el `Ledger`. Aquí no hay
ninguna llamada a subscribe, ni una raíz de composición en ninguna parte.

La suscripción en sí la cablea `FireflyApplication` en el arranque:
`subscribe_discovered_listener_beans(broker, container)` resuelve el bean de
proyección y drena su método `#[event_listener]` sobre el broker (junto a la
versión de `fn` libre `subscribe_discovered_listeners` para listeners que no
necesitan colaboradores inyectados). Así que ni la fábrica `ledger` ni ninguna
raíz de composición llaman a un helper de suscripción a mano.

> **Design note.** Todo el bucle se *declara*, no se *ensambla*. Declaras el bean
> del store, el bean del modelo de lectura, la fábrica del ledger y el bean de
> proyección; el framework descubre cada uno, autowirea sus dependencias y suscribe
> el listener: la historia de Spring de `@Configuration` + `@Bean` + escaneo de
> componentes, con el registro de endpoints de listener sustituido por el drenaje
> de inventory. Cuando un listener *no* necesita colaboradores inyectados, la forma
> más sencilla de `fn` libre —un escueto
> `#[event_listener(topic = "…")] async fn(ev: Event) -> FireflyResult<()>`— es la
> alternativa, descubierta de la misma manera.

> **Tip** **Punto de control.** Lee el informe de arranque de Lumen. La línea
> `:: cqrs handlers: … | event listeners: … | scheduled tasks: …` ahora cuenta al
> menos un event listener: la proyección que el framework acaba de suscribir.

## Paso 8 — Llegar más lejos: topics con globs y grupos de consumidores

Un topic de suscripción es un *patrón* glob (`*`, `?`, `[..]`, `{a,b}`); un evento
publicado se entrega a cada suscripción cuyo patrón coincida con su topic. Lumen se
suscribe al exacto `wallets.events`, pero un servicio multi-evento podría desplegar
un solo listener sobre toda una familia:

```rust,ignore
broker.subscribe("wallets.*", handler(|ev| async move { Ok(()) })).unwrap();
// matches wallets.events, wallets.audit, ...
```

> **Note** **Término clave — grupo de consumidores.** Un *grupo de consumidores* es
> un conjunto de suscriptores que *compiten* por los eventos de un topic: cada
> evento coincidente va a exactamente **un** miembro del grupo (round-robin),
> mientras que grupos distintos reciben cada uno su propia copia. Es el modelo de
> grupos de consumidores de Kafka y el `group` / `@KafkaListener(groupId=…)` de
> Spring: la forma de escalar una carga de trabajo horizontalmente sin
> procesamiento doble.

```rust,ignore
broker.subscribe_group("wallets.events", "projections", handler1).unwrap();
broker.subscribe_group("wallets.events", "projections", handler2).unwrap();
// each event reaches exactly one of handler1/handler2
```

Así es como escalarías la proyección de Lumen horizontalmente: ejecuta varias
instancias de proyector en un grupo y el broker reparte los eventos entre ellas
(round-robin por `(topic, group)`), cada instancia dueña de una porción del espacio
de carteras. Una suscripción sin grupo —la que crea `#[event_listener]` por
defecto— siempre recibe su propia copia.

> **Tip** **Punto de control.** En un test de `InMemoryBroker`, suscribe dos
> manejadores al mismo grupo y publica dos eventos; cada manejador se ejecuta una
> vez. Suscribe dos manejadores *sin grupo* y publica un evento; ambos se ejecutan.
> Eso es fan-out frente a consumidores en competencia, en cuatro líneas.

## Paso 9 — Hacer los fallos sobrevivibles: reintento y dead-letter

`wrap_listener(handler, publisher, policy)` es el envoltorio de reintento/DLQ
agnóstico al adaptador. Una entrega fallida se reintenta hasta `retries` veces con
backoff lineal (`retry_delay * attempt`); al agotarse, el evento se republica al
topic de dead-letter (cuando está definido), llevando la carga útil, la clave y las
cabeceras originales más las cabeceras de diagnóstico `x-original-topic` y
`x-exception`:

> **Note** **Término clave — topic de dead-letter (DLT/DLQ).** Cuando un mensaje
> sigue fallando, no quieres que bloquee el flujo para siempre. Un *topic de
> dead-letter* es donde se aparcan los mensajes agotados para una inspección o
> reproducción posterior. Es el enrutamiento a dead-letter del
> `DefaultErrorHandler` de Spring Kafka y `@RetryableTopic`.

```rust
use std::sync::Arc;
use std::time::Duration;
use firefly_eda::{handler, wrap_listener, InMemoryBroker, ListenerPolicy};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = Arc::new(InMemoryBroker::new());
let inner = handler(|_ev| async { Err(firefly_kernel::FireflyError::internal("boom")) });
let wrapped = wrap_listener(
    inner,
    broker.clone(),
    ListenerPolicy::with_retries(3)
        .retry_delay(Duration::from_millis(50))
        .dead_letter_topic("wallets.events.DLT"),
);
broker.subscribe("wallets.events", wrapped).unwrap();
# });
```

Qué acaba de pasar. `ListenerPolicy::with_retries(3)` establece tres reintentos
tras el primer intento (cuatro intentos en total); `.retry_delay(...)` añade
backoff lineal; `.dead_letter_topic(...)` nombra el topic donde aparcar los eventos
agotados. `wrap_listener` devuelve un nuevo `Handler` que suscribes en lugar del
interno. Una política sin reintentos, sin topic y sin store es un pass-through:
devuelve el manejador original sin cambios, de modo que envolver tiene coste cero
cuando no está configurado.

La proyección de Lumen toma un camino más suave: *traga* un fallo transitorio del
store y devuelve `Ok(())` en vez de hacer fallar la entrega, así que un único
mensaje envenenado nunca atasca el flujo. Esa es la decisión correcta para una
proyección de reconstrucción desde el flujo: la siguiente reentrega, o el siguiente
evento de la cartera, converge de todos modos. Un listener *con efectos
secundarios* —uno que envía un correo o llama a una API externa— es donde
`wrap_listener` y un topic de dead-letter se ganan su sustento, porque ahí el
trabajo no se puede simplemente volver a derivar.

Para un registro inspeccionable de fallos (en lugar de un topic de enrutamiento),
cablea un `EdaDeadLetterStore` mediante `ListenerPolicy::dead_letter_store`: un
evento agotado se captura en el store (consultable con `list` / `get` / `remove`).
Puedes establecer ambos —capturar *y* enrutar— en una misma política.

> **Tip** **Punto de control.** Envuelve un manejador que siempre falla con
> `ListenerPolicy::with_retries(2).dead_letter_topic("orders.DLT")` sobre un broker
> que registra las publicaciones; tras una entrega, exactamente un evento aterriza
> en `orders.DLT` con una cabecera `x-original-topic`, y el manejador envuelto
> devuelve `Ok(())` en vez de dar error.

## Paso 10 — Filtrar la entrega con filtros de eventos

`EventFilter` es una compuerta de entrega por sobre superpuesta a la coincidencia
de topics. Donde el broker decide *qué* suscripciones alcanza un topic, un filtro
decide si una suscripción alcanzada realmente se *ejecuta*. Vienen dos —un filtro
regex de cabecera y un filtro de predicado arbitrario—. Los sobres de Lumen llevan
una cabecera `aggregateType`, así que un filtro de cabecera podría restringir un
suscriptor a los eventos de `Wallet`:

```rust
use firefly_eda::{handler, with_filters, Event, HeaderEventFilter, InMemoryBroker};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = InMemoryBroker::new();
let inner = handler(|_ev: Event| async { Ok(()) });
let gated = with_filters(inner, [HeaderEventFilter::new("aggregateType", r"^Wallet$").unwrap()]);
broker.subscribe("wallets.events", gated).unwrap();
# });
```

Qué acaba de pasar. `HeaderEventFilter::new(name, pattern)` compila una regex
anclada contra la cabecera nombrada (una cabecera ausente se trata como la cadena
vacía). `with_filters(handler, [filters])` envuelve el manejador para que se
ejecute solo en los eventos que pasan *todos* los filtros; un evento que no
coincide se descarta antes de que se ejecute el cuerpo del manejador: el manejador
envuelto simplemente devuelve `Ok(())`. Una lista de filtros vacía devuelve el
manejador sin cambios (coste cero). `PredicateEventFilter::new(closure)` es la vía
de escape cuando una regex sobre una cabecera no basta: filtra por cualquier
propiedad del sobre.

> **Tip** **Punto de control.** Construye
> `HeaderEventFilter::new("aggregateType", r"^Wallet$")`, envuelve un manejador
> contador con `with_filters` y luego entrega un sobre cuyo `aggregateType` sea
> `"Account"` y otro que sea `"Wallet"`. Solo el segundo incrementa el contador. Un
> filtro de cabecera es más barato que un `if` dentro del manejador porque el
> descarte ocurre antes de que se ejecute tu código: Ejercicio 3.

## Paso 11 — Consumir reactivamente como un `Flux`

`InMemoryBroker::subscribe_reactive(topic)` es el gemelo reactivo de `subscribe`:
un `Flux<Event>` que emite cada evento entregado al topic, componiendo con el
conjunto completo de operadores de reactive-streams de Firefly. `publish_mono(event)`
es la publicación reactiva fría: no ocurre nada hasta que se suscribe el `Mono`
devuelto.

> **Note** **Término clave — `Flux` / `Mono`.** `Flux<T>` es un flujo reactivo de
> *muchos* `T`; `Mono<T>` es un flujo reactivo de *como mucho un* `T`. Son el port
> de Firefly del `Flux` / `Mono` de Project Reactor (Spring WebFlux). Ambos son
> *fríos* y *perezosos*: construir uno no hace trabajo; el trabajo se ejecuta
> cuando te suscribes (aquí, `.block().await`).

```rust
use std::sync::Arc;
use firefly_eda::{Event, InMemoryBroker};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = Arc::new(InMemoryBroker::new());
let flux = broker.subscribe_reactive("wallets.*").unwrap();

broker
    .publish_mono(Event::new("wallets.events", "WalletOpened", "lumen", None))
    .block()
    .await
    .unwrap();
broker.close().unwrap(); // terminates the Flux

let events = flux.take(1).collect_list().block().await.unwrap().unwrap();
assert_eq!(events[0].topic, "wallets.events");
# });
```

Qué acaba de pasar. `subscribe_reactive("wallets.*")` devuelve un `Flux<Event>`
respaldado por un canal acotado. `publish_mono(...)` construye un `Mono<()>` frío;
`.block().await` lo conduce, ejecutando la publicación. Cerrar el broker descarta
el emisor, lo que termina el `Flux`. Luego `flux.take(1).collect_list()` compone
dos operadores en un `Mono<Vec<Event>>`; `.block().await` produce un
`Result<Option<Vec<Event>>, _>`, así que los dos `.unwrap()` desenvuelven el
`Result` y luego el `Option`.

> **Note** Las entregas se bufferean a través de un canal acotado; cuando el
> consumidor aguas abajo se queda atrás, los eventos más nuevos se descartan
> (`onBackpressureDrop`) en lugar de bloquear o hacer fallar al publicador,
> extendiendo a la superficie reactiva la invariante del broker «un consumidor
> lento nunca hace fallar a los publicadores». Este es el mismo `Flux` sobre el que
> compone el endpoint de streaming opcional de Lumen (véase
> [Producción y despliegue](./20-production.md)).

> **Tip** **Punto de control.** La aserción de arriba se cumple: un evento
> publicado llega al `Flux`, y su `topic` es `wallets.events` aunque te
> suscribieras al glob `wallets.*`.

## Paso 12 — Eventos en proceso y externalización tras commit

El broker lleva eventos *entre* servicios. Dentro de un mismo servicio a menudo
quieres el mismo desacoplamiento sin un salto de red: un componente emite un hecho,
otros reaccionan, y ninguno de ellos sabe que los otros existen. Eso es el
`ApplicationEventPublisher` / `@EventListener` de Spring, y Firefly lo incluye como
un bus en proceso, asíncrono y seguro entre hilos, junto al broker.

> **Note** **Término clave — bus de eventos en proceso.** Un bus de
> publicación/suscripción *local al proceso*: haces `publish_event(value)` y
> cualquier `#[application_event_listener]` para ese tipo reacciona —sin broker, sin
> red, sin serialización—. El `ApplicationEventPublisher.publishEvent(...)` +
> `@EventListener` de Spring.

Publica con `publish_event`, y escucha con `#[application_event_listener]` sobre una
función asíncrona libre que toma el evento por referencia compartida. Los listeners
se descubren a lo largo del grafo de crates (el mismo escaneo de `inventory` que
encuentra tus componentes), así que no hay registro manual:

```rust,ignore
use firefly::prelude::*;

struct WalletOpened { id: String }

#[firefly::application_event_listener]
async fn audit_opening(event: &WalletOpened) {
    tracing::info!(wallet = %event.id, "wallet opened");
}

// somewhere in a command handler:
publish_event(WalletOpened { id: wallet_id }).await;
```

### Escuchar en relación con una transacción

Un listener simple se ejecuta en el instante en que publicas. A menudo eso es
demasiado pronto: no quieres enviar una notificación de «cartera abierta» hasta que
la transacción de base de datos que la abrió haya confirmado realmente.
`#[transactional_event_listener]` ata el listener a una fase del límite
`#[transactional]` circundante —`after_commit` (el predeterminado), `before_commit`,
`after_rollback` o `after_completion`—:

```rust,ignore
#[firefly::transactional_event_listener]               // after_commit
async fn notify_owner(event: &WalletOpened) {
    // Runs only once the opening transaction commits; never on a rollback.
    mailer.send_welcome(&event.id).await;
}
```

Los eventos publicados dentro de una transacción se bufferean y se despachan en la
fase elegida; una transacción revertida dispara los listeners `after_rollback` y
nunca los `after_commit`, de modo que una escritura fallida nunca puede filtrar un
efecto secundario de «éxito». Sin ninguna transacción activa, el listener recurre a
ejecutarse de inmediato (tratando el trabajo como ya confirmado), así que el mismo
manejador es útil en un test unitario o en una ruta sin datasource. Si quieres
semántica de eventos transaccionales sin ningún datasource SQL en absoluto, registra
el `LocalTransactionManager` (el equivalente en Rust del
`ResourcelessTransactionManager` de Spring).

### Tender un puente de los eventos en proceso al broker

Las dos capas se componen en el patrón que casi siempre quieres: haz el trabajo en
proceso y, una vez que confirma, publica un evento de integración al broker —nunca
un mensaje «fantasma» para una transacción que se revirtió—. Eso es la
externalización de eventos de Spring Modulith, y `externalize_after_commit` la
cablea en una línea:

```rust,ignore
// at startup, once per externalized event type:
firefly::eda::externalize_after_commit::<WalletOpened>("wallet.events", "wallet.opened");

// thereafter, an ordinary in-process publish inside a transaction...
publish_event(WalletOpened { id: wallet_id }).await;
// ...is serialized to JSON and published to the "wallet.events" topic on the
// registered broker the moment the transaction commits.
```

`externalize_after_commit` simplemente registra un listener `after_commit` que
reenvía a través de `publish_to_broker` (que serializa la carga útil y publica
mediante el `Broker` registrado con `register_broker`). Una transacción confirmada
llega a Kafka, RabbitMQ o cualquier transporte que hayas cableado; una revertida no
publica nada. El reenvío tras commit es de mejor esfuerzo: un broker ausente o un
fallo de publicación no deshace la transacción ya confirmada; echa mano de un outbox
de verdad (próximo capítulo) cuando necesites al-menos-una-vez.

Tres roles distintos, fáciles de mantener claros:

- `#[event_listener("topic")]` *consume* de un topic del broker —el análogo de
  `@KafkaListener` (la proyección de Lumen en el Paso 6)—.
- `#[application_event_listener]` / `#[transactional_event_listener]` manejan eventos
  *en proceso*.
- `externalize_after_commit` es el *puente* del segundo a un productor del broker.

> **Tip** **Punto de control.** Puedes nombrar, para cada uno de esos tres, si
> cruza un límite de proceso (solo el primero y el puente lo hacen) y si es
> consciente de transacciones (el listener transaccional y el puente lo son).

## Paso 13 — Cambiar a un transporte de producción

Cada crate de transporte implementa el mismo port `Broker`; cambia el constructor y
conserva cada manejador. Programa contra `firefly_eda::Broker` y selecciona el
adaptador en el momento del cableado —para un servicio `FireflyApplication` eso es
una palanca de configuración `firefly.*` (o un `#[bean]` que proporciona el port
`dyn Broker`)—. Sustituye el broker en memoria por uno de Kafka y la proyección, el
ledger y cada comando siguen compilando sin cambios.

| Crate                  | Backend         | Constructor                                       |
|------------------------|-----------------|---------------------------------------------------|
| `firefly-eda-kafka`    | Apache Kafka    | `new_kafka_broker(KafkaConfig)?`                  |
| `firefly-eda-rabbitmq` | RabbitMQ        | `RabbitMqBroker::new(RabbitMqBrokerConfig)`       |
| `firefly-eda-postgres` | Postgres outbox | `PostgresBroker::new(PostgresConfig::new(dsn))`   |
| `firefly-eda-redis`    | Redis Streams   | `RedisStreamsBroker::connect(RedisConfig::new(url))?` |

Kafka, por ejemplo —fíjate en que el cuerpo del manejador es idéntico al de Lumen,
y como aquí tienes un `Box<dyn Broker>` los métodos del trait son `async`—:

```rust,no_run
use firefly_eda::{handler, Event};
use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};

# async fn ex() -> firefly_eda::EdaResult<()> {
let broker = new_kafka_broker(KafkaConfig {
    brokers: vec!["kafka:9092".into()],
    client_id: "lumen".into(),
    consumer_group: "lumen-projections".into(),
    ..Default::default()
})?;

broker
    .subscribe("wallets.events", handler(|ev: Event| async move {
        println!("observed {}", ev.event_type);
        Ok(())
    }))
    .await?;

let ev = Event::new("wallets.events", "WalletOpened", "lumen", None);
broker.publish(ev).await?;
# Ok(())
# }
```

Qué acaba de pasar. `new_kafka_broker(KafkaConfig { … })?` devuelve un
`Box<dyn Broker>` (de ahí el `?`). En el *port*, `subscribe` y `publish` son métodos
de trait `async`, así que aplicas `.await` a ambos —la única diferencia con el
`InMemoryBroker` concreto del Paso 5, cuyos métodos inherentes son síncronos—. La
clausura dentro de `handler(...)` es byte a byte lo que escribirías para el broker
en memoria.

Redis Streams usa un ciclo de vida de conectar-y-luego-arrancar:

```rust,no_run
use firefly_eda::{handler, Event};
use firefly_eda_redis::{RedisConfig, RedisStreamsBroker};

# async fn ex() -> firefly_eda::EdaResult<()> {
let broker = RedisStreamsBroker::connect(
    RedisConfig::new("redis://localhost:6379/0")
        .with_streams(["wallets.events"])
        .with_group("lumen-projections"),
)?;
broker.subscribe("wallets.*", handler(|ev: Event| async move {
    println!("got {}", ev.event_type);
    Ok(())
})).await?;
broker.start().await?;
# Ok(())
# }
```

Qué acaba de pasar. `RedisStreamsBroker::connect(config)?` marca a Redis y devuelve
el broker; `RedisConfig::new(url).with_streams([...]).with_group(...)` es el builder.
Haces `subscribe` antes de `start()` —`start()` comienza a consumir de los streams
declarados—. (RabbitMQ tiene la misma forma de connect/start;
`RabbitMqBroker::new(config)` devuelve el broker y `start()` declara su topología.)

> **Note** El broker de Postgres es un **outbox transaccional**: los eventos se
> escriben en la misma transacción que tu cambio de estado y se drenan a los
> consumidores mediante `LISTEN`/`NOTIFY`, dando entrega al-menos-una-vez sin un
> broker aparte. Eso cierra el hueco entre append y publish del Paso 4; el
> [próximo capítulo](./11-event-sourcing.md) cubre la primitiva del outbox
> directamente.

> **Tip** **Punto de control.** Añade la feature `eda-kafka` y proporciona el port
> `dyn Broker` como un `#[bean]` que construye `new_kafka_broker(...)`. No necesitas
> un Kafka en marcha: `cargo build` confirma que la proyección, el ledger y los
> manejadores de comandos compilan sin cambios contra el port. Eso es el Ejercicio 4.

## Salud del broker

`EventPublisherHealthIndicator` adapta cualquier broker que implemente la sonda de
ping `BrokerHealth` a un `firefly_observability::Indicator`, exponiendo la vivacidad
del broker en `/actuator/health` bajo el id `eventPublisher`, de modo que cuando
Lumen se gradúe a un broker real, su readiness aparezca junto al resto de la salud
del servicio (véase [Observabilidad](./15-observability.md)). El broker en memoria
reporta `UP` hasta que se cierra.

## Resumen — qué cambió en Lumen

El bucle de CQRS está cerrado. Donde los manejadores de comandos del Capítulo 9
persistían eventos y dejaban que el lado de lectura se reparase solo, Lumen ahora
publica cada evento persistido y lo proyecta de vuelta automáticamente.

| Pieza | Rol |
|-------|------|
| `EVENTS_TOPIC` / `EVENT_SOURCE` | Constantes compartidas en las que coinciden el publicador y el listener |
| `to_envelope(&DomainEvent)` | Tiende un puente de un evento de dominio persistido al `Event` de transporte (key = id de cartera, las cabeceras llevan el enrutamiento) |
| `Ledger::commit` | Añade y **luego** publica cada evento: guarda antes de publicar |
| `WalletProjection` (`#[derive(Service)]` + `#[handlers]`) | El **bean** de proyección: hace `#[autowired]` del `Ledger` + `ReadModel`, su método `#[event_listener]` reconstruye el modelo de lectura desde el flujo |
| `#[event_listener(topic = "wallets.events")]` | Marca el método del bean; envía un `BeanListenerRegistration` que el framework drena (`subscribe_discovered_listener_beans`), resolviendo el bean y suscribiendo el método |
| Inyección por constructor | La proyección alcanza sus colaboradores a través de campos `#[autowired]`: sin `OnceLock`, sin `bind`; el `#[bean]` `ledger` es una fábrica pura |
| `Broker` del framework (`InMemoryBroker`) | El transporte predeterminado: cambia el adaptador por Kafka/RabbitMQ/Redis/Postgres, conserva el listener |

También sabes ahora:

- La diferencia entre un **evento de dominio** (duradero, en el store) y un
  **evento de mensajería** (el sobre de transporte, en el broker), y qué capítulo
  es dueño de cada uno.
- El alcance del broker: topics con globs, **grupos** de fan-out frente a
  consumidores en competencia, reintento/dead-letter de `wrap_listener`, **filtros**
  por sobre y la superficie reactiva `Flux`.
- Los tres roles de evento —`#[event_listener]` (consumo del broker),
  `#[application_event_listener]` / `#[transactional_event_listener]` (en proceso) y
  `externalize_after_commit` (el puente)—.

Tres principios se llevan adelante: **guarda antes de publicar** para que un
suscriptor nunca vea un hecho sin confirmar; **haz las proyecciones idempotentes**
para que la reentrega al-menos-una-vez sea inofensiva (Lumen vuelve a replegar el
flujo en lugar de aplicar un delta); y **depende del port `Broker`, no del
adaptador** para que el broker en memoria se convierta en Kafka con un cambio de una
línea.

Los eventos que Lumen publica aquí siguen estando respaldados por un store
transitorio en memoria. El [próximo capítulo](./11-event-sourcing.md) convierte esos
eventos en la *fuente de la verdad*: duraderos, reproducibles, el registro canónico
a partir del cual se recalcula cada saldo.

## Ejercicios

1. **Añade un listener `WelcomeNotifier`.** Como el notificador no necesita
   colaboradores inyectados, echa mano de la forma más sencilla de `fn` libre:
   escribe un `#[event_listener(topic = "wallets.events")] async fn` que reaccione
   solo a `WalletOpened` (comprueba `ev.event_type`) y registre una línea de
   bienvenida que lleve la cabecera `aggregateId`. El framework drena el nuevo
   listener automáticamente —no añades ninguna llamada a subscribe—. Confirma —a
   través de un test unitario de `InMemoryBroker` que publica un sobre
   `WalletOpened`— que se dispara, mientras los manejadores de comandos existentes
   quedan intactos.

2. **Demuestra la idempotencia.** En un test, construye un `Ledger` sobre un
   `MemoryEventStore` y un `InMemoryBroker`, suscribe la proyección, abre una
   cartera e ingresa dos veces. Luego publica el *mismo* sobre `MoneyDeposited` una
   segunda vez con `broker.publish(to_envelope(&event)).await` y afirma que el saldo
   de la `WalletView` del modelo de lectura no cambia: el repliegue de
   reconstrucción desde el flujo absorbe la reentrega.

3. **Filtra por tipo de agregado.** Envuelve el manejador de la proyección con
   `with_filters` y un `HeaderEventFilter::new("aggregateType", r"^Wallet$")`, luego
   publica un sobre cuya cabecera `aggregateType` sea `"Account"` y confirma que la
   proyección no se ejecuta para él. Explica por qué un filtro de cabecera es una
   guarda más barata que comprobar dentro del cuerpo del manejador. (Pista: el
   descarte ocurre antes de que se invoque el manejador.)

4. **Cambia a un broker real (boceto).** Añade la feature `eda-kafka` al crate y
   proporciona el port `dyn Broker` como un `#[bean]` que construye
   `new_kafka_broker(...)` en lugar de apoyarte en el broker en memoria por defecto.
   No necesitas un Kafka en marcha —el objetivo es confirmar que la proyección, el
   ledger y los manejadores de comandos compilan sin cambios contra el port
   `Broker`—.

5. **Enruta un fallo.** Envuelve un manejador que siempre falla con
   `wrap_listener(inner, broker.clone(), ListenerPolicy::with_retries(2)
   .dead_letter_topic("wallets.events.DLT"))`, suscríbelo y suscribe un segundo
   manejador a `wallets.events.DLT`. Publica un evento y afirma que el manejador de
   dead-letter lo observa llevando una cabecera `x-original-topic` de
   `wallets.events`.

## Adónde ir después

- Haz estos eventos duraderos y reproducibles en **[Event
  Sourcing](./11-event-sourcing.md)**, donde el store en memoria se convierte en la
  fuente de la verdad y el outbox transaccional cierra el hueco entre append y
  publish.
- Expón la vivacidad del broker y las métricas de peticiones en
  **[Observabilidad](./15-observability.md)**: el indicador de salud `eventPublisher`
  se une al resto de la superficie del actuator.
- Cablea un transporte real de Kafka o RabbitMQ y el endpoint de streaming reactivo
  en **[Producción y despliegue](./20-production.md)**.
