# Diseño dirigido por el dominio

Lumen ya puede abrir monederos y volver a leerlos, y tiene un sitio donde colocar
el modelo de lectura. Pero fíjate bien y verás que falta algo: todavía nada *posee*
las reglas. ¿Dónde está «no puedes retirar más de lo que tienes»? ¿Dónde está «un
importe debe ser positivo»? ¿Dónde está «un monedero debe tener un propietario»?
Ahora mismo eso viviría como sentencias `if` dispersas en un handler — exactamente
el tipo de regla que un futuro desarrollador puede saltarse escribiendo directamente
en el almacén.

El **diseño dirigido por el dominio (DDD)** lo resuelve haciendo que el modelo sea
responsable de sus propios invariantes. En este capítulo construirás el núcleo de
dominio de Lumen desde sus principios fundamentales: el *value object* `Money`
(inmutable, céntimos enteros, aritmética exacta) y el *aggregate* `Wallet` que
custodia las reglas de descubierto, importe-positivo y propietario-requerido — y
cada comando emite el evento de dominio que registra lo sucedido. Ambos ficheros
están tomados literalmente de
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen),
de modo que el crate que vas haciendo crecer aquí coincide línea por línea con el
servicio terminado.

Esto es modelado de Rust puro: no hay HTTP, ni base de datos, ni runtime del
framework en la ruta caliente. El framework aporta exactamente dos derives —
`#[derive(AggregateRoot)]` y `#[derive(DomainEvent)]` — y por lo demás se aparta de
tu camino, que es justo de lo que se trata: tus reglas viven en métodos corrientes
que puedes probar con tests unitarios sin ninguna E/S.

Al terminar este capítulo, serás capaz de:

- Distinguir un **value object** de una **entidad / aggregate**, y saber por qué
  `Money` es lo primero y `Wallet` lo segundo.
- Construir un value object `Money` que es inmutable, almacenado como céntimos
  enteros y cerrado bajo las operaciones que un monedero necesita (`add` /
  `subtract` / `require_positive`).
- Construir el aggregate `Wallet` de forma que sus invariantes de descubierto,
  importe-positivo y propietario-requerido sean *físicamente* inalcanzables desde
  fuera — validados antes de emitir ningún evento.
- Usar `#[derive(AggregateRoot)]` y `#[derive(DomainEvent)]` para incrustar el búfer
  de eventos del framework y sellar eventos tipados, escribiendo tú solo las reglas.
- Mapear los fallos de dominio a una familia tipada `DomainError` cuyas cadenas
  `Display` afloran literalmente como detalles de problema RFC 9457.
- Demostrar cada invariante con tests unitarios corrientes — sin base de datos, sin
  HTTP.

## Conceptos que conocerás

Antes de la primera línea de código, aquí están las ideas de DDD en las que se apoya
este capítulo. Cada una se reintroduce en su contexto donde se usa por primera vez;
esta es la versión breve.

> **Note** **Término clave — value object.** Un *value object* es un tipo de dominio
> definido por completo por sus atributos — **no tiene identidad** — y es
> **inmutable**: cada operación devuelve un valor *nuevo* en lugar de mutar en el
> sitio. Dos value objects con atributos iguales son iguales, punto. `Money` es el
> ejemplo de manual. El análogo en Java/DDD es exactamente un value object (un
> `@Embeddable` de JPA, o un `record` de Java usado como valor).

> **Note** **Término clave — entidad y aggregate.** Una *entidad* tiene una
> **identidad** que persiste a través de los cambios (un monedero sigue siendo «el
> mismo monedero» a medida que su saldo se mueve). Un *aggregate* es un grupo de
> entidades y value objects tratado como una sola unidad, con una única **raíz del
> aggregate** como su único punto de entrada — la frontera de consistencia a través
> de la cual debe fluir todo cambio. Aquí `Wallet` es la raíz del aggregate. En
> términos de Spring/JPA esto es la `@Entity` que posee sus hijos y custodia sus
> invariantes.

> **Note** **Término clave — evento de dominio.** Un *evento de dominio* es un
> registro inmutable de algo que ocurrió en el dominio, en pasado (`WalletOpened`,
> `MoneyDeposited`). El aggregate *emite* uno cada vez que cambia de estado, de modo
> que el cambio queda capturado como un hecho en lugar de quedar implícito. Esta es
> la misma noción que un `ApplicationEvent` de Spring publicado desde un método de
> dominio, pero aquí los eventos son además la fuente de verdad persistida (lo verás
> por completo en [Event Sourcing](./11-event-sourcing.md)).

> **Note** **Término clave — invariante.** Un *invariante* es una regla que debe
> cumplirse para que el modelo sea válido — «el saldo nunca baja de cero», «el
> propietario nunca está en blanco». El trabajo de un aggregate es hacer que sus
> invariantes sean imposibles de violar desde fuera. No hay anotación de Spring para
> esto; es la disciplina que la frontera del aggregate existe para imponer.

El capítulo construye dos ficheros: `src/money.rs` (el value object) y
`src/domain.rs` (el aggregate, sus eventos, la familia de errores y la vista del
modelo de lectura). Declaraste ambos en la lista `mod` allá en
[Quickstart](./02-quickstart.md), así que nada cambia en `main.rs` — estás
rellenando módulos que el punto de entrada ya nombra.

## Paso 1 — Definir la forma del value object `Money`

