# Event Sourcing

El [capítulo anterior](./10-eda-messaging.md) dejó una pregunta cortésmente sin
formular. El `Ledger` de Lumen persiste eventos de monedero y una proyección
reconstruye el modelo de lectura volviendo a plegar el stream — pero *¿qué
stream?* Hasta ahora el estado canónico del monedero ha estado implícito. Al
terminar este capítulo es explícito y fundamental: el agregado `Wallet` **no
almacena ningún saldo en absoluto**. Su saldo es una función pura de un stream de
solo anexado (append-only) de eventos `WalletOpened`, `MoneyDeposited` y
`MoneyWithdrawn`, recalculado cada vez que se carga el agregado.

Eso es **event sourcing**: en lugar de almacenar el estado actual y descartar
cada cambio, almacenas la *secuencia de cambios* y derivas el estado
reproduciéndola. Un libro mayor financiero es el dominio ideal para ello — los
contables saben desde hace siglos que la autoridad de un libro mayor proviene de
sus asientos, no del total acumulado al pie de la columna. El total es un *hecho
derivado*; los asientos son la *fuente de verdad*. Al final, un auditor que
pregunte «¿cuál era el saldo del monedero `wlt_…` tras el tercer movimiento?»
obtiene una respuesta que Lumen puede *demostrar* a partir del stream, no
simplemente reportar a partir de una columna.

Este capítulo es una construcción guiada. Introducimos cada pieza desde primeros
principios, la escribimos bloque a bloque contra la API real de
`firefly-eventsourcing`, y nos detenemos en puntos de control para que puedas
confirmar lo que tienes antes de continuar. Nada aquí se da por sentado: cada
tipo, método y derive coincide con el crate que se distribuye en
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen).

Al terminar este capítulo, serás capaz de:

- Explicar la diferencia entre **almacenamiento de estado** y **almacenamiento de
  eventos**, y por qué el saldo del monedero se convierte en un cálculo en lugar
  de una columna.
- Definir eventos de dominio con `#[derive(DomainEvent)]` y un agregado basado en
  eventos con `#[derive(AggregateRoot)]`, y saber exactamente qué genera cada
  derive.
- Implementar la forma canónica de comando — validar, `raise`, luego `apply` — y
  comprender por qué **el mismo fold se ejecuta tanto en la ruta de escritura como
  en el replay**.
- Persistir y recargar eventos a través del puerto `EventStore` con **concurrencia
  optimista**, y manejar correctamente un conflicto de concurrencia.
- Reconocer las costuras de nivel de producción que ofrece el crate — snapshots,
  proyecciones, el stream global, el transactional outbox, upcasters y
  multi-tenancy — y saber cuándo cada una merece su sitio.

## Conceptos que conocerás

Cada idea de abajo se reintroduce en contexto donde se usa por primera vez; esta
es la versión corta para que el vocabulario no resulte nuevo cuando llegues a él.

> **Note** **Término clave — event sourcing.** Un estilo de persistencia en el que
> almacenas la *secuencia ordenada de cambios* (eventos) de una entidad en lugar
> de su estado actual, y recalculas el estado reproduciendo esa secuencia. El
> análogo en Java/Spring es el `firefly-event-sourcing-spring-boot-starter` (o los
> agregados basados en eventos de Axon Framework).

> **Note** **Término clave — domain event.** Un registro inmutable de que *algo
> ocurrió* en el dominio, nombrado en pasado (`MoneyDeposited`). En event sourcing
> los eventos son el sistema de registro. Esto es distinto del envoltorio `Event`
> de EDA del [capítulo anterior](./10-eda-messaging.md), que es el *transporte* de
> un hecho; el `DomainEvent` de aquí es el *registro* duradero del mismo.

> **Note** **Término clave — agregado.** Un grupo de objetos de dominio tratados
> como una única frontera de consistencia, con una **raíz de agregado** (aggregate
> root) como punto de entrada. Cada comando pasa por la raíz, que impone las
> invariantes del agregado. El agregado de Lumen es el `Wallet`; su raíz es el
> `AggregateRoot` del framework, embebido. Este es el «agregado» del Domain-Driven
> Design que los desarrolladores de Spring conocen por las raíces `@Entity` — pero
> aquí se reconstruye a partir de eventos, no se carga desde una fila.

> **Note** **Término clave — concurrencia optimista.** Una forma de detectar
> escrituras concurrentes sin bloquear: cada escritura declara la versión que
> esperaba encontrar, y el store la rechaza si otro escritor llegó antes. El
> análogo en Spring/JPA es el bloqueo optimista con `@Version`.

## Paso 1 — Siente el cambio: almacenamiento de estado vs almacenamiento de eventos

Antes de escribir una línea, observa qué *contiene* el almacenamiento de Lumen en
cada modelo. El contraste es toda la motivación de este capítulo.

En el **modelo de almacenamiento de estado** — el que está por defecto en todas
partes — el store guarda solo el estado actual del monedero:

| id | owner | balance | version |
|----|-------|---------|---------|
| wlt_a1 | alice | 120 | 3 |

Cada ingreso y cada retirada sobrescribe `balance`. La historia ha desaparecido:
sabes que el monedero contiene 120 céntimos ahora; no puedes saber cómo llegó
hasta ahí.

En el **modelo de almacenamiento de eventos**, el store guarda el stream:

| aggregate_id | version | event_type | payload |
|--------------|---------|------------|---------|
| wlt_a1 | 1 | WalletOpened | `{"wallet_id":"wlt_a1","owner":"alice","opening_balance":100}` |
| wlt_a1 | 2 | MoneyDeposited | `{"wallet_id":"wlt_a1","amount":50}` |
| wlt_a1 | 3 | MoneyWithdrawn | `{"wallet_id":"wlt_a1","amount":30}` |

El saldo actual sigue siendo 120 céntimos — pero ahora puedes leer cada decisión
que condujo a él, reproducir hasta cualquier versión, y auditarlo todo.

Lo que acaba de ocurrir: el mismo saldo final tiene ahora una *derivación*. El
compromiso es real y merece nombrarse de antemano — las lecturas cuestan un replay
(mitigado mediante **snapshots**, Paso 8) y los eventos son inmutables (el cambio
de esquema se maneja mediante **upcasters**, Paso 11). Ambos tienen soporte de
primera clase, y los conocerás a su debido tiempo.

