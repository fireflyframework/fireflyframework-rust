# Persistencia y repositorios reactivos

Lumen ya tiene una API de monederos que devuelve un `WalletView`, pero ¿de dónde
procede esa vista? Al final de [Tu primera API HTTP](./06-first-http-api.md) la
respuesta honesta era "de un mapa en memoria". Este capítulo dota a Lumen de un
vocabulario de persistencia real y muestra la ruta de actualización exacta desde
ese mapa didáctico hasta una base de datos duradera, sin tocar un solo punto de
llamada.

El hilo conductor es un movimiento que el libro repite: *depende del contrato,
intercambia el backend*. El almacén de lectura de Lumen ya está planteado como un
repositorio; aquí aprendes el contrato del framework del que es una miniatura, la
superficie CRUD reactiva que transmite filas de forma perezosa, los adaptadores
relacional y documental que implementan esa superficie sobre Postgres / MySQL /
SQLite / MongoDB, y el límite transaccional que hace atómico un cambio con varias
escrituras. La build didáctica permanece libre de infraestructura todo el camino:
cada pieza duradera se ejercita contra un SQLite en memoria o un doble en
proceso, así que nada de lo aquí descrito necesita un servidor en ejecución.

Al terminar este capítulo, serás capaz de:

- Explicar el **patrón repositorio** como la costura entre el lado de consulta y
  el almacenamiento, y reconocer el `ReadModel` de Lumen como un repositorio
  artesanal en miniatura.
- Componer una consulta `Filter`, renderizarla como SQL parametrizado y leer un
  sobre de resultado paginado `Page<T>`.
- Manejar la **superficie CRUD reactiva** —repositorios `Mono` / `Flux`— contra
  un doble en memoria y un adaptador SQLite/Postgres real con streaming.
- Declarar un repositorio al estilo de Spring Data con `#[derive(Entity)]` +
  `#[derive(SqlxRepository)]`, y añadir consultas derivadas y personalizadas con
  `#[firefly::repository]`.
- Activar el **bloqueo optimista** y construir un pool a partir de la
  configuración con una sola llamada esperada a `auto_configure`.
- Hacer atómico un cambio con varias escrituras mediante
  `#[firefly::transactional]` y su enlistamiento ambiental.

## Conceptos que conocerás

Antes del primer listado, estas son las ideas en las que se apoya este capítulo.
Cada una se reintroduce en contexto donde se usa por primera vez; esta es la
versión corta.

> **Note** **Término clave — repositorio.** Un *repositorio* es un objeto que
> oculta cómo se almacenan las entidades tras un conjunto pequeño de operaciones
> que revelan la intención —`find_by_id`, `save`, `delete`—. Quienes lo llaman
> dependen de la *interfaz* del repositorio, no de SQL ni de un `HashMap`. Esto
> es exactamente el `Repository` / `CrudRepository` de Spring Data.

> **Note** **Término clave — puerto y adaptador.** Un *puerto* es una interfaz de
> la que depende tu código; un *adaptador* es una implementación concreta elegida
> en el momento del cableado. `firefly-data` posee los puertos (los traits de
> repositorio, el DSL de consultas); `firefly-data-sqlx` y `firefly-data-mongodb`
> son adaptadores. Intercambiar el adaptador intercambia la base de datos sin
> cambiar los puntos de llamada: esto es *arquitectura hexagonal* (puertos y
> adaptadores).

> **Note** **Término clave — `Mono` / `Flux`.** Estos son los *publishers*
> reactivos de [El modelo reactivo](./05-reactive-model.md): un `Mono<T>` resuelve
> a lo sumo un valor, y un `Flux<T>` a un flujo perezoso y con contrapresión de
> muchos. El repositorio reactivo los devuelve para que una lectura de base de
> datos pueda transmitirse fila a fila al cliente. Si has usado una biblioteca de
> reactive-streams (Project Reactor, RxJava), son los mismos `Mono` / `Flux`.

> **Note** **Término clave — bloqueo optimista.** Una estrategia de concurrencia
> en la que cada fila lleva un número de *versión*; una escritura solo tiene éxito
> si la versión que cargó aún coincide con la almacenada; de lo contrario se
> rechaza en lugar de sobrescribir silenciosamente un cambio concurrente. Esto es
> el `@Version` de Spring Data.

> **Design note.** La capa de datos de Firefly es el patrón repositorio expresado
> como Rust idiomático: dependes de un trait —`Repository<T, K>` (bloqueante) o
> `ReactiveCrudRepository<T, ID>` (reactivo)— y un adaptador aporta el SQL.
> Depende del puerto, intercambia el backend. `firefly-data` en sí mismo no posee
> ningún driver ni implica ningún motor SQL; eso es lo que hace que el intercambio
> sea mecánico.

## Paso 1 — Ver el almacén de lectura de Lumen como un repositorio

Lumen separa su modelo de escritura de su modelo de lectura: la forma de
Segregación de Responsabilidad entre Comandos y Consultas (CQRS) que desarrollas
en [CQRS](./09-cqrs.md). El lado de escritura es el `Ledger` con event sourcing;
el lado de lectura es un `ReadModel` plano y optimizado para consultas que sirve
`GET /api/v1/wallets/:id`. En `samples/lumen` el modelo de lectura es un mapa en
memoria —pequeño, exacto y sin dependencias—, la línea base adecuada para
enseñar.

Abre `samples/lumen/src/ledger.rs` y lee el tipo del modelo de lectura:

```rust,ignore
// samples/lumen/src/ledger.rs — the CQRS query side.
use std::collections::HashMap;
use std::sync::Mutex;

use firefly::prelude::*;
use crate::domain::WalletView;

/// The in-memory read model: a map of wallet id → WalletView, upserted by the
/// projection and served by the GetWallet query. It carries
/// `#[derive(Repository)]` (Spring's `@Repository`), so `container.scan()`
/// registers it as a data-access singleton — autowired as `Arc<ReadModel>` into
/// the handler and projection beans. A real service would back this with
/// firefly's reactive repository over Postgres; an in-memory map keeps the
/// teaching baseline dependency-free.
#[derive(Debug, Default, Repository)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}

impl ReadModel {
    /// Upserts a projected view, replacing any previous row for the id.
    pub fn upsert(&self, view: WalletView) {
        self.rows
            .lock()
            .expect("read model lock")
            .insert(view.id.clone(), view);
    }

    /// Looks a projected view up by id.
    pub fn find(&self, id: &str) -> Option<WalletView> {
        self.rows.lock().expect("read model lock").get(id).cloned()
    }
}
```

Qué acaba de ocurrir: dos decisiones de diseño son deliberadas.

- La superficie es un **repositorio en miniatura**. `upsert(view)` y `find(id)`
  son las únicas operaciones que necesita el lado de consulta, así que esas son
  las únicas operaciones que expone: un subconjunto artesanal de dos métodos del
  contrato de repositorio del framework que conoces en el Paso 4.
- Las claves y los valores son tipos de dominio simples: un `WalletView`
  indexado por su `id`. Así, cuando más adelante intercambies el mapa por un
  adaptador de base de datos, la *forma* que ve el resto de Lumen no se mueve:
  `find` sigue devolviendo `Option<WalletView>` y `upsert` sigue tomando uno.

> **Note** **Término clave — `#[derive(Repository)]`.** Este derive marca un tipo
> como un bean de acceso a datos: el `@Repository` de Spring. El escaneo de
> componentes lo registra como singleton, de modo que se autoinyecta (como
> `Arc<ReadModel>`) en el handler de consulta y en la proyección que lo alimenta.
> El derive trata sobre *cablear* el objeto en el contenedor; el almacenamiento
> que hay detrás es lo que contenga la struct, aquí un `Mutex<HashMap<…>>`.