Empieza por la representación. `Money` resuelve *cómo se almacena y se compara un
importe*; el aggregate `Wallet` (del Paso 5 en adelante) resolverá el
*comportamiento*. Acertar con la representación importa aquí más que casi en ningún
otro sitio: los importes se almacenan como **unidades menores** enteras (céntimos),
de modo que la aritmética es exacta — sin deriva del punto flotante binario, el
clásico bug de corrección que un tipo monetario existe para evitar.

Crea `src/money.rs` y declara el struct y sus imports:

```rust,ignore
// samples/lumen/src/money.rs
use std::fmt;

use serde::{Deserialize, Serialize};

/// An exact monetary amount, expressed in integer minor units (cents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money {
    /// The amount in minor units (cents). Kept private so the only way to a
    /// `Money` is through the validating constructors.
    cents: i64,
}
```

Qué acaba de pasar, decisión a decisión:

- **Céntimos enteros, nunca un float.** El único campo es `cents: i64`, mantenido
  *privado* para que la única vía hacia un `Money` sea a través de un constructor.
  10,00 € es `Money::cents(1_000)`; 12,50 € tras una suma es `Money::cents(1_250)`.
  Las matemáticas son exactas por construcción — no hay ningún `f64` por ningún lado
  que pueda derivar.
- **Un value object se compara por valor.** Los derives `PartialEq, Eq, PartialOrd,
  Ord` hacen que dos `Money` sean iguales exactamente cuando sus céntimos son
  iguales, y ordenables para que el monedero pueda preguntar «¿es este importe mayor
  que el saldo?».
- **`Copy`, porque es un valor.** `Money` es un único `i64`, así que se copia
  libremente; nunca andas haciendo malabares con referencias a él.
- **`#[serde(transparent)]` es el contrato de cable.** `Money` se serializa como el
  *entero de céntimos pelado* — un saldo de 10,00 € es el número JSON `1000`, no
  `{ "cents": 1000 }`. Ese es el contrato que comparten el modelo de lectura y los
  payloads de eventos, y es la razón por la que el campo puede seguir siendo privado
  sin perjudicar la forma de cable.

> **Note** **Término clave — unidades menores.** Las *unidades menores* son la unidad
> indivisible más pequeña de una moneda — céntimos para euros y dólares. Almacenar el
> dinero como un recuento entero de unidades menores (1000 céntimos, no 10,00 euros)
> mantiene la aritmética exacta. Esta es la misma disciplina que una columna `BIGINT`
> de céntimos en la base de datos o un `long` de Java de unidades menores.

> **Tip** **Punto de control.** `src/money.rs` existe con un struct `Money` que tiene
> un campo privado `cents: i64`. Todavía no compilará — los constructores vienen a
> continuación — pero la forma está fijada.

## Paso 2 — Dar a `Money` operaciones inmutables y validadoras

Un value object expone únicamente operaciones que *devuelven valores nuevos*. Añade
los constructores, los accesores y las tres operaciones que un monedero necesita.
Añade este bloque `impl` a `src/money.rs`:

```rust,ignore
impl Money {
    /// A zero amount — the opening balance of a brand-new wallet.
    pub const ZERO: Money = Money { cents: 0 };

    /// Builds a `Money` from a raw minor-unit (cent) count.
    pub const fn cents(cents: i64) -> Self {
        Money { cents }
    }

    /// Builds a `Money` from a whole-currency unit count (`from_units(10)` is €10.00).
    pub const fn from_units(units: i64) -> Self {
        Money { cents: units * 100 }
    }

    /// The amount in minor units (cents) — the wire representation.
    pub const fn cents_value(self) -> i64 {
        self.cents
    }

    /// Whether this amount is strictly positive (`> 0`).
    pub const fn is_positive(self) -> bool {
        self.cents > 0
    }

    /// Whether this amount is zero.
    pub const fn is_zero(self) -> bool {
        self.cents == 0
    }

    /// Returns a new `Money` that is `self + other` (immutable addition).
    #[must_use]
    pub const fn add(self, other: Money) -> Money {
        Money { cents: self.cents + other.cents }
    }

    /// Returns `self - other`, or `MoneyError::Overdraw` if that would go below zero.
    pub fn subtract(self, other: Money) -> Result<Money, MoneyError> {
        if other.cents > self.cents {
            return Err(MoneyError::Overdraw);
        }
        Ok(Money { cents: self.cents - other.cents })
    }

    /// Validates that this amount is strictly positive, returning it unchanged on success.
    pub fn require_positive(self) -> Result<Money, MoneyError> {
        if self.is_positive() {
            Ok(self)
        } else {
            Err(MoneyError::NonPositive)
        }
    }
}
```

Qué acaba de pasar — aquí hay cuatro decisiones de diseño que cargan peso:

- **Inmutable.** `add` es `#[must_use]` y `const`; devuelve un `Money` *fresco* y
  deja los operandos intactos. Lo mismo hace `subtract`. No hay `add_assign` — un
  value object se reemplaza, no se edita. `#[must_use]` hace que el compilador avise
  si llamas a `add` y olvidas usar el resultado, atrapando en tiempo de compilación
  el bug de «creía que esto mutaba en el sitio».
- **Cerrado bajo las operaciones del monedero.** `add` para abonos, `subtract`
  (falible, protegiendo contra el descubierto) para cargos, y `require_positive`
  para la guarda que todo comando mutador ejecuta antes de emitir un evento.
  «Cerrado» significa que toda operación que realiza un monedero toma `Money` y
  produce `Money` (o un `MoneyError`), de modo que los importes nunca se filtran a
  enteros pelados.