> **Note** Event sourcing *no* es lo mismo que la EDA del
> [capítulo anterior](./10-eda-messaging.md). Allí, el agregado almacenaba su
> estado y *publicaba* eventos como efecto secundario. Aquí los eventos *son* el
> estado: no hay una columna `balance` que mantener sincronizada — el saldo se
> calcula plegando el stream cada vez que se carga el agregado.

> **Tip** **Punto de control.** Puedes enunciar, en una frase, qué pierde o
> conserva cada tabla: el almacenamiento de estado conserva la respuesta y descarta
> el trabajo; el almacenamiento de eventos conserva el trabajo y recalcula la
> respuesta. El resto del capítulo hace concreto ese recálculo.

## Paso 2 — El modelo mental: raise, append, fold

Todo lo de abajo son tres movimientos repetidos. Un comando **levanta** (`raise`)
un evento sobre el agregado; el store **anexa** (`append`) los eventos levantados
de forma duradera bajo concurrencia optimista; una carga posterior **pliega**
(`fold`) el stream de vuelta al estado actual. Ten presente este ciclo — cada API
del capítulo es uno de estos tres movimientos.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 322" role="img"
     aria-label="Event sourcing: a command raises an event onto the aggregate, EventStore append persists the events to an append-only stream under optimistic concurrency, and a later load folds the stream back into the current state"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="150.0" y="24.0" text-anchor="middle" font-size="11.5" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">write path</text>
<rect x="50.0" y="38.5" width="200.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="36.0" width="200.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="150.0" y="59.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Command</text><text x="150.0" y="73.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Deposit { amount }</text><line x1="150.0" y1="88.0" x2="150.0" y2="102.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="150.0,110.0 145.5,102.0 154.5,102.0" fill="#b5531f"/><rect x="50.0" y="112.5" width="200.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="110.0" width="200.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="150.0" y="133.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">raise(event)</text><text x="150.0" y="147.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">→ uncommitted []</text><line x1="150.0" y1="162.0" x2="150.0" y2="176.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="150.0,184.0 145.5,176.0 154.5,176.0" fill="#b5531f"/><rect x="50.0" y="186.5" width="200.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="184.0" width="200.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="150.0" y="207.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">append(events)</text><text x="150.0" y="221.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">optimistic concurrency</text>
<text x="420.0" y="24.0" text-anchor="middle" font-size="11.5" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">event stream (append-only)</text>
<rect x="330.0" y="46.5" width="180.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="330.0" y="44.0" width="180.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="420.0" y="66.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">+100</text><text x="420.0" y="80.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">WalletOpened</text>
<line x1="420.0" y1="94.0" x2="420.0" y2="106.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="420.0,114.0 415.5,106.0 424.5,106.0" fill="#b5531f"/>
<rect x="330.0" y="116.5" width="180.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="330.0" y="114.0" width="180.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="420.0" y="136.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">+50</text><text x="420.0" y="150.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">MoneyDeposited</text>
<line x1="420.0" y1="164.0" x2="420.0" y2="176.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="420.0,184.0 415.5,176.0 424.5,176.0" fill="#b5531f"/>
<rect x="330.0" y="186.5" width="180.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="330.0" y="184.0" width="180.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="420.0" y="206.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">−30</text><text x="420.0" y="220.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">MoneyWithdrawn</text>
<line x1="250.0" y1="198.0" x2="324.6" y2="115.9" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="330.0,110.0 327.9,118.9 321.3,112.9" fill="#b5531f"/><text x="290.0" y="150.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">append</text>
<line x1="330.0" y1="244.0" x2="257.1" y2="282.3" stroke="#1f8a4c" stroke-width="3.0" stroke-linecap="round"/><polygon points="250.0,286.0 255.0,278.3 259.2,286.3" fill="#1f8a4c"/><text x="290.0" y="279.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">fold / replay</text>
<rect x="50.0" y="266.5" width="200.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="50.0" y="264.0" width="200.0" height="46.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="150.0" y="284.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">current state</text><text x="150.0" y="298.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">balance = 120</text>
</svg>
<figcaption>Tres movimientos. Un comando hace <code>raise</code> de un evento sobre el agregado; <code>EventStore::append</code> persiste los eventos no confirmados bajo concurrencia optimista; una carga posterior hace <code>fold</code> de todo el stream de solo anexado de vuelta al estado actual — los eventos son la fuente de verdad, el estado es derivado.</figcaption>
</figure>

La pieza del framework que impulsa los tres movimientos es `firefly-eventsourcing`,
reexportada a través de la fachada como `firefly::eventsourcing`.

> **Note** **Término clave — `firefly-eventsourcing`.** El crate de event sourcing
> del framework. Proporciona el `AggregateRoot` (búfer de eventos no confirmados +
> versión), el puerto `EventStore` (append/load con concurrencia optimista),
> snapshots, proyecciones, un stream global entre agregados, un transactional
> outbox, upcasters y multi-tenancy. No dependes de él directamente — llega a
> través de la única fachada `firefly`, y los dos derives (`DomainEvent`,
> `AggregateRoot`) entran a través de `firefly::prelude`.

## Paso 3 — Define los eventos de dominio del Wallet

La acción: declarar los tres eventos que el monedero puede producir. En Lumen cada
uno es una struct de payload simple que lleva `#[derive(DomainEvent)]`. Viven en
`src/domain.rs`.

```rust
use firefly::eventsourcing::{AggregateRoot, DomainEvent};
use firefly::prelude::*;
use serde::{Deserialize, Serialize};

/// Payload of the event raised when a wallet is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct WalletOpened {
    pub wallet_id: String,
    pub owner: String,
    /// The opening balance, in minor units (cents).
    pub opening_balance: i64,
}

/// Payload of the event raised when money is credited to a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyDeposited {
    pub wallet_id: String,
    pub amount: i64,
}

/// Payload of the event raised when money is debited from a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyWithdrawn {
    pub wallet_id: String,
    pub amount: i64,
}
```

Lo que acaba de ocurrir, bloque a bloque:

- Los dos **tipos** `AggregateRoot` y `DomainEvent` provienen de
  `firefly::eventsourcing`. Las dos **macros derive** del mismo nombre provienen de
  `firefly::prelude::*` — el glob que reexporta todas las macros del framework, de
  modo que un servicio depende de un solo crate y aun así escribe
  `#[derive(DomainEvent)]`.
- `Serialize`/`Deserialize` hacen que cada payload sea codificable en JSON; el
  derive necesita `Serialize` porque codifica en JSON el payload dentro del evento
  almacenado.
- Cada evento se nombra en **pasado** y lleva solo los datos que el hecho necesita.
  `opening_balance` y `amount` están en unidades menores (céntimos) — Lumen nunca
  almacena dinero en coma flotante.

Ahora la parte importante — qué genera `#[derive(DomainEvent)]`. Para cada struct
produce:

- una `pub const EVENT_TYPE: &'static str` igual al nombre de la struct
  (`"WalletOpened"`, `"MoneyDeposited"`, `"MoneyWithdrawn"`) — el discriminador de
  enrutamiento;
- un accesor `event_type()` que devuelve esa const;
- un método `to_domain_event(aggregate_id, aggregate_type, version)` que codifica
  en JSON el payload dentro de un `DomainEvent` del framework.

Esa const `EVENT_TYPE` generada es lo *único* que referencian el agregado y su
fold, de modo que el tipo de evento nunca es un literal de cadena suelto en los
puntos de llamada — y un renombrado de la struct se propaga automáticamente.

> **Note** **Término clave — `DomainEvent` (el tipo de cable).** Junto al derive
> hay una struct concreta `firefly::eventsourcing::DomainEvent`: la forma de cable
> de cada evento persistido, con un `aggregate_id`, `aggregate_type`, `version`
> en base 1, `event_type`, `time`, un `payload` en base64, un `metadata` opcional y
> un `tenant_id` opcional. Su JSON es un contrato estable, versionado y neutral
> respecto al lenguaje — compatible byte a byte con los ports de Java, .NET, Go y
> Python, de modo que cualquier servicio que lo respete interopera con
> independencia del lenguaje.

> **Tip** **Punto de control.** `cargo build` compila las tres structs. En un test
> rápido puedes afirmar `WalletOpened::EVENT_TYPE == "WalletOpened"` y hacer un
> round-trip de un payload con `serde_json::to_vec` / `from_slice`. Los eventos
> existen; todavía nada los levanta.

## Paso 4 — Define el agregado Wallet

La acción: declarar el agregado que produce esos eventos. El `Wallet` lleva
`#[derive(AggregateRoot)]`, que encuentra el campo `AggregateRoot` del framework
embebido y conecta el discriminador de tipo y los accesores. De forma crucial, el
estado proyectado (`owner`, `balance`, `opened`) **no se almacena** — se pliega a
partir del stream.

```rust
use firefly::eventsourcing::{AggregateRoot, DomainEvent};

use crate::money::Money;

/// The aggregate-type discriminator stamped onto every event a wallet raises.
pub const AGGREGATE_TYPE: &str = "Wallet";

#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    /// The framework aggregate root — uncommitted-event buffer + version.
    pub root: AggregateRoot,
    pub owner: String,
    /// Folded from the stream; never stored.
    pub balance: Money,
    /// Whether the wallet has been opened (an empty stream is "absent").
    pub opened: bool,
}
```

Lo que acaba de ocurrir:

- El campo embebido `root: AggregateRoot` es la contabilidad interna del framework
  — contiene el id del agregado, la versión actual y el búfer de eventos *no
  confirmados* que los comandos levantan pero que el store aún no ha persistido.
  Rust compone este campo en lugar de heredar de una clase base.
- `#[derive(AggregateRoot)]` localiza ese campo `root` (el nombre de campo por
  defecto; se anula con `#[firefly(field = "...")]`) y genera una const
  `Wallet::AGGREGATE_TYPE` más los accesores `aggregate()` / `aggregate_mut()`
  sobre la raíz embebida. `#[firefly(aggregate_type = "Wallet")]` fija el
  discriminador explícitamente (de todos modos tomaría por defecto el nombre de la
  struct).
- `owner`, `balance` y `opened` son **campos proyectados**: existen solo en memoria
  y se reconstruyen plegando el stream. `Money` es el value object de Lumen basado
  en céntimos de `src/money.rs`.

> **Note** **Término clave — eventos no confirmados (uncommitted events).** Eventos
> que un comando ha levantado (`raise`) sobre la raíz del agregado pero que el
> store aún no ha persistido. Viven en el búfer de la raíz hasta que los
> `take_uncommitted()` y se los entregas a `EventStore::append`. Piensa en ellos
> como la escritura pendiente del agregado.

> **Tip** **Punto de control.** `cargo build` tiene éxito y `Wallet::AGGREGATE_TYPE`
> evalúa a `"Wallet"`. El agregado está declarado pero todavía no tiene
> comportamiento — el Paso 5 añade los comandos.

## Paso 5 — Escribe un comando: validar, raise, apply

La acción: dar comportamiento al monedero. Cada comando sigue la forma canónica de
event sourcing — validar la invariante, hacer `raise` del evento correspondiente
sobre la raíz embebida, y luego aplicarlo al estado en memoria. Aquí está
`deposit`, más el pequeño helper privado que serializa un payload y lo levanta.

