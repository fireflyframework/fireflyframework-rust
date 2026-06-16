# Programación y notificaciones

Un servicio de monedero real hace mucho más que responder a peticiones HTTP.
Barre monederos abandonados durante la noche, recalcula intereses, reintenta
transferencias atascadas y —la parte que el cliente nota— envía por correo un
extracto diario. Nada de eso lo dispara una petición: se ejecuta según un reloj,
o como reacción a algo que ocurre en otro sitio. Este capítulo dota a Lumen de su
primera pieza de trabajo en *segundo plano* y traza la superficie de integración
que cuelga de ella.

Cuatro preocupaciones del framework cubren esta historia de back-office, y
Firefly ofrece cada una detrás de la misma fachada `firefly` de la que dependes
desde el [Inicio rápido](./02-quickstart.md):

- **Programación** (`firefly-scheduling`) — ejecutar código con un temporizador.
- **Notificaciones** (`firefly-notifications`) — entregar mensajes a través de
  proveedores intercambiables (correo, SMS, push).
- **Webhooks salientes** (`firefly-callbacks`) — enviar eventos firmados a otros
  sistemas que quieren reaccionar a Lumen.
- **Webhooks entrantes** (`firefly-webhooks`) — recibir y validar callbacks del
  proveedor de pagos externo de Lumen.

Construiremos la pieza de programación de principio a fin —una tarea real,
registrada y en ejecución— y luego trazaremos exactamente dónde se enganchan la
notificación, el webhook saliente y el callback entrante. Lumen mantiene la tarea
programada deliberadamente diminuta —registra que se ha ejecutado— para que veas
el cableado sin arrastrar el SDK de un proveedor a la base de enseñanza.

Al terminar este capítulo, serás capaz de:

- Declarar una tarea programada con `#[scheduled]` y entender cómo el framework
  la *descubre* y la arranca sin una sola línea de cableado en `main`.
- Distinguir los cuatro tipos de disparador —cron, cron con zona, frecuencia fija,
  retardo fijo— y elegir el adecuado para un trabajo dado.
- Leer la gramática cron que Firefly acepta, incluidas las formas con zona horaria
  y las macros.
- Despachar una notificación a través del `Dispatcher` agnóstico al canal y ver
  cómo un proveedor real encaja detrás del mismo trait.
- Esbozar un webhook saliente firmado con `firefly-callbacks` y un webhook
  entrante validado con `firefly-webhooks`, y saber dónde colgaría cada uno del
  calendario de Lumen.

## Conceptos que conocerás

Antes de la primera línea de código, estas son las ideas en las que se apoya este
capítulo. Cada una se reintroduce en contexto allí donde se usa por primera vez;
esta es la versión breve.

> **Note** **Término clave — tarea programada.** Una *tarea programada* es un
> fragmento de código que el framework ejecuta con un temporizador en lugar de
> como respuesta a una petición. Tú escribes el trabajo; un *disparador* decide
> cuándo se activa. El análogo en Spring es un método anotado con `@Scheduled`.

> **Note** **Término clave — disparador.** Un *disparador* (trigger) es la regla
> que responde a «¿cuándo se ejecuta esta tarea la próxima vez?» —cada minuto, a
> las 2 a. m. todos los días, 30 segundos después de que terminara la última
> ejecución—. Firefly ofrece cuatro tipos de disparador (cron, cron con zona,
> frecuencia fija, retardo fijo); Spring expresa las mismas opciones mediante
> `@Scheduled(cron=…)`, `fixedRate` y `fixedDelay`.

> **Note** **Término clave — descubrimiento en tiempo de enlazado.** Firefly
> encuentra tus tareas programadas en *tiempo de enlazado* usando el crate
> `inventory`: cada `#[scheduled]` envía un registro a un registro de tiempo de
> compilación, y el framework vacía ese registro al arrancar. El análogo en Spring
> es el escaneo de componentes —salvo que aquí ocurre en tiempo de
> compilación/enlazado sin reflexión en tiempo de ejecución, de modo que «qué hay
> programado» es un conjunto fijo e inspeccionable—.

