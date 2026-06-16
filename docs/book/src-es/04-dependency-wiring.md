# Cableado de dependencias

En el [Inicio rápido](./02-quickstart.md) escribiste un `main` de una sola línea y
viste cómo `FireflyApplication::new("lumen").run()` arrancaba un servicio entero.
Una línea de esa secuencia de arranque hizo algo que un servicio normalmente
construye a mano: ensambló el **grafo de objetos** — construyó la caché, el bus
CQRS, el almacén de eventos, el ledger y el controlador que depende de todos
ellos, en el orden correcto, y los conectó. Este capítulo trata sobre *cómo*.

La respuesta breve es que tú nunca escribes ese ensamblaje. En un servicio
Firefly **declaras** cada colaborador como un bean — una struct con un derive de
estereotipo, o un método factory —, marcas sus dependencias con `#[autowired]` y
el **escaneo de componentes** del framework descubre cada declaración en el
arranque y cablea el grafo por ti. No hay raíz de composición escrita a mano, ni
`build_app`, ni una lista de llamadas `new(...)` enhebradas a lo largo de una
función. Tú dices *qué* necesita cada pieza; el contenedor lo provee.

Aprenderemos esto del mismo modo que enseña el resto del libro: contra los beans
reales de Lumen, los que están en `samples/lumen`. Al terminar serás capaz de leer
el cableado de Lumen, ampliarlo y explicar exactamente qué hizo el framework en el
arranque. El capítulo inmediatamente siguiente,
[Inyección de dependencias y autoconfiguración](./04a-dependency-injection.md),
recorre después en profundidad toda la superficie del contenedor; este capítulo te
da el modelo mental funcional sobre el que se apoya.

Al terminar este capítulo, serás capaz de:

- Explicar qué son un **bean**, un **estereotipo** y un **escaneo de componentes**, y
  cómo reemplazan a una raíz de composición escrita a mano.
- Declarar beans de dos formas — un derive de estereotipo sobre una struct que
  posees y una factory `#[bean]` para las que no —, y saber cuándo recurrir a cada
  una.
- Usar `#[autowired]` para inyectar una sola dependencia, una colección completa,
  una opcional o un `Provider` diferido.
- Vincular un trait con su implementación mediante `provides` y desambiguar varios
  candidatos con `primary` y `order`.
- Leer el inventario de beans de Lumen en el informe de arranque y rastrear cómo
  `FireflyApplication` resuelve el grafo a partir de las declaraciones.

## Conceptos que conocerás

Antes de la primera declaración, aquí tienes las cuatro ideas en las que se apoya
este capítulo. Cada una se reintroduce en su contexto cuando se usa por primera
vez; esta es la versión breve.

> **Note** **Término clave — bean.** Un *bean* es un objeto que el framework
> construye y gestiona por ti, y luego entrega a quien lo declare como dependencia.
> Tú declaras los beans; el contenedor los descubre en el arranque y los conecta.
> Esto es exactamente la noción de Spring de un bean gestionado por el contexto de
> aplicación.

> **Note** **Término clave — inyección de dependencias (DI).** La *inyección de
> dependencias* significa que un componente no construye sus propios colaboradores
> — declara *qué* necesita y el framework se lo provee. La pieza que hace ese
> aprovisionamiento es el **contenedor de DI**. El contenedor de Firefly es el
> análogo en Rust del `ApplicationContext` de Spring.

> **Note** **Término clave — estereotipo.** Un *estereotipo* es un derive que
> pones sobre una struct para convertirla en un bean gestionado y registrar su rol
> arquitectónico — lógica de negocio, acceso a datos, capa HTTP, etcétera. Los
> cinco estereotipos de Firefly (`Service`, `Component`, `Repository`,
> `Configuration`, `Controller`) reflejan los `@Service`, `@Component`,
> `@Repository`, `@Configuration` y `@Controller` de Spring.

> **Note** **Término clave — escaneo de componentes.** Un *escaneo de componentes*
> es la pasada de arranque que encuentra cada bean declarado y lo registra. Spring
> escanea el classpath con reflexión; Rust no tiene reflexión en tiempo de
> ejecución, así que el escaneo de Firefly es *en tiempo de enlazado* — cada derive
> de estereotipo emite un registro que el escaneo recopila del binario compilado.

## Paso 1 — Ver el cableado que ya no escribes

Abre `samples/lumen/src/web.rs` y lee el comentario de documentación de su módulo.
Llama al archivo "la **raíz de composición**", e inmediatamente después te dice que
no hay ninguna escrita a mano.

> **Note** **Término clave — raíz de composición.** La *raíz de composición* es el
> único lugar de un programa donde se ensambla el grafo de objetos — donde cada
> componente se construye y se conecta. En muchos frameworks escribes esta función
> a mano. En Firefly el framework *es* la raíz de composición: escanea tus beans y
> los cablea, de modo que nunca deletreas el grafo.

Recuerda el `main` de una sola línea del Inicio rápido:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

Esa única llamada ensambla el grafo de objetos completo de Lumen. Dentro de
`run()`, el cableado ocurre en tres movimientos:

1. Construye la pila web y **autorregistra** los beans de infraestructura propios
   del framework — el `Bus` CQRS, el `Broker` de eventos, la caché, el registro de
   métricas, el planificador — en el contenedor, para que *tus* beans puedan
   inyectarlos por autowiring.
2. **Escanea los componentes** del grafo de crates: cada tipo derivado de un
   estereotipo y cada factory `#[bean]` de Lumen se descubre, se comprueba contra
   sus condiciones y se registra.
3. **Resuelve** los controladores en orden para montarlos, lo que construye
   recursivamente los colaboradores con autowiring de cada controlador en orden de
   dependencias — exactamente el grafo que construiría una raíz escrita a mano, pero
   derivado de las declaraciones en lugar de deletreado.

Lo que acaba de ocurrir: nada en tu código nombra el orden en el que se construyen
la caché, el bus, el almacén, el ledger y el controlador. Declaraste cada uno junto
a sí mismo; el contenedor calculó el orden a partir de los tipos de dependencia.
Ese es todo el truco, y el resto del capítulo es la mecánica que hay detrás.

> **Tip** **Punto de control.** Abre `samples/lumen/src/web.rs` y localiza el
> comentario que dice que no hay "**ninguna raíz de composición escrita a mano ni
> builder**". Todos los ejemplos de abajo provienen de este archivo (y de sus
> hermanos `ledger.rs` y `commands.rs`). Estás leyendo el cableado real, no un
> juguete.

## Paso 2 — Declarar un bean propio con un estereotipo

El bean más simple es una struct que puedes anotar directamente. La haces visible
al contenedor derivando un **estereotipo**. El modelo de lectura de Lumen es
exactamente este caso — un mapa en memoria que escribe la proyección y lee la
consulta `GetWallet`:

```rust,ignore
// src/ledger.rs — the CQRS query side, a scanned data-access bean.
use std::collections::HashMap;
use std::sync::Mutex;
use firefly::prelude::*;

/// The in-memory read model — a `#[derive(Repository)]` bean (Spring's
/// `@Repository`): the projection upserts it, `GetWallet` reads it.
#[derive(Debug, Default, Repository)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}
```

Lo que acaba de ocurrir, bloque a bloque:

- `#[derive(Repository)]` es el estereotipo. Declara `ReadModel` como un bean
  gestionado *y* registra su rol como capa de acceso a datos. Ese único derive es
  todo el registro que hay — sin llamada a `register(...)`, sin entrada en una
  lista.
- `Default` permite al contenedor construir el bean sin argumentos. Una struct
  derivada de un estereotipo sin campos `#[autowired]` se construye mediante su
  `Default` y luego se registra como un **singleton** (una única instancia
  compartida para el proceso).
- Los campos propios de la struct (`rows`) son estado ordinario. Solo los campos
  que marcas con `#[autowired]` — y `ReadModel` no tiene ninguno — se rellenan
  desde el contenedor.

Los cinco estereotipos difieren únicamente en el rol arquitectónico que comunican;
los cinco registran el tipo como un bean gestionado:

| Derive                     | Rol                                                       |
|----------------------------|----------------------------------------------------------|
| `#[derive(Service)]`       | Capa de lógica de negocio: orquestación de casos de uso. |
| `#[derive(Component)]`     | Bean gestionado genérico sin un rol específico.          |
| `#[derive(Repository)]`    | Capa de acceso a datos: bases de datos, almacenamiento externo, puertos. |
| `#[derive(Configuration)]` | Un contenedor de factories que puede albergar métodos `#[bean]`. |
| `#[derive(Controller)]`    | Capa HTTP (`#[rest_controller]` se construye sobre esto).|

> **Design note.** El rol que registra cada estereotipo no es cosmético. Se
> almacena en el bean, de modo que la vista `/beans` del panel de administración (y
> el informe de arranque) puede agrupar los beans por capa — `[repository]
> ReadModel`, `[service] WalletHandlers`, etcétera — la misma introspección de DI
> que expone Spring Boot Actuator.

> **Tip** **Punto de control.** `ReadModel` se convierte en un bean a partir de un
> derive y un `Default`. Quédate con esa imagen: *un derive de estereotipo es el
> registro.*

## Paso 3 — Inyectar dependencias con `#[autowired]`

Un modelo de lectura no tiene colaboradores, pero la mayoría de los beans sí. Para
declarar lo que un bean necesita, marca un campo con `#[autowired]` y el contenedor
lo rellena por tipo. Esta es la forma de escribir en Rust la inyección por
constructor: tú declaras *qué*, el contenedor lo provee. El controlador de wallet
de Lumen es el caso de manual (el `WalletApi` real de `src/web.rs`):

