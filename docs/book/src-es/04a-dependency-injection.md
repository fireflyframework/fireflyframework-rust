# Inyección de dependencias y autoconfiguración

El [capítulo anterior](./04-dependency-wiring.md) recorrió el cableado de Lumen a
vista de pájaro: **declara beans** y el escaneo de componentes de
`FireflyApplication` los descubre y los conecta en el arranque. Este capítulo es
el recorrido guiado, desde los primeros principios, de ese contenedor — el motor
que convierte un crate lleno de declaraciones `#[derive(...)]` y `#[bean]` en un
grafo de objetos cableado. Construiremos toda la superficie de DI una idea a la
vez, siempre contra los propios colaboradores de Lumen (`ReadModel`, `Ledger`,
`WalletApi`), de modo que al final nada del contenedor sea una caja negra.

No necesitas haber terminado el capítulo anterior para seguir este — cada
concepto se reintroduce aquí en contexto. Pero deberías tener un crate de Lumen
que compile y se ejecute (desde [Quickstart](./02-quickstart.md)), porque los
ejemplos reflejan código que ya se distribuye en
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen).

Al terminar este capítulo, serás capaz de:

- Explicar la **inversión de control** a la manera de Rust, y por qué el
  descubrimiento de Firefly ocurre en *tiempo de enlazado* en lugar de mediante
  reflexión.
- Declarar un bean con un **derive de estereotipo** y cablear sus dependencias
  con inyección por constructor mediante `#[autowired]`.
- Desambiguar adaptadores en competencia con `#[firefly(primary)]`, nombres y
  cualificadores, y leer las cuatro reglas de resolución en orden de prioridad.
- Producir beans que no posees con factorías `#[derive(Configuration)]` +
  `#[bean]`, incluidas factorías **async** que realizan E/S en el arranque.
- Limitar beans mediante **perfiles** y la familia `condition_on_*`, y dejar que
  una **autoconfiguración** se retire en cuanto declaras tu propio bean.
- Enmarcar un bean con hooks de ciclo de vida, elegir su **ámbito** e
  introspeccionar todo el grafo a través de la vista `/beans`.

## Conceptos que conocerás

Antes de la primera línea de código, aquí están los términos que sostienen todo.
Cada uno se reintroduce en contexto la primera vez que se usa; esta es la versión
corta para que el mapa esté en tu cabeza desde el principio.

> **Note** **Término clave — bean.** Un *bean* es cualquier valor que el
> framework construye, cablea, posee y entrega a quien lo necesita. Tú declaras
> los beans; el contenedor los descubre en el arranque y los conecta. Esto es
> exactamente la noción de Spring de un bean gestionado por el contexto de
> aplicación.

> **Note** **Término clave — contenedor (contenedor de DI).** El *contenedor* es
> el registro que contiene cada bean, resuelve las dependencias de un bean y
> construye el grafo de objetos en orden de dependencias. El contenedor de
> Firefly es el tipo `firefly::container::Container`, normalmente operado a través
> de un `ApplicationContext`. El análogo en Spring es el `ApplicationContext` /
> `BeanFactory`.

> **Note** **Término clave — inversión de control (IoC).** La *inversión de
> control* significa que el framework llama a tus constructores en el orden
> correcto, en lugar de que tu código los llame a mano. Tú declaras *qué*
> necesita un bean; el contenedor decide *cuándo* y *en qué orden* construirlo. La
> inyección de dependencias es el mecanismo concreto que materializa la IoC.

> **Note** **Término clave — puerto y adaptador.** Un *puerto* es una capacidad
> abstracta de la que tu código depende (un trait, p. ej. `EventStore`); un
> *adaptador* es una implementación concreta de él (p. ej. `MemoryEventStore`).
> Depender del puerto y seleccionar el adaptador en tiempo de cableado es lo que
> permite que una misma base de código Lumen se ejecute sobre infraestructura en
> memoria en los tests e infraestructura real en producción. Este es el
> vocabulario de la arquitectura hexagonal.

> **Design note.** El contenedor de Firefly ofrece un modelo de DI declarativo —
> derives de estereotipo, `#[autowired]`, factorías `#[bean]`, `primary`,
> perfiles, la familia `condition_on_*` y hooks de ciclo de vida. Como Rust no
> tiene reflexión en tiempo de ejecución, el descubrimiento ocurre en **tiempo de
> enlazado** (a través del crate `inventory`) y el autowiring lo genera una macro
> derive en lugar de inferirse en tiempo de ejecución; un bean es descubrible
> exactamente cuando su crate está enlazado. El modelo es: declara la intención,
> deja que el contenedor proporcione las instancias. La superficie resulta
> familiar si has usado un framework con todo incluido, pero el mecanismo en
> tiempo de enlazado es propio de Firefly.

## Paso 1 — Ve el problema que resuelve la inversión de control

Empieza escribiendo el cableado *a mano*, como lo harías sin un contenedor, para
que el valor de invertirlo sea concreto. El lado de lectura de Lumen mantiene
vistas de carteras en un `ReadModel`; su lado de escritura es un `Ledger` sobre un
event store y un broker; su superficie HTTP es un controlador `WalletApi` que
necesita tanto el bus CQRS como el ledger. Cableado a mano, eso es un ensamblaje
pequeño pero real:

```rust,ignore
let store = Arc::new(MemoryEventStore::new());
let broker = Arc::new(InMemoryBroker::new());
let read_model = Arc::new(ReadModel::default());
let ledger = Arc::new(Ledger::new(Arc::clone(&store), Arc::clone(&broker)));
// ... and then hand the collaborators to the controller's state, in order.
```

El problema no son las cuatro líneas — es que **tú** eres responsable del
*orden*. `Ledger` debe existir antes que `WalletApi`; `store` y `broker` deben
existir antes que `Ledger`. Añade un quinto colaborador y vuelves a re-tejer la
función. El contenedor invierte esto: cada bean *declara* sus dependencias, y el
contenedor llama a los constructores en orden de dependencias por ti.

> **Note** **Término clave — raíz de composición.** La *raíz de composición* es el
> único lugar de un programa donde se ensambla todo el grafo de objetos. El bloque
> escrito a mano de arriba *es* una raíz de composición. En Firefly el framework
> es la raíz de composición: escanea tus beans y los cablea, de modo que nunca
> deletreas el grafo en una función.

Qué acaba de pasar: viste el cableado exacto que el contenedor asumirá. La
recompensa no son solo menos líneas — es un registro central que el panel de
administración introspecciona (la página `/beans`), un informe de arranque que
registra el grafo línea a línea, y un *error de resolución en el arranque* en
lugar de un pánico en lo más profundo de la primera petición cuando falta algo.

> **Tip** **Punto de control.** Puedes nombrar los tres colaboradores nucleares de
> Lumen — `ReadModel`, `Ledger`, `WalletApi` — y decir cuál depende de cuál. Ten
> ese grafo en mente; cada paso de abajo cablea una arista de él.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 300" role="img"
     aria-label="Dependency-injection bean graph: a Container scans stereotype beans and autowires WalletApi to the Ledger and ReadModel, which in turn autowire the EventStore and Broker ports, in dependency order"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="16" y="14" width="528" height="272" rx="14" fill="#fbf3e3" stroke="#e6d4b0" stroke-width="1.3"/>