> **Note** **Término clave — canal / dispatcher.** Un *canal* es un transporte que
> entrega un mensaje (correo, SMS, push); un *dispatcher* enruta un mensaje al
> canal registrado para su tipo. Programas contra el *puerto* del canal (un trait)
> y registras un proveedor concreto en el momento del cableado. El análogo en
> Spring es un `NotificationService` que pone una cara a remitentes conectables.

## Paso 1 — Declarar una tarea programada

El trabajo en segundo plano de Lumen vive en `src/housekeeping.rs`. La función
completa es una única `async fn` sin argumentos que lleva un atributo
`#[scheduled(...)]`. Crea el archivo con este contenido:

```rust,ignore
// src/housekeeping.rs
use std::sync::atomic::{AtomicU64, Ordering};

use firefly::prelude::*;

/// The number of times the heartbeat has run — observable from a test (and, in
/// a real service, a counter you would surface on `/actuator/metrics`).
static HEARTBEAT_TICKS: AtomicU64 = AtomicU64::new(0);

/// A periodic housekeeping heartbeat. `#[scheduled(fixed_rate = "60s")]` makes
/// the framework call this on every tick after the initial delay.
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> {
    HEARTBEAT_TICKS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
```

Luego añade el módulo a la raíz de tu crate para que se compile y se escanee. En
`src/main.rs` la línea ya existe en la lista de módulos de Lumen (la configuraste
en el [Inicio rápido](./02-quickstart.md)); si vas siguiendo el tutorial de forma
incremental, añádela ahora:

```rust,ignore
// src/main.rs
mod housekeeping;
```

Lo que acaba de ocurrir, pieza a pieza:

- **`#[scheduled(fixed_rate = "60s", initial_delay = "5s")]`** es toda la
  declaración. `fixed_rate = "60s"` dice «dispara cada 60 segundos»;
  `initial_delay = "5s"` dice «espera 5 segundos tras el arranque antes del primer
  disparo». Las duraciones se escriben como cadenas legibles (`"60s"`, `"5m"`,
  `"500ms"`).
- **`ledger_heartbeat` es el trabajo.** Es una `async fn` corriente que no toma
  argumentos y devuelve un `Result`. Aquí solo incrementa un contador atómico; un
  despliegue real barrería monederos abandonados o lanzaría una generación de
  extractos.
- **`firefly::prelude::*`** trae todo lo que necesita la superficie del framework
  —incluido el propio macro `#[scheduled]` y el tipo `Scheduler` que conocerás en
  el Paso 3—. Esa única importación de la fachada lo cubre.

> **Note** **Término clave — registro `inventory`.** `inventory` es el crate de
> Rust que Firefly usa para el descubrimiento en tiempo de enlazado. El macro
> `#[scheduled]` hace dos cosas: genera un ayudante
> `schedule_ledger_heartbeat(&scheduler)` y envía un `ScheduledRegistration` al
> registro de `inventory`. Tú nunca llamas al ayudante: el framework itera el
> registro en el arranque. Este es el mismo mecanismo de descubrimiento que
> encuentra tus controladores y tus handlers de CQRS.

> **Tip** **Punto de control.** `cargo build` compila sin errores. Escribiste una
> función guiada por temporizador y no registraste nada a mano: el atributo hizo
> el registro.

## Paso 2 — Deja que el framework sea el dueño del scheduler

No escribiste ningún `tokio::spawn`, ningún `Scheduler::new()`, ni ninguna llamada
a `start()` —y no lo harás—. `FireflyApplication::run()` (la única línea en el
`main` de Lumen) es el dueño del scheduler. Durante el pipeline de arranque que
leíste en
[Inicio rápido, Paso 6](./02-quickstart.md#step-6--understand-what-run-does), el
framework:

1. Construye un `Scheduler`.
2. Vacía el registro de `inventory` —llamando a
   `firefly::scheduling::register_discovered_scheduled(&scheduler)` para registrar
   cada tarea `#[scheduled]` (y una llamada hermana para las tareas declaradas como
   métodos de bean).
3. Arranca el scheduler en una tarea de tokio en segundo plano, de modo que se
   ejecuta durante toda la vida del proceso.

Esto significa que `main` no cambia nunca cuando añades una tarea programada: la
nueva tarea se *descubre*, no se enhebra a través de un punto de entrada. Es la
misma propiedad que se cumplía para los controladores y los handlers de CQRS en
capítulos anteriores.

Para las pruebas, Lumen mantiene un pequeño ayudante que construye un scheduler
*nuevo* y ejecuta el mismo descubrimiento contra él, de modo que una prueba pueda
introspeccionar el calendario sin arrancar la aplicación completa ni esperar a un
tick:

```rust,ignore
// src/housekeeping.rs (continued)
/// Registers the heartbeat on a fresh scheduler and returns it — used by the
/// tests to assert it registered. `main()` does NOT call this:
/// `FireflyApplication` drains the same `inventory` registry and starts the
/// scheduler.
pub fn build_scheduler() -> std::sync::Arc<Scheduler> {
    let scheduler = std::sync::Arc::new(Scheduler::new());
    // `#[scheduled]` tasks are DISCOVERED and registered through the
    // inventory/DI registry — no manual `schedule_<fn>` calls.
    firefly::scheduling::register_discovered_scheduled(&scheduler);
    scheduler
}

/// How many heartbeat ticks have run so far.
pub fn heartbeat_ticks() -> u64 {
    HEARTBEAT_TICKS.load(Ordering::Relaxed)
}
```

Lo que acaba de ocurrir: `build_scheduler` existe *solo* para las pruebas. Llama
exactamente al mismo `register_discovered_scheduled` que llama el framework, de
modo que la prueba ejercita el descubrimiento real. `Scheduler::new()` devuelve un
scheduler vacío cuyo proveedor de bloqueo distribuido es un no-op (comportamiento
de instancia única); la llamada de registro lo puebla desde el registro de
`inventory`.

> **Note** **Término clave — bloqueo distribuido.** Cuando ejecutas más de una
> copia de un servicio, normalmente quieres que un trabajo programado se ejecute en
> *exactamente una* de ellas. Un *bloqueo distribuido* (el modelo de
> Spring/ShedLock) permite que una tarea adquiera un bloqueo con nombre antes de
> cada tick y omita el tick si otra instancia lo posee. El `Scheduler::new()` por
> defecto usa un bloqueo no-op (cada tick se ejecuta), lo cual es correcto para una
> única instancia; para el caso en clúster se ofrecen bloqueos respaldados por
> Redis y Postgres.

El scheduler ejecuta cada tarea en su propia tarea de tokio con recuperación ante
pánicos —una tarea que entra en pánico se registra en el log y el calendario
continúa— y `stop()` apaga de forma ordenada, dejando que terminen primero las
ejecuciones en curso. Como `run()` captura SIGINT/SIGTERM, ese apagado ordenado
queda cableado en el ciclo de vida de Lumen sin coste alguno.

> **Tip** **Punto de control.** Ejecuta Lumen con `cargo run` y observa cómo el
> contador `scheduled tasks:` del informe de arranque sube para incluir
> `ledger_heartbeat`. Cinco segundos después del arranque, el heartbeat empieza a
> dispararse una vez por minuto —en silencio, ya que solo incrementa un contador—.

## Paso 3 — Observar el calendario desde una prueba

La tarea está registrada y haciendo tic, pero ¿cómo lo *demuestras* sin esperar 60
segundos? Dos costuras hacen el calendario observable. Primero, el scheduler expone
una instantánea de cada tarea registrada; segundo, el contador atómico del
heartbeat registra cada ejecución. El módulo de pruebas de Lumen comprueba ambas:

```rust,ignore
// src/housekeeping.rs (test module)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_task_registers() {
        let scheduler = build_scheduler();
        let names: Vec<String> = scheduler.tasks().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"ledger_heartbeat".to_string()));
    }

    #[tokio::test]
    async fn heartbeat_runs() {
        let before = heartbeat_ticks();
        ledger_heartbeat().await.unwrap();
        assert_eq!(heartbeat_ticks(), before + 1);
    }
}
```

Lo que acaba de ocurrir:

- **`scheduler.tasks()`** devuelve un `Vec<TaskDescriptor>` —una instantánea
  inmutable tomada en el momento del registro, donde cada entrada lleva un `name`,
  el descriptor del disparador y cualquier metadato de bloqueo—. La primera prueba
  introspecciona el calendario sin esperar: se registró, así que su nombre está
  presente.
- **`ledger_heartbeat().await`** llama directamente al cuerpo. Como el trabajo es
  una `async fn` corriente, una prueba puede invocarlo sin el scheduler en absoluto
  y comprobar el efecto secundario: el contador avanzó exactamente uno.

La misma instantánea `tasks()` alimenta el actuator: las tareas programadas
aparecen en `GET /actuator/scheduledtasks` (en el puerto de gestión) y en la vista
de Tareas Programadas del panel de administración, ambas presentadas en
[Observabilidad](./15-observability.md).

> **Tip** **Punto de control.** `cargo test heartbeat` pasa. Demostraste las dos
> mitades —la tarea se registró, y su cuerpo se ejecuta y es observable— sin un
> solo `sleep` en la prueba.

## Paso 4 — Elegir el disparador adecuado

`#[scheduled]` cubre el caso del día a día, pero el `Scheduler` subyacente expone
directamente los cuatro tipos de disparador, que es lo que usaría una generación de
extractos real de Lumen. Cada tipo responde a «¿cuándo la próxima vez?» de forma
distinta:

```rust,ignore
use std::{sync::Arc, time::Duration};
use firefly::prelude::Scheduler;

let s = Arc::new(Scheduler::new());

// Cron: a 5-field expression (or the 6-field form with leading seconds).
// Returns Result — the expression is parsed and can be rejected.
s.cron("daily-statements", "0 2 * * *", || async { Ok(()) }).unwrap();

// FixedRate: fire every period from a fixed anchor (the schedule slips if a run
// is slow, because the grid is anchored, not chained to the last finish).
s.fixed_rate("metrics-emit", Duration::from_secs(30), || async { Ok(()) });

// FixedDelay: fire `delay` after the previous run *finished* (no overlap).
s.fixed_delay("sweep-abandoned", Duration::from_secs(300), || async { Ok(()) });
```

Lo que acaba de ocurrir: registraste tres tareas en un scheduler a mano. Cada
clausura es una fábrica —el scheduler la llama una vez por disparo para obtener un
future nuevo— y cada una devuelve `Result<(), TaskError>`, de modo que una
ejecución fallida se registra a nivel `warn` y el calendario continúa. Observa que
`cron` devuelve un `Result` porque la expresión se parsea; los registros de
frecuencia/retardo toman una `Duration` tipada y no pueden fallar.

Los cuatro tipos, y cuándo recurrir a cada uno:

| Disparador          | Comportamiento                                              |
|---------------------|-------------------------------------------------------------|
| `CronTrigger`       | Se dispara cuando el reloj de pared **local** coincide con la expresión |
| `ZonedCronTrigger`  | Se dispara según la expresión en una zona horaria IANA      |
| `FixedRateTrigger`  | Se dispara cada periodo desde un ancla de inicio fija (se desfasa con ejecuciones lentas) |
| `FixedDelayTrigger` | Se dispara el retardo después de que terminara la ejecución anterior |

> **Design note.** La distinción entre frecuencia fija y retardo fijo es la que
> hace tropezar a la gente. La *frecuencia fija* cuelga una rejilla de un ancla
> fija: cada 30 s clavados, así que si una ejecución tarda 35 s, la siguiente se
> dispara de inmediato (se desfasó). El *retardo fijo* encadena: espera el retardo
> *después* de que cada ejecución termine, de modo que una ejecución lenta empuja
> la siguiente y dos ejecuciones nunca se solapan. Usa frecuencia fija para un
> muestreo constante (emitir una métrica cada 30 s); usa retardo fijo para trabajo
> en serie que no debe acumularse (barrer, esperar, volver a barrer).

Para una zona horaria —Lumen ejecutaría los extractos a las 9 a. m. en la región
del cliente— construye un `ZonedCronTrigger` en lugar de depender del reloj local
del host:

```rust,ignore
use firefly::scheduling::{parse_cron, ZonedCronTrigger};

// 1-5 is Monday through Friday (the day-of-week domain is 0 = Sunday .. 6 = Saturday).
let expr = parse_cron("0 9 * * 1-5").unwrap();
let trigger = ZonedCronTrigger::in_zone(expr, "America/New_York").unwrap();
```

Lo que acaba de ocurrir: `parse_cron` convierte la cadena de 5 campos en un
`CronExpr` tipado, y `ZonedCronTrigger::in_zone` evalúa esa expresión en la zona
IANA nombrada. Ambas llamadas devuelven un `Result` —una expresión malformada o un
nombre de zona desconocido es un error duro que manejas en el registro, nunca una
programación errónea silenciosa—. Para registrar una tarea cron con zona en una
sola llamada, el scheduler también ofrece `s.cron_in_zone(name, expr, zone, run)`.

> **Note** **Gramática cron.** El parser de Firefly acepta la expresión canónica de
> 5 campos `minuto hora día-del-mes mes día-de-la-semana`, una forma opcional de 6
> campos con un campo de **segundos** a la cabeza, el comodín `?` de Quartz (tratado
> como `*`) y las macros `@hourly` / `@daily` / `@weekly` / `@monthly` / `@yearly`.
> El día de la semana va de `0` (domingo) a `6` (sábado). Cuando tanto el día del
> mes como el día de la semana están restringidos, la regla se dispara cuando
> coincide **cualquiera** de los dos (comportamiento de Vixie cron). El atributo
> `#[scheduled]` acepta el mismo `cron = "…"` (con un `zone = "…"` opcional) en
> lugar de `fixed_rate` / `fixed_delay`.

> **Tip** **Punto de control.** Sabes nombrar los cuatro disparadores y explicar
> por qué una generación de extractos usa cron (una hora de reloj de pared), un
> emisor de métricas usa frecuencia fija (muestreo constante) y un barrido usa
> retardo fijo (sin solapamiento).

## Paso 5 — Despachar una notificación

El heartbeat es el gancho para la mensajería saliente. En un tick real de
extractos, Lumen construiría un mensaje por monedero y se lo entregaría a un
dispatcher. El crate `firefly-notifications` te da un sobre `Notification`
agnóstico al canal, un trait de transporte `Channel` y un `Dispatcher` que enruta
un mensaje al canal registrado para su `Kind`:

```rust,ignore
use std::sync::Arc;
use firefly_notifications::{Dispatcher, Kind, MemoryChannel, Notification};

let dispatcher = Dispatcher::new();
dispatcher.register(Arc::new(MemoryChannel::new(Kind::EMAIL)));

dispatcher
    .dispatch(Notification {
        channel: Kind::EMAIL,
        to: "alice@example.com".into(),
        subject: "Your Lumen statement".into(),
        body: "Closing balance: $42.00".into(),
        ..Notification::default()
    })
    .await
    .unwrap();
```

Lo que acaba de ocurrir, bloque a bloque:

- **`Dispatcher::new()`** crea un enrutador vacío. **`register`** añade un canal,
  indexado por el `Kind` al que sirve. Aquí `MemoryChannel::new(Kind::EMAIL)` es un
  canal integrado que simplemente registra cada mensaje que recibe —ideal para las
  pruebas, y exactamente lo que usa Lumen para que la base de enseñanza no arrastre
  el SDK de ningún proveedor—.
- **`dispatch`** construye un sobre `Notification` y lo enruta. El campo `channel`
  (`Kind::EMAIL`) selecciona el canal registrado; `to`, `subject` y `body` son el
  mensaje. `..Notification::default()` rellena los campos restantes (id, plantilla,
  variables, marca de tiempo) con sus valores cero.