Por qué importa: el handler `GetWallet` depende de "dame la vista para este id",
no de un `HashMap`. Ese es todo el sentido de tratar el almacén de lectura como
un repositorio, y la razón por la que el resto de este capítulo puede sustituir
el mapa por una base de datos real sin que el handler lo note.

> **Tip** **Punto de control.** Desde un checkout del framework, ejecuta
> `cargo test -p lumen --lib ledger` y observa cómo pasan las pruebas de ida y
> vuelta del modelo de lectura. Has confirmado que la línea base en memoria
> funciona antes de intercambiar nada por debajo.

## Paso 2 — Componer una consulta con el DSL `Filter`

Antes de bajar el almacén de lectura a una base de datos real, necesitas una
forma de *pedir* filas: un valor de consulta que los adaptadores puedan
renderizar a SQL. `firefly-data` proporciona uno: el DSL `Filter`.

> **Note** **Término clave — DSL `Filter`.** Un `Filter` es un valor componible
> que agrupa una lista de predicados (campo, operador, valor), cero o más órdenes
> de clasificación y una ventana de página. Se renderiza como una cláusula `WHERE`
> parametrizada mediante `to_sql()` —nunca con interpolación de cadenas—, de modo
> que los valores se enlazan como `$1`, `$2`, … y la inyección de SQL es
> estructuralmente imposible.

Construye una consulta de "monederos ricos" —`balance >= 100_000`, los más nuevos
primero, primera página de 20—:

```rust
use firefly_data::{Direction, Filter, Op, Predicate};
use serde_json::json;

let filter = Filter::default()
    .where_eq("owner", json!("alice"))
    .add(Predicate { field: "balance".into(), op: Op::Gte, value: json!(100_000) })
    .order_by("version", Direction::Desc)
    .paged(0, 20);

let (where_clause, args) = filter.to_sql();
// where_clause: a parameter-indexed " WHERE ..." fragment
// args:         the bound values, in order
assert!(where_clause.contains("WHERE"));
assert_eq!(args.len(), 2);
```

Qué acaba de ocurrir, bloque a bloque:

- `.where_eq("owner", json!("alice"))` añade un predicado de igualdad; es azúcar
  para `.add(Predicate { field, op: Op::Eq, value })`.
- `.add(Predicate { … op: Op::Gte … })` añade el predicado `balance >= 100_000`
  de forma explícita: cada operador es alcanzable de esta manera.
- `.order_by("version", Direction::Desc)` añade un orden de clasificación (los
  más nuevos primero).
- `.paged(0, 20)` fija una ventana de página de base cero: página `0`, tamaño
  `20`.
- `to_sql()` devuelve el par `(where_clause, args)`. Dos predicados produjeron
  dos argumentos enlazados, que es lo que confirma `assert_eq!(args.len(), 2)`.

Los operadores de `Op` cubren `Eq`, `Ne`, `Lt`, `Lte`, `Gt`, `Gte`, `Like`,
`ILike`, `In` e `IsNil`. `IsNil` renderiza `IS NULL` y **no** consume ninguna
ranura de argumento, de modo que una lista de predicados y su lista de argumentos
siempre permanecen alineadas.

> **Note** `Filter::to_sql()` renderiza el valor predeterminado de PostgreSQL
> (marcadores `$1`, comillado `"id"`). `Filter::to_sql_with(&dialect)` renderiza
> el *mismo* árbol de consulta para otro backend: conoces `SqlDialect` en el Paso
> 6, donde es la costura que hace que una sola cadena de consulta se ejecute en
> tres bases de datos.

> **Tip** **Punto de control.** Coloca ese fragmento en un `#[test]` y ejecútalo.
> Ambas aserciones pasan: la cláusula contiene `WHERE`, y se enlazaron exactamente
> dos valores. Ahora tienes un valor de consulta que los adaptadores del Paso 6
> saben ejecutar.

## Paso 3 — Leer el sobre `Page<T>`

Una consulta que pagina necesita una forma estable que devolver. `Page<T>` es el
sobre canónico de resultado paginado con un layout JSON versionado, de modo que
cualquier cliente que respete el contrato lo deserializa de forma uniforme: un
SDK generado lo consume sin tratamiento específico por servicio:

```rust,ignore
pub struct Page<T> {
    pub content: Vec<T>,
    pub number: usize,       // zero-based page index
    pub size: usize,
    pub total_elements: u64,
    pub total_pages: usize,  // derived from total_elements / size
}
```

Qué acaba de ocurrir: un endpoint *listar monederos* —una extensión natural de
Lumen— devuelve un `Page<WalletView>` para que un cliente pueda paginar las
cuentas sin cargar nunca la tabla entera. `content` lleva las filas de esta
página; `number` / `size` hacen eco de la ventana solicitada; `total_elements` y
`total_pages` permiten que una interfaz de usuario renderice un paginador.

> **Note** `Page<T>` es el lado de *respuesta* de la paginación: lo que vuelve.
> También existe un lado de *petición*, `Pageable` (número de página, tamaño,
> orden), que conoces en el Paso 5. Mantenlos diferenciados: quien llama envía un
> `Pageable`, y un repositorio consciente del recuento devuelve un `Page<T>`.

## Paso 4 — Conocer el contrato de repositorio

`ReadModel` es un subconjunto de dos métodos de un contrato real del framework.
Hay dos, que comparten la misma idea en capas distintas.

El puerto **bloqueante**, `Repository<T, K>`, es el contrato `async_trait`
object-safe; `MemoryRepository` lo implementa para pruebas, y un adaptador lo
respalda con un driver en producción:

```rust,ignore
#[async_trait]
pub trait Repository<T, K>: Send + Sync {
    async fn find_by_id(&self, id: &K) -> Result<T, DataError>;
    async fn find(&self, filter: &Filter) -> Result<Page<T>, DataError>;
    async fn save(&self, entity: T) -> Result<T, DataError>;
    async fn delete(&self, id: &K) -> Result<(), DataError>;
    // find_page(&Pageable), count, … with defaults
}
```

Sobre él, `firefly-data` añade la superficie CRUD **reactiva**, construida sobre
`Mono` / `Flux`. Es puramente aditiva: nada de la API bloqueante `Repository`
cambia:

| Método                       | Devuelve                                    |
|------------------------------|---------------------------------------------|
| `find_all()`                 | `Flux<T>`                                    |
| `find_all_by_id(ids)`        | `Flux<T>`                                    |
| `find_by_id(id)`             | `Mono<T>`                                    |
| `exists_by_id(id)`           | `Mono<bool>`                                 |
| `save(e)`                    | `Mono<T>`                                    |
| `save_all(es)`               | `Flux<T>`                                    |
| `delete_by_id(id)`           | `Mono<()>`                                   |
| `delete_all()`               | `Mono<()>`                                   |
| `count()`                    | `Mono<u64>`                                  |
| `Specification` + `Pageable` | `ReactiveSpecificationRepository` (Paso 5)   |