- **`subtract` es donde vive el descubierto.** Devuelve `Result<Money, MoneyError>`:
  restar más de lo que tienes es `MoneyError::Overdraw`, no un saldo negativo
  silencioso. Este es el *único* sitio donde se comprueba la regla de «nunca por
  debajo de cero» — el aggregate la reutiliza en lugar de reimplementarla.
- **`const` donde es posible.** `ZERO`, `cents`, `from_units`, `cents_value`,
  `is_positive`, `is_zero` y `add` son `const fn`, así que `Money::ZERO` y
  `Money::cents(100)` pueden usarse en contextos const. `subtract` y
  `require_positive` no son `const` porque devuelven un `Result`.

> **Note** **Término clave — `#[must_use]`.** Anotar una función con `#[must_use]` le
> indica al compilador que avise cuando se ignora su valor de retorno. En una
> operación inmutable como `add` es el guardarraíl que convierte «olvidé que el
> resultado es un valor nuevo» en una advertencia en tiempo de compilación en lugar
> de una actualización perdida.

## Paso 3 — Renderizar e informar de los fallos de `Money` a mano

Dos piezas más completan el value object: un `Display` legible para humanos y el
error tipado que devuelven sus operaciones. Lumen escribe a mano tanto `Display`
como `std::error::Error` para `MoneyError` en lugar de derivarlos — eso mantiene
honesta la promesa de una sola dependencia del libro hasta el fondo, hasta los enums
de error.

Añade el tipo de error y los dos impls de `Display` a `src/money.rs`:

```rust,ignore
/// The typed error a `Money` operation can fail with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoneyError {
    /// An amount was expected to be strictly positive (`> 0`) but was not.
    NonPositive,
    /// A subtraction would drop the balance below zero.
    Overdraw,
}

impl fmt::Display for MoneyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoneyError::NonPositive => f.write_str("amount must be positive"),
            MoneyError::Overdraw => f.write_str("amount exceeds balance"),
        }
    }
}

impl std::error::Error for MoneyError {}

impl fmt::Display for Money {
    /// Renders the amount as a fixed two-decimal major-unit string
    /// (`1250` cents → `"12.50"`), the human-readable form used in logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.cents < 0 { "-" } else { "" };
        let abs = self.cents.abs();
        write!(f, "{sign}{}.{:02}", abs / 100, abs % 100)
    }
}
```

Qué acaba de pasar:

- **`MoneyError` es un enum cerrado** con dos casos — exactamente las dos formas en
  que una operación de `Money` puede fallar. Como deriva `PartialEq, Eq`, los tests
  pueden afirmar `err == MoneyError::Overdraw` directamente.
- **`Display` lleva el texto del mensaje**, y `impl std::error::Error for
  MoneyError {}` lo convierte en un error de primera clase para que `?` y los trait
  objects funcionen. El bloque vacío basta porque `Error` tiene métodos por defecto.
- **El propio `Display` de `Money`** convierte `1250` en `"12.50"` para los logs y el
  banner — la forma de unidad mayor que lee un humano, mantenida aparte de la forma
  de unidad menor que lleva el cable.

> **Design note.** `MoneyError` escribe a mano `Display` y `std::error::Error` en
> lugar de derivarlos con `thiserror`. Eso es a propósito: Lumen depende de
> exactamente un crate de Firefly más `axum` y `serde`, y el libro mantiene esa
> promesa hasta el fondo — incluso los enums de error. Dos impls de trait por tipo de
> error es un precio pequeño por una lista de dependencias honesta, y `Money` mismo
> es un tipo congelado y comparable por valor que Rust hace inmutable como garantía
> del compilador.

> **Tip** **Punto de control.** Ejecuta `cargo test --lib money` (o `cargo build`).
> `src/money.rs` ahora compila por sí solo: un value object de campo privado, tres
> operaciones, un error de dos variantes y un `Display` que imprime `"12.50"`. La
> aritmética es exacta y el tipo no puede construirse en un estado inválido.

## Paso 4 — Montar el aggregate `Wallet` y sus eventos

`Money` resolvió la representación. El aggregate `Wallet` posee el *comportamiento*:
es la frontera de consistencia, el único punto de entrada a través del cual debe
fluir todo cambio a un monedero, de modo que los invariantes no pueden saltarse.

El `Wallet` de Lumen está event-sourced — cada comando produce un evento de dominio,
y el estado del monedero es el *resultado* de plegar esos eventos. Toda la
maquinaria del event store llega en [Event Sourcing](./11-event-sourcing.md); aquí
construyes solo la forma DDD. El aggregate incrusta el `AggregateRoot` del framework
(un búfer de eventos no confirmados más una versión), y dos derives hacen el trabajo
mecánico.

> **Note** **Término clave — `#[derive(AggregateRoot)]`.** Este derive encuentra el
> campo `AggregateRoot` de `firefly` incrustado en tu struct y genera una constante
> asociada `AGGREGATE_TYPE` más accesores `aggregate()` / `aggregate_mut()` sobre él.
> El propio `AggregateRoot` incrustado lleva el búfer de eventos no confirmados, el id
> del aggregate y la versión — de modo que tu struct contiene solo el estado
> proyectado y las reglas. El análogo en Spring/Axon es una raíz `@Aggregate` que el
> framework gestiona.