<text x="36.0" y="36.0" text-anchor="start" font-size="12" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Container  ·  scan() wires beans in dependency order</text>
<rect x="196.0" y="56.5" width="168.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="196.0" y="54.0" width="168.0" height="48.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="280.0" y="75.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">WalletApi</text><text x="280.0" y="89.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">#[derive(Controller)]</text>
<rect x="70.0" y="152.5" width="168.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="70.0" y="150.0" width="168.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="154.0" y="171.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Ledger</text><text x="154.0" y="185.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">#[derive(Service)]</text>
<rect x="322.0" y="152.5" width="168.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="322.0" y="150.0" width="168.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="406.0" y="171.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ReadModel</text><text x="406.0" y="185.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">#[derive(Component)]</text>
<rect x="70.0" y="236.5" width="168.0" height="44.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="70.0" y="234.0" width="168.0" height="44.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="154.0" y="253.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">EventStore</text><text x="154.0" y="267.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">#[derive(Repository)]</text>
<rect x="322.0" y="236.5" width="168.0" height="44.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="322.0" y="234.0" width="168.0" height="44.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="406.0" y="253.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Broker</text><text x="406.0" y="267.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">port — #[autowired]</text>
<line x1="248.0" y1="102.0" x2="176.8" y2="145.8" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="170.0,150.0 174.5,142.0 179.2,149.6" fill="#b5531f"/><text x="209.0" y="124.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">autowired</text>
<line x1="312.0" y1="102.0" x2="383.2" y2="145.8" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="390.0,150.0 380.8,149.6 385.5,142.0" fill="#b5531f"/><text x="351.0" y="124.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">autowired</text>
<line x1="154.0" y1="198.0" x2="154.0" y2="226.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="154.0,234.0 149.5,226.0 158.5,226.0" fill="#b5531f"/>
<line x1="406.0" y1="198.0" x2="406.0" y2="226.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="406.0,234.0 401.5,226.0 410.5,226.0" fill="#b5531f"/>
</svg>
<figcaption>El contenedor escanea los beans de estereotipo y los cablea en orden de dependencias. <code>WalletApi</code> autocablea el <code>Ledger</code> y el <code>ReadModel</code>; el <code>Ledger</code> autocablea los puertos <code>EventStore</code> y <code>Broker</code> — sin raíz de composición a mano.</figcaption>
</figure>

## Paso 2 — Declara un bean con un derive de estereotipo

Un **bean** es cualquier valor que el contenedor construye, cablea y posee.
Conviertes un tipo en bean con un **derive de estereotipo** — una anotación ligera
que genera un método `firefly_register(&Container)` *y* envía un thunk de escaneo
en tiempo de enlazado para que el escaneo de componentes pueda encontrarlo.

> **Note** **Término clave — estereotipo.** Un *estereotipo* es un derive que a la
> vez convierte un tipo en bean y documenta su rol arquitectónico. Se distribuyen
> cinco, todos funcionalmente equivalentes — difieren solo en la intención que
> registran y la etiqueta que llevan a la vista `/beans`. Son los estereotipos
> `@Component` / `@Service` / `@Repository` / `@Configuration` / `@Controller` de
> Spring.

| Derive | Documenta |
|--------|-----------|
| `#[derive(Component)]` | un bean gestionado genérico |
| `#[derive(Service)]` | lógica de negocio / caso de uso |
| `#[derive(Repository)]` | acceso a datos / un puerto |
| `#[derive(Configuration)]` | un contenedor de factorías `#[bean]` |
| `#[derive(Controller)]` | un bean de controlador web |

El read model de Lumen es un bean de acceso a datos, así que lleva
`#[derive(Repository)]`. Este es código real de `samples/lumen/src/ledger.rs`:

```rust,ignore
use firefly::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Default, Repository)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}
// generated: ReadModel::firefly_register(&container)  +  a component-scan thunk
```

Qué acaba de pasar: derivar `Repository` registró `ReadModel` como bean singleton
y envió un thunk de escaneo para que `container.scan()` lo encuentre en el
arranque. Elegir `Repository` en lugar de `Component` no cuesta nada técnicamente,
pero le dice a todo lector — y a la página `/beans` — exactamente para qué sirve
`ReadModel`.

> **Note** **Término clave — singleton.** Un bean *singleton* tiene una sola
> instancia, cacheada tras la primera vez que se resuelve, y compartida por todo
> lo que depende de ella. Es el ámbito por defecto; conocerás los demás en el Paso
> 9. El ámbito de bean por defecto de Spring es el mismo.

Por qué importa: la proyección que llena el read model y el query handler que lo
lee, ambos autocablean `Arc<ReadModel>` — y como es un singleton, comparten *el
mismo* mapa. Una lectura tras escritura ve la escritura.

> **Tip** **Punto de control.** Un struct con un derive de estereotipo es un bean.
> Si ejecutaste Lumen y abriste `http://localhost:8081/admin/`, `ReadModel`
> aparece en la página `/beans` etiquetado como `repository`.

## Paso 3 — Inyecta dependencias con `#[autowired]`

Las dependencias de un bean son sus campos `#[autowired]`. El contenedor resuelve
cada uno por tipo y lo asigna antes de que el bean se construya. El **tipo del
campo** controla la *forma* de la inyección:

> **Note** **Término clave — autowiring (inyección por constructor).** El
> *autowiring* significa que el contenedor llena un campo resolviendo su tipo
> desde el registro, en lugar de que tú pases el valor. Firefly lo hace en tiempo
> de construcción, de modo que una dependencia requerida que falta es un error
> ruidoso en el arranque, no un `None` tres marcos dentro de una petición. Esta es
> la inyección por constructor de `@Autowired` de Spring.

| Tipo del campo | Resuelve mediante |
|------------|--------------|
| `Arc<T>` | `resolve::<T>()` (requerido) |
| `Option<Arc<T>>` | `resolve::<T>().ok()` (opcional) |
| `Vec<Arc<T>>` | `resolve_all::<T>()` (todas las implementaciones) |
| `Provider<T>` | un handle diferido (`container.provider::<T>()`) |

La **proyección** del read model de Lumen es un bean `#[derive(Service)]` que
autocablea el `Ledger` (cuyo event store reproduce) y el `ReadModel` que alimenta.
Este es código real de Lumen:

```rust,ignore
use firefly::prelude::*;
use std::sync::Arc;

#[derive(Service)]
struct WalletProjection {
    #[autowired]
    ledger: Arc<Ledger>,
    #[autowired]
    read_model: Arc<ReadModel>,
}
```

Cuando el contenedor construye `WalletProjection`, resuelve primero `Arc<Ledger>` y
`Arc<ReadModel>` — construyendo cada uno recursivamente si aún no lo ha hecho — y
luego construye la proyección con ambos campos asignados. El orden se *deriva de
los tipos de los campos*, nunca se escribe.

> **Note** **Término clave — `Arc<T>`.** `Arc<T>` es el puntero compartido de Rust
> con conteo de referencias atómico. Los beans se comparten (un singleton tiene
> muchos poseedores), así que el contenedor siempre entrega un `Arc`. Clonar un
> `Arc` es barato — incrementa un contador, no los datos — por lo que los structs
> de bean suelen ser `#[derive(Clone)]` con campos `Arc`.