Qué acaba de ocurrir: estos son los métodos de `ReactiveCrudRepository<T, ID>` de
Spring Data, nombre por nombre. Un detalle importa para el resto del capítulo: un
`find_by_id` "sin fila" resuelve a un `Mono` **vacío**, el equivalente reactivo de
que `ReadModel::find` de Lumen devuelva `None`.

> **Note** **Término clave — `block()` / `collect_list()`.** Los publishers son
> perezosos: nada se ejecuta hasta que los conduces. En un contexto `async`,
> `Mono::block().await` conduce un `Mono` hasta su resultado, devolviendo
> `Result<Option<T>, FireflyError>` —`Ok(None)` es el fallo del `Mono` vacío—.
> `Flux::collect_list()` reúne un flujo en un `Mono<Vec<T>>`, así que
> `flux.collect_list().block().await` devuelve `Result<Option<Vec<T>>, _>`. (Un
> `Mono<T>` también implementa `IntoFuture`, de modo que puedes hacer
> `repo.save(x).await` directamente cuando lo prefieras).

Este es el contrato del que el `ReadModel` de Lumen es un subconjunto artesanal.
Bájalo a `ReactiveCrudRepository<WalletView, String>` y `find_by_id` / `save` /
`count` vienen del framework. El siguiente paso hace exactamente eso, en memoria.

## Paso 5 — Manejar la superficie reactiva en memoria

`ReactiveMemoryRepository` es el gemelo reactivo de `MemoryRepository`: la forma
sin infraestructura de ejercitar la API reactiva real. Es la versión reactiva del
almacén de lectura de Lumen, que guarda vistas de monedero:

```rust
use firefly_data::{ReactiveCrudRepository, ReactiveMemoryRepository};

#[derive(Clone, PartialEq, Debug)]
struct WalletView { id: String, owner: String, balance: i64, version: i64 }

#[tokio::main]
async fn main() {
    // The closure tells the repository how to read an entity's id.
    let repo = ReactiveMemoryRepository::new(|w: &WalletView| w.id.clone());

    // save -> Mono<T>, driven with block().
    repo.save(WalletView { id: "wlt_1".into(), owner: "alice".into(), balance: 1000, version: 1 })
        .block().await.unwrap();

    // find_all -> Flux<T>, collected to a Vec.
    let all = repo.find_all().collect_list().block().await.unwrap().unwrap();
    assert_eq!(all.len(), 1);

    // find_by_id miss -> empty Mono (Lumen's `ReadModel::find` returning None).
    assert_eq!(repo.find_by_id("ghost".into()).block().await.unwrap(), None);

    // count -> Mono<u64>.
    assert_eq!(repo.count().block().await.unwrap(), Some(1));
}
```

Qué acaba de ocurrir, línea a línea:

- `ReactiveMemoryRepository::new(|w| w.id.clone())` construye un almacén vacío
  cuyos ids se derivan mediante la closure de extracción de clave.
- `repo.save(...).block().await.unwrap()` conduce el `Mono` de `save` hasta su
  finalización; el `unwrap()` descarta el `Result`, y el `Some(view)` interno es
  el valor persistido.
- `repo.find_all().collect_list().block().await.unwrap().unwrap()` encadena los
  tres operadores reactivos: `find_all()` devuelve un `Flux`, `collect_list()` lo
  pliega en un `Mono<Vec<_>>`, y `block().await` lo conduce. El primer `unwrap()`
  desenvuelve el `Result`, y el segundo desenvuelve el `Option<Vec<_>>`.
- `repo.find_by_id("ghost".into()).block().await.unwrap()` es el caso de fallo:
  resuelve a `None`, el contrato del `Mono` vacío del Paso 4.

Por qué importa: intercambiar `ReadModel` por este repositorio es mecánico.
`upsert` pasa a ser `save`, `find` pasa a ser `find_by_id(...).block().await`, y
el handler `GetWallet` mantiene su forma `Option<WalletView>`. Acabas de demostrar
la costura sin ninguna base de datos en el bucle.

### El ordenamiento y la paginación vienen gratis

`ReactiveSortingRepository<T, ID>` añade ordenamiento y paginación de colección
completa —`find_all_sorted(RequestSort) -> Flux<T>` y `find_all_paged(Pageable)
-> Flux<T>`— y no escribes **ningún** código para ello. Es un `impl` general sobre
cualquier repositorio que sea a la vez un `ReactiveCrudRepository` y un
`ReactiveSpecificationRepository`, de modo que cada `ReactiveMemoryRepository` y
cada repositorio SQL lo adquieren automáticamente.

> **Note** **Término clave — `Pageable` / `RequestSort`.** Estos son el lado de
> *petición* de la paginación (`Pageable` / `Sort` de Spring).
> `RequestSort::by(["owner"])` ordena de forma ascendente por un campo;
> `RequestSort::of([Order::desc("id")])` construye una lista de orden explícita.
> `Pageable::of(page, size, sort)` los agrupa y, de forma crucial, **`page` es
> de base 1** y la llamada devuelve un `Result` (una página fuera de rango es un
> error, no un panic), así que haces `.unwrap()` / `?` sobre ella.

```rust
use firefly_data::{
    Pageable, ReactiveCrudRepository, ReactiveMemoryRepository, ReactiveSortingRepository,
    RequestSort,
};

#[derive(Clone, PartialEq, Debug, serde::Serialize)]
struct WalletView { id: String, owner: String, balance: i64 }

#[tokio::main]
async fn main() {
    let repo = ReactiveMemoryRepository::new(|w: &WalletView| w.id.clone());
    for (id, owner) in [("w1", "carol"), ("w2", "alice"), ("w3", "bob")] {
        repo.save(WalletView { id: id.into(), owner: owner.into(), balance: 0 })
            .block().await.unwrap();
    }

    // find_all_sorted(RequestSort) — ordered by owner ascending, streamed as a Flux.
    let sorted = repo
        .find_all_sorted(RequestSort::by(["owner"]))
        .collect_list().block().await.unwrap().unwrap();
    assert_eq!(sorted[0].owner, "alice");

    // find_all_paged(Pageable) — page 1 (1-based), size 2, sorted; a Flux window.
    let page = repo
        .find_all_paged(Pageable::of(1, 2, RequestSort::by(["owner"])).unwrap())
        .collect_list().block().await.unwrap().unwrap();
    assert_eq!(page.len(), 2);
}
```

Qué acaba de ocurrir: `find_all_sorted` ejecutó una `Specification` de tipo
match-all con el orden proyectado sobre ella; `find_all_paged` ejecutó la misma
con una ventana `LIMIT`/`OFFSET`. Observa `Pageable::of(1, 2, …).unwrap()`: la
página `1` es la *primera* página, y el `unwrap()` gestiona el `Result`.

> **Note** `find_all_paged` transmite la página como una ventana `Flux` en lugar
> de almacenar en búfer un sobre `Page<T>`. Recurre a `Page<T>` (Paso 3) más una
> consulta de recuento cuando realmente necesites totales; recurre a la ventana
> con streaming cuando no.