> **Note** **Término clave — `#[derive(DomainEvent)]`.** Este derive sella un struct
> de payload de evento con un discriminador estable `EVENT_TYPE` (el nombre de su
> struct) y genera una conversión `to_domain_event(...)` hacia el evento de cable del
> framework. Tú declaras el payload como un struct serializable corriente; el derive
> aporta la etiqueta de tipo para que nunca deletrees los nombres de evento como
> literales de cadena pelados en los sitios de llamada.

Crea `src/domain.rs` con sus imports, la constante `AGGREGATE_TYPE` y los tres
payloads de evento:

```rust,ignore
// samples/lumen/src/domain.rs
use firefly::eventsourcing::{AggregateRoot, DomainEvent};
use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::money::{Money, MoneyError};

/// The aggregate-type discriminator stamped onto every event a Wallet raises.
/// `#[derive(AggregateRoot)]` also exposes it as `Wallet::AGGREGATE_TYPE`.
pub const AGGREGATE_TYPE: &str = "Wallet";

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
    /// The deposited amount, in minor units (cents).
    pub amount: i64,
}

/// Payload of the event raised when money is debited from a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyWithdrawn {
    pub wallet_id: String,
    /// The withdrawn amount, in minor units (cents).
    pub amount: i64,
}
```

Qué acaba de pasar, línea a línea:

- **`use firefly::eventsourcing::{AggregateRoot, DomainEvent};`** importa los dos
  *tipos* del framework — el struct raíz incrustable y el struct de evento de cable.
- **`use firefly::prelude::*;`** trae al ámbito las macros derive del framework,
  incluyendo `AggregateRoot`, `DomainEvent` y `Schema` (usada por la vista del modelo
  de lectura en el Paso 8). Todo te llega a través de la única fachada `firefly`.
- **`AGGREGATE_TYPE`** es el discriminador de cadena sellado en cada evento que emite
  el monedero. Se declara como una constante pública *y* el derive lo reexpone como
  `Wallet::AGGREGATE_TYPE`, de modo que ambas formas de escribirlo nombran el mismo
  valor.
- **Cada payload de evento está en pasado** (`WalletOpened`, no `OpenWallet`) y lleva
  `#[derive(DomainEvent)]`. El derive le da a cada uno una const `EVENT_TYPE` igual a
  su nombre de struct (`"WalletOpened"`, etc.), que usas en los sitios de emisión en
  lugar de teclear la cadena a mano.

## Paso 5 — Declarar el struct raíz del aggregate

Ahora el aggregate en sí. Incrusta el `AggregateRoot` del framework como un campo
llamado `root`, lleva el estado proyectado (`owner`, `balance`, `opened`) y deriva
`AggregateRoot` para generar los accesores y la constante `AGGREGATE_TYPE`.

Añade el struct a `src/domain.rs`:

```rust,ignore
/// The event-sourced wallet aggregate.
#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    /// The framework aggregate root — uncommitted-event buffer + version.
    pub root: AggregateRoot,
    /// The owner's display name.
    pub owner: String,
    /// The current balance as a `Money` value object.
    pub balance: Money,
    /// Whether the wallet has been opened (an empty stream is "absent").
    pub opened: bool,
}
```

Qué acaba de pasar:

- **`root: AggregateRoot`** es el campo incrustado del framework. Contiene el búfer
  de eventos no confirmados, el id del aggregate (`root.id`) y la versión
  (`root.version`). El derive localiza este campo por su tipo.
- **`#[firefly(aggregate_type = "Wallet")]`** le dice al derive qué cadena usar para
  `Wallet::AGGREGATE_TYPE`. Coincide con la constante `AGGREGATE_TYPE` que declaraste
  en el Paso 4 — ambos nombran `"Wallet"`.
- **`owner` / `balance` / `opened`** son el *estado proyectado* — el resultado de
  aplicar los eventos del monedero. `balance` es un value object `Money`, así que el
  aggregate reutiliza todas las garantías de aritmética exacta de los Pasos 1–3.
  `opened` distingue un monedero real de un stream de eventos vacío («ausente»).
- **`Clone`** permite que un handler tome una copia de trabajo de un monedero
  rehidratado sin tocar el original — útil bajo la consistencia eventual que
  introduce CQRS.

> **Tip** **Punto de control.** `cargo build` todavía falla — `Wallet` no tiene
> métodos aún y `DomainError` está sin definir — pero los derives deberían resolverse.
> Si el compilador se queja de que no encuentra un campo `AggregateRoot`, confirma
> que el campo incrustado está tipado exactamente como `AggregateRoot` (de
> `firefly::eventsourcing`), no tu propio tipo.

## Paso 6 — Escribir el método factoría: `open`

`open` es la *única* forma de traer un monedero a la existencia. Valida las entradas,
construye el aggregate y `raise` el evento de apertura. Usar una factoría en lugar de
un constructor público garantiza que el evento `WalletOpened` nunca se olvide — no hay
canal trasero que produzca un monedero sin registrar su nacimiento.

Añade un bloque `impl Wallet` con `open`:

```rust,ignore
impl Wallet {
    /// Opens a fresh wallet, raising a `WalletOpened` event.
    pub fn open(
        id: impl Into<String>,
        owner: impl Into<String>,
        opening_balance: Money,
    ) -> Result<Self, DomainError> {
        let id = id.into();
        let owner = owner.into();
        if owner.trim().is_empty() {
            return Err(DomainError::OwnerRequired);
        }
        if opening_balance.cents_value() < 0 {
            return Err(DomainError::NonPositiveAmount);
        }
        let mut wallet = Wallet {
            root: AggregateRoot::new(&id, AGGREGATE_TYPE),
            owner: owner.clone(),
            balance: Money::ZERO,
            opened: false,
        };
        wallet.raise(
            WalletOpened::EVENT_TYPE,
            &WalletOpened {
                wallet_id: id,
                owner,
                opening_balance: opening_balance.cents_value(),
            },
        );
        wallet.balance = opening_balance;
        wallet.opened = true;
        Ok(wallet)
    }
}
```

Qué acaba de pasar, por orden:

- **Validar primero, construir después.** Dos invariantes se imponen *antes* de
  emitir ningún evento: el propietario debe ser no vacío (`OwnerRequired`), y el
  saldo de apertura no debe ser negativo (un saldo de apertura *cero* está permitido
  explícitamente — la comprobación es `< 0`, no `<= 0`).
- **`AggregateRoot::new(&id, AGGREGATE_TYPE)`** construye la raíz incrustada con el id
  de este monedero y su etiqueta de tipo de aggregate, en la versión 0 con un búfer
  de eventos vacío.
- **`wallet.raise(WalletOpened::EVENT_TYPE, &WalletOpened { ... })`** registra el
  evento de nacimiento. `WalletOpened::EVENT_TYPE` es el discriminador que generó
  `#[derive(DomainEvent)]` (la cadena `"WalletOpened"`), así que el sitio de llamada
  nunca lo deletrea a mano. `raise` es un pequeño helper que añades en el Paso 9;
  serializa el payload y lo empuja sobre `root`.
- **El estado se actualiza después del evento.** `wallet.balance = opening_balance` y
  `wallet.opened = true` ponen el estado proyectado para que coincida con lo que el
  evento describe. El evento es el hecho; los campos son la proyección cacheada de
  él.

> **Note** **Término clave — método factoría.** Un *método factoría* es una función
> estática (asociada) que construye una instancia plenamente válida, en lugar de
> exponer un constructor público. Es la única puerta hacia el aggregate, así que
> puede imponer los invariantes de nacimiento y garantizar que el evento
> `WalletOpened` siempre se emita. El análogo en Spring/DDD es una factoría estática
> sobre la raíz del aggregate (o un servicio de dominio que lo produce).

## Paso 7 — Escribir los métodos de comportamiento: `deposit` y `withdraw`

Los dos comandos mutadores siguen una sola forma: comprobar que el monedero existe,
validar el importe, aplicar la operación de `Money`, emitir el evento, actualizar el
estado. La regla de descubierto se impone exactamente una vez — por
`Money::subtract`, que ya construiste en el Paso 2.

Añade estos métodos al mismo bloque `impl Wallet`:

```rust,ignore
    /// Credits `amount` to the wallet, raising a `MoneyDeposited` event.
    pub fn deposit(&mut self, amount: Money) -> Result<(), DomainError> {
        self.require_opened()?;
        let amount = amount.require_positive()?;
        self.raise(
            MoneyDeposited::EVENT_TYPE,
            &MoneyDeposited { wallet_id: self.root.id.clone(), amount: amount.cents_value() },
        );
        self.balance = self.balance.add(amount);
        Ok(())
    }

    /// Debits `amount` from the wallet, raising a `MoneyWithdrawn` event.
    pub fn withdraw(&mut self, amount: Money) -> Result<(), DomainError> {
        self.require_opened()?;
        let amount = amount.require_positive()?;
        let remaining = self.balance.subtract(amount)?; // Overdraw → InsufficientFunds
        self.raise(
            MoneyWithdrawn::EVENT_TYPE,
            &MoneyWithdrawn { wallet_id: self.root.id.clone(), amount: amount.cents_value() },
        );
        self.balance = remaining;
        Ok(())
    }

    fn require_opened(&self) -> Result<(), DomainError> {
        if self.opened {
            Ok(())
        } else {
            Err(DomainError::NotFound(self.root.id.clone()))
        }
    }
```

Qué acaba de pasar — lee `withdraw` con cuidado, porque es donde la frontera de
consistencia se gana el sueldo:

- **El orden es *validar y luego mutar*.** `require_opened()`, `require_positive()` y
  la comprobación de descubierto de `subtract` se ejecutan todas **antes** de emitir
  el evento. Si la retirada produjera un descubierto, `Money::subtract` devuelve
  `MoneyError::Overdraw`, el `?` lo convierte en `DomainError::InsufficientFunds`
  (mediante el impl `From` que añades en el Paso 8), y el método retorna *sin emitir
  nada*.
- **El invariante es inalcanzable desde fuera.** «El saldo nunca baja de cero» no
  puede violarse, porque la única ruta hacia una retirada pasa primero por este
  guante. No hay setter en `balance`, ni forma de saltarse `subtract`. Esa es la
  diferencia entre una guarda a nivel de servicio (una convención que alguien puede
  olvidar) y un invariante de aggregate (una restricción física).
- **`require_opened` convierte «ausente» en un error tipado.** Un comando contra un
  monedero que nunca se abrió devuelve `DomainError::NotFound(id)`, que la frontera
  web mapea más adelante a un 404. Tanto `deposit` como `withdraw` lo comprueban
  primero.
- **`self.root.id.clone()`** lee el id de la raíz incrustada para sellar cada evento
  con el monedero al que pertenece.