Qué acaba de pasar: declaraste *qué* necesita la proyección, y el contenedor lo
proporcionará en orden de dependencias. Un campo sin atributo se llena con
`Default::default()`; un campo `#[firefly(value = "${...}")]` se enlaza desde la
configuración (Paso 8).

> **Note** Una dependencia **requerida** que falta (`Arc<T>` sin proveedor) es un
> ruidoso `ContainerError::NoSuchBean` en tiempo de resolución, con sugerencias
> aproximadas de tipo «¿querías decir…?», en lugar de un fallo silencioso. Hazla
> opcional con `Option<Arc<T>>` y el campo pasa a ser `None` en lugar de un error.

> **Note** **La regla `Default`.** La factoría generada construye el struct como un
> literal, llenando los campos `#[autowired]` / `#[firefly(value = ...)]` desde el
> contenedor y **todos los demás campos** con `Default::default()`. Por tanto, un
> struct de estereotipo necesita `#[derive(Default)]` si — y solo si — tiene al
> menos un campo que no sea ni `#[autowired]` ni `#[firefly(value = ...)]` (como el
> campo `rows` de `ReadModel`). Un struct todo-autowired, o un contenedor sin
> campos como un `#[derive(Configuration)]`, compila sin él.

> **Note** Autocablear un campo `Provider<T>` requiere que el contenedor tenga un
> handle a *sí mismo*, cosa que solo un contenedor compartido tiene. Construye uno
> con `Container::shared()` (o llama a `install_shared_handle()` sobre un
> `Arc<Container>`); un `ApplicationContext` ya lo hace por ti. Resolver un campo
> `Provider<T>` contra un `Container::new()` pelado provoca un pánico con un
> mensaje que te indica usar `shared()`.

> **Tip** **Punto de control.** Puedes predecir el orden de construcción de
> `WalletProjection`: el contenedor resuelve primero `Ledger` y `ReadModel`, luego
> construye la proyección. En ningún sitio escribiste ese orden — los tipos de los
> campos lo codifican.

## Paso 4 — Depende de un puerto, obtén el adaptador

Lumen depende de *puertos* — `EventStore`, `Broker` — y elige un adaptador en
tiempo de cableado. El contenedor expresa «depende del puerto, obtén el
adaptador» con una **vinculación** (binding) a un objeto-trait: registra el tipo
concreto, vincula el trait a él, y luego resuelve el trait.

```rust,ignore
use firefly::prelude::*;

let c = Container::new();
firefly::register_all!(&c, [MemoryEventStore]);
c.bind::<dyn firefly::eventsourcing::EventStore, MemoryEventStore>(|a| a);
let store: Arc<dyn firefly::eventsourcing::EventStore> = c.resolve().unwrap();
```

Qué acaba de pasar: `register_all!` registró el `MemoryEventStore` concreto; `bind`
registró «el trait `EventStore` lo satisface `MemoryEventStore`»; y `resolve`
devuelve entonces el adaptador a través del tipo del puerto. Los consumidores
autocablean `Arc<dyn EventStore>` y nunca nombran el adaptador concreto.

> **Note** `bind::<I, T>(|a| a)` provoca un pánico si `T` no está registrado
> primero — bind es una vista sobre un registro *existente*, así que registra el
> tipo concreto antes de vincularle un trait.

Cuando **varios** adaptadores respaldan un puerto — la clásica división en memoria
frente a Postgres — exactamente uno debe marcarse como **primary**, o la
resolución falla ruidosamente:

> **Note** **Término clave — bean primary.** Cuando más de un bean satisface un
> tipo, el marcado con `#[firefly(primary)]` es la elección por defecto. Sin un
> primary y con más de un candidato, la resolución falla en lugar de adivinar.
> Esto es `@Primary` de Spring.

```rust,ignore
#[derive(Repository)]
#[firefly(primary)]                 // the default adapter
pub struct MemoryEventStore { /* … */ }

#[derive(Repository)]
pub struct PostgresEventStore { /* … */ }   // activated by profile/condition
```

Las reglas de resolución, en estricto orden de prioridad:

1. **Registro directo** — un tipo `T` registrado directamente se resuelve a él.
2. **Vinculación única** — una implementación vinculada a un trait se resuelve a
   ella.
3. **`#[firefly(primary)]`** — entre varias vinculaciones, gana la primary.
4. **Error** — `NoSuchBean` cuando nada coincide; `NoUniqueBean` (nombrando cada
   candidato en competencia) cuando varios coinciden sin primary.

Qué acaba de pasar: aprendiste los únicos cuatro resultados que `resolve` puede
producir. Mover `primary` de un adaptador al otro es el *único* cambio necesario
para intercambiar el almacén de respaldo de Lumen — nada de `Ledger` cambia,
porque `Ledger` depende del puerto.

> **Tip** **Punto de control.** Dados dos adaptadores vinculados a un puerto sin
> primary, puedes predecir el error: `NoUniqueBean`, nombrando ambos candidatos.
> Añade `#[firefly(primary)]` a uno y la resolución tiene éxito — sin cambio en el
> consumidor.

### `#[firefly(order)]` y beans con nombre

`#[firefly(order = N)]` controla la secuencia de inicialización y el orden en que
`resolve_all::<T>()` devuelve las implementaciones (el menor se ejecuta primero);
las constantes `HIGHEST_PRECEDENCE` / `LOWEST_PRECEDENCE` marcan los extremos.
Cuando dos beans comparten un tipo — digamos un almacén primary y una réplica de
lectura — dale a uno un **nombre** y selecciónalo con un **cualificador**:

> **Note** **Término clave — cualificador.** Un *cualificador* nombra cuál de
> varios beans del mismo tipo quieres en un punto de inyección. El bean productor
> lleva `#[firefly(name = "…")]`; el campo consumidor lleva
> `#[firefly(qualifier = "…")]`. Esto es `@Qualifier` de Spring.

```rust,ignore
#[derive(Repository)]
#[firefly(name = "replica")]
pub struct ReplicaStore { /* … */ }

#[derive(Service)]
pub struct ReportService {
    #[firefly(qualifier = "replica")] store: Arc<ReplicaStore>,
}
```

Qué acaba de pasar: el cualificador desambigua por nombre donde el tipo por sí
solo es ambiguo. Un cualificador mal escrito que apunta al tipo equivocado es un
claro `NoSuchBean`, no una inyección silenciosamente errónea.

## Paso 5 — Produce beans que no posees con factorías `#[bean]`

No toda dependencia es un tipo que puedas anotar — un cliente de terceros necesita
argumentos de constructor, una interfaz necesita un adaptador construido a mano.
Para estos, un contenedor `#[derive(Configuration)]` expone **métodos factoría**
`#[bean]`. Cada método se indexa por su **tipo de retorno**; sus argumentos
`Arc<Dep>` se resuelven desde el contenedor, de modo que una factoría puede
depender de otros beans.

> **Note** **Término clave — factoría de beans.** Una *factoría de beans* es un
> método cuyo valor de retorno se convierte en un bean, indexado por el tipo de
> retorno. La usas cuando un bean no puede simplemente derivar un estereotipo —
> necesita lógica de construcción o envuelve un tipo externo. Esto es el método
> `@Bean` sobre una clase `@Configuration` de Spring.