```rust,ignore
/// Credits `amount` to the wallet, raising a `MoneyDeposited` event.
pub fn deposit(&mut self, amount: Money) -> Result<(), DomainError> {
    self.require_opened()?;
    let amount = amount.require_positive()?;
    self.raise(
        MoneyDeposited::EVENT_TYPE,
        &MoneyDeposited {
            wallet_id: self.root.id.clone(),
            amount: amount.cents_value(),
        },
    );
    self.balance = self.balance.add(amount);
    Ok(())
}

/// Serialises a `#[derive(DomainEvent)]` payload and raises it onto the embedded
/// root under `event_type` — the discriminator from the generated `EVENT_TYPE`.
fn raise<P: Serialize>(&mut self, event_type: &str, payload: &P) {
    let bytes = serde_json::to_vec(payload).expect("domain event payload serialises");
    self.root.raise(event_type, bytes);
}
```

Lo que acaba de ocurrir, en orden:

1. `require_opened()?` impone la invariante: no puedes ingresar en un monedero que
   nunca se abrió. Una comprobación fallida devuelve `DomainError::NotFound` y **no**
   levanta evento alguno.
2. `amount.require_positive()?` rechaza un ingreso no positivo antes de que se
   registre ningún evento.
3. `self.raise(MoneyDeposited::EVENT_TYPE, …)` registra el hecho. Observa que el
   tipo de evento es la const generada, nunca un literal de cadena. El helper
   privado `raise` codifica el payload en JSON y llama a
   `self.root.raise(event_type, bytes)`.
4. `self.balance = self.balance.add(amount)` actualiza la proyección en memoria.

El `AggregateRoot::raise` del framework hace dos cosas: empuja el evento al búfer
de no confirmados (para que el ledger pueda persistirlo más tarde) e incrementa la
versión en uno. Ese incremento de versión es lo que después impulsa la concurrencia
optimista.

`withdraw` tiene la misma forma con una guarda extra que merece verse, porque la
saga de transferencia de [Sagas, Workflows & TCC](./12-sagas.md) depende de ella:

```rust,ignore
/// Debits `amount` from the wallet, raising a `MoneyWithdrawn` event.
pub fn withdraw(&mut self, amount: Money) -> Result<(), DomainError> {
    self.require_opened()?;
    let amount = amount.require_positive()?;
    let remaining = self.balance.subtract(amount)?; // Overdraw → InsufficientFunds
    self.raise(
        MoneyWithdrawn::EVENT_TYPE,
        &MoneyWithdrawn {
            wallet_id: self.root.id.clone(),
            amount: amount.cents_value(),
        },
    );
    self.balance = remaining;
    Ok(())
}
```

Por qué importa: `Money::subtract` se calcula *primero* y rechaza un descubierto
con `MoneyError::Overdraw` (mapeado a `DomainError::InsufficientFunds`) **antes** de
que se alcance siquiera `raise`. Una retirada fallida, por tanto, no levanta evento
alguno, dejando el stream limpio. Esa guarda de descubierto es el disparador de
fallo en el que se apoya la saga de transferencia.

> **Tip** **Punto de control.** Con `open`, `deposit` y `withdraw` escritos, un test
> unitario puede abrir un monedero, ingresar 50, retirar 30, luego llamar a
> `wallet.take_uncommitted()` y afirmar que contiene exactamente tres eventos en
> orden. Los propios tests de `domain.rs` de Lumen hacen precisamente esto.

## Paso 6 — Rehidrata: pliega el stream de vuelta al estado

La acción: reconstruir un monedero a partir de sus eventos. La **rehidratación** es
la ruta de carga — reproduce el stream ordenado completo a través del mismo `apply`
que usan los comandos. Un stream vacío produce un monedero *sin abrir*, que es como
el ledger distingue «ausente» de «existe».

> **Note** **Término clave — rehidratación.** Reconstruir el estado actual de un
> agregado plegando su stream de eventos desde el principio. El análogo en
> Spring/Axon es el `load` de un repositorio basado en eventos, que reproduce los
> eventos del agregado dentro de una instancia nueva.

```rust,ignore
/// Rebuilds a wallet by folding `events` (its full ordered stream).
pub fn rehydrate(id: &str, events: &[DomainEvent]) -> Self {
    let mut wallet = Wallet {
        root: AggregateRoot::new(id, AGGREGATE_TYPE),
        owner: String::new(),
        balance: Money::ZERO,
        opened: false,
    };
    for event in events {
        wallet.apply(event);
        // Keep the root version in lock-step with the stream head so a
        // subsequent command appends at the right expected version.
        wallet.root.version = event.version;
    }
    wallet
}

/// Folds one persisted event into the projected state.
fn apply(&mut self, event: &DomainEvent) {
    match event.event_type.as_str() {
        WalletOpened::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<WalletOpened>(&event.payload) {
                self.owner = p.owner;
                self.balance = Money::cents(p.opening_balance);
                self.opened = true;
            }
        }
        MoneyDeposited::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<MoneyDeposited>(&event.payload) {
                self.balance = self.balance.add(Money::cents(p.amount));
            }
        }
        MoneyWithdrawn::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<MoneyWithdrawn>(&event.payload) {
                self.balance = Money::cents(self.balance.cents_value() - p.amount);
            }
        }
        _ => {}
    }
}
```

Lo que acaba de ocurrir:

- `rehydrate` parte de un monedero en blanco (`opened: false`, saldo cero) y pliega
  cada evento a través de `apply`, manteniendo `root.version` al paso de la cabeza
  del stream. Tras el fold, `root.version` es igual a la versión del último evento
  — que es exactamente el token contra el que el siguiente comando hará append.
- `apply` hace match sobre `event.event_type` contra las constantes `EVENT_TYPE`
  generadas — las mismas constantes bajo las que los comandos hacen `raise` — de
  modo que el fold de escritura y el fold de replay nunca pueden discrepar sobre el
  nombre de un evento.

Una sutileza que merece una pausa. `apply` pliega `MoneyWithdrawn` con una resta
*cruda* (`self.balance.cents_value() - p.amount`) en lugar del `Money::subtract`
guardado contra descubierto que usa el *comando* `withdraw`. Esa asimetría es
deliberada: **el replay nunca revalida**. La guarda ya se ejecutó en tiempo de
escritura, y una retirada fallida no levantó evento alguno, de modo que cada evento
del stream es un hecho que ya pasó su invariante. El replay simplemente lo aplica.

> **Design note.** Esta es la garantía de corrección de event sourcing hecha
> concreta. Un comando hace `raise` de un evento y `apply` muta los campos
> proyectados; una carga reproduce el *mismo* `apply` para reconstruir el estado.
> Lumen no registra ninguna tabla de handlers — hace `match` sobre la const
> `EVENT_TYPE` generada, la forma idiomática de Rust de impedir que el fold de
> escritura y el fold de replay discrepen jamás sobre el nombre de un evento.

> **Tip** **Punto de control.** Esta es la ley que hay que demostrar: open +
> deposit + withdraw sobre un monedero *escritor*, toma su stream no confirmado,
> luego `Wallet::rehydrate` un monedero nuevo a partir de ese stream y afirma que
> el saldo, el propietario y la versión reconstruidos coinciden — estado
> recalculado a partir de eventos, nunca almacenado. El test
> `rehydrate_folds_the_full_stream` de Lumen hace exactamente esto.

## Paso 7 — Persiste y recarga a través del `EventStore`

La acción: hacer que los eventos sean duraderos. El `AggregateRoot` del framework
acumula `DomainEvent`s a medida que los haces `raise`; los `take_uncommitted` y
los `append` a un `EventStore`. El store impone concurrencia optimista — le pasas
la versión que cargaste, y el append de un escritor concurrente falla.

Aquí está el movimiento aislado, contra el store en proceso:

```rust
use firefly::eventsourcing::{AggregateRoot, EventStore, MemoryEventStore};