> **Note** La razón de ser de un aggregate es ser la *única* forma de cambiar su
> estado. Como `deposit` y `withdraw` toman `&mut self` y no hay setters públicos,
> toda mutación se canaliza a través de estos métodos y pasa por sus guardas. Un
> futuro desarrollador no puede «simplemente escribir en el almacén» y saltarse las
> reglas — las reglas son la puerta.

## Paso 8 — Añadir la familia tipada `DomainError`

Los errores son un enum cerrado con cadenas `Display` estables — estables porque los
tests afirman sobre ellas y afloran literalmente como el `detail` del problema
RFC 9457 una vez mapeadas en la frontera HTTP (cableas ese mapeo en
[CQRS](./09-cqrs.md) y [Tu primera API HTTP](./06-first-http-api.md)).

Añade `DomainError` y sus impls a `src/domain.rs`:

```rust,ignore
/// The typed domain-error family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// A command referenced an amount that was not strictly positive.
    NonPositiveAmount,
    /// A withdrawal (or transfer debit) exceeded the available balance.
    InsufficientFunds,
    /// A command targeted a wallet that was never opened.
    NotFound(String),
    /// The owner name was empty when opening a wallet.
    OwnerRequired,
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DomainError::NonPositiveAmount => f.write_str("amount must be positive"),
            DomainError::InsufficientFunds => f.write_str("insufficient funds"),
            DomainError::NotFound(id) => write!(f, "wallet {id} not found"),
            DomainError::OwnerRequired => f.write_str("owner is required"),
        }
    }
}

impl std::error::Error for DomainError {}

impl From<MoneyError> for DomainError {
    fn from(e: MoneyError) -> Self {
        match e {
            MoneyError::NonPositive => DomainError::NonPositiveAmount,
            MoneyError::Overdraw => DomainError::InsufficientFunds,
        }
    }
}
```

Qué acaba de pasar:

- **`From<MoneyError> for DomainError` es el puente.** Es lo que permite que
  `withdraw` escriba `self.balance.subtract(amount)?` y haga que un descubierto
  aritmético aflore como `InsufficientFunds`. El value object informa del hecho
  aritmético (`Overdraw`); el aggregate lo traduce a lenguaje de dominio
  (`InsufficientFunds`). El operador `?` llama a este impl `From` automáticamente.
- **Las cadenas `Display` son el contrato.** `"insufficient funds"`, `"amount must
  be positive"`, `"wallet {id} not found"`, `"owner is required"` — estas cadenas
  exactas se convierten en el `detail` del problema RFC 9457 en la frontera web, y
  los tests afirman sobre ellas, así que son estables.
- **`Display` y `Error` escritos a mano, de nuevo.** Como `MoneyError`, `DomainError`
  detalla sus impls de `Display` y `Error` en lugar de derivarlos con `thiserror`:
  sin crate extra, una sola dependencia.

> **Note** Lumen devuelve un `DomainError` tipado en lugar de lanzar una excepción.
> `InsufficientFunds` / `NonPositiveAmount` / `OwnerRequired` se convierten en
> problemas 422 y `NotFound` se convierte en un 404, decidido por un `match` en la
> frontera web — un valor devuelto, comprobado por el compilador, sin tabla de
> excepción-a-estado que mantener sincronizada. Escribirás ese `match` en
> [CQRS](./09-cqrs.md).

> **Tip** **Punto de control.** `cargo build` ahora resuelve `Wallet::open` /
> `deposit` / `withdraw` y su tipo de error. El helper `raise` todavía falta
> (siguiente paso), así que la build aún no está en verde — pero toda regla de
> dominio queda ya expresada como código.

## Paso 9 — Añadir el helper `raise` y la vista del modelo de lectura

Dos piezas terminan `src/domain.rs`. Primero, el helper privado `raise` que llaman
los métodos de comando — serializa un payload `#[derive(DomainEvent)]` y lo empuja
sobre la raíz incrustada. Segundo, la vista plana del modelo de lectura que el
aggregate entrega.

> **Note** **Término clave — modelo de lectura / proyección.** Un *modelo de lectura*
> (o *proyección*) es una vista plana y optimizada para consulta de un aggregate,
> separada del rico aggregate en sí. El aggregate es el modelo de *escritura* —
> impositor de reglas, emisor de eventos; el modelo de lectura es lo que devuelven
> las consultas — serializable, sin comportamiento. Mantenerlos separados es el
> corazón de CQRS ([CQRS](./09-cqrs.md)). El análogo en Spring es una proyección /
> DTO de lectura de JPA distinta de la entidad gestionada.

Añade el método `view` y el helper `raise` al bloque `impl Wallet`, y luego el struct
`WalletView`:

```rust,ignore
    /// The current read-model view of this aggregate.
    pub fn view(&self) -> WalletView {
        WalletView {
            id: self.root.id.clone(),
            owner: self.owner.clone(),
            balance: self.balance.cents_value(),
            version: self.root.version,
        }
    }

    /// Serialises a `#[derive(DomainEvent)]` payload and raises it onto the
    /// embedded root under `event_type`.
    fn raise<P: Serialize>(&mut self, event_type: &str, payload: &P) {
        let bytes = serde_json::to_vec(payload).expect("domain event payload serialises");
        self.root.raise(event_type, bytes);
    }
}