- **El newtype `Kind`** lleva los canales canónicos `Kind::EMAIL`, `Kind::SMS` y
  `Kind::PUSH`, además de `Kind::new("...")` para un transporte personalizado.
  Despachar a un tipo sin canal registrado devuelve
  `NotificationError::NoChannel` —un error tipado, no un descarte silencioso—.

> **Note** **Término clave — puerto y adaptador.** Un *puerto* es el trait contra el
> que programas (`Channel`); un *adaptador* es una implementación concreta tras él
> (`MemoryChannel`, o un remitente SMTP real). Como cada canal es un
> `Arc<dyn Channel>`, los pesados SDK de los proveedores quedan fuera de cualquier
> servicio que no seleccione ese canal: escribes tu lógica de extractos contra el
> puerto y registras el adaptador concreto en el momento del cableado. Esto es
> arquitectura hexagonal, la misma forma que conociste en
> [Diseño guiado por el dominio](./08-domain-driven-design.md).

Para producción, registra un canal real en lugar de `MemoryChannel` —el mismo
trait `Channel`, entrega real—. Cada proveedor vive en su propio crate, de modo
que su SDK solo se compila en los servicios que se suman explícitamente:

| Crate                            | Canal   | Respaldo                         |
|----------------------------------|---------|----------------------------------|
| `firefly-notifications-smtp`     | correo  | `lettre` (MIME real, STARTTLS)   |
| `firefly-notifications-twilio`   | SMS     | Twilio                           |
| `firefly-notifications-firebase` | push    | Firebase Cloud Messaging         |
| `firefly-notifications-sendgrid` | correo  | SendGrid                         |
| `firefly-notifications-resend`   | correo  | Resend                           |

Así que el extracto diario de Lumen es, en una frase: «en el tick del heartbeat,
construye una `Notification` por monedero y haz `dispatch` de ella». Cambiar el
canal en memoria por uno SMTP real es un cambio de registro de una sola línea,
nunca una reescritura de la lógica de extractos.

> **Tip** **Punto de control.** Sabes seguir un mensaje desde `dispatch` a través
> del enrutado indexado por `Kind` hasta un canal, y sabes nombrar dónde encajaría
> un proveedor de correo real (la llamada a `register`) sin tocar el código de
> extractos.

## Paso 6 — Enviar un webhook saliente

Cuando otro sistema necesita *reaccionar* a un evento de Lumen —un monitor de
fraude que quiere cada depósito grande, por ejemplo—, Lumen le envía un webhook
saliente con `firefly-callbacks`. Los servicios registran `Target`s; el
`HmacDispatcher` firma cada payload, reintenta con backoff exponencial y registra
cada `Attempt` en un `Store` conectable para auditoría:

```rust,ignore
use std::sync::Arc;
use firefly_callbacks::{CallbackEvent, DispatcherConfig, HmacDispatcher, MemoryStore};

let store = Arc::new(MemoryStore::new());
// Defaults: 3 attempts, 200 ms initial delay, doubling between retries.
let dispatcher = HmacDispatcher::new(store, DispatcherConfig::default());

// On a large deposit, Lumen would publish a CallbackEvent; the dispatcher signs
// and POSTs it to every registered Target with a stable HMAC-SHA256 signature.
```

Lo que acaba de ocurrir: `HmacDispatcher::new` toma un `Store` (aquí el que está en
memoria, que conserva cada intento de entrega para inspección) y un
`DispatcherConfig`. Cualquier campo dejado en su valor cero se rellena con el valor
por defecto, así que `DispatcherConfig::default()` significa 3 intentos con un
primer retardo de 200 ms, duplicándose. Ante un evento disparador, Lumen publica un
`CallbackEvent`; el dispatcher hace POST del payload a cada `Target` registrado y
registra una fila `Attempt` por intento, independientemente del resultado.

> **Note** **Término clave — firma HMAC.** HMAC (código de autenticación de mensajes
> basado en hash) permite a un receptor verificar que un payload vino de ti y no fue
> manipulado, usando un secreto compartido. Firefly firma cada entrega con
> HMAC-SHA256 con clave en el secreto del destino, de modo que cualquier receptor
> que posea el mismo secreto puede verificar la petición con una llamada de
> biblioteca estándar —sin necesidad de código específico de Firefly—.