> **Tip** **Punto de control.** Ejecuta ambos `main` anteriores (cada uno como un
> pequeño binario o un `#[tokio::test]`). El primero comprueba un ciclo de ida y
> vuelta save/find/count y un fallo de `Mono` vacío; el segundo comprueba el orden
> de ordenar-luego-paginar. Cada repositorio reactivo en el resto del capítulo se
> comporta de forma idéntica: solo cambia el almacenamiento que hay detrás.

## Paso 6 — Bajar a un repositorio SQL real con streaming

El repositorio en memoria demuestra la *forma*. Ahora hazlo duradero. El
adaptador relacional, `firefly-data-sqlx`, sirve PostgreSQL, MySQL y SQLite desde
una sola base de código, y **SQLite-en-memoria es el valor predeterminado sin
infraestructura**: el mismo papel que desempeña el mapa en memoria en
`samples/lumen`, pero ejercitando el adaptador real.

> **Note** **Término clave — enum `Db`.** `Db` etiqueta un pool de conexiones con
> su backend: `Db::Postgres(PgPool)`, `Db::MySql(MySqlPool)`,
> `Db::Sqlite(SqlitePool)`. El repositorio elige el `SqlDialect` correspondiente
> en tiempo de ejecución a partir de esa etiqueta, de modo que "nueva base de
> datos relacional" es un nuevo pool, no un nuevo adaptador.

```rust
use firefly_data::{ReactiveCrudRepository, TableConfig};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone, PartialEq)]
struct WalletView { id: String, owner: String, balance: i64 }

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
sqlx::query(r#"CREATE TABLE "wallet_views" ("id" TEXT PRIMARY KEY, "owner" TEXT NOT NULL, "balance" BIGINT NOT NULL)"#)
    .execute(&pool).await.unwrap();

let repo: SqlxReactiveRepository<WalletView, String> = SqlxReactiveRepository::new(
    Db::Sqlite(pool),
    TableConfig::new("wallet_views", "id", ["id", "owner", "balance"]),
    // RowMapper: decode a WalletView from each row — backend-agnostic via AnyRow.
    |row: &AnyRow| Ok::<_, FireflyError>(WalletView {
        id: row.get_str("id")?,
        owner: row.get_str("owner")?,
        balance: row.get_i64("balance")?,
    }),
    // RowWriter: the entity's (column, value) pairs for the upsert.
    |w: &WalletView| vec![
        ColumnValue::new("id", w.id.clone()),
        ColumnValue::new("owner", w.owner.clone()),
        ColumnValue::new("balance", w.balance),
    ],
);
repo.save(WalletView { id: "wlt_1".into(), owner: "alice".into(), balance: 1000 })
    .block().await.unwrap();
# });
```

Qué acaba de ocurrir, argumento por argumento:

- `Db::Sqlite(pool)` etiqueta el pool de SQLite; el repositorio lee su backend de
  la etiqueta.
- `TableConfig::new("wallet_views", "id", ["id", "owner", "balance"])` nombra la
  tabla, su columna id y las columnas a proyectar: el `RowMapper` debe decodificar
  filas con la forma de exactamente estas columnas.
- La closure **`RowMapper`** decodifica una fila. `AnyRow` es el envoltorio de
  fila agnóstico del backend; `get_str` / `get_i64` leen columnas por nombre, así
  que la misma closure funciona en los tres backends relacionales.
- La closure **`RowWriter`** produce los pares `(columna, valor)` que el adaptador
  renderiza en un `UPSERT` consciente del dialecto (`ON CONFLICT … DO UPDATE` para
  Postgres/SQLite, `ON DUPLICATE KEY UPDATE` para MySQL).

Por qué importa: cambiar a Postgres o MySQL es `Db::Postgres(pg_pool)` /
`Db::MySql(my_pool)`: los puntos de llamada del repositorio no cambian. Esa es la
ruta de actualización que promete el comentario de `samples/lumen`: el `ReadModel`
en memoria pasa a ser un `SqlxReactiveRepository<WalletView, String>`, y el
handler `GetWallet` no se entera. Las lecturas se transmiten desde el flujo de
filas de sqlx a un `Flux`, de modo que una tabla de un millón de filas nunca cae
entera en memoria.

> **Design note.** Esto es arquitectura hexagonal: puertos y adaptadores. Tu
> servicio depende del *puerto* del repositorio (`ReactiveCrudRepository`); el
> *adaptador* (el crate relacional, el crate de Mongo) se elige en el momento del
> cableado. Cambiar Postgres por MySQL es un cambio de pool, no un cambio de
> código: los puntos de llamada nunca se mueven. Añadir una base de datos *nueva*
> es "escribir un crate `firefly-data-<tech>` que implemente los puertos", no
> "reescribir la capa de datos". `firefly-data` incluye tres impls de
> `SqlDialect` (`PostgresDialect`, `MySqlDialect`, `SqliteDialect`) y un descenso
> `Specification::to_mongo()`, de modo que el mismo árbol de consulta se renderiza
> correctamente por backend.

> **Tip** **Punto de control.** Ejecuta ese fragmento como una prueba. Crea una
> tabla `wallet_views` en `sqlite::memory:`, guarda una fila a través del
> adaptador real y vuelve sin error: el almacén de lectura de producción,
> ejercitado de extremo a extremo con cero infraestructura externa.

El constructor toma un `Db`, un `TableConfig`, un `RowMapper` y un `RowWriter`;
tres builders encadenables añaden comportamiento transversal, cada uno
devolviendo un repositorio nuevo:

- `.with_auditor(Auditor)` — estampa `created_at` / `updated_at` / `created_by` /
  `updated_by` en cada escritura (insert frente a update se decide según si la
  fila ya existe): auditoría automática.
- `.with_soft_delete(SoftDeletePolicy)` — oculta las filas con borrado lógico de
  cada lectura y convierte `delete_by_id` en una estampa `deleted_at` en lugar de
  un `DELETE` físico: borrado lógico (soft delete).
- `.with_version_column("version")` — activa el bloqueo optimista (Paso 8).

## Paso 7 — Declarar el repositorio al estilo de Spring Data

Rara vez construyes el repositorio a mano como en el Paso 6. Para una entidad
tipada, dos derives te dan la experiencia de Spring Data "declara un repositorio,
obtén la implementación". Así es exactamente como el sample
[`lumen-ledger`](./22-layered-microservices.md) cablea su persistencia.

> **Note** **Término clave — `#[derive(Entity)]`.** Este derive genera el mapeo
> `@Table` / `@Id` / `@Version` / `@Column` de una entidad a partir de sus campos.
> Las columnas escalares (`String`, `i64`, `Uuid` como texto, `DateTime<Utc>` como
> texto) se mapean automáticamente; un campo no escalar (un enum tipado) usa
> `#[firefly(with(read = "...", write = "..."))]` para nombrar sus convertidores:
> el límite `@Enumerated(STRING)`.