/// The read-model projection of a wallet — the wire shape served by
/// `GET /api/v1/wallets/:id` and stored in the read-model repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WalletView {
    pub id: String,
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied).
    pub version: i64,
}
```

Qué acaba de pasar:

- **`raise` es el único sitio donde se serializan los eventos.** Toma el
  discriminador `EVENT_TYPE` y un payload serializable, codifica el payload a bytes
  con `serde_json::to_vec`, y llama a `self.root.raise(event_type, bytes)` — el
  método del framework sobre `AggregateRoot` que añade al búfer de eventos no
  confirmados e incrementa la versión. Todo método de comando se enruta a través de
  este helper, así que la serialización vive en exactamente un punto.
- **`view()` produce el modelo de lectura bajo demanda.** Copia `id`, `owner`,
  `balance` (como céntimos pelados vía `cents_value()`) y `version` (de
  `root.version`) en un `WalletView` plano. El aggregate nunca se serializa a *sí
  mismo* — entrega una vista.
- **`WalletView` deriva `Schema`.** Eso hace que aparezca en los docs OpenAPI
  autogenerados como un schema de componente, de modo que la respuesta de
  `GET /api/v1/wallets/:id` queda documentada con cero código extra (véase
  [OpenAPI](./06a-openapi.md)).
- **`version` permite a un cliente detectar el desfase.** Bajo la consistencia
  eventual que introduce CQRS, un cliente puede comparar versiones para advertir que
  leyó una proyección desfasada.

Mantener `Wallet` (rico, impositor de reglas, emisor de eventos) y `WalletView`
(plano, serializable, sin comportamiento) como tipos separados es la misma separación
dominio/persistencia que [Persistencia](./07-persistence.md) trazó alrededor del
repositorio: el aggregate nunca se serializa a sí mismo, y la forma de cable nunca
lleva un invariante.

> **Tip** **Punto de control.** `cargo build` está en verde. Tanto `src/money.rs`
> como `src/domain.rs` compilan, y `Wallet::open(...).view()` hace un ida y vuelta de
> un monedero desde una llamada de factoría hasta una vista serializable — con cada
> regla impuesta por el camino.

## Paso 10 — Demostrar los invariantes con tests unitarios

Como el aggregate es un struct corriente con métodos corrientes, puedes ejercitar
cada regla sin base de datos y sin HTTP. Estos son los tests unitarios que vienen en
`samples/lumen/src/domain.rs`. Añade un bloque `#[cfg(test)] mod tests`:

```rust,ignore
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_validates_owner_and_balance() {
        assert_eq!(
            Wallet::open("w1", "  ", Money::cents(100)).unwrap_err(),
            DomainError::OwnerRequired
        );
        assert_eq!(
            Wallet::open("w1", "alice", Money::cents(-1)).unwrap_err(),
            DomainError::NonPositiveAmount
        );
        let w = Wallet::open("w1", "alice", Money::ZERO).unwrap();
        assert!(w.opened);
        assert_eq!(w.balance, Money::ZERO);
    }

    #[test]
    fn withdraw_rejects_overdraft() {
        let mut w = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
        assert_eq!(
            w.withdraw(Money::cents(101)).unwrap_err(),
            DomainError::InsufficientFunds
        );
        // The failed command raised no event beyond the open.
        assert_eq!(w.root.uncommitted().len(), 1);
    }

    #[test]
    fn deposit_and_withdraw_update_balance_and_raise_events() {
        let mut w = Wallet::open("w1", "alice", Money::cents(100)).unwrap();
        w.deposit(Money::cents(50)).unwrap();
        assert_eq!(w.balance, Money::cents(150));
        w.withdraw(Money::cents(30)).unwrap();
        assert_eq!(w.balance, Money::cents(120));
    }

    #[test]
    fn wallet_view_wire_shape() {
        let w = Wallet::open("wlt_1", "alice", Money::cents(250)).unwrap();
        let json = serde_json::to_string(&w.view()).unwrap();
        assert_eq!(
            json,
            r#"{"id":"wlt_1","owner":"alice","balance":250,"version":1}"#
        );
    }
}
```

Qué acaba de pasar:

- **`open_validates_owner_and_balance`** demuestra los invariantes de nacimiento: un
  propietario en blanco es `OwnerRequired`, un saldo de apertura negativo es
  `NonPositiveAmount`, y un saldo de apertura *cero* tiene éxito (el monedero queda
  `opened` con saldo `ZERO`).
- **`withdraw_rejects_overdraft`** es el test que carga peso. Tras una retirada
  rechazada, el búfer de eventos no confirmados del aggregate (`w.root.uncommitted()`)
  sigue teniendo *exactamente un* evento — el `WalletOpened` de la factoría. El
  descubierto nunca produjo un `MoneyWithdrawn`, así que nada parcial puede llegar a
  persistirse jamás. Esta es la frontera de consistencia hecha concreta.
- **`deposit_and_withdraw_update_balance_and_raise_events`** recorre el camino feliz:
  un depósito sube el saldo, una retirada lo baja, y la aritmética es exacta
  (`100 + 50 - 30 = 120`).
- **`wallet_view_wire_shape`** fija el contrato de cable. El `view()` de un monedero
  recién abierto se serializa exactamente a
  `{"id":"wlt_1","owner":"alice","balance":250,"version":1}` — confirmando el
  `#[serde(transparent)]` de `Money` (el saldo es el número pelado `250`, no
  `{ "cents": 250 }`) y el orden de campos de `WalletView`.