```rust,ignore
use std::sync::Arc;
use firefly::cqrs::QueryCache;
use firefly::prelude::*;

/// The wallet HTTP surface — a `#[derive(Controller)]` DI bean. Its
/// collaborators are autowired from the container; `#[rest_controller]`
/// auto-mounts it (Chapter 6).
#[derive(Clone, Controller)]
pub struct WalletApi {
    #[autowired]
    pub bus: Arc<Bus>,                 // the CQRS bus it dispatches through
    #[autowired]
    pub ledger: Arc<Ledger>,           // the application service the saga + stream use
    #[autowired]
    pub query_cache: Arc<QueryCache>,  // invalidated after a mutation
}
```

> **Note** **Término clave — `Arc<T>`.** `Arc` es el puntero compartido con conteo
> de referencias atómico de Rust. El contenedor reparte singletons compartidos, de
> modo que una dependencia inyectada llega como `Arc<T>` — muchos beans pueden
> mantener el mismo `Arc<Ledger>` y todos ven la única instancia. Allí donde veas
> `#[autowired] field: Arc<T>`, léelo como "dame el `T` compartido".

Lo que acaba de ocurrir: cuando el contenedor construye `WalletApi`, primero
resuelve el `Bus`, luego el `Ledger` (construyendo recursivamente *sus* propias
dependencias — las verás en el Paso 5), después el `QueryCache`, y solo entonces
inyecta los tres y devuelve el controlador. No escribiste ningún constructor; los
tipos de los campos *son* la firma del constructor.

Una dependencia que no existe aflora como un **error de resolución claro en el
arranque** — un "no such bean" con nombre que apunta al tipo que falta — y no como
un panic tres marcos más adentro en tiempo de ejecución. El cableado con fallo
rápido es la idea central.

`#[autowired]` inyecta más que un único `Arc<T>`. La *forma* del campo selecciona
el modo de inyección:

- `#[autowired] widgets: Vec<Arc<Widget>>` inyecta **cada** `Widget` registrado,
  ordenado por el `order` de cada bean — inyección de colección, la forma de reunir
  todas las implementaciones de un puerto.
- `#[autowired] maybe: Option<Arc<Thing>>` inyecta `Some` cuando hay un `Thing`
  registrado y `None` cuando no lo hay — una dependencia opcional que no aborta el
  arranque si está ausente.
- `#[autowired] tickets: Provider<Ticket>` inyecta un manejador **diferido**:
  `tickets.get()` resuelve un valor nuevo en cada llamada, la forma de obtener un
  transient dentro de un singleton.

> **Note** **Término clave — `Provider<T>`.** Un `Provider<T>` es un manejador
> perezoso a un bean en lugar del bean en sí. Llamar a `tickets.get()` lo resuelve
> bajo demanda. Es el análogo en Rust del `ObjectProvider` / `Provider<T>` de
> Spring, y la forma en que un singleton de larga vida obtiene un bean de vida
> corta cada vez que necesita uno.

> **Tip** **Punto de control.** `WalletApi` nombra tres colaboradores y no
> construye ninguno. Si eliminaras la línea `#[autowired] ledger`, el controlador ya
> no pediría un ledger — el campo es la petición completa.

## Paso 4 — Declarar beans que no posees con factories `#[bean]`

No todo colaborador es una struct sobre la que puedas poner un derive. El almacén
de eventos, la caché de consultas, el servicio JWT y el ledger se *construyen* —
toman argumentos de constructor, o vienen de un crate de terceros, o una factory es
sencillamente la forma más clara de expresarlos. Para estos, declaras un contenedor
`#[derive(Configuration)]` y le das métodos factory `#[bean]`.

> **Note** **Término clave — factory `#[bean]`.** Un método `#[bean]` es una
> factory: el contenedor lo llama y registra lo que devuelva como un bean, indexado
> por el **tipo de retorno** del método. El contenedor lleva
> `#[derive(Configuration)]`. Esto es la clase `@Configuration` de Spring con
> métodos `@Bean`, uno a uno.

Aquí está el contenedor `LumenBeans` completo de Lumen, en `src/web.rs`:

```rust,ignore
// src/web.rs
use std::sync::Arc;
use firefly::cqrs::QueryCache;
use firefly::eda::Broker;
use firefly::eventsourcing::{EventStore, MemoryEventStore};
use firefly::prelude::*;
use firefly::security::{BearerLayer, FilterChain, JwtService};

/// Lumen's `@Configuration` holder. Its `#[bean]` factory methods **declare**
/// the app's domain beans. `container.scan()` discovers and registers them —
/// the framework does the registration, so there is no `register_arc` to call.
#[derive(Configuration)]
struct LumenBeans;

#[bean]
impl LumenBeans {
    /// The in-memory event store (`@Bean`).
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }

    /// The read-side query cache honouring `GetWallet`'s 30s TTL (`@Bean`).
    #[bean]
    fn query_cache(&self) -> QueryCache {
        QueryCache::new()
    }

    /// The HS256 JWT service (`@Bean`).
    #[bean]
    fn jwt_service(&self) -> JwtService {
        JwtService::new(crate::security::DEMO_SIGNING_KEY)
    }

    /// The security filter chain + bearer layer — auto-discovered and applied
    /// by `FireflyApplication`, no `.security(...)` call (Chapter 14).
    #[bean]
    fn security_filter_chain(&self) -> FilterChain {
        crate::security::security_layers().1
    }
    #[bean]
    fn bearer_layer(&self) -> BearerLayer {
        crate::security::security_layers().0
    }

    /// The ledger application service — a **pure factory** whose parameters are
    /// **autowired**: the container resolves the event store and the
    /// framework-provided `Broker` port by type, then hands them to the factory.
    #[bean]
    fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