```rust,ignore
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, firefly::Entity)]
#[firefly(table = "wallets")]
pub struct Wallet {
    #[firefly(id)]
    pub id: Uuid,
    pub account_number: String,
    pub owner: String,
    pub balance: i64,
    pub currency: String,
    // A typed enum maps through explicit converters — @Enumerated(STRING).
    #[firefly(with(read = "WalletStatus::from_token", write = "WalletStatus::as_str"))]
    pub status: WalletStatus,
    // Optimistic-locking version (@Version) — bumped by the store on update.
    #[firefly(version)]
    pub version: i64,
    // Audit stamps, managed by the store's Auditor.
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Luego `#[derive(SqlxRepository)]` sobre una struct que contiene el
`SqlxReactiveRepository` de la entidad:

```rust,ignore
use firefly::data::{DataError, Pageable};
use firefly::data_sqlx::SqlxReactiveRepository;
use uuid::Uuid;

#[derive(firefly::SqlxRepository)]
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}
```

Qué acaba de ocurrir: `#[derive(SqlxRepository)]` registra `WalletRepository`
como un bean `@Repository` **construido a partir del datasource `Db`
inyectado** (cableando el bloqueo `@Version` de la entidad y la auditoría
`@CreatedDate`/`@LastModifiedDate`), e implementa `ReactiveCrudRepository` por
delegación. No hay factoría `#[bean]` ni CRUD escrito a mano: el derive construye
el `SqlxReactiveRepository` interno a partir del `Db` autoinyectado, exactamente
como el `interface WalletRepository extends ReactiveCrudRepository<Wallet, UUID>`
de Spring Data.

> **Note** **Término clave — id `Uuid` (cualquiera).** El `ID` del repositorio no
> tiene cota, como el `CrudRepository<T, ID>` de Spring Data: el adaptador sqlx
> acepta cualquier clave `serde::Serialize` a través de su trait `SqlKey`, de modo
> que un `Uuid`, un `i64`, un `String`, un enum o una struct de clave compuesta
> funcionan todos sin baile de newtypes. La clave se enlaza en su forma serde-JSON
> contra la columna id.

### Consultas derivadas y personalizadas — `#[firefly::repository]`

Más allá del CRUD, la macro `#[firefly::repository]` deriva una consulta
directamente a partir de un *nombre de método*: `find_by_owner(&str)` pasa a ser
`WHERE owner = ?`. Aplícala a un bloque `impl` de métodos stub tipados; la macro
descarta el cuerpo de marcador de posición (`unimplemented!()`) y genera uno real
que ordena los argumentos y delega en el motor de ejecución. El **tipo de retorno
selecciona la operación**:

| Forma de retorno                | Llamada generada          |
|---------------------------------|---------------------------|
| `Result<Vec<T>, DataError>`     | `find_by_derived`         |
| `Result<Option<T>, DataError>`  | `find_by_derived` (primer)|
| `Result<i64, DataError>`        | `count_by_derived`        |
| `Result<bool, DataError>`       | `exists_by_derived`       |
| `Result<u64, DataError>`        | `delete_by_derived`       |

Este es el bloque real de consultas derivadas del `WalletRepository` de
`lumen-ledger`:

```rust,ignore
use firefly::data::{DataError, Pageable};

#[firefly::repository]
impl WalletRepository {
    /// `SELECT … WHERE owner = ?` — every wallet of one owner.
    pub async fn find_by_owner(&self, owner: &str) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }

    /// `SELECT COUNT(*) WHERE status = ?`.
    pub async fn count_by_status(&self, status: &str) -> Result<i64, DataError> {
        unimplemented!()
    }

    /// Paged `SELECT … WHERE status = ?` — a trailing `Pageable` makes it paged.
    pub async fn find_by_status(
        &self,
        status: &str,
        page: Pageable,
    ) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }
}
```

Qué acaba de ocurrir: los *nombres* de los métodos son la gramática —prefijo
(`find` / `count` / `exists` / `delete`), luego `By`, y luego una cadena de
condiciones de propiedad `And` / `Or` (`find_by_owner_and_status`)—. Un método
cuyo **último argumento es un `Pageable`** es una consulta derivada *paginada*: el
orden y la ventana del pageable se añaden al `WHERE` generado. Construye la página
con `Pageable::of(page, size, sort)`: recuerda que `page` es de **base 1** y la
llamada devuelve un `Result`:

```rust,ignore
use firefly::data::{Pageable, RequestSort};

# async fn ex(wallets: &WalletRepository) -> Result<(), firefly::data::DataError> {
let page = Pageable::of(1, 20, RequestSort::by(["account_number"])).unwrap();
let active = wallets.find_by_status("active", page).await?;
# Ok(())
# }
```

Cuando la gramática de nombres no puede expresar la consulta, anota el stub con
`#[query(...)]` y escribe tú mismo el SQL. Un marcador de posición `:name` enlaza
el argumento llamado `name`, y el tipo de retorno selecciona la operación
exactamente igual que para los métodos derivados —`Vec<T>` / `Option<T>` para una
lista, `i64` para un recuento, `bool` para un exists, `u64` para una sentencia
*modificadora* (el recuento de filas afectadas)—:

```rust,ignore
use firefly::data::DataError;

#[firefly::repository]
impl WalletRepository {
    // Native SQL; :status binds to the `status` argument.
    #[query("SELECT id, owner FROM wallets WHERE status = :status ORDER BY id DESC")]
    async fn list_by_status(&self, status: &str) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }

    // u64 return -> a modifying statement; the value is the affected-row count.
    #[query("UPDATE wallets SET status = :to WHERE status = :from")]
    async fn retire(&self, from: &str, to: &str) -> Result<u64, DataError> {
        unimplemented!()
    }
}
```

Qué acaba de ocurrir: `#[query("…")]` es una abreviatura de `#[query(sql = "…")]`
(SQL nativo). Para una consulta portable y orientada a entidades, usa la forma
similar a JPQL `#[query(jpql = "…", entity = "Wallet")]`, cuyo `FROM <Entity>` se
transpila a la tabla configurada para que la misma cadena se ejecute en Postgres,
MySQL o SQLite. Por debajo, el parser de nombres de método baja una consulta
derivada a través del `SqlDialect` activo, y `#[query]` baja a los helpers
`query_list` / `query_count` / `query_exists` / `query_execute` del repositorio.

> **Tip** Recurre a la gramática de nombres de método para predicados sencillos y
> a un `Pageable` final para lecturas paginadas; usa `#[query(...)]` para
> cualquier cosa que la gramática de nombres no pueda expresar. Un servicio
> relacional `Account` / `Order` escribe estas; el propio lado de lectura de
> Lumen, con event sourcing, permanece como un `ReadModel` artesanal en memoria
> porque sus necesidades de consulta son exactamente dos métodos.

## Paso 8 — Activar el bloqueo optimista

Una entidad versionada necesita protección contra actualizaciones perdidas.
Nombrar la columna de versión en el repositorio convierte un `save` en un
**upsert condicional protegido por versión**: cada escritura incrementa la versión
y protege la actualización-en-conflicto sobre la versión con la que se cargó la
entidad (`WHERE version = <loaded>`). Si un escritor concurrente avanzó la versión
almacenada, la actualización protegida coincide con cero filas y el save se
rechaza en lugar de sobrescribir silenciosamente el otro cambio.