> **Tip** **Punto de control.** Ejecuta `cargo test --lib`. Todos los tests de
> dominio pasan — y se ejecutaron sin base de datos, sin servidor HTTP y sin runtime
> del framework. Esa es la recompensa de un núcleo de dominio que es solo structs y
> métodos: las reglas son comprobables en microsegundos.

## Resumen — el núcleo de dominio de Lumen

Lumen tiene ahora un núcleo de dominio que posee sus reglas:

- **`src/money.rs`** — un value object `Money`: inmutable, céntimos enteros, campo
  privado, `#[serde(transparent)]` para que viaje por el cable como un número pelado,
  cerrado bajo `add` / `subtract` / `require_positive`, con un `MoneyError` escrito a
  mano (sin `thiserror`) y un `Display` que imprime `"12.50"`.
- **`src/domain.rs`** — el aggregate `Wallet` que lleva `#[derive(AggregateRoot)]`
  (que genera `AGGREGATE_TYPE` y los accesores `aggregate()` / `aggregate_mut()` sobre
  la raíz incrustada). `open` / `deposit` / `withdraw` imponen los tres invariantes —
  propietario requerido, importes positivos, sin descubierto — *antes* de emitir un
  evento, de modo que un comando rechazado deja el búfer de eventos intacto.
- **Tres payloads `#[derive(DomainEvent)]`** (`WalletOpened`, `MoneyDeposited`,
  `MoneyWithdrawn`), cada uno sellado con un discriminador `EVENT_TYPE` estable,
  emitidos a través del único helper privado `raise`.
- **La familia tipada `DomainError`** con cadenas `Display` estables que afloran como
  detalles de problema RFC 9457, más el puente `From<MoneyError>` que convierte un
  descubierto aritmético en `InsufficientFunds`.
- **`WalletView`** — la proyección plana del modelo de lectura que el aggregate
  entrega vía `view()`, derivando `Schema` para los docs, mantenida como un tipo
  separado del aggregate impositor de reglas.

Ahora también sabes:

- La diferencia entre un **value object** (sin identidad, inmutable, comparado por
  valor — `Money`) y un **aggregate** (una identidad y una frontera de consistencia —
  `Wallet`), y por qué cada regla pertenece a donde le corresponde.
- Que un aggregate hace sus invariantes *físicamente* inalcanzables validando antes
  de mutar y no exponiendo setters — la diferencia entre una convención y una
  restricción.
- Que `#[derive(AggregateRoot)]` y `#[derive(DomainEvent)]` aportan el búfer de
  eventos y los discriminadores de tipo, de modo que el único código de event
  sourcing que escribes a mano son las reglas.

El ciclo de vida completo de los payloads de eventos — cómo se persisten, y el
pliegue `rehydrate` / `apply` que reconstruye un monedero desde su stream — recibe su
tratamiento en [Event Sourcing](./11-event-sourcing.md). Aquí, la forma que importa es
la de DDD: un value object que no puedes corromper y un aggregate que no puedes
saltarte.

## Ejercicios

1. **Rompe un invariante, mira cómo aguanta.** En un bloque `#[cfg(test)]`, abre un
   monedero con `Money::cents(100)` y llama a `withdraw(Money::cents(200))`. Afirma
   que el error es `DomainError::InsufficientFunds` *y* que
   `w.root.uncommitted().len()` sigue siendo `1` — demostrando que el comando
   rechazado no emitió ningún evento más allá del de apertura.

2. **Añade una regla `transfer` (solo dominio).** Escribe una función libre
   `fn transfer(from: &mut Wallet, to: &mut Wallet, amount: Money) ->
   Result<(), DomainError>` que llame a `from.withdraw(amount)?` y luego a
   `to.deposit(amount)?`. Comprueba que una transferencia que excede el saldo del
   origen falla en el tramo de la retirada y deja el saldo del *destino* sin cambios.
   (La versión real y persistida se convierte en la saga en [Sagas](./12-sagas.md).)

3. **Confirma la forma de cable.** Serializa el `view()` de un monedero recién
   abierto con `serde_json::to_string` y afirma que es igual a
   `{"id":"wlt_1","owner":"alice","balance":250,"version":1}` — verificando que el
   `#[serde(transparent)]` de `Money` y el orden de campos de `WalletView` producen el
   contrato que comparten el modelo de lectura y los clientes.

4. **Justifica el error escrito a mano.** En dos frases, explica por qué `MoneyError`
   y `DomainError` implementan `Display` / `Error` a mano en lugar de derivarlos con
   `thiserror`, y qué le costaría a la promesa de una sola dependencia del libro
   añadir el crate.

5. **Permite un depósito de cero — y luego decide en contra.** Cambia `deposit` para
   que acepte un importe de cero y ejecuta la suite; anota qué test se rompe y por qué
   que `require_positive` rechace el cero es la regla correcta para un comando de
   monedero. Revierte el cambio.

## Adónde ir después

- Separa la ruta de escritura de la de lectura con el bus de comandos/consultas en
  **[CQRS](./09-cqrs.md)** — donde los comandos de `Wallet` se convierten en handlers
  despachados por el bus y `DomainError` se convierte en el mapeo a problema RFC 9457.
- Expón estas reglas sobre HTTP en
  **[Tu primera API HTTP](./06-first-http-api.md)**, donde un `DomainError` devuelto
  se renderiza como un documento de problema 422 o 404.
- Persiste los eventos y reconstruye un monedero desde su stream en
  **[Event Sourcing](./11-event-sourcing.md)** — el ciclo de vida completo de los
  payloads que emitiste aquí.