#[tokio::main]
async fn main() {
    let store = MemoryEventStore::new();

    let mut user = AggregateRoot::new("u1", "User");
    user.raise("UserCreated", br#"{"name":"alice"}"#);
    user.raise("UserRenamed", br#"{"name":"bob"}"#);

    let events = user.take_uncommitted();
    // expected_version 0 -> this is a brand-new aggregate.
    if let Err(err) = store.append(&user.id, 0, events).await {
        eprintln!("append failed (raced): {err}");
    }

    assert_eq!(store.load("u1").await.unwrap().len(), 2);
}
```

Lo que acaba de ocurrir: dos llamadas a `raise` almacenan en búfer dos eventos e
incrementan la raíz a la versión 2. `take_uncommitted()` vacía el búfer (una fusión
idiomática de Rust de «devolver los eventos» + «borrarlos»). `append(&id, 0, events)`
los persiste, donde `0` es la **versión esperada** — la cabeza que esperábamos
encontrar antes de escribir. Como el agregado es completamente nuevo, esa cabeza es
`0`; el append tiene éxito. Releer el stream devuelve ambos eventos en orden.

El puerto `EventStore` — el contrato que implementa cada store:

```rust,ignore
#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, aggregate_id: &str, expected_version: i64,
                    events: Vec<DomainEvent>) -> Result<(), EventSourcingError>;
    async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError>;
    async fn load_after(&self, aggregate_id: &str, since_version: i64)
        -> Result<Vec<DomainEvent>, EventSourcingError>;
    async fn stream_all(&self, after_event_id: Option<&str>, limit: usize, tenant: Option<&str>)
        -> Result<Vec<StreamedEvent>, EventSourcingError>;
}
```

> **Note** **Término clave — puerto `EventStore` / adaptador `MemoryEventStore`.**
> El trait `EventStore` es la frontera de persistencia — un *puerto* en el sentido
> hexagonal. `MemoryEventStore` es el *adaptador* en proceso sobre el que Lumen se
> ejecuta por defecto, ideal para desarrollo y tests. `SqlEventStore::new(db)` es el
> adaptador de producción sobre el puerto `Database` de `firefly-transactional`.
> Intercambiarlos es un cambio de una línea en el `#[bean]` `event_store` de
> `LumenBeans` — exactamente como intercambiar el broker en el
> [capítulo anterior](./10-eda-messaging.md).

Ese bean es el único sitio donde reside la elección:

```rust,ignore
#[bean]
impl LumenBeans {
    /// The in-memory event store (`@Bean`).
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }
    // ...
}
```

> **Tip** **Punto de control.** Ejecuta el ejemplo anterior (o el `#[tokio::test]`
> equivalente). `store.load("u1")` devuelve un `Vec` de longitud 2. Si en su lugar
> llamas a `store.append(&user.id, 5, events)` para un agregado nuevo, obtienes
> `Err(EventSourcingError::Concurrency)` — prueba de que la comprobación de versión
> esperada está activa.

## Paso 8 — Conéctalo al Ledger y maneja la concurrencia

La acción: ligar la persistencia al dominio en un único servicio de aplicación. El
`Ledger` de Lumen (introducido en el [capítulo anterior](./10-eda-messaging.md))
posee el store y el broker. Cada comando rehidrata, ejecuta el método de dominio, y
confirma con concurrencia optimista. Aquí están `deposit` y la ruta de carga:

```rust,ignore
/// Credits `amount` to `wallet_id`, persisting + publishing `MoneyDeposited`.
pub async fn deposit(&self, wallet_id: &str, amount: Money) -> Result<WalletView, DomainError> {
    let mut wallet = self.load(wallet_id).await?;
    let expected = wallet.root.version;
    wallet.deposit(amount)?;
    self.commit(&mut wallet, expected).await?;
    Ok(wallet.view())
}

/// Rehydrates the aggregate from its persisted stream.
async fn load(&self, wallet_id: &str) -> Result<Wallet, DomainError> {
    let events = self.load_events(wallet_id).await?;
    Ok(Wallet::rehydrate(wallet_id, &events))
}

/// Loads the full event stream, mapping an absent aggregate to a domain 404.
pub async fn load_events(&self, wallet_id: &str) -> Result<Vec<DomainEvent>, DomainError> {
    match self.store.load(wallet_id).await {
        Ok(events) => Ok(events),
        Err(EventSourcingError::AggregateNotFound) => {
            Err(DomainError::NotFound(wallet_id.to_string()))
        }
        Err(e) => Err(DomainError::NotFound(format!("{wallet_id}: {e}"))),
    }
}
```

Lo que acaba de ocurrir: `deposit` carga el monedero (rehidratándolo a partir de su
stream), captura `wallet.root.version` como `expected`, ejecuta el comando de
dominio, y luego confirma en `expected`. La versión a la que rehidrató el monedero
**es** el token que el append debe igualar. `commit` (mostrado por completo en el
[capítulo anterior](./10-eda-messaging.md)) hace append en `expected`, y luego
publica cada evento anexado al broker para que la proyección pueda reaccionar. Los
dos capítulos se encuentran aquí: este aporta el store duradero y reproducible; el
otro lleva cada evento anexado al cable.