Así es exactamente como Lumen produce sus beans de infraestructura nucleares —
código real de `samples/lumen/src/web.rs`:

```rust,ignore
use firefly::prelude::*;
use std::sync::Arc;
use firefly::cqrs::QueryCache;
use firefly::eda::Broker;
use firefly::eventsourcing::{EventStore, MemoryEventStore};

#[derive(Configuration)]
struct LumenBeans;

#[bean]
impl LumenBeans {
    /// The in-memory event store.
    #[bean]
    fn event_store(&self) -> MemoryEventStore {
        MemoryEventStore::new()
    }

    /// The read-side query cache honouring `GetWallet`'s 30s TTL.
    #[bean]
    fn query_cache(&self) -> QueryCache {
        QueryCache::new()
    }

    /// The ledger application service — autowires the event store and the
    /// framework-provided `Broker` port.
    #[bean]
    fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

Qué acaba de pasar: `LumenBeans` es un contenedor `@Configuration` sin campos. Sus
tres métodos producen tres beans, cada uno indexado por su tipo de retorno
(`MemoryEventStore`, `QueryCache`, `Ledger`). Los argumentos del método `ledger`
(`Arc<MemoryEventStore>`, `Arc<dyn Broker>`) se resuelven desde el contenedor — de
modo que una factoría puede cablearse a sí misma desde otros beans. **No** llamas a
nada para cablear estos: `Container::scan()` descubre el contenedor *y* sus
métodos `#[bean]` (cada uno envía su propio thunk de escaneo) y los registra
automáticamente.

> **Note** Por esto el `Ledger` de Lumen es un struct simple, no un bean
> `#[derive(Service)]`: lo *produce* esta factoría en lugar de derivarse. Un
> `#[autowired] ledger: Arc<Ledger>` aguas abajo (en `WalletApi`, en la
> proyección) lo encuentra porque la factoría registró el valor bajo el tipo
> `Ledger`. Intercambiar `MemoryEventStore` por un adaptador de Postgres es una
> línea en este contenedor; el resto de Lumen queda intacto.

Un método `#[bean]` devuelve un **tipo concreto (con tamaño)** — esa es la clave
del bean. Para exponerlo tras un puerto, añade
`#[firefly(provides = "dyn Broker")]` al contenedor o llama a `Container::bind`
tras el registro. Las opciones por método reflejan las de nivel de struct:
`#[bean(name = "...", scope = "...", primary, order = N, profile = "...",
condition_on_property = "k=v", condition_on_class = "…", condition_on_bean = "T",
condition_on_missing_bean = "T", condition_on_single_candidate = "T")]`.

> **Tip** **Punto de control.** Puedes explicar por qué `LumenBeans` no llama a
> ningún `register_*` ni a `bind`: `container.scan()` descubre cada `#[bean]` y
> registra su valor de retorno bajo el tipo de retorno. El framework hace el
> registro.

### Beans async (`async fn #[bean]`)

Un bean que realiza E/S para construirse a sí mismo — abrir un pool de base de
datos, conectar con un broker, precalentar una caché — declara su factoría
`async`. El servicio [`lumen-ledger`](./22-layered-microservices.md) hace
exactamente esto; código real de su `WalletPersistenceConfig`:

```rust,ignore
use firefly::data_sqlx::Db;
use firefly::prelude::*;

#[derive(Configuration, Default)]
pub struct WalletPersistenceConfig;

#[firefly::bean]
impl WalletPersistenceConfig {
    /// The `Db` datasource bean — an async factory that opens the connection
    /// pool and applies the schema with `await`.
    #[bean]
    async fn data_source(&self) -> Db {
        connect_and_migrate().await
    }
}
```

Qué acaba de pasar: el framework aparca una factoría `async` durante el
`container.scan()` síncrono y la `await`ea durante `Container::init_async_beans()`
— ejecutado por el bootstrap inmediatamente después del escaneo, antes de que se
resuelvan controladores, handlers y singletons eager — y luego instala el
resultado como un singleton listo. Los beans async se secuencian mediante
`#[bean(order = N)]`, de modo que uno puede autocablear otro inicializado antes.

> **Note** Esto es el «un `@Bean` hace E/S bloqueante en tiempo de refresco del
> contexto» de Spring Boot, salvo que la E/S se `await`ea en lugar de bloquear un
> hilo. Un fallo de la factoría se reporta como un error `BeanCreation` nombrando
> el bean — el «Error creating bean named '…'» de Spring.

`FireflyApplication` drena los beans async en su propia ruta de bootstrap. Si
construyes un `ApplicationContext` directamente, llama a
**`build_async().await`** en lugar de `build()` — el `build()` síncrono no puede
`await`ear un bean async pendiente y provoca un **pánico** en lugar de dejarlo
silenciosamente sin inicializar:

```rust,ignore
let ctx = ApplicationContext::builder().build_async().await?;   // awaits async beans
```

En `lumen-ledger`, el `Db` de esta factoría async es a partir de lo que se
construye el repositorio `#[derive(SqlxRepository)]` — el repositorio abre el pool
y ejecuta la migración con `await` antes de que llegue cualquier petición.

## Paso 6 — Limita beans por perfil y condición

Hasta ahora cada bean siempre existe. Las condiciones responden a una pregunta
distinta: «¿debería este bean existir *en absoluto*, dado el entorno?» Este es el
mecanismo que permite que una misma base de código Lumen se ejecute sobre
infraestructura en memoria en los tests e infraestructura real en producción sin
un `if` en el código del servicio.

> **Note** **Término clave — perfil.** Un *perfil* es una bandera de entorno con
> nombre (`dev`, `test`, `prod`) que limita qué beans están activos. Un bean con
> `#[firefly(profile = "prod")]` existe solo cuando `prod` está activo. Los
> perfiles soportan una gramática booleana — `prod & cloud`, `dev | test`,
> `!staging`, paréntesis. Esto es `@Profile` de Spring.

> **Note** **Término clave — bean condicional.** Un *bean condicional* existe solo
> cuando se cumple una condición declarada — una propiedad de configuración está
> establecida, una etiqueta de característica está presente, otro bean existe o
> falta. El contenedor las evalúa en tiempo de escaneo. Esto es la familia
> `@ConditionalOn*` de Spring Boot.

`Container::scan` evalúa las condiciones en **dos pasadas**.

**Pasada 1** asienta los hechos de configuración/perfil — conocibles antes de que
se construya ningún bean:

```rust,ignore
#[derive(Repository)]
#[firefly(profile = "prod", condition_on_property = "lumen.store.postgres=true")]
pub struct PostgresEventStore { /* … */ }
```

**Pasada 2** evalúa las condiciones dependientes del registro — conocibles solo
después de que la pasada 1 se asiente. Esto habilita el patrón
**por-defecto-con-anulación**: distribuye un respaldo que cede ante cualquier
implementación provista por el usuario:

```rust,ignore
#[derive(Repository)]
#[firefly(condition_on_property = "lumen.store.postgres=true")]
pub struct PostgresEventStore { /* … */ }   // real store when configured

#[derive(Repository)]
#[firefly(condition_on_missing_bean = "EventStore")]
pub struct MemoryEventStore { /* … */ }      // fallback whenever none is wired
```

Qué acaba de pasar: con la propiedad sin establecer, `PostgresEventStore` se omite
en la pasada 1, así que en la pasada 2 no hay bean `EventStore` — y el
`condition_on_missing_bean` de `MemoryEventStore` se dispara, registrando el
respaldo. Establece la propiedad y `PostgresEventStore` se registra en la pasada
1, de modo que el respaldo se retira. Ni un `if` a la vista.

La familia condicional completa: `condition_on_property = "key=value"`,
`condition_on_class = "label"`, `condition_on_bean = "Type"`,
`condition_on_missing_bean = "Type"`, `condition_on_single_candidate = "Type"`,
más `profile = "expr"`.

> **Design note.** El escaneo evalúa las condiciones en dos pasadas precisamente
> para que las dependientes del registro (`condition_on_bean`,
> `condition_on_missing_bean`, `condition_on_single_candidate`) puedan ver el
> *resultado* de la pasada de configuración/perfil. Este escaneo en dos pasadas es
> cómo *toda* la autoconfiguración propia de Firefly se retira cuando proporcionas
> tu propio bean — el tema del siguiente paso.

> **Tip** **Punto de control.** Puedes predecir qué almacén se resuelve: con
> `lumen.store.postgres` sin establecer, el respaldo en memoria; con él
> establecido a `true`, el almacén Postgres. El código de servicio que depende de
> `Arc<dyn EventStore>` es idéntico en ambos casos.

## Paso 7 — Deja que una autoconfiguración se aparte de tu camino

Una **autoconfiguración** es cómo un starter aporta valores por defecto sensatos
que desaparecen en el momento en que declaras los tuyos. Es el patrón
por-defecto-con-anulación del Paso 6, empaquetado.

> **Note** **Término clave — autoconfiguración.** Una *autoconfiguración* es un
> contenedor `@Configuration` cuyos `#[bean]` están protegidos por
> `condition_on_missing_bean` y se registran **los últimos** en el escaneo — de
> modo que tu propio bean siempre gana y el valor por defecto aporta algo solo
> cuando no escribiste nada. Esto es `@AutoConfiguration` de Spring Boot.

Deriva `#[derive(AutoConfiguration)]` y protege cada `#[bean]`:

```rust,ignore
use firefly::prelude::*;

#[derive(AutoConfiguration, Default)]
pub struct CacheAutoConfiguration;

#[firefly::bean]
impl CacheAutoConfiguration {
    #[bean(condition_on_missing_bean = "CacheClient", condition_on_property = "cache.type=memory")]
    fn cache_client(&self) -> CacheClient {
        CacheClient::in_memory()           // the default — only if you didn't wire one
    }
}
```

Qué acaba de pasar: `#[derive(AutoConfiguration)]` es un `#[derive(Configuration)]`
cuyos beans se registran **los últimos** durante el escaneo. Como sus `#[bean]`
llevan `condition_on_missing_bean`, el escaneo en dos pasadas registra primero tu
bean incondicional, y luego *omite* el valor por defecto de la autoconfiguración —
de modo que tu bean siempre gana, y nunca escribes un `if`. Quita el starter de
tus dependencias y el código de la autoconfiguración no se enlaza, así que no
aporta nada: el descubrimiento ocurre en tiempo de enlazado, no por reflexión.

> **Design note.** «Presente exactamente cuando está enlazado» es todo el truco.
> Una autoconfiguración aporta sus valores por defecto precisamente cuando su
> crate está compilado dentro, y no aporta nada una vez quitas el starter — sin
> escaneo del classpath, sin reflexión, sin registro `spring.factories` que
> mantener.

## Paso 8 — Trae la configuración directamente a un bean

Un servicio no debería pasar la configuración a través de su constructor a mano.
Dos atributos de estereotipo la traen directamente. Se cubren por completo en
[Configuración](./03-configuration.md) §«Enlazar configuración directamente a un
bean»; este es el resumen relevante para la DI.

**Valor único — `#[firefly(value = "${key:default}")]`** enlaza un único escalar
resuelto y con marcadores expandidos sobre un campo (parseado vía `FromStr`); la
cola `:default` aporta un respaldo cuando la clave está ausente:

> **Note** **Término clave — marcador (placeholder).** Un *marcador* como
> `${lumen.web.addr}` se reemplaza en tiempo de enlazado con el valor de
> configuración resuelto, con una cola `:default` opcional. Solo se soportan
> marcadores `${...}` — las expresiones SpEL `#{...}` quedan fuera de alcance
> (véase el Paso 12). Esto es `@Value` de Spring en su forma de marcador.

```rust,ignore
#[derive(Service)]
pub struct WalletApiConfig {
    #[firefly(value = "${lumen.web.addr:127.0.0.1:8080}")] addr: String,
}
```

**Subárbol completo — `#[derive(ConfigProperties)]`** enlaza un struct `serde`
bajo un prefijo y lo registra como un singleton inyectable; cualquier bean puede
entonces autocablearlo:

> **Note** **Término clave — propiedades de configuración.** Un struct de
> *propiedades de configuración* enlaza un subárbol completo de configuración
> (todo bajo un prefijo) en un struct `serde` tipado y lo registra como un bean.
> Esto es `@ConfigurationProperties` más `@EnableConfigurationProperties` de
> Spring.

```rust,ignore
use serde::Deserialize;
use firefly::prelude::*;
use std::sync::Arc;

#[derive(Deserialize, ConfigProperties, Default)]
#[firefly(prefix = "lumen.web")]
pub struct WebProperties {
    pub addr: String,
    #[serde(default)] pub admin_addr: String,
}

#[derive(Service)]
pub struct ReportService {
    #[autowired] props: Arc<WebProperties>,   // the bound subtree, injected
}
```

Qué acaba de pasar: `ConfigProperties` es el sexto derive consciente del
contenedor junto a los cinco estereotipos. Genera un `firefly_register` que enlaza
y registra el struct, y lleva una etiqueta de estereotipo `config_properties` a
`/beans`. Añadir `#[firefly(validate)]` (con `#[derive(Validate)]` en el struct)
ejecuta las restricciones declarativas del struct enlazado tras el enlazado, y una
violación **hace fallar la creación del bean** en el arranque en lugar de arrancar
una configuración malformada — el `@Validated` de Spring.

> **Note** `#[firefly(value = "${...}")]` es `@Value` (forma de marcador), y
> `#[derive(ConfigProperties)]` + `#[firefly(prefix = "...")]` es
> `@ConfigurationProperties`. Ambos enlazan contra el mismo mapa de configuración
> fusionado, resuelto por perfil y con marcadores expandidos descrito en
> [Configuración](./03-configuration.md).

## Paso 9 — Elige el ámbito de un bean

Cada bean tiene un **ámbito** que controla cuánto vive su instancia. Pásalo como
`#[firefly(scope = "...")]`. Casi todos los beans de Lumen son singletons; recurres
a los demás raramente pero deliberadamente.