Lo que acaba de ocurrir, bloque a bloque:

- `#[derive(Configuration)] struct LumenBeans;` es el contenedor. El escaneo lo
  descubre del mismo modo que descubre `ReadModel` — un derive.
- `#[bean] impl LumenBeans { ... }` marca todo el bloque impl como portador de
  factories de beans, y cada método `#[bean]` interno se registra como su propio
  bean. El contenedor indexa cada uno por su tipo de retorno: `event_store` registra
  un `MemoryEventStore`, `query_cache` registra un `QueryCache`, y así
  sucesivamente.
- `event_store`, `query_cache` y `jwt_service` solo toman `&self` — son factories
  sin dependencias. El contenedor las llama y registra el resultado.
- `ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>)` es la
  importante: sus **parámetros se resuelven a su vez desde el contenedor** por tipo
  antes de que el método se ejecute. Un bean puede depender de un bean. El
  contenedor construye el `MemoryEventStore` (a partir de la factory `event_store`
  de arriba) y provee el `Broker` del framework, y luego llama a `ledger`.

> **Note** **Término clave — puerto y adaptador.** Un *puerto* es una interfaz (en
> Rust, un trait object como `Arc<dyn Broker>` o `Arc<dyn EventStore>`); un
> *adaptador* es una implementación concreta de él. "Depende del puerto, inyecta el
> adaptador" significa que un bean pide el trait y el contenedor provee la
> implementación que esté registrada. Este es el vocabulario de la arquitectura
> hexagonal que usa el resto del libro.

Fíjate en que la factory `ledger` ensancha `Arc<MemoryEventStore>` a `Arc<dyn
EventStore>` antes de entregárselo a `Ledger::new`. El `Ledger` almacena el
*puerto*, no el almacén concreto — de modo que cambiar `MemoryEventStore` por un
almacén respaldado por Postgres es un cambio de una línea en esta factory, y el
ledger, los handlers y el controlador ni se enteran.

Tres ideas sostienen todo el diseño. Léelas despacio; todo lo posterior es
consecuencia:

- **El framework hace el registro.** Nunca llamas a `register_arc` ni a
  `Container::bind`. `container.scan()` descubre el contenedor `LumenBeans` *y* cada
  método `#[bean]`, y registra los valores producidos indexados por tipo de retorno.
- **Los puertos se resuelven por tipo.** La factory `ledger` toma `broker: Arc<dyn
  Broker>` y el contenedor provee el broker del framework — "depende de la interfaz,
  inyecta la implementación".
- **Un bean puede depender de un bean.** `ledger(&self, store, broker)` arrastra
  otros dos beans por tipo; el contenedor los construye primero y luego llama a la
  factory — la misma construcción ordenada por dependencias que haría una raíz
  escrita a mano, derivada de los tipos de los parámetros.

> **Design note.** Un contenedor `#[derive(Configuration)]` con métodos `#[bean]`
> es el análogo de `@Configuration` + `@Bean` de Spring: una factory cuyos métodos
> producen beans indexados por tipo de retorno, resolviendo sus propios argumentos
> desde el contenedor. Lumen declara así todo su grafo de dominio, y el escaneo de
> componentes convierte las declaraciones en el grafo de objetos cableado.

> **Tip** **Punto de control.** Ahora tienes ambas formas de declarar un bean: un
> derive de estereotipo sobre una struct que posees (`ReadModel`) y una factory
> `#[bean]` para las cosas que construyes (`event_store`, `ledger`). Lumen usa un
> derive cuando puede anotar el tipo y una factory cuando no puede.

## Paso 5 — Rastrear una resolución de principio a fin

Junta los Pasos 2 a 4 siguiendo una sola resolución: ¿cómo construye
`FireflyApplication` el `WalletApi`?

1. El escaneo ya ha registrado cada bean: `LumenBeans` y sus cinco factories, el
   `ReadModel`, los beans de servicio `WalletHandlers` y `WalletProjection`, y el
   propio `WalletApi` — además del `Bus`, `Broker`, caché, planificador y registros
   propios del framework.
2. Para montar el controlador, el contenedor llama a `resolve::<WalletApi>()`. Los
   tipos de los campos dicen que necesita `Arc<Bus>`, `Arc<Ledger>` y
   `Arc<QueryCache>`.
3. `Arc<Bus>` y `Arc<QueryCache>` ya existen (el framework preregistró el bus; la
   factory `query_cache` produjo la caché). Se entregan directamente.
4. `Arc<Ledger>` aún no existe, así que el contenedor llama a la factory `ledger`.
   Esa factory necesita `Arc<MemoryEventStore>` y `Arc<dyn Broker>`. El contenedor
   construye el almacén a partir de la factory `event_store`, provee el broker del
   framework y llama a `ledger` — produciendo el `Ledger`.
5. Con los tres colaboradores en mano, el contenedor construye `WalletApi` y lo
   guarda en caché como un singleton.

