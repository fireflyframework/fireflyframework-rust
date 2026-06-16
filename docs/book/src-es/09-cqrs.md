# CQRS

En [Diseño guiado por el dominio](./08-domain-driven-design.md), el agregado
`Wallet` de Lumen aprendió a hacer cumplir sus propias reglas, y el modelo de
lectura encontró un hogar. Pero un controlador todavía necesita una forma de
*entregar* una instrucción al lado de escritura y de hacer una *pregunta* al
lado de lectura, y de hacerlo sin que ambos caminos compartan una ruta de
código, de modo que las lecturas puedan cachearse y las escrituras validarse de
forma independiente.

Este capítulo traza esa línea nítida. Conecta el bus de comandos/consultas de
Lumen de extremo a extremo, exactamente como lo hace el crate
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen)
que se distribuye: cuatro structs de mensaje, un bean de manejadores y la
costura del controlador que despacha a través del bus y mantiene honesta la
caché de lectura tras una escritura.

Al terminar este capítulo, serás capaz de:

- Explicar qué te aporta la **segregación de responsabilidad entre comandos y
  consultas (CQRS)** y cómo Firefly mantiene un único bus tipado mientras sigue
  reportando comandos y consultas por separado.
- Definir los comandos `OpenWallet` / `Deposit` / `Withdraw` de Lumen y su
  consulta `GetWallet` como structs `#[derive(Command)]` / `#[derive(Query)]`,
  con validación a nivel de campo y un TTL de caché de consulta generado por ti.
- Escribir el **bean de manejadores** `WalletHandlers`: un `#[derive(Service)]`
  cuyo impl `#[handlers]` lleva métodos `#[command_handler]` /
  `#[query_handler]` que alcanzan a sus colaboradores a través de campos
  `#[autowired]`.
- Entender cómo `FireflyApplication` vierte esos manejadores sobre un `Bus`
  proporcionado por el framework e instala el middleware de correlación, caché
  de consulta y validación, sin código de cableado en Lumen.
- Despachar desde el controlador con `bus.send` / `bus.query`, mapear un
  `CqrsError` al estado RFC 9457 correcto, y hacer cumplir la consistencia
  lectura-tras-escritura invalidando la familia de consultas cacheada tras cada
  mutación.

## Conceptos que conocerás

Antes del primer mensaje, aquí tienes las ideas en las que se apoya este
capítulo. Cada una se reintroduce en su contexto donde se usa por primera vez;
esta es la versión corta.

> **Note** **Término clave — Segregación de responsabilidad entre comandos y
> consultas (CQRS).** Un patrón que enruta los **comandos** que cambian el
> estado y las **consultas** de solo lectura a través de manejadores separados,
> de modo que ambas mitades puedan evolucionar, escalar y optimizarse de forma
> independiente: lecturas cacheadas, escrituras validadas. El análogo en Spring
> es una aplicación CQRS dividida en componentes `@CommandHandler` /
> `@QueryHandler` (por ejemplo, tal como los nombra Axon Framework).

> **Note** **Término clave — mensaje.** Un *mensaje* es el valor tipado que le
> entregas al bus: un comando (que muta) o una consulta (que lee). Cada mensaje
> de Lumen es un struct serializable simple. En términos de Spring/Axon, un
> mensaje es el DTO de comando o de consulta que envías (`send`) o consultas
> (`query`) a través de un gateway.

> **Note** **Término clave — bus.** El *bus* es el despachador de
> comandos/consultas de Firefly. Empareja cada mensaje con exactamente un
> manejador mediante `std::any::TypeId`, lo hace pasar por una cadena de
> middleware y devuelve el resultado del manejador. El análogo en Spring/Axon es
> el `CommandGateway` / `QueryGateway`, salvo que aquí es un único `Arc<Bus>`
> en proceso que proporciona el framework.

> **Note** **Término clave — bean de manejadores.** Un *bean de manejadores* es
> un bean de inyección de dependencias ordinario cuyos métodos atienden comandos
> y consultas. Sus colaboradores llegan por inyección de constructor, y el
> framework registra cada método en el bus al arrancar. Esto es el `@Component`
> de Spring que lleva métodos `@CommandHandler` / `@QueryHandler`.

> **Note** **Término clave — middleware.** Un *middleware* envuelve cada despacho
> con comportamiento transversal —validación, caché, correlación— antes y
> después de que el manejador se ejecute. El análogo en Spring es un
> `HandlerInterceptor` o un `MessageHandlerInterceptor` de Axon. Firefly instala
> por ti una pequeña cadena por defecto.