```rust,ignore
use firefly_data::{DataError, Repository, TableConfig};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone)]
struct Account { id: String, balance: i64, version: i64 }

# async fn ex(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
let repo: SqlxRepository<Account, String> = SqlxRepository::new(
    Db::Postgres(pool),
    TableConfig::new("accounts", "id", ["id", "balance", "version"]),
    |row: &AnyRow| Ok::<_, FireflyError>(Account {
        id: row.get_str("id")?,
        balance: row.get_i64("balance")?,
        version: row.get_i64("version")?,
    }),
    |a: &Account| vec![
        ColumnValue::new("id", a.id.clone()),
        ColumnValue::new("balance", a.balance),
        // The loaded version — the conditional upsert guards on it.
        ColumnValue::new("version", a.version),
    ],
)
.with_version_column("version");

// Two callers loaded the same Account at version 1. The first save wins
// (the row is now version 2); the second save's guard (WHERE version = 1)
// matches nothing, so it fails with OptimisticLock — the caller reloads + retries.
let stale = repo.save(Account { id: "acc_1".into(), balance: 50, version: 1 }).await;
assert!(matches!(stale, Err(DataError::OptimisticLock)));
# Ok(())
# }
```

Qué acaba de ocurrir: `.with_version_column("version")` hizo que cada `save`
fuera condicional respecto a la versión cargada. El `SqlxRepository::save`
bloqueante expone una escritura obsoleta como `DataError::OptimisticLock`; el
`save` reactivo la expone a través de su canal `FireflyError` (un 409), que
`firefly_data_sqlx::is_optimistic_lock(&err)` detecta para que un servicio pueda
mapearlo a un conflicto de dominio. (La detección de conflictos se aplica en
Postgres y SQLite; en MySQL la versión se incrementa pero la protección no se
aplica).

> **Note** Una escritura obsoleta falla en lugar de sobrescribir silenciosamente
> un cambio concurrente; quien llama recarga y reintenta. El lado de *escritura*
> de Lumen alcanza la misma garantía contra actualizaciones perdidas de forma
> distinta —a través del `append` de concurrencia optimista del event store (ver
> [Event Sourcing](./11-event-sourcing.md))—, pero un repositorio relacional
> `Account` / `Order` usa una columna de versión.

> **Tip** **Punto de control.** El
> `models/src/repositories/wallet/v1/wallet_repository.rs` de `lumen-ledger` tiene
> una prueba, `optimistic_locking_rejects_a_stale_write`, que carga una fila dos
> veces, escribe una vez a través de cada handle y comprueba que la segunda es un
> conflicto `is_optimistic_lock`. Ejecútala: `cargo test -p lumen-ledger-models`.

## Paso 9 — Construir el pool a partir de la configuración

En cada fragmento hasta ahora el pool se construía a mano. En un servicio real,
los ajustes de conexión viven en la configuración, y Firefly los convierte en un
pool en vivo —más un gestor de transacciones registrado— en una sola llamada
esperada al arranque. No hay ningún contenedor de inyección de dependencias en el
bucle: cargas la configuración, enlazas una struct `serde` simple y haces `await`
sobre una función.

`DataSourceProperties` es esa struct, enlazada desde el árbol de configuración
`firefly.datasource.*`:

```rust,ignore
use firefly_data_sqlx::DataSourceProperties;

// Bound from `firefly.datasource.*` (e.g. an application.yaml / env overrides).
pub struct DataSourceProperties {
    pub url: String,                  // scheme picks the backend (see below)
    pub max_connections: u32,         // `0` leaves the driver default
    pub min_connections: u32,         // `0` leaves the driver default
    pub acquire_timeout_ms: u64,      // `0` leaves the driver default
    pub idle_timeout_ms: u64,         // `0` leaves the driver default
    pub max_lifetime_ms: u64,         // `0` leaves the driver default
}
```

Qué acaba de ocurrir: el **esquema de la URL selecciona el backend** (cada uno
tras su feature de cargo): `postgres://` / `postgresql://` → PostgreSQL,
`mysql://` → MySQL, `sqlite:` → SQLite. Así, un cambio de configuración de
`postgres://…` a `mysql://…` mueve todo el servicio a MySQL sin editar código: la
promesa de base de datos enchufable, dirigida desde la configuración.

Tres puntos de entrada construyen el `Db`:

- `Db::connect(url).await -> Result<Db, FireflyError>` — un pool a partir de una
  URL, usando los valores predeterminados del driver.
- `Db::connect_with(&props).await` — un pool que respeta el `DataSourceProperties`
  completo (tamaños, timeouts, lifetimes).
- `data_sqlx::auto_configure(&props).await` — la **ruta de arranque de una sola
  llamada**: construye el pool *y* registra un `SqlxTransactionManager`, de modo
  que `#[firefly::transactional]` resuelve su gestor sin cableado manual. El `Db`
  devuelto construye luego tus repositorios tipados.

La forma de una secuencia de arranque es: cargar configuración → enlazar
`DataSourceProperties` → `await auto_configure` una vez → construir repositorios a
partir del `Db` devuelto:

```rust,ignore
use firefly_data::TableConfig;
use firefly_data_sqlx::{auto_configure, AnyRow, ColumnValue, DataSourceProperties, SqlxReactiveRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone, PartialEq)]
struct WalletView { id: String, owner: String, balance: i64 }

# async fn boot(props: DataSourceProperties) -> Result<(), Box<dyn std::error::Error>> {
// One awaited call: builds the pool AND registers the SqlxTransactionManager.
let db = auto_configure(&props).await?;

// The returned Db builds typed repositories — no DI container involved.
let wallets: SqlxReactiveRepository<WalletView, String> = SqlxReactiveRepository::new(
    db.clone(),
    TableConfig::new("wallet_views", "id", ["id", "owner", "balance"]),
    |row: &AnyRow| Ok::<_, FireflyError>(WalletView {
        id: row.get_str("id")?,
        owner: row.get_str("owner")?,
        balance: row.get_i64("balance")?,
    }),
    |w: &WalletView| vec![
        ColumnValue::new("id", w.id.clone()),
        ColumnValue::new("owner", w.owner.clone()),
        ColumnValue::new("balance", w.balance),
    ],
);
// Because auto_configure registered the manager, a `#[firefly::transactional]`
// fn that writes through `wallets` is now atomic with no further wiring.
# Ok(())
# }
```

Qué acaba de ocurrir: `auto_configure(&props)` hizo los dos trabajos de arranque a
la vez —construyó el pool y registró el `SqlxTransactionManager` en el proceso—,
de modo que el límite transaccional del Paso 10 no necesita cableado adicional.

> **Design note.** La configuración dirige el runtime, no un contenedor. Una
> struct `serde` simple se enlaza desde `firefly.datasource.*`, y un único
> `auto_configure` esperado construye el pool y registra el gestor de
> transacciones: el cableado es explícito, comprobado por el compilador y visible
> en un solo lugar, en lugar de ensamblado por reflexión en tiempo de ejecución.

### Migraciones de esquema

La tabla que leen esos repositorios necesita existir primero. `firefly-migrations`
es un ejecutor de migraciones SQL solo hacia adelante. Los ficheros se nombran
`V{version}__{description}.sql` (p. ej. `V001__init.sql`); cada uno se ejecuta una
vez, en orden de versión, dentro de una transacción:

```rust,ignore
use firefly_migrations::{run, DirSource};