Cada entrega lleva estas cabeceras, idénticas byte a byte a los ports de Firefly en
Java, .NET, Go y Python, de modo que un receptor escrito contra cualquiera de ellos
verifica las entregas de Lumen sin cambios:

- `X-Firefly-Event` — el tipo de evento.
- `X-Firefly-Event-Id` — el id del evento.
- `X-Firefly-Timestamp` — segundos Unix en que se envió la petición.
- `X-Firefly-Signature` — `sha256=<hmac-hex>` con clave en el secreto del destino.

> **Tip** **Punto de control.** Sabes describir qué es un webhook saliente (Lumen
> haciendo POST de un evento firmado a un destino registrado) y nombrar la cabecera
> que comprueba un receptor (`X-Firefly-Signature`).

## Paso 7 — Recibir un webhook entrante

La imagen especular es `firefly-webhooks`: cuando el proveedor de pagos externo de
Lumen llama de *vuelta* —un cargo liquidado, un pago fallido—, el pipeline entrante
valida la firma, deduplica y despacha el evento a un procesador. Configura un
pipeline con un validador de proveedor y monta su router:

```rust,ignore
use std::sync::Arc;
use firefly_webhooks::{web, MemoryDlq, Pipeline, StripeValidator};

let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
pipeline.register_validator(StripeValidator::new(b"whsec_test"));
let app: axum::Router = web::router(pipeline); // mount under /api/webhooks/...
```

Lo que acaba de ocurrir: `Pipeline::new` toma una *cola de mensajes fallidos*
(dead-letter queue) —aquí la `MemoryDlq` en memoria, donde aterrizan para su
inspección posterior los eventos cuyo procesamiento falla—. `register_validator`
adjunta una comprobación de firma por proveedor; el `StripeValidator` está indexado
por el secreto del webhook (`whsec_test`). `web::router` convierte el pipeline en un
`axum::Router` que montas junto a las demás rutas de Lumen.

> **Note** **Término clave — cola de mensajes fallidos (DLQ).** Una *cola de
> mensajes fallidos* (dead-letter queue) es adonde va un mensaje cuando no puede
> procesarse —un webhook validado cuyo procesador dio error, por ejemplo—.
> Aparcarlo en una DLQ en lugar de descartarlo significa que puedes inspeccionarlo,
> corregirlo y reproducirlo más tarde. Este es el mismo patrón de EDA que conociste
> en [EDA y mensajería](./10-eda-messaging.md).

Se ofrecen validadores para los proveedores comunes; cada uno conoce la cabecera y
el algoritmo que usa su proveedor, de modo que registrar uno es toda la
integración:

| Validador         | Cabecera                | Algoritmo                                     |
|-------------------|-------------------------|-----------------------------------------------|
| `HmacValidator`   | `X-Signature` (por defecto) | HMAC-SHA256 hex (prefijo `sha256=` opcional) |
| `StripeValidator` | `Stripe-Signature`      | `t=<unix>,v1=<hmac-hex>`, tolerancia de 5 minutos |
| `GitHubValidator` | `X-Hub-Signature-256`   | HMAC-SHA256 hex                               |
| `TwilioValidator` | `X-Twilio-Signature`    | HMAC-SHA1 base64 de la URL + campos de formulario ordenados |

Pruebas ese receptor con los firmadores de `firefly-testkit` —`sign_stripe`,
`sign_github`, `sign_twilio`, `sign_hmac`—, que producen valores de cabecera
idénticos byte a byte a lo que esperan los validadores, de modo que una petición de
prueba firmada se valida exactamente igual que lo haría la de un proveedor real.
Los usarás en [Pruebas](./18-testing.md).

> **Tip** **Punto de control.** Sabes nombrar ambas mitades de la historia de
> integración: saliente (`firefly-callbacks`, tú firmas y envías) y entrante
> (`firefly-webhooks`, tú validas e ingieres), y señalar el validador que coincide
> con un proveedor dado.