Lo que acaba de ocurrir: el contenedor construyó el grafo **empezando por las
hojas** — almacén y broker antes que el ledger, ledger antes que el controlador —
puramente a partir de los tipos de dependencia. Ese ordenamiento es el trabajo que
una raíz de composición solía hacer a mano. Aquí está *derivado*, y se recalcula
correctamente en el momento en que añades o quitas una dependencia.

La misma recursión cablea el resto de Lumen. El bean de handler CQRS inyecta el
ledger y el modelo de lectura del mismo modo (el `WalletHandlers` real de
`src/commands.rs`):

```rust,ignore
/// The CQRS handler bean — Spring's `@Component` command/query handler. Its
/// collaborators are `#[autowired]` from the DI container.
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
    async fn deposit(&self, cmd: Deposit) -> Result<WalletView, CqrsError> {
        self.ledger
            .deposit(&cmd.wallet_id, Money::cents(cmd.amount))
            .await
            .map_err(to_cqrs)
    }
    // ... open_wallet, withdraw, get_wallet ...
}
```

Aquí se inyectan los mismos singletons `Arc<Ledger>` y `Arc<ReadModel>` que también
mantienen el controlador y la proyección — una única instancia de cada uno,
compartida por todo bean que la pida. (La maquinaria de `#[handlers]` /
`#[command_handler]` que pone estos métodos en el bus es el tema de
[CQRS y mensajería](./09-cqrs.md); por ahora, fíjate únicamente en que un handler es
un bean y obtiene sus colaboradores por autowiring.)

> **Tip** **Punto de control.** Rastréalo en tu cabeza una vez más: `WalletApi` →
> `Ledger` → `MemoryEventStore` + `Broker`. Si sabes nombrar esa cadena, entiendes
> el resolver.

## Paso 6 — Usar los beans de infraestructura del framework por tipo

Quizá hayas notado que `WalletApi` inyecta `Arc<Bus>` y la factory `ledger` inyecta
`Arc<dyn Broker>`, y sin embargo Lumen nunca *declara* un bus ni un broker. Vienen
del framework. Antes de que se ejecute el escaneo, `FireflyApplication` preregistra
sus propios beans de infraestructura en el contenedor, de modo que cualquiera de tus
beans puede inyectarlos por tipo:

| Bean (resolver por tipo)  | Tipo                                             |
|---------------------------|--------------------------------------------------|
| `Bus`                     | `Arc<cqrs::Bus>` (validación preinstalada)       |
| adaptador de caché        | `Arc<dyn cache::Adapter>` (Memory por defecto)   |
| broker                    | `Arc<dyn eda::Broker>` (InMemory por defecto)    |
| planificador              | `Arc<scheduling::Scheduler>`                     |
| registro de métricas      | `Arc<actuator::MetricRegistry>`                  |
| compuesto de salud        | `Arc<actuator::HealthComposite>`                |

Lo que acaba de ocurrir: el contenedor *es* la raíz de composición de Lumen, y el
framework lo siembra primero con estos colaboradores. Por eso una factory `#[bean]`
puede tomar `broker: Arc<dyn Broker>` y simplemente recibir uno — el broker ya
estaba registrado. Llegas a cualquiera de estos inyectándolo por autowiring en un
bean; ajustas los mandos de *configuración* que tienen debajo (CORS, idempotencia,
cabeceras de seguridad, direcciones de enlace) a través de
`FireflyApplication::configure`:

```rust,ignore
firefly::FireflyApplication::new("lumen")
    .configure(|cfg: &mut CoreConfig| {
        // adjust CoreConfig / WebStack knobs here
    })
    .run()
    .await
```

> **Note** **Término clave — autoconfiguración.** La *autoconfiguración* consiste
> en que el framework preregistra beans de infraestructura sensatos (un broker en
> memoria, una caché en memoria, el registro de métricas) para que tu aplicación
> funcione con cero cableado, permitiéndote a la vez sobrescribir cualquiera de
> ellos. Es el mecanismo tras los valores por defecto de Spring Boot que "simplemente
> funcionan", tratado al completo en
> [Inyección de dependencias y autoconfiguración](./04a-dependency-injection.md).

> **Tip** **Punto de control.** Ningún bean de `samples/lumen` construye un `Bus`,
> un `Broker` ni una caché — los inyectan por autowiring. Haz un grep de
> `samples/lumen/src` buscando `Arc<Bus>` y `Arc<dyn Broker>` y confirma que cada
> uso es un consumidor, nunca un productor.

## Paso 7 — Vincular un trait con su implementación

Hasta ahora cada dependencia inyectada ha sido un tipo concreto o un puerto del
framework. Cuando *tú* posees tanto un trait (un puerto) como su implementación (un
adaptador), los vinculas en el derive con `provides` y luego resuelves el trait —
"depende del puerto, obtén el adaptador":

```rust,ignore
trait Clock: Send + Sync { fn now(&self) -> u64; }

#[derive(Component, Default)]
#[firefly(provides = "dyn Clock", primary)]
struct SystemClock;
impl Clock for SystemClock { fn now(&self) -> u64 { 42 } }

// elsewhere: c.resolve::<dyn Clock>() yields the SystemClock instance.
```