| Ámbito | Comportamiento | Úsalo para |
|-------|-----------|---------|
| `singleton` (por defecto) | una instancia, cacheada tras la primera resolución | servicios sin estado, pools, cachés |
| `transient` | una instancia nueva en cada resolución | estado por operación (el contexto de borrador de una saga) |
| `request` | uno por petición HTTP (necesita un `ScopeHandler` de petición) | el usuario autenticado, un id de traza de petición |
| `session` | uno por sesión (necesita un `ScopeHandler` de sesión) | estado por sesión |

```rust,ignore
#[derive(Component)]
#[firefly(scope = "transient")]
pub struct TransferContext {            // a fresh scratch pad per transfer
    pub steps: Vec<String>,
}
```

Qué acaba de pasar: el nombre del ámbito es la variante en minúsculas de
`Scope::{Singleton, Transient, Request, Session}` (definido en
`crates/container/src/scope.rs`). Un bean `transient` se reconstruye en cada
resolución; los ámbitos `request` y `session` necesitan un handler, que se cubre a
continuación.

### Ámbitos de petición y sesión: el SPI `ScopeHandler`

Rust no tiene un thread-local ambiental de petición como sí tiene un contenedor
reflexivo de la JVM, así que los ámbitos `request` y `session` se **operan
explícitamente** mediante una implementación del SPI `ScopeHandler` — el análogo
directo del `org.springframework...config.Scope` de Spring. Un handler cachea una
instancia por clave (por petición, por sesión) y la desaloja cuando esa clave
termina:

> **Note** **Término clave — SPI (interfaz de proveedor de servicio).** Un *SPI*
> es un trait que el framework define y un *host* implementa para enchufar
> comportamiento. `ScopeHandler` es el SPI para el ciclo de vida de
> petición/sesión: instalas una implementación y el contenedor opera el ámbito a
> través de ella.

```rust,ignore
use firefly::prelude::*;
use std::sync::Arc;

// A host installs the handlers once at startup. Until one is installed,
// resolving a request/session-scoped bean is a NoSuchBean ("no active
// request context"), matching the Spring/pyfly behaviour.
container.register_request_scope(Arc::new(my_request_handler));
container.register_session_scope(Arc::new(my_session_handler));
```

Qué acaba de pasar: `register_request_scope` / `register_session_scope` respaldan
los dos ámbitos integrados; los ámbitos personalizados arbitrarios usan
`register_scope("name", handler)` (que rechaza nombres vacíos y los cuatro
integrados). Los tres viven en `Container` (`crates/container/src/lib.rs`); un
`ScopeHandler` es solo un `get(name, factory)` más `remove(name)`
(`crates/container/src/scope.rs`).

> **Design note.** El ciclo de vida de petición/sesión está *instalado*, no es
> implícito. Un `Container` pelado sin handler instalado reporta `NoSuchBean` para
> esos ámbitos en lugar de filtrar silenciosamente un singleton — el aplazamiento
> es explícito, el mismo compromiso descrito en el Paso 12.

### `RefreshScope` — paridad de recarga de configuración

Un cuarto handler, ya hecho, se distribuye para la recarga de configuración:
`RefreshScope` (`crates/container/src/scope.rs`). Un bean con ámbito refresh se
cachea como un singleton, pero una llamada a `refresh()` desaloja **todas** las
instancias con ámbito refresh para que la siguiente resolución las reconstruya
contra la nueva configuración — el hook que un futuro `/actuator/refresh` llamaría
ante un cambio de configuración. Regístralo bajo el nombre convencional
`REFRESH_SCOPE_NAME` (`"refresh"`):

```rust,ignore
use firefly::prelude::*;
use std::sync::Arc;

let refresh = Arc::new(firefly::container::RefreshScope::new());
container.register_scope(firefly::container::REFRESH_SCOPE_NAME, refresh.clone())?;

// On a config-change event:
let evicted: Vec<String> = refresh.refresh();   // rebuild on next resolve
```

Para un único singleton hay un hook más ligero: `container.reset_instance::<T>()`
descarta solo la instancia cacheada de `T` para que se reconstruya en la siguiente
resolución (devuelve si realmente se desalojó una instancia). Es la forma por-bean
de la misma idea de refresco-al-cambiar-configuración.

> **Note** `RefreshScope` / `REFRESH_SCOPE_NAME` reflejan el `@RefreshScope` de
> Spring Cloud, y `reset_instance::<T>()` es el hook de refresco por-bean. Ambos
> existen para que una recarga de configuración pueda reconstruir los beans
> afectados sin reiniciar el proceso.

### `#[firefly(lazy)]` — renunciar a la pasada de precalentamiento eager

Por defecto los singletons se construyen de forma **eager** cuando un
`ApplicationContext` arranca (Paso 11). `#[firefly(lazy)]` excluye a un singleton
**de esa pasada de precalentamiento eager** — entonces se construye en la primera
resolución en su lugar:

```rust,ignore
#[derive(Service)]
#[firefly(lazy)]                         // skipped at startup; built on first use
pub struct ExpensiveReportEngine { /* … */ }
```

> **Note** `#[firefly(lazy)]` es `@Lazy`: quita el bean de la pasada de
> precalentamiento del arranque para que un singleton caro o de uso poco frecuente
> se construya solo cuando algo lo resuelve. A diferencia de Spring, **no** crea un
> proxy y por tanto no rompe un verdadero ciclo de dependencias en tiempo de
> construcción; recurre a `Provider<T>` para aplazar una dependencia (Paso 12).

## Paso 10 — Enmarca un bean con hooks de ciclo de vida

Un bean a menudo necesita una configuración única tras inyectar sus dependencias,
y un desmontaje limpio al apagarse. Nombra un método por hook en el atributo del
struct:

> **Note** **Término clave — hooks de ciclo de vida.** Un hook *post-construct* se
> ejecuta una vez después de que el bean se construye y sus dependencias se
> inyectan; un hook *pre-destroy* se ejecuta al apagarse. Son claves de atributo
> en el struct, no macros independientes. Estos son `@PostConstruct` /
> `@PreDestroy` de Spring (el sustituto de `InitializingBean` / `DisposableBean`).

```rust,ignore
#[derive(Service)]
#[firefly(post_construct = "on_start", pre_destroy = "on_stop")]
pub struct ProjectionListener { /* … */ }

impl ProjectionListener {
    fn on_start(&self) { /* subscribe to wallets.events */ }
    fn on_stop(&self)  { /* unsubscribe, flush */ }
}
```

Qué acaba de pasar: `on_start` se ejecuta tras la construcción — con todos los
campos `#[autowired]` asignados — de modo que puede consultar a los colaboradores
con seguridad. `Container::destroy()` (el `close()` del `ApplicationContext`)
ejecuta cada hook `pre_destroy` en orden de construcción **inverso**, de modo que
un listener que arrancó después del read model se detiene antes que él.

> **Note** En el Lumen real la proyección del read model se suscribe al broker vía
> `#[event_listener]` (el mecanismo de EDA), no un hook `post_construct` — el hook
> aquí es el patrón general que cualquier bean usa para una configuración única.
> Verás la ruta de EDA en
> [Mensajería y arquitectura orientada a eventos](./10-eda-messaging.md).

## Paso 11 — Comprende el arranque eager y el fallo rápido