> **Design note.** Todo el camino es Rust ordinario: sin proxies, sin reflexión,
> solo un registro tipado indexado por `TypeId` y una llamada a método. Los
> manejadores de Lumen viven en un bean de inyección de dependencias
> (`#[derive(Service)]` + `#[handlers]`), de modo que cada uno alcanza a sus
> colaboradores a través de `self.<campo autowired>`. Una aplicación más simple
> puede escribir un manejador como un `async fn` libre en su lugar; la
> [alternativa de fn libre](#step-4--know-the-free-fn-handler-alternative) más
> abajo cubre esa forma.

## Paso 1 — Entender el trait `Message`

**Acción.** Antes de escribir ningún mensaje, observa el contrato que satisface
todo comando y consulta. Cada mensaje implementa `Message`. Nunca escribirás
este impl a mano —los derives lo generan—, pero conocer su forma explica a qué
reacciona el middleware:

```rust,ignore
pub trait Message: Clone + Serialize + Send + Sync + 'static {
    fn kind() -> MessageKind { MessageKind::Command }   // Command / Query split
    fn validate(&self) -> Result<(), CqrsError> { Ok(()) }   // ValidationMiddleware
    fn cache_ttl(&self) -> Option<Duration>     { None }     // QueryCache
}
```

**Qué acaba de ocurrir.** Los *supertraits* del trait declaran lo que un mensaje
debe ser, y sus *métodos* son valores por defecto sobreescribibles que el
middleware correspondiente recoge automáticamente:

- `Clone` representa la invocación del manejador por valor, y `Serialize` siembra
  la clave de caché de consulta (la caché calcula el hash del JSON del mensaje).
- `kind()` reporta si el mensaje es un comando o una consulta. El valor por
  defecto es `MessageKind::Command`; `#[derive(Query)]` lo sobreescribe.
- `validate()` es el hook de validación previa al despacho que llama el
  `ValidationMiddleware`. El valor por defecto acepta todo, así que un mensaje
  simple pasa intacto.
- `cache_ttl()` es la suscripción a la caché que lee el middleware `QueryCache`.
  El valor por defecto `None` significa "no cacheable", así que los comandos
  atraviesan la caché directamente.

> **Note** **Término clave — `MessageKind`.** Un enum de dos variantes,
> `MessageKind::Command` / `MessageKind::Query`, que registra la naturaleza de
> escritura/lectura de un tipo de mensaje. El bus almacena el kind de cada
> manejador en el momento del registro para poder listar comandos y consultas
> por separado: esa es la segregación en "Segregación de responsabilidad entre
> comandos y consultas".

> **Tip** **Punto de control.** Deberías ser capaz de decir, de un tirón, para
> qué sirve cada uno de los tres métodos: `kind()` separa comando de consulta,
> `validate()` controla el despacho, `cache_ttl()` suscribe una consulta a la
> caché. El resto del capítulo consiste sobre todo en hacer que los derives los
> rellenen por ti.

## Paso 2 — Definir los comandos y la consulta de Lumen

**Acción.** Crea `src/commands.rs`. Los cuatro mensajes son structs simples que
llevan `#[derive(Command)]` / `#[derive(Query)]`, los cuales generan el impl de
`Message`. El atributo de campo `#[firefly(validate)]` hace que un campo sea
obligatorio (el `validate()` generado rechaza un `String` vacío o un número no
positivo), y `#[firefly(cache_ttl = "...")]` se refleja en el `cache_ttl`
generado de la consulta:

```rust,ignore
// samples/lumen/src/commands.rs
use std::sync::Arc;

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Wallet, WalletView};
use crate::ledger::{Ledger, ReadModel};
use crate::money::Money;

/// `POST /api/v1/wallets` command — open a new wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command, Builder, Schema)]
#[serde(default)]
pub struct OpenWallet {
    /// The wallet owner's display name — required.
    #[firefly(validate)]
    #[builder(into)]
    pub owner: String,
    /// The opening balance, in minor units (cents); must be `>= 0`.
    #[serde(rename = "openingBalance")]
    #[builder(default)]
    pub opening_balance: i64,
}

/// `POST /api/v1/wallets/:id/deposit` command — credit a wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct Deposit {
    /// The wallet to credit — required.
    #[firefly(validate)]
    #[serde(rename = "walletId")]
    pub wallet_id: String,
    /// The amount to credit, in minor units (cents); must be `> 0`.
    #[firefly(validate)]
    pub amount: i64,
}

/// `POST /api/v1/wallets/:id/withdraw` command — debit a wallet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct Withdraw {
    #[firefly(validate)]
    #[serde(rename = "walletId")]
    pub wallet_id: String,
    #[firefly(validate)]
    pub amount: i64,
}

/// `GET /api/v1/wallets/:id` query — cached for 30 seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    /// The wallet id to fetch.
    pub id: String,
}
```

**Qué acaba de ocurrir.** Tres derives hacen el trabajo pesado:

- `#[derive(Command)]` / `#[derive(Query)]` generan el impl de `Message` de cada
  struct. `Command` mantiene el `kind()` por defecto de `MessageKind::Command`;
  `Query` lo sobreescribe a `MessageKind::Query`. Esa única diferencia es toda la
  división CQRS: `OpenWallet` / `Deposit` / `Withdraw` se registran como comandos
  y `GetWallet` se registra como consulta, sin anotación adicional.
- `#[firefly(validate)]` en un campo lo hace obligatorio: el `validate()`
  generado rechaza un `String` vacío o un número no positivo *en código generado
  en tiempo de compilación*, no mediante reflexión en tiempo de ejecución. En
  `Deposit::amount` rechaza una cantidad cero o negativa antes de que el
  manejador se ejecute siquiera, de modo que el agregado nunca llega a invocarse
  con datos estructuralmente erróneos.
- `#[firefly(cache_ttl = "30s")]` en `GetWallet` se refleja en el `cache_ttl()`
  generado, que el middleware `QueryCache` lee del mensaje para memoizar las
  lecturas durante 30 segundos.

Unas pocas decisiones se hacen eco del capítulo de dominio. Los comandos llevan
céntimos como `i64`, no un objeto de valor `Money`: el manejador construye
`Money`, manteniendo el contrato de transmisión como un número desnudo y la
validación simple. Y `#[serde(rename = ...)]` mantiene el JSON en camelCase
(`openingBalance`, `walletId`) mientras los campos de Rust permanecen en
snake_case.

> **Note** `OpenWallet` también deriva `Builder` (el `@Builder` de Lombok) y
> `Schema` (que alimenta la documentación OpenAPI). `Builder` le da un
> constructor fluido —`OpenWallet::builder().owner("ada").build()`— con
> `opening_balance` por defecto a cero. Ninguno de los dos derives afecta al
> comportamiento CQRS; van de acompañantes porque `OpenWallet` es también un
> cuerpo de petición.

> **Tip** **Punto de control.** `cargo build` compila `src/commands.rs`. El
> comportamiento de validación y caché es testeable sin un bus, porque los
> derives colocan los métodos en el propio tipo:
>
> ```rust,ignore
> assert!(OpenWallet::default().validate().is_err());   // empty owner rejected
> assert!(Deposit { wallet_id: "wlt_1".into(), amount: 0 }.validate().is_err());
> assert!(GetWallet::default().cache_ttl().is_some());  // the 30s TTL
> ```

## Paso 3 — Escribir el bean de manejadores

**Acción.** Añade el bean de manejadores a `src/commands.rs`. Los manejadores de
Lumen viven en un **bean de inyección de dependencias**, el análogo en Rust de
un `@Component` de Spring que lleva métodos `@CommandHandler` / `@QueryHandler`.
`WalletHandlers` es un `#[derive(Service)]` cuyos colaboradores —el `Ledger` del
lado de escritura y el `ReadModel` del lado de lectura— se obtienen con
`#[autowired]` desde el contenedor. La macro a nivel de impl `#[handlers]` (la
hermana CQRS de `#[rest_controller]`) marca los métodos: cada
`#[command_handler]` / `#[query_handler]` es un `async fn(&self, msg) ->
Result<.., CqrsError>`, de modo que un manejador alcanza a sus colaboradores a
través de `self`: sin globales de proceso, sin raíz de composición:

```rust,ignore
// samples/lumen/src/commands.rs (continued)

/// Maps a `DomainError` onto the bus's `CqrsError` channel. The web layer
/// restores the precise HTTP status from the detail message.
fn to_cqrs(e: DomainError) -> CqrsError {
    CqrsError::handler(e.to_string())
}

/// The CQRS **handler bean** — Spring's `@Component` command/query handler. Its
/// collaborators are `#[autowired]` from the DI container; `#[handlers]`
/// registers each method on the bus.
#[derive(Service)]
struct WalletHandlers {
    /// The write-side application service (autowired).
    #[autowired]
    ledger: Arc<Ledger>,
    /// The read-side projection store the `GetWallet` query reads (autowired).
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletHandlers {
    /// Handles `OpenWallet`.
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

    /// Handles `Deposit`.
    #[command_handler]
    async fn deposit(&self, cmd: Deposit) -> Result<WalletView, CqrsError> {
        self.ledger
            .deposit(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }

    /// Handles `Withdraw`.
    #[command_handler]
    async fn withdraw(&self, cmd: Withdraw) -> Result<WalletView, CqrsError> {
        self.ledger
            .withdraw(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }

    /// Handles `GetWallet` — serve from the projected read model, falling back
    /// to folding the event stream when the projection has not yet caught up.
    #[query_handler]
    async fn get_wallet(&self, q: GetWallet) -> Result<WalletView, CqrsError> {
        if let Some(view) = self.read_model.find(&q.id) {
            return Ok(view);
        }
        let events = self.ledger.load_events(&q.id).await.map_err(to_cqrs)?;
        Ok(Wallet::rehydrate(&q.id, &events).view())
    }
}
```

**Qué acaba de ocurrir.** Cada manejador de comando construye el objeto de valor
`Money` a partir del `i64` del comando, delega en el servicio de aplicación
`Ledger` autowired (que rehidrata el agregado, ejecuta el comando de dominio y
persiste; véase [Event Sourcing](./11-event-sourcing.md)), y mapea un
`DomainError` sobre el canal `CqrsError` del bus mediante `to_cqrs`.

La consulta `get_wallet` es el patrón de lectura-tras-escritura en miniatura:
sirve primero desde el `ReadModel` proyectado, y *solo* si la proyección aún no
se ha puesto al día recurre a replegar el flujo de eventos
(`Wallet::rehydrate(..).view()`). Ese repliegue es lo que evita que una lectura
inmediatamente posterior a una escritura devuelva un saldo obsoleto bajo la
consistencia eventual que introduce la proyección.

> **Note** Un método `#[handlers]` toma `&self` más exactamente un argumento de
> mensaje y devuelve un `Result<.., CqrsError>`. Como el bean es un bean
> ordinario del contenedor, sus colaboradores llegan por **inyección de
> constructor** a través de campos `#[autowired]`: el mismo cableado que usa
> cualquier otro bean de Firefly, sin un global de proceso que sembrar. Añadir un
> manejador es añadir un método; el framework lo encuentra.

**Por qué importa.** Tras la macro, cada `#[command_handler]` /
`#[query_handler]` envía un `BeanHandlerRegistration` a un registro `inventory`
de tiempo de compilación. Al arrancar, `FireflyApplication` resuelve
`WalletHandlers` desde el contenedor —cableando su `Ledger` + `ReadModel`
`#[autowired]`— e instala un cierre de bus que captura el bean resuelto, de modo
que cada despacho llama a `self.open_wallet(..)` y compañía. Lumen no escribe
**ninguna** llamada de registro: el framework vierte los manejadores del bean por
ti (Paso 5).

> **Tip** **Punto de control.** `cargo build` sigue compilando. Puedes ejercitar
> el bean directamente sin HTTP y sin bus, construyéndolo con los mismos
> colaboradores que el contenedor inyectaría:
>
> ```rust,ignore
> let handlers = WalletHandlers {
>     ledger: Arc::new(Ledger::new(
>         Arc::new(MemoryEventStore::new()),
>         Arc::new(InMemoryBroker::new()),
>     )),
>     read_model: Arc::new(ReadModel::default()),
> };
> let opened = handlers
>     .open_wallet(OpenWallet { owner: "alice".into(), opening_balance: 100 })
>     .await
>     .unwrap();
> assert_eq!(opened.balance, 100);
> ```

## Paso 4 — Conocer la alternativa del manejador como `fn` libre

**Acción.** Nada que escribir para Lumen aquí, pero vale la pena conocer la
segunda forma, porque una aplicación más simple recurre a ella. Un manejador no
tiene por qué ser un bean. La forma de `fn` libre es la opción natural para un
manejador *sin colaboradores* (el sample `macro-quickstart` del framework la
usa): marca un `async fn(msg) -> Result<R, CqrsError>` libre con
`#[command_handler]` / `#[query_handler]`:

```rust,ignore
// The simpler form — a free fn with no collaborators to inject.
#[command_handler]
pub async fn place_order(cmd: PlaceOrder) -> Result<OrderView, CqrsError> {
    Ok(OrderView::from(cmd))
}
```

**Qué acaba de ocurrir.** La macro lee el tipo del argumento (`PlaceOrder`) como
clave de despacho, genera un helper `register_place_order(bus)` **y** envía un
`HandlerRegistration` al registro `inventory` que el framework vierte, de modo
que el manejador como fn libre se descubre e instala exactamente igual que la
forma de bean.

**Por qué importa.** Como una función libre no puede poseer un `Ledger` ni un
`ReadModel`, esta forma encaja con manejadores que computan puramente a partir
del mensaje (o que alcanzan un global de proceso). En el momento en que un
manejador necesita colaboradores inyectados —como *todos* los de Lumen— la forma
de bean del Paso 3 es la opción natural: obtiene la inyección de constructor
gratis y mantiene el manejador como un método simple sobre un `@Component`.
Lumen solo tiene manejadores de bean; el camino de fn libre no vierte ninguno
propio.

## Paso 5 — Dejar que el framework cablee el bus

**Acción.** De nuevo, no hay código de cableado que escribir: ese es el objetivo.
El `Bus` y el `QueryCache` se declaran como `#[bean]`s en `LumenBeans` (el
contenedor `#[derive(Configuration)]` en `src/web.rs`), el controlador
`WalletApi` autowire el `Arc<Bus>`, y `FireflyApplication` hace el resto al
arrancar. La caché de consulta es una fábrica `#[bean]` sencilla:

```rust,ignore
// samples/lumen/src/web.rs — LumenBeans (#[derive(Configuration)]).
#[bean]
impl LumenBeans {
    /// The read-side query cache honouring `GetWallet`'s 30s TTL (`@Bean`).
    #[bean]
    fn query_cache(&self) -> QueryCache {
        QueryCache::new()
    }
    // ... event_store, jwt_service, ledger, security beans ...
}
// The read store is *not* a `#[bean]` here — `ReadModel` is its own bean,
// registered by the scan directly.
```

**Qué acaba de ocurrir.** Al arrancar, `FireflyApplication`:

- **Vierte los manejadores de bean descubiertos** con
  `firefly::cqrs::register_discovered_handler_beans(&bus, &container)`: resuelve
  `WalletHandlers` desde el contenedor —cableando su `Ledger` + `ReadModel`— e
  instala cada método `#[command_handler]` / `#[query_handler]` sobre el bus.
- **Vierte cualquier manejador como `fn` libre** con
  `firefly::cqrs::register_discovered_handlers(&bus)`, de modo que ambas formas
  coexisten (Lumen solo tiene manejadores de bean, así que esto no vierte ninguno
  propio).
- **Instala automáticamente la cadena de middleware del bus**: validación
  (instalada primero por el núcleo), luego un propagador de correlación, y
  después el middleware de caché de lectura `QueryCache` siempre que haya un bean
  `QueryCache` presente.

Lumen no llama a ninguno de estos vertidos. Conceptualmente, el framework
ejecuta:

```rust,ignore
// What FireflyApplication does for you — no Lumen code calls this.
firefly::cqrs::register_discovered_handlers(&bus);                  // free-fn handlers
firefly::cqrs::register_discovered_handler_beans(&bus, &container); // WalletHandlers' 4 methods
```

> **Note** **¿De dónde sale el `Bus`?** Es un bean de infraestructura
> proporcionado por el framework: el núcleo registra un `Arc<Bus>` en el
> contenedor antes del escaneo, de modo que el controlador `WalletApi` puede
> autowirearlo (`#[autowired] pub bus: Arc<Bus>`) y el framework puede verter los
> manejadores descubiertos sobre él. Tú declaras los beans de *aplicación*
> (`QueryCache`, el ledger); el bus se cablea por ti.

**Por qué importa.** Los manejadores de bean se resuelven desde el *mismo*
contenedor que construye el controlador y la saga, de modo que cada colaborador
—manejador, controlador, proyección— comparte el único `Ledger` y el único
`ReadModel` que el contenedor mantiene. No hay una segunda copia del modelo de
lectura que pueda desincronizarse.

En la cadena de despacho se incluyen tres entradas de middleware. El framework
las instala automáticamente (una cuarta, autorización, llega en el borde HTTP
con [Seguridad](./14-security.md)). El middleware se ejecuta con el primero
registrado = más externo:

| Middleware                  | Comportamiento                                                      |
|-----------------------------|---------------------------------------------------------------|
| `ValidationMiddleware`      | llama a `Message::validate` antes del despacho, cortocircuita ante un error; instalado primero por el núcleo, así que es el más externo |
| `CorrelationMiddleware`     | asegura-o-genera el id de correlación para el despacho (siguiente paso) |
| `QueryCache::middleware()`  | memoiza los resultados de mensajes cuyo `cache_ttl` es `Some`; instalado cuando existe un bean `QueryCache` |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 300" role="img"
     aria-label="CQRS dispatch: a message is matched to a handler by TypeId, passes the Validation, Correlation and QueryCache middleware chain with validation outermost, then reaches your command or query handler"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="280.0" y="24.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">send / query a message</text>
<line x1="280.0" y1="30.0" x2="280.0" y2="46.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,54.0 275.5,46.0 284.5,46.0" fill="#b5531f"/>
<rect x="180.0" y="58.5" width="200.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="180.0" y="56.0" width="200.0" height="46.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="76.0" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">msg ↦ TypeId</text><text x="280.0" y="90.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">matched to a handler</text>
<text x="280.0" y="120.0" text-anchor="middle" font-size="11.5" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">middleware chain</text>
<line x1="280.0" y1="102.0" x2="280.0" y2="122.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,130.0 275.5,122.0 284.5,122.0" fill="#b5531f"/>
<rect x="96.0" y="142.5" width="60.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="96.0" y="140.0" width="60.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="126.0" y="170.5" text-anchor="middle" font-size="18" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">V</text>
<line x1="156.0" y1="166.0" x2="176.0" y2="166.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="184.0,166.0 176.0,170.5 176.0,161.5" fill="#b5531f"/>
<rect x="184.0" y="142.5" width="60.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="184.0" y="140.0" width="60.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="214.0" y="170.5" text-anchor="middle" font-size="18" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">C</text>
<line x1="244.0" y1="166.0" x2="264.0" y2="166.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="272.0,166.0 264.0,170.5 264.0,161.5" fill="#b5531f"/>
<rect x="272.0" y="142.5" width="60.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="272.0" y="140.0" width="60.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="302.0" y="170.5" text-anchor="middle" font-size="18" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Q</text>
<text x="280.0" y="212.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">V = Validation   ·   C = Correlation   ·   Q = QueryCache</text>
<line x1="280.0" y1="222.0" x2="280.0" y2="240.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,248.0 275.5,240.0 284.5,240.0" fill="#b5531f"/>
<rect x="190.0" y="252.5" width="180.0" height="44.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="190.0" y="250.0" width="180.0" height="44.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="280.0" y="269.0" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">your handler</text><text x="280.0" y="283.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Command or Query</text>
</svg>
<figcaption>Un mensaje se empareja con su manejador mediante <code>TypeId</code>, y luego recorre la cadena de middleware registrada —<code>Validation</code> como la más externa, después <code>Correlation</code>, después <code>QueryCache</code>— antes de que el manejador se ejecute. El ámbito de correlación se abre antes de la capa de caché, de modo que todo lo que esta registra lleva el id.</figcaption>
</figure>

> **Tip** **Punto de control.** `cargo run` arranca Lumen y la línea de CQRS del
> informe de arranque cuenta tus manejadores: tres comandos y una consulta. La
> vista de admin `/cqrs` en el puerto de gestión (`:8081`) los lista, con badge
> azul para los comandos y verde para las consultas.

## Paso 6 — Ver cómo el bus segrega comandos y consultas

**Acción.** Observa cómo el bus mantiene separadas ambas mitades, aunque
compartan un único registro. El bus despacha comandos y consultas a través de un
único registro indexado por `TypeId`, pero no los trata como intercambiables:
cada manejador registrado lleva el **kind** del mensaje que atiende, expuesto
como `Message::kind() -> MessageKind`:

```rust,ignore
pub enum MessageKind { Command, Query }
```

**Qué acaba de ocurrir.** El valor por defecto es `MessageKind::Command`.
`#[derive(Command)]` mantiene ese valor por defecto; `#[derive(Query)]`
sobreescribe `kind()` para devolver `MessageKind::Query`. Nada en el
`src/commands.rs` de Lumen cambia: `OpenWallet` / `Deposit` / `Withdraw` ya son
comandos y `GetWallet` ya es una consulta, así que la segregación se desprende
de los derives que introdujo el Paso 2. El bus registra el kind de cada mensaje
en el momento del registro y te permite preguntar por ambas mitades por
separado:

```rust,ignore
use firefly::cqrs::{Bus, MessageKind};

let bus = Bus::new();
// In a unit test you populate a bus explicitly; the app boot resolves the
// `WalletHandlers` bean and drains its methods with
// `register_discovered_handler_beans(&bus, &container)`.
bus.register(|cmd: OpenWallet| async move { /* ... */ });   // three commands + one query

// Inspect the registry, split by CQRS kind.
let commands = bus.command_handler_names();      // ["...::Deposit", "...::OpenWallet", "...::Withdraw"]
let queries  = bus.query_handler_names();        // ["...::GetWallet"]
assert_eq!(bus.handler_count(), 4);

// The general form both of the above delegate to:
assert_eq!(bus.handler_names_by_kind(MessageKind::Query), queries);

// Type-level membership and removal.
assert!(bus.has_handler::<GetWallet>());
assert!(bus.unregister::<GetWallet>());          // true — one was present
assert!(!bus.has_handler::<GetWallet>());
```

`command_handler_names()` y `query_handler_names()` son envoltorios finos sobre
`handler_names_by_kind(MessageKind)`, cada uno devolviendo los nombres de tipo
completamente cualificados ordenados alfabéticamente: la misma lista que devuelve
`handler_names()`, pero filtrada a un solo kind. `handler_count()` es el tamaño
total del registro; `has_handler::<C>()` comprueba la pertenencia de un tipo de
mensaje; y `unregister::<C>()` elimina un manejador, devolviendo si había uno
presente (útil cuando un test quiere intercambiar un manejador sin reconstruir
el bus).

**Por qué importa.** Esto es exactamente lo que consume la vista de admin
`/cqrs`: como el bus conoce el kind de cada manejador, el panel etiqueta cada
registro con un badge (comandos en azul, consultas en verde) y muestra recuentos
separados de comandos/consultas, en lugar de una sola lista indiferenciada de
manejadores.

> **Note** Firefly mantiene un único `Bus` y recupera la división
> comando/consulta del `kind()` de cada mensaje (fijado por el derive `Command` /
> `Query`), en lugar de hacerlo a partir de dos buses distintos.
> `command_handler_names()` / `query_handler_names()` son las vistas filtradas
> que renderiza el panel de admin `/cqrs`; `has_handler::<C>()` /
> `unregister::<C>()` comprueban la pertenencia y eliminan un manejador por tipo.

## Paso 7 — Seguir el id de correlación a través del límite de despacho

**Acción.** Entiende el middleware que mantiene rastreable una petición lógica.
Un comando rara vez actúa solo. `bus.send(Deposit { .. })` ejecuta un manejador
que puede iniciar la saga de transferencia ([Sagas](./12-sagas.md)) o lanzar una
tarea de seguimiento con `tokio::spawn`, y cada una de ellas abandona la tarea
de la petición original. Para que los logs y las trazas se lean como *una* sola
operación, todas deben compartir un único id de correlación.

> **Note** **Término clave — id de correlación.** Un único identificador estampado
> en todo lo que se hace para una petición lógica, de modo que sus logs y trazas
> puedan unirse. Firefly lo enhebra a través de un task-local; la capa web fija
> uno por petición HTTP. El análogo en Spring es el `traceId` del MDC propagado
> por Sleuth / Micrometer Tracing.

`firefly::cqrs::CorrelationMiddleware` lo hace cumplir en el límite de despacho.
El framework lo instala en cada bus de `FireflyApplication`, entre las capas de
validación y de caché de consulta, de modo que nunca lo cableas a mano. Si
construyes un bus tú mismo, añádelo como cualquier otro middleware:

```rust,ignore
use firefly::cqrs::{Bus, CorrelationMiddleware};

let bus = Bus::new();
bus.use_middleware(CorrelationMiddleware::new());   // earlier-registered = more outer
```

**Qué acaba de ocurrir.** En cada despacho el middleware **asegura-o-genera** un
id de correlación: si la petición ya se está ejecutando bajo uno —la capa de
correlación de `firefly-web` fija un id task-local por petición HTTP— reutiliza
ese id, de modo que el comando y la saga/tarea lanzada que dispara trazan todos
al mismo valor. Si no hay ningún id ambiente presente (un trabajo en segundo
plano, un test, un despacho interno), genera uno nuevo para el lapso de ese
despacho y restaura el ámbito previo a la salida, de modo que operaciones
hermanas nunca se filtran ids entre sí.

```rust,ignore
// Inside a handler (or anything it calls), the id is observable:
let trace = firefly_kernel::correlation_id();   // Some(<id>) under the middleware
```

**Por qué importa.** En el bus de Lumen, el framework instala primero
`ValidationMiddleware` (así que es el más externo), luego `CorrelationMiddleware`
y después `QueryCache`. El mismo id que la capa HTTP estampó en
`POST /wallets/:id/deposit` fluye hacia el manejador `Deposit`, hacia la saga de
transferencia que pueda iniciar, y hacia los eventos que la saga publica, sin
que ningún manejador toque el id de forma explícita. Como la correlación se sitúa
por delante de `QueryCache` en la cadena, el ámbito de correlación ya está
abierto antes de que se ejecute la capa de caché, de modo que cualquier cosa que
la caché registre lleva el id también:

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 420 320" role="img"
     aria-label="The CQRS bus dispatch with validation outermost: a message is matched by TypeId, passes the Validation, Correlation and QueryCache chain, then reaches your handler"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <text x="210" y="20" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">send / query a message</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="28" x2="210" y2="46"/><polygon points="210,54 206,46 214,46"/>
  </g>
  <rect x="130" y="56" width="160" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="80" text-anchor="middle" font-size="12" fill="#3a2a1c"
        font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">msg &#8614; TypeId</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="94" x2="210" y2="112"/><polygon points="210,120 206,112 214,112"/>
  </g>
  <text x="210" y="136" text-anchor="middle" font-size="11.5" font-weight="600" fill="#7a6450">middleware chain</text>
  <g>
    <rect x="80" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="103" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">V</text>
    <rect x="187" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="210" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">C</text>
    <rect x="294" y="146" width="46" height="44" rx="8" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
    <text x="317" y="174" text-anchor="middle" font-size="15" font-weight="700" fill="#2a1d10"
          font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Q</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="126" y1="168" x2="181" y2="168"/><polygon points="187,168 179,164 179,172"/>
    <line x1="233" y1="168" x2="288" y2="168"/><polygon points="294,168 286,164 286,172"/>
  </g>
  <g font-size="10.5" fill="#7a6450">
    <text x="80" y="208">V = ValidationMiddleware   C = Correlation   Q = QueryCache</text>
  </g>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="222" x2="210" y2="264"/><polygon points="210,272 206,264 214,264"/>
  </g>
  <rect x="140" y="274" width="140" height="38" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="298" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">your handler</text>
</svg>
<figcaption>El framework registra primero <code>ValidationMiddleware</code> (la más externa), luego <code>CorrelationMiddleware</code> y después <code>QueryCache</code>: el ámbito de correlación se abre antes de que se ejecute la capa de caché, de modo que todo lo que esta registra lleva el id.</figcaption>
</figure>

> **Design note.** `CorrelationMiddleware` asegura que una petición lógica
> mantenga un único id de correlación a través del límite del comando y de
> cualquier saga o continuación lanzada con `tokio::spawn` que dispare: reutiliza
> un id ambiente cuando está presente (la capa web fija uno por petición HTTP) y
> genera uno en caso contrario, restaurando el ámbito previo a la salida. Firefly
> enhebra el id a través de un task-local que este middleware acota por despacho,
> de modo que un manejador nunca tiene que pasarlo a mano.

## Paso 8 — Despachar desde el controlador

**Acción.** Cablea la superficie HTTP al bus. El `#[rest_controller]`
(construido en [Tu primera API HTTP](./06-first-http-api.md)) mantiene el `Bus` y
despacha a través de `send` / `query`. `Bus::query` es un sinónimo legible de
`send`. Un despacho fallido es un `CqrsError`, que la capa web mapea al estado
RFC 9457 correcto:

```rust,ignore
// samples/lumen/src/web.rs — WalletApi handlers.
#[post("/wallets")]
async fn open(
    State(api): State<WalletApi>,
    Json(body): Json<OpenWallet>,
) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
    let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
    Ok((axum::http::StatusCode::CREATED, Json(view)))
}

#[get("/wallets/:id")]
async fn get(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
) -> WebResult<Json<WalletView>> {
    let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
    Ok(Json(view))
}
```

**Qué acaba de ocurrir.** `api.bus.send(body)` empareja el tipo de `body`
(`OpenWallet`) con el manejador de comando `open_wallet` y lo hace pasar por la
cadena de middleware; `api.bus.query(GetWallet { id })` hace lo mismo para la
consulta. El controlador autowire el `Arc<Bus>` (`#[autowired] pub bus:
Arc<Bus>`), de modo que `api.bus` ya tiene un receptor: sin estado construido a
mano.

`cqrs_to_web` es la costura donde un fallo de dominio se convierte en un estado
HTTP. Lee el `CqrsError` y su cadena de detalle —que, recordemos, es el texto
estable de `Display` del `DomainError` del capítulo anterior— y elige el estado:

```rust,ignore
// samples/lumen/src/web.rs
fn cqrs_to_web(err: CqrsError) -> WebError {
    match err {
        CqrsError::Validation(detail) => WebError::from(FireflyError::validation(detail)),
        CqrsError::Handler(detail) => {
            if detail.ends_with("not found") {
                WebError::from(FireflyError::not_found(detail))            // 404
            } else if detail == DomainError::InsufficientFunds.to_string()
                || detail == DomainError::NonPositiveAmount.to_string()
                || detail == DomainError::OwnerRequired.to_string()
            {
                WebError::from(FireflyError::validation(detail))           // 422
            } else {
                WebError::from(FireflyError::not_found(detail))
            }
        }
        other => WebError::from(FireflyError::internal(other.to_string())), // 500
    }
}
```

**Por qué importa.** Por esto el capítulo de dominio insistió en que las cadenas
de `Display` fueran *estables*: son el contrato con el que `cqrs_to_web` hace
coincidencia para recuperar el estado preciso. Un `CqrsError` de validación se
convierte en un problema 422; un detalle de manejador "not found" se convierte
en un 404; un detalle de fondos insuficientes o de cantidad no positiva se
convierte en un 422; cualquier otra cosa cae a un 500, todo renderizado como RFC
9457 `application/problem+json`.

> **Tip** **Punto de control.** Con `cargo run` levantado, abre una cartera y
> vuelve a leerla:
>
> ```bash
> curl -s -XPOST localhost:8080/api/v1/wallets \
>   -H 'content-type: application/json' \
>   -d '{"owner":"alice","openingBalance":100}'
> # 201 with {"id":"...","owner":"alice","balance":100}
>
> curl -s -XPOST localhost:8080/api/v1/wallets \
>   -H 'content-type: application/json' -d '{"owner":""}'
> # 422 problem+json — the empty owner failed the #[firefly(validate)] check
> ```

## Paso 9 — Mantener frescas las lecturas tras una escritura

**Acción.** Cierra la brecha de lectura-tras-escritura. `GetWallet` se cachea
durante 30 segundos. Sin cuidado, un depósito actualizaría el saldo mientras un
`GetWallet` cacheado seguiría sirviendo el antiguo hasta durante 30 segundos.
Lumen invalida la familia de consultas cacheada tras cada mutación:

```rust,ignore
// samples/lumen/src/web.rs — deposit handler.
#[post("/wallets/:id/deposit")]
async fn deposit(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    Json(body): Json<AmountBody>,
) -> WebResult<Json<WalletView>> {
    let cmd = Deposit { wallet_id: id, amount: body.amount };
    let view: WalletView = api.bus.send(cmd).await.map_err(cqrs_to_web)?;
    api.query_cache.invalidate_type::<GetWallet>();   // read-after-write
    Ok(Json(view))
}
```

**Qué acaba de ocurrir.** Aquí es donde `WalletApi` adquiere el campo que [Tu
primera API HTTP](./06-first-http-api.md) aplazó: junto a `bus`, el controlador
**autowire** el `Arc<QueryCache>` del contenedor (`#[autowired] pub query_cache:
Arc<QueryCache>`), de modo que `api.query_cache` tiene un receptor. El mismo bean
`QueryCache` que el framework registra como middleware del bus es el que el
controlador invalida: una sola caché, leída por el bus e invalidada por el
manejador.

`QueryCache::invalidate_type::<GetWallet>()` desaloja todo resultado cacheado de
exactamente ese tipo de consulta. El manejador de retirada hace lo mismo, y la
saga de transferencia ([Sagas](./12-sagas.md)) —que toca dos carteras— invalida
toda la familia `GetWallet`.

**Por qué importa.** La consistencia lectura-tras-escritura vive en el *límite
del bus*, no dentro del manejador. El manejador computa el nuevo estado; el
controlador, tras haber mutado, desaloja la caché para que el siguiente
`GetWallet` recompute. El intercambio de backend de la caché de consulta (Redis
/ Postgres) y la invalidación dirigida por eventos reciben su propio tratamiento
en [Caché](./17-caching.md); aquí, lo importante es que una mutación y su
desalojo de caché se sitúan codo con codo en el camino de escritura.

> **Tip** **Punto de control.** Deposita en la cartera que abriste, luego vuelve
> a leerla: el nuevo saldo llega de inmediato, aunque `GetWallet` esté cacheado
> durante 30 segundos, porque el manejador de depósito desalojó la entrada
> cacheada:
>
> ```bash
> curl -s -XPOST localhost:8080/api/v1/wallets/<id>/deposit \
>   -H 'content-type: application/json' -d '{"amount":50}'
> curl -s localhost:8080/api/v1/wallets/<id>   # balance reflects the deposit
> ```

## Paso 10 — Despachar de forma reactiva (opcional)

**Acción.** Cuando quieras un resultado perezoso y componible, usa la superficie
reactiva del bus. El bus envuelve el resultado eventual en un `Mono<R>` perezoso:
la misma búsqueda de manejador, la misma cadena de middleware, ejecutada solo
cuando el `Mono` se suscribe, se bloquea o se espera (await). Estos métodos toman
`&Arc<Bus>` para que el `Mono` pueda poseer el bus:

| Método                          | Devuelve       |
|---------------------------------|---------------|
| `Bus::send_mono(cmd)`           | `Mono<R>`     |
| `Bus::query_mono(q)`            | `Mono<R>`     |
| `Bus::send_mono_with_context`   | `Mono<R>`     |
| `Bus::query_mono_with_context`  | `Mono<R>`     |

Las variantes `*_with_context` llevan un `ExecutionContext` explícito al despacho
—el id de correlación, el tenant y el principal autenticado— para cuando un
`Mono` se compone fuera del ámbito task-local que establece la capa HTTP (un
trabajo en segundo plano o un pipeline reactivo ensamblado antes de que el
contexto de la petición esté en juego). Los `send_mono` / `query_mono` simples
heredan cualquier contexto que sea ambiente en el momento de la suscripción.

Un `GetWallet` reactivo, componiendo sobre el `Mono` de [El modelo
reactivo](./05-reactive-model.md):

```rust,ignore
use std::sync::Arc;
use firefly::cqrs::Bus;

let balance = bus
    .query_mono::<_, WalletView>(GetWallet { id: wallet_id })
    .map(|view| view.balance)
    .block()
    .await?;            // Some(<cents>) or None
```

**Qué acaba de ocurrir.** `query_mono` describe el despacho sin ejecutarlo;
`.map(..)` compone una transformación sobre el `Mono` aún perezoso;
`.block().await` finalmente ejecuta la cadena y produce
`Result<Option<i64>, FireflyError>`: `Some` en caso de acierto, `None` si el
`Mono` se completó vacío.

**Por qué importa.** Como `firefly-reactive` fija su canal de error a
`FireflyError`, un despacho fallido se mapea de `CqrsError` a un `FireflyError`
fiel al estado (validación → 422, manejador ausente → 500), con el `CqrsError`
original preservado como `source()`. Así, un comando reactivo fluye
directamente hacia la pila de problemas RFC 9457 sin dejar de ser inspeccionable.

## Paso 11 — Demostrar el cableado con tests

**Acción.** El `src/commands.rs` de Lumen ejercita el bean de manejadores
directamente sin HTTP: el test que se distribuye en el crate. El bean opera sobre
sus colaboradores `#[autowired]`, así que el test lo construye con el mismo
`Ledger` + `ReadModel` que el contenedor inyectaría y llama a sus métodos (el
cableado completo del bus se cubre de extremo a extremo con los tests HTTP, que
arrancan todo el `FireflyApplication`):

```rust,ignore
#[tokio::test]
async fn handler_bean_operates_on_its_autowired_collaborators() {
    let handlers = WalletHandlers {
        ledger: Arc::new(Ledger::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )),
        read_model: Arc::new(ReadModel::default()),
    };

    let opened = handlers
        .open_wallet(OpenWallet { owner: "alice".into(), opening_balance: 100 })
        .await
        .unwrap();
    assert_eq!(opened.balance, 100);

    let after = handlers
        .deposit(Deposit { wallet_id: opened.id.clone(), amount: 50 })
        .await
        .unwrap();
    assert_eq!(after.balance, 150);

    let fetched = handlers
        .get_wallet(GetWallet { id: opened.id.clone() })
        .await
        .unwrap();
    assert_eq!(fetched.id, opened.id);
}
```

**Qué acaba de ocurrir.** Como el manejador es un método simple sobre un struct
simple, el test no necesita bus ni contenedor de inyección de dependencias: solo
los colaboradores. Abre, deposita y vuelve a leer, afirmando que el saldo se
mueve como se espera.

El derive de validación también es testeable por sí solo —sin necesidad de
bus—, porque `#[derive(Command)]` genera `validate()` directamente sobre el tipo:

```rust,ignore
#[test]
fn deposit_validates_required_fields() {
    assert!(Deposit::default().validate().is_err());
    assert!(
        Deposit { wallet_id: "wlt_1".into(), amount: 0 }.validate().is_err(),
        "zero amount fails the #[firefly(validate)] check"
    );
    assert!(Deposit { wallet_id: "wlt_1".into(), amount: 10 }.validate().is_ok());
}

#[test]
fn get_wallet_carries_cache_ttl() {
    assert!(GetWallet::default().cache_ttl().is_some());
}
```

> **Tip** **Punto de control.** `cargo test` está en verde. El test del bean de
> manejadores y los tests de validación/caché pasan sin un servidor en marcha, y
> los tests de integración HTTP arrancan todo el `FireflyApplication` para cubrir
> el bus de extremo a extremo.

## Resumen — qué cambió en Lumen

Los caminos de lectura y escritura de Lumen ahora están separados, tipados y
despachados por bus:

- **`src/commands.rs`** — `OpenWallet` / `Deposit` / `Withdraw` llevan
  `#[derive(Command)]` con `#[firefly(validate)]` en los campos obligatorios;
  `GetWallet` lleva `#[derive(Query)]` con `#[firefly(cache_ttl = "30s")]`. Los
  derives generan el impl de `Message`, las comprobaciones de `validate()`, el
  `cache_ttl` de la consulta y el `kind()` de cada mensaje (la división
  comando/consulta).
- **El bean `WalletHandlers`** (`#[derive(Service)]` + `#[handlers]`) lleva los
  métodos `#[command_handler]` / `#[query_handler]` y hace `#[autowired]` del
  `Ledger` + `ReadModel`: un manejador de comandos/consultas `@Component` de
  Spring. Los manejadores de comando construyen el objeto de valor `Money` y
  delegan en `self.ledger`; la consulta sirve `self.read_model` y recurre a
  replegar el flujo para la frescura lectura-tras-escritura. Una aplicación más
  simple puede escribir un manejador sin colaboradores como un `async fn` libre
  en su lugar (se aplica la misma macro `#[command_handler]`).
- **Inyección de constructor, sin global de proceso.** El bean de manejadores
  alcanza a sus colaboradores a través de campos `#[autowired]` que el contenedor
  rellena, de modo que no hay ningún `OnceLock` que sembrar ni paso `bind`: el
  `#[bean]` `ledger` es una fábrica pura.
- **El bus** es un bean proporcionado por el framework que `WalletApi`
  autowire; el framework resuelve el bean de manejadores desde el contenedor y
  vierte sus métodos sobre el bus (`register_discovered_handler_beans`, junto al
  `register_discovered_handlers` de `fn` libre) e instala automáticamente el
  middleware de validación, correlación y `QueryCache`. El controlador despacha
  vía `bus.send` / `bus.query`, con `cqrs_to_web` mapeando un `CqrsError` (que
  lleva la cadena `Display` del dominio) al estado RFC 9457 correcto: 422 para
  reglas de negocio, 404 para no encontrado.
- **La segregación comando/consulta** se desprende de los derives: un único
  `Bus`, `command_handler_names()` / `query_handler_names()` filtrando por
  `kind()`, y el panel de admin `/cqrs` renderizando ambas mitades por separado.
- **La lectura-tras-escritura** se hace cumplir en el límite del bus:
  `query_cache.invalidate_type::<GetWallet>()` se ejecuta tras cada mutación.

Ahora también sabes que el bus expone una superficie reactiva (`send_mono` /
`query_mono`, que devuelven un `Mono<R>` perezoso) cuyo canal de error es
`FireflyError`, de modo que un despacho reactivo fluye directamente hacia la pila
de problemas RFC 9457.

## Ejercicios

1. **Observa el cortocircuito de la validación.** En un test, construye un `Bus`,
   añade `ValidationMiddleware::new()`, registra un cierre de manejador de
   `Deposit` que conduzca un `WalletHandlers` que construyas a mano, y haz
   `bus.send` de un `Deposit { wallet_id: "wlt_1".into(), amount: 0 }`. Afirma que
   el resultado es un `CqrsError::Validation` y que el ledger nunca se tocó (abre
   primero una cartera, luego deposita cero y confirma que su saldo no ha
   cambiado).

2. **Demuestra la caché, luego rómpela.** Contra el router ensamblado por el
   framework (`build_router().await`), haz `query(GetWallet { id })` dos veces y
   confirma que la segunda se sirve desde caché (instrumenta `ReadModel::find` o
   traza un contador). Deposita en la cartera, luego vuelve a hacer `query`:
   afirma que el nuevo saldo regresa, probando que `invalidate_type::<GetWallet>()`
   hizo su trabajo.

3. **Añade un comando `CloseWallet`.** Define `CloseWallet { #[firefly(validate)]
   wallet_id: String }` con `#[derive(Command)]`, luego añade un método
   `#[command_handler] async fn close_wallet(&self, cmd: CloseWallet) ->
   Result<WalletView, CqrsError>` al impl `#[handlers]` de `WalletHandlers` que
   devuelva un `WalletView`, y despáchalo. El framework resuelve el bean y vierte
   el nuevo método automáticamente: no añades ninguna llamada de registro. (Aún no
   necesitas un `close` de dominio: devolver la vista actual basta para ejercitar
   el cableado).

4. **Composición reactiva.** Reescribe el manejador del controlador `get` para
   usar `bus.query_mono::<_, WalletView>(GetWallet { id }).map(|v| v.balance)` y
   devolver solo el saldo como JSON. Fíjate en dónde el canal `FireflyError` toma
   el relevo de `CqrsError`.

5. **Inspecciona la división.** En un test, registra los cuatro manejadores de
   Lumen en un `Bus`, luego afirma que `bus.command_handler_names()` tiene tres
   entradas y `bus.query_handler_names()` tiene una. Confirma que
   `bus.handler_count()` es `4` y que `bus.has_handler::<GetWallet>()` es `true`.
   Esto es exactamente lo que renderiza el panel de admin `/cqrs`.

## Adónde ir después

El bus despacha *dentro* del servicio. Para propagar lo que ocurrió *entre*
colaboradores —la proyección del modelo de lectura, los suscriptores externos—
difunde eventos de dominio. Continúa hacia
**[Arquitectura dirigida por eventos y mensajería](./10-eda-messaging.md)**.

- Los manejadores delegan en el `Ledger`, que rehidrata el agregado y persiste
  sus eventos: esa maquinaria es **[Event Sourcing](./11-event-sourcing.md)**.
- Un comando que toca dos carteras se ejecuta como una saga compensatoria en
  **[Sagas, flujos de trabajo y TCC](./12-sagas.md)**.
- El intercambio de backend de la caché de consulta y la invalidación dirigida
  por eventos reciben su propio tratamiento en **[Caché](./17-caching.md)**.