let src = DirSource { dir: "migrations".into() };
run(&mut db, &src)?;                                       // applies pending migrations in order
let status = firefly_migrations::inspect(&mut db, &src)?;  // applied + pending
```

La [CLI](./19-cli.md) lo envuelve: `firefly db init`,
`firefly db migrate -m "create wallet_views"`,
`firefly db upgrade --url sqlite://lumen.db` y `firefly db status`. Un
`V001__wallet_views.sql` que cree la tabla del modelo de lectura es todo el
esquema que necesita el lado de lectura duradero de Lumen.

## Paso 10 — Hacer atómico un cambio con varias escrituras

Un único `save` es atómico por sí solo. Una *transferencia* —cargar una cuenta,
abonar otra, escribir dos asientos contables— debe ser atómica en su conjunto:
las cuatro escrituras se confirman, o ninguna lo hace. Para eso sirve
`#[firefly::transactional]`.

> **Note** **Término clave — `#[firefly::transactional]`.** Anota una `async fn`
> que devuelve `Result<_, E>` (donde `E: From<TxError>`) y el cuerpo se ejecuta
> dentro de una transacción: **commit en `Ok`, rollback en `Err`**. Esto es el
> `@Transactional` de Spring, hecho declarativo en Rust.

```rust,ignore
use firefly::transactional::TxError;
use firefly_data_sqlx::SqlxReactiveRepository;

#[derive(Debug, Clone)] struct Account { id: String, balance: i64 }
#[derive(Debug, Clone)] struct Entry   { id: String, account: String, delta: i64 }

#[firefly::transactional]   // defaults: REQUIRED, datasource isolation, read-write
async fn transfer(
    accounts: &SqlxReactiveRepository<Account, String>,
    ledger: &SqlxReactiveRepository<Entry, String>,
    from: Account,
    to: Account,
    amount: i64,
) -> Result<(), TxError> {
    // All four writes enlist in the same ambient transaction. If any await
    // returns Err, the whole unit of work rolls back; otherwise it commits.
    accounts.save(Account { balance: from.balance - amount, ..from.clone() })
        .into_future().await?;
    accounts.save(Account { balance: to.balance + amount, ..to.clone() })
        .into_future().await?;
    ledger.save(Entry { id: "e1".into(), account: from.id, delta: -amount })
        .into_future().await?;
    ledger.save(Entry { id: "e2".into(), account: to.id, delta: amount })
        .into_future().await?;
    Ok(())
}
```

Qué acaba de ocurrir, y por qué es transparente:

> **Note** **Término clave — enlistamiento ambiental.** Mientras un ámbito
> transaccional está activo, el gestor guarda la transacción abierta en un
> task-local. Cada escritura de `SqlxReactiveRepository` / `SqlxRepository`
> *dentro de la fn* se enruta a esa transacción activa en lugar de a una conexión
> fresca del pool. Así, una secuencia simple de llamadas `repo.save(...).await?`
> es atómica **sin cambiar el código del repositorio**: no enhebras una conexión
> ni un `&mut Tx` a través de cada llamada.

El atributo acepta el vocabulario completo de Spring: `propagation` (`required` /
`requires_new` / `nested` / `supports` / `not_supported` / `mandatory` /
`never`), `isolation` (`read_committed` / `repeatable_read` / `serializable` /
…), `read_only`, `timeout_ms` y `manager = "<expr>"` —el
`@Transactional("txManager")` de Spring, que se ejecuta contra un
`TransactionManager` explícito (p. ej. `self.tx_manager()`) en lugar del registro
global del proceso—. Esto es exactamente lo que hace el
`WalletServiceImpl::transfer_tx` de `lumen-ledger`:

```rust,ignore
// lumen-ledger/core/src/services/wallet/v1/wallet_service_impl.rs (excerpt)
#[firefly::transactional(manager = "self.tx_manager()")]
async fn transfer_tx(&self, from: Uuid, to: Uuid, amount: i64)
    -> Result<WalletResponse, ServiceError>
{
    // … preconditions checked before any write …
    let saved_source = self.persist(source).await?;   // debit
    self.persist(dest).await?;                         // credit — if this fails,
    Ok(saved_source)                                   // the debit rolls back
}
```

### Reglas de rollback — nombrar un patrón, no un tipo de excepción

Por defecto, cada `Err` hace rollback. Spring nombra *tipos* de excepción para
refinar eso; como el `Result` de Rust ya separa el fallo del éxito, el análogo de
Firefly nombra un **patrón** de error (cualquier patrón de match para el tipo de
error de la fn, alternativas `A | B` incluidas). Entonces:

- `no_rollback_for = "P"` — **el `noRollbackFor` de Spring**: un `Err` que
  coincide con `P` **confirma** en lugar de hacer rollback;
- `rollback_only_for = "P"` — hace rollback **solo** para los errores que
  coinciden con `P`, confirmando el resto;
- con ambos, `no_rollback_for` gana en caso de solapamiento.

```rust,ignore
// Persist the audit row even when the domain rejects the charge, but still roll
// back on any infrastructure failure — @Transactional(noRollbackFor = …).
#[firefly::transactional(no_rollback_for = "BillingError::Rejected(_)")]
async fn charge(&self, req: Charge) -> Result<Receipt, BillingError> {
    self.audit.save(/* … */).await?;        // committed even on a Rejected error
    self.gateway.settle(req).await          // a Backend error still rolls back
}
```

> **Warning** No existe `rollback_for`. El `rollbackFor` de Spring es *aditivo*:
> añade tipos de excepción a las runtime-exceptions que ya hacen rollback. Rust no
> tiene división checked/unchecked (cada `Err` hace rollback por defecto), así que
> una regla aditiva sería inocua. Por eso `rollback_only_for` se nombra así para
> señalar que *restringe* (en lugar de ampliar) el conjunto de rollback, de modo
> que un port de Spring nunca se invierte silenciosamente. Escribir `rollback_for`
> es un error de compilación amigable que te apunta a las dos reglas anteriores.

Para control programático existen `firefly::transactional::transactional(opts, f)`
y `transactional_on(&manager, opts, f)` para un gestor explícito, con builders
`TxOptions`, `Propagation` e `Isolation`. El adaptador sqlx —
`SqlxTransactionManager`, registrado una vez al arranque (la ruta `auto_configure`
del Paso 9 lo hace por ti)— aporta el comportamiento real: propagación completa,
aislamiento, read-only, un timeout de sentencia y anidamiento `NESTED` basado en
`SAVEPOINT`.

> **Tip** **Punto de control.** La protección contra escrituras parciales está
> probada de extremo a extremo en el `tests/transactional.rs` de
> `firefly-data-sqlx` y en las pruebas de servicio de `lumen-ledger`: una
> transferencia cuyo abono falla tras el cargo deja *ambas* cuentas sin cambios.
> Ejecuta `cargo test -p lumen-ledger-core` para verlo.

### Almacén documental — `firefly-data-mongodb`