Un `Container` pelado construye los singletons de forma **perezosa** — el primer
`resolve::<T>()` construye `T` y lo cachea. El `ApplicationContext`, sin embargo,
es **eager** por defecto, igualando el arranque con fallo rápido de Spring.
`ApplicationContext::build()` escanea el grafo de crates, registra los
supervivientes, y luego **precalienta** inmediatamente cada singleton no-lazy
resolviéndolo una vez (`crates/firefly/src/context.rs`).

> **Note** **Término clave — inicialización eager / pasada de precalentamiento.**
> La *pasada de precalentamiento* es `build()` resolviendo cada singleton no-lazy
> una vez en el arranque, de modo que estén todos construidos antes de la primera
> petición. Esto es lo que te da la garantía de Spring de «valida el cableado en el
> arranque».

Dos cosas se derivan de esa pasada de precalentamiento:

- **Fallo rápido.** Un error de construcción — una dependencia requerida que
  falta, un pánico en un `post_construct` — aflora en el *arranque*, no en lo más
  profundo de la primera petición.
- **`post_construct` se ejecuta en el arranque.** Como precalentar un bean lo
  construye, el hook `post_construct` de cada singleton no-lazy se dispara durante
  `build()`, antes de que el contexto te entregue el contenedor.

Eso pone todo el ciclo de vida del bean en una línea:

> **escaneo → registro → precalentar singletons no-lazy → `post_construct` →
> (servir) → `close()` → `pre_destroy` en orden inverso**

Puedes excluir la pasada de precalentamiento por completo con `.eager(false)`, o
excluir un único bean con `#[firefly(lazy)]`:

```rust,ignore
use firefly::prelude::*;

let ctx = ApplicationContext::builder()
    .profiles(["prod"])
    .eager(false)            // skip the warm pass; everything builds lazily
    .build();
```

> **Design note.** El precalentamiento eager en `build()` es una política a nivel
> de *contexto*: un `Container` pelado permanece perezoso. `.eager(false)` apaga
> toda la pasada de precalentamiento; `#[firefly(lazy)]` excluye un bean de ella.
> `close()` es el apagado simétrico — ejecuta cada `pre_destroy` en orden de
> construcción inverso y desaloja los singletons cacheados.

### La superficie del builder de `ApplicationContext`

`ApplicationContext::builder()` acepta más que `.profiles()` y `.property()`; la
superficie completa (`crates/firefly/src/context.rs`):

| Método | Propósito |
|--------|---------|
| `.profiles([...])` | perfiles activos (por defecto `FIREFLY_PROFILE`, luego `"default"`) |
| `.property(k, v)` / `.properties(map)` | añade propiedades de configuración para marcadores y condiciones |
| `.config_sources(vec![...])` | fusiona capas `firefly_config::Source` (env, YAML) en el mapa de propiedades |
| `.class(label)` | marca una «etiqueta» de característica presente para comprobaciones `condition_on_class` |
| `.eager(bool)` | precalienta los singletons no-lazy en `build()` — por defecto `true` |
| `.build()` | construye el contenedor compartido, escanea y luego (por defecto) precalienta los singletons |
| `.build_async().await` | lo mismo, pero `await`ea los beans async pendientes (Paso 5) |

> **Note** `.class(label)` es el sustituto de Firefly para `@ConditionalOnClass`:
> Rust no tiene un classpath que sondear, así que un host declara qué
> características opcionales están «presentes» por etiqueta, y los beans
> `condition_on_class = "label"` se limitan según ello.

> **Tip** **Punto de control.** Puedes enunciar cuándo aflora una dependencia que
> falta: bajo un `ApplicationContext` eager, en `build()`; bajo un `Container`
> perezoso pelado, en el primer `resolve`. Lumen arranca a través de
> `FireflyApplication`, que es eager — así que los errores de cableado hacen fallar
> el arranque, no la primera petición.

## Paso 12 — Escaneo de componentes frente a `register_all!`

Ya has usado ambas rutas de descubrimiento; aquí está el contraste en un solo
lugar. `Container::scan()` recopila el thunk de escaneo de cada derive de
estereotipo no genérico a lo largo de todo el grafo de crates, aplica condiciones y
perfiles, y registra los supervivientes. Opéralo a través del `ApplicationContext`,
que construye el contexto de condiciones a partir de los perfiles activos y la
configuración:

```rust,ignore
use firefly::prelude::*;
use std::sync::Arc;

let ctx = ApplicationContext::builder()
    .profiles(["prod"])
    .property("lumen.store.postgres", "true")
    .build();                              // scans, then (by default) warms singletons
let store: Arc<dyn firefly::eventsourcing::EventStore> =
    ctx.resolve().unwrap();                // -> PostgresEventStore (prod)
println!("{} beans registered", ctx.bean_count());
```

Para escanear solo parte del grafo de crates, pasa las rutas de módulo base:

```rust,ignore
let c = Container::new();
c.scan_packages(&["lumen::domain", "lumen::ledger"]);  // these modules only
```

Un registro coincide cuando la ruta de módulo que lo define es igual a un paquete
base o es descendiente de él; las condiciones y los perfiles se aplican
exactamente como en `scan()`.

Los **beans genéricos** no pueden inventariarse (la monomorfización se elige en el
punto de uso), así que registra esos con el respaldo de lista explícita — el mismo
`register_all!` que usaste en el Paso 4:

```rust,ignore
let c = Container::new();
firefly::register_all!(&c, [ReadModel, Ledger, WalletApi]);  // calls each firefly_register
let api = c.resolve::<WalletApi>().unwrap();
```

> **Design note.** El descubrimiento ocurre en tiempo de enlazado vía `inventory`,
> no en tiempo de ejecución vía reflexión — así que un bean solo es descubrible si
> su crate está realmente enlazado. `register_all!` es el respaldo explícito para
> los genéricos, que no pueden inventariarse.

### Introspección — la vista `/beans`

El contenedor es observable. `container.beans()` devuelve un `BeanDescriptor` por
registro (nombre, tipo, ámbito, estereotipo, primary, inicializado, conteo de
resoluciones), y `bean_stats()` agrega los conteos por estereotipo — exactamente lo
que renderiza la página `/beans` del panel de administración, y lo que
`firefly beans --url …` (el [capítulo de la CLI](./19-cli.md)) imprime. Los errores
también llevan diagnósticos: `fuzzy_suggestions(name)` alimenta las pistas «¿querías
decir…?», y `CircularDependency` se captura con una pila de resolución por hilo.

> **Tip** **Punto de control.** Con Lumen en ejecución, abre
> `http://localhost:8081/admin/` y encuentra la página `/beans`. `ReadModel`
> (`repository`), `LumenBeans` (`configuration`), `WalletHandlers` /
> `WalletProjection` (`service`) y `WalletApi` (`controller`) están todos ahí — el
> mismo grafo que el informe de arranque registró línea a línea.

## Qué cambia el modelo de Rust