Ahora el caso de concurrencia, porque en un sistema real dos escritores compiten.
Supón que un ingreso desde la aplicación y una retirada de comisión desde un job
cargan ambos el monedero `wlt_a1` en la versión 3, cada uno aplica un cambio, y cada
uno intenta hacer append en `expected_version = 3`. El primer append gana y el
stream avanza a 4; el segundo ahora no coincide, y el store devuelve
`EventSourcingError::Concurrency`. Lumen mapea eso a un `DomainError::NotFound` que
lleva un detalle de «modificación concurrente» para que el llamante reintente desde
una carga fresca. Nunca gestionas números de versión a mano — la versión a la que
rehidrató el monedero es el token, y el store lo impone.

> **Note** `append(id, expected_version, events)` impone concurrencia optimista: la
> versión rehidratada es el token, y un append obsoleto falla con
> `EventSourcingError::Concurrency`. Atrápalo y reintenta el ciclo
> cargar-mutar-guardar (o expón un 409) — nunca lo tragues, o te arriesgas a perder
> una escritura.

> **Tip** **Punto de control.** Haz append del evento de apertura de un monedero en
> `expected_version = 0`. Luego, *sin recargar*, levanta un segundo evento y hazle
> append *también* en `expected_version = 0`. El segundo append devuelve
> `EventSourcingError::Concurrency`. Una carga fresca (que avanza `expected` a 1)
> habría tenido éxito — ese es todo el mecanismo en cuatro líneas.

## Paso 9 — La ruta más fina: agregados tipados y el repositorio

Lumen pliega el stream a mano en `Wallet::apply` porque enseña la mecánica con
claridad. Para agregados más grandes, el framework ofrece una ruta más fina:
implementar `EventSourcedAggregate` — un `apply_event` tipado más serialización de
snapshot opcional — y dejar que `EventSourcedRepository` ate `load` (snapshot +
replay) y `save` (append + política de snapshot) juntos.

```rust,ignore
use firefly_eventsourcing::{
    AggregateRoot, DomainEvent, EventSourcedAggregate, EventSourcedRepository,
    EventSourcingError, MemoryEventStore,
};
use std::sync::Arc;

#[derive(Default)]
struct Wallet { root: AggregateRoot, balance: i64 }

impl EventSourcedAggregate for Wallet {
    const AGGREGATE_TYPE: &'static str = "Wallet";
    fn root(&self) -> &AggregateRoot { &self.root }
    fn root_mut(&mut self) -> &mut AggregateRoot { &mut self.root }
    fn apply_event(&mut self, event: &DomainEvent) -> Result<(), EventSourcingError> {
        if event.event_type == "Credited" {
            let amount: i64 = serde_json::from_slice(&event.payload)
                .map_err(|e| EventSourcingError::Projection(e.to_string()))?;
            self.balance += amount;
        }
        Ok(())
    }
}

# async fn ex() -> Result<(), EventSourcingError> {
let repo = EventSourcedRepository::<Wallet>::new(Arc::new(MemoryEventStore::new()));

let mut w = Wallet::default();
w.root_mut().raise("Credited", b"500");
repo.save(&mut w).await?;                     // append uncommitted

let reloaded = repo.load(&w.root.id).await?;  // snapshot + replay
assert!(reloaded.is_some());
# Ok(())
# }
```

Lo que acaba de ocurrir: `EventSourcedAggregate` es el contrato del trait — expone
la raíz embebida vía `root()` / `root_mut()` y el fold del lado de lectura vía
`apply_event`. El repositorio entonces orquesta el pegamento que de otro modo cada
servicio basado en eventos escribe a mano: `save` calcula la versión esperada a
partir del lote no confirmado y hace append con concurrencia optimista; `load`
devuelve `Ok(Some(_))` cuando el agregado tiene eventos y `Ok(None)` cuando nunca se
persistió. Un evento sin handler debería devolver `EventSourcingError::Projection`
para que la reconstrucción falle ruidosamente en lugar de corromper el estado en
silencio.

`EventSourcedRepository::with_snapshots(store, snapshots, interval)` habilita
capturas de estado periódicas para que la rehidratación no reproduzca toda la
historia — que es el siguiente paso.

> **Tip** **Punto de control.** Puedes articular cuándo es correcta cada ruta:
> plegar a mano (`Wallet::apply`) cuando el agregado es pequeño y quieres la
> mecánica a la vista; `EventSourcedRepository` cuando quieres que se maneje por ti
> la orquestación de load/save/snapshot. Ambas terminan en el mismo `EventStore`.

## Paso 10 — Snapshots: acotar el coste del replay

Event sourcing cambia simplicidad de escritura por coste de lectura: un monedero
con 10.000 movimientos reproduce 10.000 eventos en cada carga. Los **snapshots**
recortan eso.

> **Note** **Término clave — snapshot.** Un punto de control serializado del estado
> de un agregado en una versión concreta. En la carga, el repositorio
> deserializa el snapshot más reciente y reproduce solo los eventos *posteriores* a
> él — convirtiendo un replay de 10.000 eventos en uno de 1.000 si el snapshot está
> en la versión 9.000. El análogo en Axon es su disparador de snapshots.

Los monederos de Lumen son lo bastante efímeros como para que el replay completo
del store en memoria esté bien, así que el sample no conecta snapshots — pero la
costura es una sola llamada al constructor:

```rust,ignore
use firefly_eventsourcing::{EventSourcedRepository, MemorySnapshotStore};

// Checkpoint each time a wallet's stream crosses a 100-event boundary.
let repo = EventSourcedRepository::<Wallet>::with_snapshots(
    store,
    Arc::new(MemorySnapshotStore::new()),
    100,
);
```

Lo que acaba de ocurrir: `with_snapshots(store, snapshots, interval)` hace un punto
de control del estado del agregado cada vez que un stream *cruza* una frontera de
intervalo. El disparador es un cruce, no una divisibilidad exacta, de modo que un
lote que se sitúa a horcajadas del umbral (versión 95 → 105) aun así hace snapshot.
En la carga, el repositorio restaura el snapshot más reciente y reproduce solo los
eventos posteriores a él.

> **Design note.** Los snapshots son una optimización, nunca un requisito de
> corrección. Elimínalos y el sistema es más lento pero sigue siendo correcto — los
> eventos siguen siendo la fuente de verdad, y el snapshot es solo un fold cacheado
> del prefijo.