Los mismos puertos alcanzan una base de datos documental. `MongoRepository<T, ID>`
pone una colección de MongoDB tras los **mismos** traits `ReactiveCrudRepository`
+ `ReactiveSpecificationRepository`, bajando una `Specification` mediante
`Specification::to_mongo()` exactamente como los adaptadores relacionales la bajan
mediante `to_sql`. Un mixin `BaseDocument` (embebido con `#[serde(flatten)]`)
lleva las estampas de auditoría y la columna de borrado lógico, y las lecturas se
transmiten de forma perezosa desde el cursor del driver como un `Flux`:

```rust,no_run
use firefly_data::ReactiveCrudRepository;
use firefly_data_mongodb::{BaseDocument, MongoRepository};
use mongodb::bson::{Bson, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct WalletDocument {
    #[serde(rename = "_id")] id: String,
    owner: String,
    balance: i64,
    #[serde(flatten)] base: BaseDocument,
}

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
let collection = client.database("lumen").collection::<Document>("wallet_views");
let repo: MongoRepository<WalletDocument, String> =
    MongoRepository::new(collection, |w: &WalletDocument| Bson::String(w.id.clone()));

repo.save(WalletDocument {
    id: "wlt_1".into(), owner: "alice".into(), balance: 1000, base: BaseDocument::new(),
}).block().await?;
# Ok(())
# }
```

Qué acaba de ocurrir: como los cuatro backends se sitúan tras los mismos puertos,
un servicio que codifica contra `ReactiveCrudRepository` / `Specification` se mueve
de Postgres a MySQL, SQLite o MongoDB intercambiando el constructor del adaptador.

> **Note** Lumen no incorpora ninguno de estos adaptadores en su build
> predeterminada: son features de cargo opcionales en la fachada `firefly`
> (`firefly = { version = "26.6", features = ["data-sqlx"] }`), reexportadas como
> `firefly::data_sqlx` / `firefly::data_mongodb`. La build didáctica permanece
> ligera; la build de producción añade exactamente el driver que necesita. Esta es
> la misma historia de una sola dependencia de [Quickstart](./02-quickstart.md):
> ningún starter que olvidar, ningún desfase de versiones.

## Resumen

Lumen tiene ahora una historia de persistencia clara, aunque su build didáctica
permanezca libre de infraestructura:

- **El almacén de lectura es un repositorio.** `ReadModel` (en
  `samples/lumen/src/ledger.rs`) es un bean de acceso a datos `#[derive(Repository)]`
  que envuelve un `Mutex<HashMap<String, WalletView>>` y expone exactamente
  `upsert(view)` y `find(id)` —las dos operaciones que necesita el lado de
  consulta—. El handler `GetWallet` depende del *contrato*, no del mapa.
- **La línea base es en memoria por elección.** El mapa mantiene la huella de
  dependencias en un solo crate de Firefly; el comentario del código nombra la
  actualización explícitamente.
- **La actualización es un intercambio de adaptador.** Bajar `ReadModel` a
  `ReactiveCrudRepository<WalletView, String>` —`SqlxReactiveRepository` para
  Postgres/MySQL/SQLite, `MongoRepository` para Mongo— convierte `upsert` en
  `save` y `find` en `find_by_id`, sin cambiar la forma `Option<WalletView>` del
  handler. Una nueva base de datos es un nuevo pool (relacional) o un nuevo crate
  de adaptador, nunca una reescritura.

También sabes ahora:

- Cómo componer un `Filter`, renderizarlo con `to_sql()` y leer un `Page<T>`.
- Que la superficie reactiva devuelve `Mono` / `Flux`; los conduces con
  `block().await` (→ `Result<Option<T>, _>`) y `collect_list()`, y un fallo es un
  `Mono` vacío.
- Que `Pageable::of(page, size, sort)` es de **base 1** y devuelve un `Result`.
- Que `#[derive(Entity)]` + `#[derive(SqlxRepository)]` te dan un repositorio al
  estilo de Spring Data, y `#[firefly::repository]` añade consultas derivadas y
  consultas `#[query(...)]`.
- Que `with_version_column` es el bloqueo optimista `@Version`, que
  `auto_configure` construye el pool y registra el gestor de transacciones en una
  sola llamada esperada, y que `#[firefly::transactional]` hace atómico un cambio
  con varias escrituras mediante enlistamiento ambiental, con *patrones* de
  rollback, no `rollback_for`.

## Ejercicios

1. **Reviste `ReadModel` como un trait.** Define
   `trait WalletViews { fn upsert(&self, v: WalletView); fn find(&self, id: &str) -> Option<WalletView>; }`,
   impleméntalo para el `ReadModel` en memoria y haz que el handler `GetWallet`
   tome `&dyn WalletViews`. Confirma que el resto de Lumen sigue compilando: prueba
   de que el lado de consulta depende del contrato, no del mapa.

2. **Respalda el modelo de lectura con SQLite.** Usando el listado de
   `SqlxReactiveRepository` del Paso 6, crea una tabla `wallet_views` en
   `sqlite::memory:`, haz `save` de dos vistas y `find_all().collect_list()` sobre
   ellas. Comprueba que ambas vuelven. Este es el almacén de lectura de
   producción, ejercitado de extremo a extremo contra el adaptador real sin
   infraestructura externa.

3. **Pagina los monederos.** Construye un `Filter` que seleccione monederos con
   `balance >= 100_000` ordenados por `version` descendente, página `(0, 20)`, e
   imprime su `to_sql()`. Luego describe (una frase cada uno) cómo se renderizaría
   el mismo filtro bajo `MySqlDialect` y `SqliteDialect` mediante `to_sql_with`.

4. **Añade una consulta derivada.** En un repositorio `#[derive(SqlxRepository)]`,
   añade un método `#[firefly::repository]` `find_by_owner(&self, owner: &str) ->
   Result<Vec<Wallet>, DataError>`, y luego una variante paginada
   `find_by_status(&self, status: &str, page: Pageable) -> Result<Vec<Wallet>, DataError>`.
   Construye el `Pageable` con `Pageable::of(1, 20, RequestSort::by(["id"]))` y
   confirma que compila. Observa que `page` es la *primera* página, no la segunda.

5. **Traza el intercambio.** Enumera las líneas exactas de
   `samples/lumen/src/ledger.rs` que cambiarían si `ReadModel` pasara a ser un
   `SqlxReactiveRepository<WalletView, String>`, y cuáles líneas del handler
   `GetWallet` *no* cambiarían, confirmando que el límite del adaptador se
   sostiene.

## Adónde ir después

- Modela el negocio en sí —el value object `Money` y el agregado `Wallet`— en
  **[Diseño guiado por el dominio](./08-domain-driven-design.md)**.
- Ve cómo se separan el almacén de lectura y el almacén de escritura, y cómo el
  lado de consulta lee desde el repositorio que acabas de plantear, en
  **[CQRS](./09-cqrs.md)**.
- Observa cómo cobra vida la proyección que *escribe* en `ReadModel` en
  **[EDA y mensajería](./10-eda-messaging.md)** y
  **[Event Sourcing](./11-event-sourcing.md)**.
- Ve la capa de persistencia completa al estilo de Spring Data cableada entre
  crates en **[Microservicios en capas](./22-layered-microservices.md)**.