El contenedor de Firefly deliberadamente *no* es un clon línea por línea del de
Spring. El único hecho estructural detrás de cada diferencia es que **Rust no tiene
reflexión en tiempo de ejecución**: un bean se construye mediante una clausura
factoría generada que resuelve sus propias dependencias y construye el struct de
una sola vez, en lugar de las fases «instanciar → poblar → llamar a init» de
Spring. No hay una costura tras la instanciación donde tejer comportamiento, ni
forma de entregarle a un bean un handle reflexivo al contenedor. Las siguientes
características de Spring se reemplazan, por tanto, por modismos de Rust en lugar de
portarse — cada una una elección deliberada, no una característica ausente:

- **Sin `BeanPostProcessor` / `BeanFactoryPostProcessor`.** No hay fase de
  intercepción tras la instanciación ni pasada de reescritura de definiciones. El
  comportamiento transversal se compone explícitamente — envuelve un colaborador, o
  usa una factoría `#[bean]` para construir la instancia ya cableada que quieres.
- **Sin interfaces `*Aware`** (`ApplicationContextAware`, `EnvironmentAware`, …). A
  un bean no se le entrega el contexto mediante un callback. Autocablea lo que
  realmente necesitas — `Arc<WebProperties>` para configuración, `Provider<T>` para
  una dependencia diferida — en lugar de alcanzar de vuelta al contenedor.
- **Sin `FactoryBean`.** Un método factoría `#[bean]` produce cualquier tipo y se
  indexa por su tipo de retorno; eso cubre «un bean cuyo trabajo es construir otro
  bean» sin una abstracción `FactoryBean` distinta.
- **Sin proxies con ámbito `@Scope(proxyMode)`.** Un bean con ámbito
  `request`/`session` se resuelve a través de su `ScopeHandler`, no se inyecta en
  un singleton mediante un proxy transparente. Para traer una dependencia de ámbito
  más corto (o diferida) a un bean de vida más larga, mantén un `Provider<T>` y
  llama a `.get()` en el punto de uso — el sustituto idiomático tanto de los
  proxies `@Lazy` como de los proxies con ámbito.
- **Sin fases `SmartLifecycle` por bean.** El contenedor tiene `post_construct` /
  `pre_destroy` (el sustituto de `InitializingBean` / `DisposableBean`), y el orden
  de arranque/parada a nivel de aplicación vive en el crate `firefly-lifecycle`
  separado — no hay negociación de `start`/`stop`/`isRunning`/fase por bean.
- **Sin SpEL `#{...}`.** `#[firefly(value = "${...}")]` solo hace inyección de
  marcadores. Las expresiones, las referencias a métodos/beans y la aritmética en
  la configuración quedan intencionadamente fuera de alcance para el modismo de
  Rust tipado.

> **Design note.** Lee esta lista como *sustituciones*, no como carencias:
> `Provider<T>` hace las veces de `@Lazy` / proxies con ámbito / `ObjectFactory`
> cuando necesitas aplazar o alcanzar una dependencia de ámbito más corto;
> `post_construct` / `pre_destroy` hacen las veces de `InitializingBean` /
> `DisposableBean`; una factoría `#[bean]` hace las veces de `FactoryBean`; y la
> composición explícita hace las veces del tejido de `BeanPostProcessor`. La
> maquinaria reflexiva ha desaparecido, pero cada trabajo que hacía tiene una
> contraparte tipada y comprobada en compilación.

## Resumen

Este capítulo fue un recorrido guiado, no un paso-a-paso de código — el cableado
que documenta ya es cómo funciona `samples/lumen`. Ahora sabes cómo:

- Declarar un bean con un **derive de estereotipo** (`Component` / `Service` /
  `Repository` / `Configuration` / `Controller`) y leer su rol en la vista
  `/beans`.
- Cablear dependencias con inyección por constructor **`#[autowired]`**, y razonar
  sobre el orden de construcción a partir solo de los tipos de los campos.
- Depender de un **puerto** y seleccionar el **adaptador** con
  `#[firefly(primary)]`, nombres y cualificadores — conociendo al dedillo las
  cuatro reglas de resolución.
- Producir beans externos o construidos con factorías **`#[derive(Configuration)]`
  + `#[bean]`**, incluidas factorías **async** que `await`ean E/S en el arranque
  (drenadas por `init_async_beans()` / `build_async()`).
- Limitar beans por **perfil** y la familia **`condition_on_*`**, y dejar que una
  **autoconfiguración** se retire en el momento en que declaras tu propio bean.
- Elegir un **ámbito**, instalar el SPI **`ScopeHandler`** para los ámbitos
  request/session/refresh, enmarcar un bean con **hooks de ciclo de vida**, y
  confiar en el arranque **eager con fallo rápido**.
- Descubrir beans por **escaneo de componentes** o el respaldo **`register_all!`**
  para genéricos, e introspeccionar todo el grafo a través de **`/beans`**.

Los beans de Lumen, entre todos, ejercitan el conjunto completo de estereotipos:
`@Configuration` + `@Bean` (`LumenBeans`), `@Service` (`WalletHandlers`,
`WalletProjection`, el `RouteContributor` de streaming), `@Repository`
(`ReadModel`) y `@Controller` + `@Autowired` (`WalletApi`). Cada atributo mostrado
— `autowired`, `primary`, `order`, `qualifier`, `scope`, `lazy`, `profile`, la
familia `condition_on_*`, `post_construct`, `pre_destroy`, `provides`, `value` y
`prefix` — es una opción real sobre los derives de estereotipo.

## Ejercicios

1. **Resuelve el grafo a mano.** Toma `ReadModel`, `Ledger` y `WalletApi`,
   cablealos con `register_all!(&c, [ReadModel, Ledger, WalletApi])` y
   `c.resolve::<WalletApi>()`, y confirma que el contenedor construye el grafo en
   orden de dependencias — el mismo grafo que el escaneo de `FireflyApplication`
   construye en el arranque.
2. **Por-defecto-con-anulación.** Da a `MemoryEventStore`
   `#[firefly(condition_on_missing_bean = "EventStore")]` y a `PostgresEventStore`
   `#[firefly(condition_on_property = "lumen.store.postgres=true")]`. Escanea con y
   sin la propiedad establecida; confirma qué almacén se resuelve cada vez.
3. **Intercambio de primary.** Vincula dos adaptadores a un puerto sin primary y
   observa el error `NoUniqueBean` nombrando ambos candidatos. Añade
   `#[firefly(primary)]` a uno y mira cómo la resolución tiene éxito — sin cambio
   en el bean consumidor.
4. **Orden de ciclo de vida.** Añade hooks `post_construct` / `pre_destroy` a dos
   beans donde uno depende del otro; llama a `ApplicationContext::close()` y
   confirma que el dependiente se desmonta primero.
5. **Lee el grafo en vivo.** Ejecuta Lumen, abre `http://localhost:8081/admin/` y
   encuentra la página `/beans`. Mapea cada entrada de vuelta al código que la
   declaró — las factorías `#[bean]` en `LumenBeans`, el `#[derive(Repository)]`
   `ReadModel`, el `#[derive(Controller)]` `WalletApi` — y anota la etiqueta de
   estereotipo que lleva cada una.

## Adónde ir después

Ahora tienes en la mano el contenedor de DI completo de Firefly — el motor que
cablea cada bean en Lumen, desde las factorías `#[bean]` hasta el controlador
autocableado. El modelo reactivo sustenta todo lo que sigue — continúa a **[El
modelo reactivo](./05-reactive-model.md)**.