## Paso 11 — Proyecciones, el stream global y el outbox

Estas tres costuras son la forma en que event sourcing alimenta al resto de un
sistema. No conectarás todas ellas en la base de enseñanza, pero conocer la forma
de cada una es parte de comprender el modelo.

### Proyecciones — construir modelos de lectura a partir de la historia

> **Note** **Término clave — proyección.** Un handler del lado de lectura que
> consume eventos para construir un modelo de lectura optimizado para consultas.
> Debe ser **idempotente**, porque los eventos pueden reproducirse durante la
> recuperación. El análogo en Spring es un `@EventListener` del lado de consulta
> que actualiza una tabla de lectura.

Una `Projection` se registra en un `ProjectionRunner`, que puede reproducir los
eventos de un agregado a través de ella. Este es el hermano del *event store* del
oyente del *event bus* del [capítulo anterior](./10-eda-messaging.md): la
`WalletProjection` viva de Lumen reacciona a los eventos a medida que se publican,
mientras que un `ProjectionRunner` puede reproducir la historia desde el principio
para reconstruir un modelo de lectura desde cero.

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{FunctionProjection, ProjectionRunner};

let runner = ProjectionRunner::new();
runner.register(Arc::new(FunctionProjection::new("balances", |event| async move {
    // update a read-model row from the event ...
    Ok(())
})));

runner.replay(&store, "wlt_a1").await?;  // replay one aggregate's stream
```

Esta reconstruibilidad es exclusiva de event sourcing. Si el modelo de lectura de
Lumen alguna vez se pierde o cambia su esquema, detienes el proyector, limpias el
modelo de lectura, y reproduces cada stream — la historia está ahí mismo en el
store. Un modelo de almacenamiento de estado no puede hacer esto; descartó la
historia en tiempo de escritura.

### El stream global — modelos de lectura entre agregados

`EventStore::stream_all` expone el stream global, entre agregados y ordenado de
eventos con un cursor reanudable — el motor de los modelos de lectura que abarcan
muchos agregados (piensa en «todos los movimientos de todos los monederos, en
orden»). El runner lo consume por lotes, al menos una vez (at-least-once) y en
orden:

```rust,ignore
// Drive one batch; returns the next cursor + any per-event error.
let (next_cursor, err) = runner
    .drive_once(&store, None, 100, None)
    .await?;

// Or replay the whole global stream from a start cursor.
let cursor = runner.replay_all(&store, None, 100, None).await?;
```

Lo que acaba de ocurrir: `drive_once` aplica una página y devuelve el cursor desde
el que reanudar, avanzándolo solo más allá de los eventos aplicados con *éxito* — de
modo que un evento fallido se reintenta en la siguiente llamada en lugar de
saltarse. `replay_all` vacía el stream global completo desde un cursor de inicio,
paginando `batch_size` cada vez.

### El transactional outbox — cerrar la brecha entre append y publish

El [capítulo anterior](./10-eda-messaging.md) señaló una brecha en `Ledger::commit`:
hace append, luego publica, y un fallo *entre* ambos persiste el hecho pero pierde
la difusión. `TransactionalOutbox` cierra esa brecha.

> **Note** **Término clave — transactional outbox.** Un patrón en el que un escritor
> *encola* (`enqueue`) un evento de forma duradera (idealmente en la misma
> transacción de store que el append) en lugar de publicarlo directamente, y un
> relay en segundo plano reenvía cada registro pendiente a un broker, reintentando
> ante fallo. Registrar el evento de forma duradera *antes* de despacharlo es lo que
> garantiza la entrega al menos una vez frente a fallos. Este es el mismo patrón de
> outbox que los equipos de Spring implementan en torno a su message broker.

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EdaSink, TransactionalOutbox};

let outbox = TransactionalOutbox::new(Arc::new(EdaSink::new(
    broker,           // the Arc<dyn firefly_eda::Publisher>
    "wallet.events",  // destination topic
    "lumen",          // logical source stamped onto every Event::source
)))
.with_max_attempts(5);

outbox.enqueue(some_event).await;       // a writer enqueues
outbox.start().await;                   // background relay forwards + retries
// ... later
let dead = outbox.dead_letters().await; // exhausted records, for inspection
outbox.stop().await;
```

Lo que acaba de ocurrir: un escritor hace `enqueue` de un `DomainEvent`; el relay
(arrancado con `start()`) sondea y reenvía cada registro pendiente a un
`OutboxSink`, reintentando hasta `max_attempts`. El `EdaSink` por defecto tiende un
puente de cada `DomainEvent` a un `firefly_eda::Event` y lo publica — duradero esta
vez. Los registros que agotan `max_attempts` se convierten en **dead letters**:
excluidos del bucle de publicación y expuestos vía `dead_letters()` para inspección
o reintento manual. Esta es la ruta de actualización a producción — y exactamente
por qué la proyección se construyó para ser **idempotente** en el capítulo anterior:
la entrega al menos una vez significa que un evento puede llegar dos veces.

## Paso 12 — Evolución de esquema y multi-tenancy

Dos costuras más completan el modelo. Ambas operan en la ruta de lectura, de modo
que la historia almacenada permanece inmutable.

### Upcasters — migrar eventos antiguos en la lectura

> **Note** **Término clave — upcaster.** Una transformación aplicada a un evento
> almacenado cuando se *lee*, migrándolo de un esquema antiguo al actual. Los
> consumidores siempre observan eventos del esquema actual; la historia almacenada
> nunca se reescribe. Esta es la respuesta de event sourcing a la migración de
> esquema.

Supón que Lumen necesita más adelante un campo `reference` en cada ingreso para
conciliación: los eventos nuevos lo llevan, los eventos `MoneyDeposited` antiguos no,
y un upcaster cubre el hueco en la carga:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EventUpcaster, MemoryEventStore};