Lo que acaba de ocurrir, bloque a bloque:

- `#[derive(Component, Default)]` registra `SystemClock` como un bean gestionado,
  como de costumbre.
- `#[firefly(provides = "dyn Clock")]` *adicionalmente* vincula el trait object
  `dyn Clock` con esta implementación. Ahora un bean puede inyectar `Arc<dyn Clock>`
  y el contenedor le entrega el `SystemClock`.
- `primary` lo marca como el predeterminado cuando varios beans satisfacen el mismo
  trait.

> **Note** **Término clave — `primary` y `order`.** Cuando varios beans satisfacen
> un trait, `#[firefly(... primary)]` elige el que devuelve un `resolve::<dyn
> Trait>()` simple (el `@Primary` de Spring), y `#[firefly(order = N)]` fija la
> posición que toma un bean cuando se recopilan *todos* — mediante `resolve_all::<dyn
> Trait>()` o mediante un campo `Vec<Arc<...>>` inyectado por autowiring (el `@Order`
> de Spring).

`provides` en el derive es la forma **amigable con el escaneo** de vincular un
trait. Cuando en cambio ensamblas un contenedor a mano (en un test acotado,
pongamos), el movimiento equivalente es una llamada explícita a `Container::bind::<dyn
Trait, Concrete>()`; ambos registran el mismo mapeo de trait a adaptador. Para el
caso poco frecuente en que necesitas una instancia con nombre *específica* en lugar
de cualquiera que satisfaga, el contenedor también admite resolución por
cualificador-por-nombre. Los tres — `bind`, beans con nombre y toda la superficie de
desambiguación — se tratan en
[Inyección de dependencias y autoconfiguración](./04a-dependency-injection.md).

Lumen mismo usa `provides` para su endpoint de streaming protegido por feature, que
registra como un puerto `RouteContributor` que el framework descubre y fusiona:

```rust,ignore
#[cfg(feature = "streaming")]
#[derive(Service)]
#[firefly(provides = "dyn firefly::web::RouteContributor")]
struct StreamingRoutes {
    #[autowired]
    api: Arc<WalletApi>,
}
```

> **Tip** **Punto de control.** Ahora puedes vincular un puerto con un adaptador
> sin una raíz de composición: `provides` en el derive y luego `resolve::<dyn
> Trait>()`. Añade una segunda implementación sin `primary` y resolver el trait pasa
> a ser un error de *bean no único* — el contenedor negándose a adivinar.

## Paso 8 — Condicionar beans por condición y por perfil

Una misma base de código tiene que ejecutarse con adaptadores en memoria baratos en
desarrollo e infraestructura real en producción. El mecanismo es el **registro
condicional**: un bean puede declarar las circunstancias bajo las cuales debería
existir siquiera, y el escaneo respeta eso a medida que recopila cada registro.

```rust,ignore
// Registered only when the property is present and not false.
#[derive(Service, Default)]
#[firefly(condition_on_property = "feature.audit=on")]
struct AuditService;

// Registered only under the named profile.
#[derive(Service, Default)]
#[firefly(profile = "prod")]
struct PostgresHealthCheck;
```

Lo que acaba de ocurrir: `condition_on_property = "feature.audit=on"` registra
`AuditService` solo cuando esa propiedad de configuración está establecida;
`profile = "prod"` registra `PostgresHealthCheck` solo cuando el perfil `prod` está
activo. El escaneo las evalúa a medida que descubre cada bean, de modo que el
contenedor acaba conteniendo *exactamente* los beans que el entorno reclama — sin
ningún `if` en el código de tu servicio.

> **Note** **Término clave — perfil.** Un *perfil* es una porción de entorno con
> nombre — `dev`, `test`, `prod` — que conmuta qué beans y qué configuración están
> activos. Firefly lee los perfiles activos de la configuración (la variable de
> entorno `FIREFLY_PROFILE` por defecto); los perfiles se introducen en
> [Configuración](./03-configuration.md) y aquí se usan para condicionar beans,
> exactamente como hace el `@Profile` de Spring.

El mismo condicionamiento funciona en las factories `#[bean]` — `#[bean(profile =
"prod")]` registra una factory solo bajo el perfil `prod` — y es el motor tras cada
nota de "cambia el adaptador para producción" de este libro. Lumen puede quedarse en
memoria con fines didácticos mientras un despliegue de producción cambia a
infraestructura real solo mediante configuración.

> **Tip** **Punto de control.** Añade `#[firefly(condition_on_property =
> "wallet.enabled=true")]` a un bean `#[derive(Service)]` desechable, ejecuta Lumen y
> observa cómo *no* aparece en el informe de arranque hasta que estableces la
> propiedad. La condición decidió la existencia del bean antes de la construcción.

## Paso 9 — Engancharse al ciclo de vida de un bean

Los beans de infraestructura reales necesitan *actuar* una vez cableados — abrir un
pool, suscribirse a un topic — y deshacerlo al apagar. Nombra los métodos en el
derive:

```rust,ignore
#[derive(Service, Default)]
#[firefly(post_construct = "started", pre_destroy = "stopped")]
struct ProjectionSubscriber { /* ... */ }

impl ProjectionSubscriber {
    fn started(&mut self) { /* subscribe the read-model projection */ }
    fn stopped(&self)     { /* drain and unsubscribe */ }
}
```

Lo que acaba de ocurrir: `post_construct = "started"` nombra un método que se
ejecuta *después* de que el bean se construya y sus campos `#[autowired]` se
inyecten; `pre_destroy = "stopped"` nombra un método que se ejecuta en
`container.destroy()`. La destrucción ocurre en **orden inverso al de
construcción**, de modo que un suscriptor iniciado después del almacén se
desmantela antes que él — un desmontaje limpio sin una secuencia de apagado escrita
a mano.

> **Note** **Término clave — `post_construct` / `pre_destroy`.** Estos son los
> análogos en Rust de `@PostConstruct` y `@PreDestroy` de Spring (y de las llamadas
> de ciclo de vida de JSR-250): un método que se ejecuta una vez tras completarse el
> cableado y un método que se ejecuta al apagar, con la garantía de orden inverso.

> **Tip** **Punto de control.** Los hooks de ciclo de vida son la forma en que un
> bean hace *él mismo* su cableado de una sola vez, en lugar de que una raíz de
> composición lo haga tras la construcción. El contenedor posee el ordenamiento.

## Paso 10 — Leer el inventario de beans en el arranque

Has declarado beans, los has inyectado por autowiring, has vinculado un puerto, has
condicionado algunos y le has dado hooks de ciclo de vida a uno. El framework
imprime exactamente lo que cableó. Ejecuta Lumen y lee el informe de arranque:

```bash
cargo run
```

El bloque `:: beans (…) ::` lista cada bean registrado, agrupado por estereotipo:
`LumenBeans` y sus factories, `WalletApi`, los beans `ledger` / `event_store`, el
`[repository] ReadModel`, el `[service] WalletHandlers` y `WalletProjection`. Estos
son los mismos datos que renderiza la vista `/beans` del panel de administración, en
el puerto de gestión en `http://localhost:8081/admin/`.

> **Note** **Término clave — escaneo de componentes (en tiempo de enlazado).** Como
> Rust no tiene reflexión en tiempo de ejecución, cada derive de estereotipo emite un
> registro de `inventory` en tiempo de compilación, y `firefly::scan(&container)`
> (equivalentemente `container.scan()`) recopila cada uno de los que se enlazaron en
> el binario y los registra — respetando condiciones y perfiles a medida que avanza.
> `FireflyApplication` ejecuta este escaneo por ti en el arranque.

Lo que acaba de ocurrir: el informe *es* el inventario que produjo el escaneo. Nada
es reflexivo ni está oculto — "qué está cableado" se imprime línea a línea. Una
dependencia que faltase habría abortado el arranque con un error de resolución con
nombre antes de que este informe llegara a imprimirse.

> **Warning** El descubrimiento en tiempo de enlazado tiene una peculiaridad
> específica de Rust. Un bean solo es descubrible cuando los registros de su crate
> están **enlazados en el binario**. Para una aplicación de un solo crate como Lumen
> eso es automático. Pero en un servicio multicrate, un crate de *capa* del que el
> binario solo depende transitivamente — un crate `-models` o `-core` cuyos beans
> nunca se nombran directamente — puede ser **eliminado como código muerto** por el
> enlazador, beans incluidos. Fuerza el enlazado de esos con
> [`firefly::link!`](./22-layered-microservices.md) en la raíz del crate del binario
> (`firefly::link!(my_core, my_models);`) y protege el resultado con
> `firefly::assert_discovered(...)`. El Lumen de un solo crate nunca necesita esto;
> la nota está aquí para que el vacío del informe ante un crate eliminado nunca sea
> un misterio.

> **Tip** **Punto de control.** El bloque `:: beans ::` nombra cada bean que
> declaraste en este capítulo, sin ninguna llamada de registro en parte alguna de tu
> código. Esa es la recompensa: tú escribiste declaraciones, el framework escribió el
> grafo.

## La única vía de escape — `register_all!`

El escaneo de componentes es el camino que toman Lumen y el framework, y es el
predeterminado para todo en `samples/lumen`. Hay exactamente un mecanismo de
respaldo explícito, para los dos casos que el escaneo no puede alcanzar:

```rust,ignore
let c = Container::new();
firefly::register_all!(&c, [ReadModel, Ledger, WalletApi]);
let api = c.resolve::<WalletApi>().expect("controller resolves");
```

Recurre a `register_all!` para los **beans genéricos** — la monomorfización de un
tipo genérico se elige en el lugar de uso, así que no se puede inventariar — o
simplemente para mantener el cableado local a un único test acotado. Ambos registran
los mismos beans contra el mismo contenedor; el escaneo se limita a construirte la
lista a partir del inventario de tiempo de enlazado. El punto de entrada de más bajo
nivel por debajo del escaneo es el `ApplicationContext`, que envuelve el contenedor
con la secuencia de arranque completa y resulta práctico en un test:

```rust,ignore
use firefly::prelude::*;

let ctx = ApplicationContext::builder()
    .profiles(["test"])
    .property("feature.audit", "on")
    .build();
let c = ctx.container();

// Every stereotype-derived bean in the crate graph is discovered and wired.
let api = c.resolve::<WalletApi>().expect("scan registered the controller");
```

La taxonomía de errores es precisa: un bean que falta, un bean no único sin
`primary` y una dependencia circular detectada afloran cada uno como un error
distinto y con nombre en tiempo de resolución — los datos que también reporta la
vista `/beans` de administración.
[Inyección de dependencias y autoconfiguración](./04a-dependency-injection.md)
trata en profundidad toda la superficie del contenedor — scopes, beans con nombre,
`bind`, `register_all!` y el modelo de errores.

## Resumen — qué cambió en Lumen

| Antes | Después de este capítulo |
|--------|--------------------|
| el cableado imaginado como una raíz de composición escrita a mano | entendido como **beans declarados** que el escaneo de componentes descubre y cablea — sin raíz que mantener |
| un derive de estereotipo parecía decorativo | visto como **el registro en sí**: un derive crea un bean gestionado y registra su capa |
| `#[autowired]` parecía una anotación de un solo valor | conocido como cuatro modos de inyección — `Arc<T>`, `Vec<Arc<T>>`, `Option<Arc<T>>`, `Provider<T>` |
| los puertos parecían abstractos | vistos en concreto como `Arc<dyn Broker>` / `Arc<dyn EventStore>` — una factory `#[bean]` para cambiar un adaptador |
| no estaba claro cómo `FireflyApplication` resuelve el grafo | nombrado: preregistrar beans de infra → escanear → resolver controladores, construyendo colaboradores empezando por las hojas en orden de dependencias |

También sabes ahora:

- Por qué un servicio Firefly no tiene `build_app` — las declaraciones más un
  escaneo de componentes reemplazan al grafo escrito a mano, y el framework *es* la
  raíz de composición.
- Que las condiciones y los perfiles condicionan la existencia de un bean, de modo
  que una misma base de código se ejecuta en memoria en dev y sobre infraestructura
  real en prod sin un `if`.
- Que `post_construct` / `pre_destroy` le dan a un bean su propio cableado y
  desmontaje de una sola vez, ordenados por el contenedor.
- Que `register_all!` y `ApplicationContext::builder()` son los respaldos explícitos
  para genéricos y tests acotados — todo lo demás se escanea.

## Ejercicios

1. **Lee el inventario de beans.** Ejecuta Lumen y lee el bloque `:: beans (…) ::`
   del informe de arranque. Encuentra `LumenBeans`, `WalletApi`, las factories
   `ledger` / `event_store` y el bean de acceso a datos `[repository] ReadModel`,
   agrupados por estereotipo — los mismos datos que renderiza la vista `/beans` del
   panel de administración en `http://localhost:8081/admin/`.
2. **Añade un bean y obsérvalo aparecer.** Añade un pequeño `#[derive(Service)]` a
   `web.rs`, ejecuta Lumen y confirma que aparece en el informe — no escribiste
   ninguna llamada de registro. Luego añade `#[firefly(condition_on_property =
   "wallet.enabled=true")]` y obsérvalo desaparecer hasta que establezcas la
   propiedad.
3. **Vincula un puerto automáticamente.** Define un trait `Clock`, dale a
   `SystemClock` `#[firefly(provides = "dyn Clock", primary)]` y resuelve `dyn
   Clock`. Añade una segunda implementación *sin* `primary`, observa el error de bean
   no único y luego mueve `primary` al que quieras como predeterminado.
4. **Cambia un almacén desde una sola factory.** Cambia el `#[bean]` `event_store` de
   `LumenBeans` para que devuelva un almacén distinto, y explica en una frase por qué
   la factory `ledger`, los handlers y el controlador no necesitan ningún cambio —
   dependen del *puerto* `EventStore`, no del almacén concreto.
5. **Rastrea una resolución.** Elige `WalletHandlers` y anota, en orden, cada bean
   que el contenedor debe construir antes de poder construir ese handler. Comprueba
   tu respuesta contra los campos `#[autowired]` de `src/commands.rs` y la factory
   `ledger` de `src/web.rs`.

## Adónde ir después

- Profundiza en el contenedor en **[Inyección de dependencias y
  autoconfiguración](./04a-dependency-injection.md)** — scopes, beans con nombre y
  cualificadores, `Container::bind`, toda la superficie condicional y el modelo de
  autoconfiguración que este capítulo solo esbozó.
- Mira exactamente qué hace `run()`, etapa por etapa, en **[Arranque con
  FireflyApplication](./04b-bootstrap.md)** — la secuencia de arranque que impulsa el
  escaneo que acabas de aprender.
- Luego conoce las primitivas reactivas sobre las que se construye cada capítulo
  posterior en **[El modelo reactivo — Mono y Flux](./05-reactive-model.md)**, y dale
  a Lumen sus primeros endpoints en **[Tu primera API HTTP](./06-first-http-api.md)**.