## Resumen — qué cambió en Lumen

Este capítulo dotó a Lumen de su primer trabajo en segundo plano y trazó su
superficie de integración.

| Antes | Después de este capítulo |
|-------|--------------------------|
| sin trabajo en segundo plano | `src/housekeeping.rs` declara `ledger_heartbeat` con `#[scheduled(fixed_rate = "60s", initial_delay = "5s")]` |
| nada con temporizador | el framework descubre y arranca la tarea; hace tic una vez por minuto, observable mediante un `AtomicU64` y `scheduler.tasks()` |
| `main` no enhebra nada | `main` sigue siendo la única línea `FireflyApplication::run()` —la tarea se descubre, no se cablea— |
| — | el contador del heartbeat es la costura donde se engancharían una notificación de extracto diario, un webhook saliente de saldo cambiado y un callback entrante de proveedor |

Además, ahora sabes:

- Que `#[scheduled]` genera un ayudante `schedule_<fn>` *y* envía un
  `ScheduledRegistration` al registro de `inventory` que el framework vacía con
  `register_discovered_scheduled(&scheduler)` —de modo que nunca mantienes a mano
  una lista de llamadas de registro—.
- Los cuatro tipos de disparador y cuándo aplica cada uno, además de la gramática
  cron que Firefly acepta (5 campos, 6 campos con segundos, `?`, las macros estilo
  `@daily`, zonas IANA mediante `ZonedCronTrigger`).
- Que las notificaciones, los webhooks salientes y los webhooks entrantes cambian
  cada uno un proveedor por un registro de una sola línea, nunca un cambio de
  código, porque cada transporte es un objeto de trait (`Arc<dyn Channel>`, un
  `Target` registrado, un `Validator` registrado).

Lumen ahora hace trabajo en segundo plano y sabe exactamente dónde cuelga su
mensajería.

## Ejercicios

1. **Pon la generación de extractos en cron.** Sustituye el heartbeat de
   `fixed_rate` por una tarea `#[scheduled(cron = "0 2 * * *")]` (o registra
   `s.cron("statements", "0 2 * * *", ..)` directamente en un `Scheduler`) y
   comprueba que aparece en `scheduler.tasks()` por su nombre —sin esperar a un
   tick—.
2. **Despacha en el tick.** Dentro de `ledger_heartbeat`, construye un `Dispatcher`
   con un `MemoryChannel::new(Kind::EMAIL)`, despacha un extracto de una línea y
   comprueba (vía `MemoryChannel::messages`) que el mensaje quedó registrado.
3. **Firma y recibe.** Levanta un `Pipeline` con un `StripeValidator`, monta
   `web::router(..)` y usa `firefly_testkit::sign_stripe` para conducir una petición
   firmada a través de él con un `TestClient` —comprueba que se acepta, luego
   manipula el cuerpo y comprueba que se rechaza—.
4. **Audita lo saliente.** Cablea un `HmacDispatcher` sobre un `MemoryStore`,
   despacha un `CallbackEvent` a un `Target` y lee las filas `Attempt` registradas
   del store para confirmar que se disparó la política de reintentos.
5. **Elige retardo fijo en lugar de frecuencia fija.** Registra una tarea
   `fixed_delay` cuyo cuerpo duerma más que el retardo, ejecuta el scheduler
   brevemente y observa que las ejecuciones nunca se solapan —luego explica por qué
   una tarea de frecuencia fija con el mismo periodo se habría desfasado en su
   lugar—.

## Adónde ir después

- Profundiza en la caché del lado de lectura que introdujo la capa de CQRS en
  **[Caché](./17-caching.md)**.
- Revisita el feed `/actuator/scheduledtasks` del actuator y la vista de Tareas
  Programadas del panel de administración en **[Observabilidad](./15-observability.md)**.
- Prueba los schedulers, los dispatchers y los validadores de webhook con los
  canales en memoria y los firmadores de `firefly-testkit` en
  **[Pruebas](./18-testing.md)**.