let store = MemoryEventStore::with_upcasters(vec![Arc::new(MyUpcaster)]);
// every event returned by load / load_after passes through applicable upcasters
```

Un `EventUpcaster` implementa `applies_to(&event) -> bool` y
`upcast(event) -> DomainEvent`. Los datos antiguos se vuelven legibles sin una
migración; los datos nuevos se escriben en el esquema actual; los eventos en sí
permanecen inmutables. Nunca reescribes la historia.

### Multi-tenancy — un store, muchos tenants

Un `DomainEvent::tenant_id` opcional (estampado desde `AggregateRoot::with_tenant`,
persistido y filtrable, omitido del JSON cuando es `None`) se enhebra a través de
`append` / `load` / `stream_all`. Un único store sirve a muchos tenants con
aislamiento por tenant en el stream global — la ruta que tomaría un despliegue de
Lumen multibanco para mantener separados los streams de monedero de cada tenant.
Como el campo se omite del JSON cuando es `None`, un Lumen de un solo tenant
serializa byte a byte de forma idéntica al formato de cable entre lenguajes.

> **Tip** **Punto de control.** Puedes nombrar, para cada costura, qué te cuesta si
> *no* la usas: sin snapshots → cargas más lentas; sin outbox → un fallo puede
> perder una publicación; sin upcaster → los eventos antiguos se vuelven ilegibles
> tras un cambio de esquema; sin tenant id → necesitas un store por tenant. Ninguna
> de ellas cambia la fuente de verdad — todas son preocupaciones de ruta de lectura
> o de entrega superpuestas sobre el mismo stream inmutable.

## Resumen — qué cambió en Lumen

El saldo del monedero ya no es un valor almacenado — es un *cálculo* sobre un stream
inmutable, y el stream es el sistema de registro.

| Pieza | Rol |
|-------|------|
| `#[derive(DomainEvent)]` | Genera `EVENT_TYPE` + `event_type()` + `to_domain_event(...)` para cada struct de payload |
| `#[derive(AggregateRoot)]` | Genera `AGGREGATE_TYPE` + `aggregate()` / `aggregate_mut()` sobre el `root` embebido |
| Comando de `Wallet` (`deposit` / `withdraw`) | Valida la invariante, hace `raise` del evento, aplica al estado |
| `Wallet::apply` / `rehydrate` | El mismo fold se ejecuta en escritura y en replay — un stream vacío es «sin abrir» |
| `EventStore` / `MemoryEventStore` | El log de solo anexado; `SqlEventStore` para producción |
| `append(id, expected_version, …)` | Concurrencia optimista — la versión rehidratada es el token |
| `EventSourcedRepository` | Ata load (snapshot + replay) y save (append + política de snapshot) juntos |
| `ProjectionRunner` | Reconstruye modelos de lectura a partir de la historia (el hermano del lado del store del oyente de EDA) |
| `TransactionalOutbox` | Cierra la brecha entre append y publish con relay al menos una vez |
| `EventUpcaster` / `tenant_id` | Evolución de esquema en la lectura; aislamiento por tenant sobre un único store |

Tres ideas se llevan adelante:

- **Los eventos son la verdad.** No hay columna de saldo que pueda desviarse; el
  saldo se pliega a partir del stream en cada carga.
- **Escritura y replay comparten un fold.** `apply` se ejecuta de la misma manera
  ya sea que un comando acabe de levantar el evento o que una carga esté
  reconstruyendo a partir de la historia — y el replay nunca revalida, porque cada
  evento almacenado ya pasó su invariante. Esa simetría es la garantía de
  corrección.
- **Depende del puerto `EventStore`.** El store en memoria se convierte en SQL con
  un intercambio de bean de una línea, igual que el broker se convirtió en Kafka —
  el dominio nunca cambia.

Cuando un proceso de negocio abarca múltiples agregados y necesita compensación —
mover dinero de un monedero a otro, atómicamente — plegar un único stream ya no
basta. Ese es el siguiente capítulo.

## Ejercicios

1. **Reproduce hasta un punto en el tiempo.** Abre un monedero y haz tres ingresos.
   Carga el stream crudo con `ledger.load_events(&id)`, toma solo los eventos con
   `version <= 2`, y `Wallet::rehydrate` un monedero nuevo a partir de esa porción.
   Afirma que el saldo es igual a apertura + primer ingreso solamente — la «consulta
   de viaje en el tiempo» que un modelo de almacenamiento de estado no puede
   responder.

2. **Demuestra que la guarda de descubierto no levanta evento.** Abre un monedero
   con 100 céntimos, intenta `withdraw` de 101, y afirma que da error con
   `DomainError::InsufficientFunds`. Luego llama a `wallet.root.uncommitted()` y
   afirma que el búfer sigue conteniendo exactamente un evento (el `WalletOpened`) —
   el comando fallido dejó el stream limpio.

3. **Fuerza un conflicto de concurrencia optimista.** Haz append del evento de
   apertura de un monedero en `expected_version = 0`. Luego, sin recargar, levanta
   un segundo evento y hazle append *también* en `expected_version = 0`. Afirma que
   el segundo append devuelve `EventSourcingError::Concurrency`, y explica por qué
   una carga fresca (que avanza `expected` a 1) habría tenido éxito.

4. **Añade una reconstrucción con `ProjectionRunner`.** Registra una
   `FunctionProjection` que cuente el número de eventos `MoneyDeposited` por
   monedero en un mapa en memoria, `replay` el stream de un monedero a través de
   ella, y afirma el conteo. Luego limpia el mapa y reproduce de nuevo —
   confirmando que el modelo de lectura es reconstruible solo a partir del store,
   sin tráfico de eventos en vivo.

5. **Intercambia el store (sobre el papel).** Lee el `#[bean]` `event_store` en
   `LumenBeans`, luego escribe el cambio de una línea que devolvería un
   `SqlEventStore::new(db)` en lugar de un `MemoryEventStore::new()`. Observa que
   ningún comando, ningún `apply` y ningún `rehydrate` cambiaría — solo el bean. Esa
   es la recompensa de depender del puerto `EventStore`.

## Adónde ir después

- Coordina un proceso entre **dos** monederos — debita uno, acredita el otro, y
  compensa cuando el crédito falla — en
  **[Sagas, Workflows & TCC](./12-sagas.md)**. La saga de transferencia se construye
  directamente sobre la guarda de descubierto y el token de concurrencia optimista
  de este capítulo.
- Revisa cómo cada evento anexado alcanza la proyección por el cable en
  **[Event-Driven Architecture & Messaging](./10-eda-messaging.md)** — la mitad de
  transporte de la historia que este capítulo completó.
