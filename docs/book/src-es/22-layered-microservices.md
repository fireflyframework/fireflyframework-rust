# Microservicios por capas

Hasta ahora, todas las muestras de Lumen han vivido en un *único* crate. Esa es la
forma adecuada mientras aprendes un subsistema a la vez, pero no es como se
construye realmente un servicio de core bancario en producción. Los servicios
reales —como los de la plataforma [firefly-oss](https://github.com/firefly-oss)—
se dividen en **módulos por capas**, cada uno una unidad compilada por separado
con exactamente una responsabilidad: el contrato público puede publicarse sin
arrastrar el código de persistencia, la lógica de negocio puede probarse de forma
unitaria sin la pila web, y un consumidor de un SDK externo solo incorpora los
DTOs y nada más.

En este capítulo construyes esa forma. `lumen-ledger` (en
[`samples/lumen-ledger/`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen-ledger))
es un microservicio de monedero/libro mayor organizado como **cinco crates** —el
análogo en Rust de un proyecto Maven multimódulo— dispuesto al estilo Java con
**un tipo público por archivo** bajo una ruta de paquete `<domain>/v1`. Reutiliza
todas las ideas del framework que ya conociste (beans de DI, el repositorio
sqlx, transacciones, validación, problems RFC 9457, OpenAPI) y muestra cómo se
componen *a través de la frontera de un crate* solo mediante descubrimiento.

Al terminar este capítulo, serás capaz de:

- Disponer un servicio como cinco crates por capas con las flechas de dependencia
  apuntando estrictamente hacia adentro, y saber qué estereotipo del framework
  pertenece a cada capa.
- Declarar un `@Repository` al estilo de Spring Data sobre una `@Entity` real con
  dos derives —sin factory, sin CRUD escrito a mano— construido a partir de un
  **bean de datasource asíncrono**.
- Escribir un `@Service` que programa contra el trait `ReactiveCrudRepository`
  del repositorio, ejecuta una **transferencia atómica** bajo
  `#[transactional]`, y traduce un filtro a una `Specification` en tiempo de
  ejecución.
- Cablear todo el grafo con una sola línea `firefly::link!` y protegerlo con
  `assert_discovered`, para luego ejecutar y probar el servicio en proceso.
- Entregar un SDK tipado a los llamadores aguas abajo —escrito a mano contra los
  DTOs compartidos, o generado a partir del documento OpenAPI en vivo.

## Conceptos que conocerás

Antes del primer crate, aquí están las ideas en las que se apoya este capítulo.
Cada una se reintroduce en contexto donde se usa por primera vez; esta es la
versión breve.

> **Note** **Término clave — módulo por capas.** Un *módulo por capas* es un
> crate compilado por separado que posee exactamente una preocupación
> arquitectónica: el contrato público, el modelo de persistencia, la lógica de
> negocio, la superficie web o un cliente saliente. Dividir un servicio de esta
> forma es el equivalente en Rust de un proyecto Maven multimódulo: cada módulo
> compila, prueba y versiona por su cuenta, y las capas inferiores nunca importan
> las superiores.

> **Note** **Término clave — estereotipo.** Un *estereotipo* es el rol que un
> bean desempeña en la aplicación: controlador, servicio, repositorio,
> componente, configuración. Firefly marca cada uno con su propio derive
> (`#[derive(Controller)]`, `#[derive(Service)]`, …) exactamente como Spring los
> marca con `@RestController`, `@Service`, `@Repository`, `@Component`,
> `@Configuration`. El framework clasifica cada bean descubierto por su
> estereotipo en el informe `/actuator/beans`.

> **Note** **Término clave — descubrimiento en tiempo de enlazado.** Firefly
> descubre beans, controladores y esquemas en *tiempo de enlazado* usando el
> crate `inventory`: cada macro registra una entrada que el enlazador recopila en
> el binario final. La trampa es que un enlazador de Rust **elimina por dead-strip**
> cualquier crate que el binario nunca referencie —una dependencia en `Cargo.toml`
> por sí sola no es una referencia. La macro `firefly::link!` aporta esa
> referencia para que los registros de un crate de capa sobrevivan hasta el
> binario.

## Paso 1 — Disponer los cinco crates

La primera decisión son las fronteras de los módulos. `lumen-ledger` usa cinco,
uno por preocupación, nombrados según la convención de firefly-oss:

| Crate | Contiene | Estereotipo que aporta |
|---|---|---|
| `firefly-sample-lumen-ledger-interfaces` | DTOs (`#[derive(Schema, Validate)]`) + el enum `WalletStatus` — el contrato público | — (datos puros) |
| `firefly-sample-lumen-ledger-models` | la `@Entity` `Wallet` + el `WalletRepository` de sqlx + el `@Configuration` del datasource | `@Entity`, `@Repository`, `@Bean` |
| `firefly-sample-lumen-ledger-core` | el `@Service`, el `@Mapper`, un `@Component` | `@Service`, `@Component` |
| `firefly-sample-lumen-ledger-web` | el `@RestController` + el binario `FireflyApplication` | `@RestController` |
| `firefly-sample-lumen-ledger-sdk` | un cliente saliente tipado sobre la API | — (una biblioteca cliente) |

Cada crate fija un nombre de biblioteca corto para que el código se lea con
claridad a través de la frontera —`lumen_ledger_interfaces`, `lumen_ledger_models`,
`lumen_ledger_core`, `lumen_ledger_sdk`— mientras que el nombre del paquete sigue
siendo plenamente cualificado para la publicación. Por ejemplo, el `Cargo.toml`
de `-interfaces`:

```toml
[package]
name = "firefly-sample-lumen-ledger-interfaces"

[lib]
name = "lumen_ledger_interfaces"
path = "src/lib.rs"

[dependencies]
firefly = { workspace = true }
serde = { workspace = true }
```

Las flechas de dependencia apuntan estrictamente **hacia adentro**:

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 320" role="img"
     aria-label="Layered crate stack: interfaces, models, core and web crates with dependencies pointing strictly inward toward the interfaces contract, and an sdk crate that depends only on interfaces"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="140.0" y="32.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="30.0" width="260.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="270.0" y="52.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-interfaces</text><text x="270.0" y="66.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">DTOs · the public contract</text>
<line x1="270.0" y1="106.0" x2="270.0" y2="88.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="270.0,80.0 274.5,88.0 265.5,88.0" fill="#b5531f"/>
<text x="334.0" y="97.0" text-anchor="start" font-size="9.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends on</text>
<rect x="140.0" y="108.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="106.0" width="260.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="270.0" y="128.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-models</text><text x="270.0" y="142.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">@Entity · @Repository · @Bean</text>
<line x1="270.0" y1="182.0" x2="270.0" y2="164.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="270.0,156.0 274.5,164.0 265.5,164.0" fill="#b5531f"/>
<text x="334.0" y="173.0" text-anchor="start" font-size="9.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends on</text>
<rect x="140.0" y="184.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="182.0" width="260.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="270.0" y="204.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-core</text><text x="270.0" y="218.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">@Service · @Mapper · @Component</text>
<line x1="270.0" y1="258.0" x2="270.0" y2="240.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="270.0,232.0 274.5,240.0 265.5,240.0" fill="#b5531f"/>
<text x="334.0" y="249.0" text-anchor="start" font-size="9.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends on</text>
<rect x="140.0" y="260.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="258.0" width="260.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="270.0" y="280.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-web</text><text x="270.0" y="294.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">@RestController · the binary</text>
<rect x="444.0" y="184.5" width="112.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="444.0" y="182.0" width="112.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="500.0" y="204.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-sdk</text><text x="500.0" y="218.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">typed client</text>
<path d="M500.0,182.0 Q415.4,145.7 401.3,62.9" fill="none" stroke="#d4793a" stroke-width="3.0" stroke-dasharray="6 5" stroke-linecap="round"/><polygon points="400.0,55.0 405.8,62.1 396.9,63.6" fill="#b5531f"/>
<text x="500.0" y="252.0" text-anchor="middle" font-size="9.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">→ -interfaces</text>
</svg>
<figcaption>Cinco crates compilados por separado. Las dependencias apuntan estrictamente <strong>hacia adentro</strong>: <code>-web</code> conoce a <code>-core</code>, que conoce a <code>-models</code>, que conoce a <code>-interfaces</code> — y el crate del contrato no conoce a nadie. <code>-sdk</code> depende solo de <code>-interfaces</code>, de modo que un llamador enlaza los DTOs sin el código de persistencia ni el web.</figcaption>
</figure>

Una capa inferior nunca depende de una superior. El crate `-web` conoce el
servicio `-core`; el servicio conoce el repositorio `-models`; el repositorio
conoce el contrato `-interfaces` —y el crate del contrato no conoce a nadie. El
`-sdk` depende solo de `-interfaces`, de modo que un llamador enlaza los DTOs sin
arrastrar jamás el código de persistencia ni el web. En concreto, `-models`
depende de `-interfaces`, `-core` depende de ambos, y `-web` depende de los tres:

```toml
# firefly-sample-lumen-ledger-web/Cargo.toml
[dependencies]
firefly = { workspace = true, features = ["admin", "data-sqlx"] }
firefly-sample-lumen-ledger-interfaces = { path = "../interfaces" }
firefly-sample-lumen-ledger-models = { path = "../models" }
firefly-sample-lumen-ledger-core = { path = "../core" }
```

> **Note** **Término clave — un tipo por archivo.** Cada archivo hoja contiene
> exactamente un `struct` / `trait` / `enum`
> (`dtos/wallet/v1/wallet_response.rs` → `WalletResponse`), igual que la
> convención de una clase por archivo de Java. Los archivos `mod` intermedios
> (`dtos/wallet/v1.rs`) simplemente reexportan sus hojas, y el `lib.rs` de cada
> crate añade reexportaciones planas de conveniencia
> (`pub use services::wallet::v1::WalletService;`) para que un consumidor escriba
> `lumen_ledger_core::WalletService`, no la ruta completa.

> **Tip** **Punto de control.** Puedes imaginar el árbol antes de escribir una
> línea: cinco directorios bajo `samples/lumen-ledger/`, cada uno con su propio
> `Cargo.toml`, y una ruta de paquete `src/<domain>/v1/` dentro. Las flechas de
> arriba te dicen qué `Cargo.toml` puede listar cuál —si alguna vez encuentras un
> crate inferior importando uno superior, las capas están mal.

## Paso 2 — Asignar los estereotipos a las capas

Antes de cualquier código, fija en tu cabeza qué estereotipo del framework aporta
cada capa. Cada tipo de abajo es un **bean de DI** que el framework descubre
durante `container.scan()` —no hay raíz de composición que los ensamble a mano,
igual que no la había en [Inicio rápido](./02-quickstart.md).

```text
@RestController  (web)    →  #[rest_controller] + #[derive(Controller)]   WalletController
   │ autowires
@Service         (core)   →  #[derive(Service)] + #[firefly(provides = "dyn WalletService")]
   │ autowires
@Mapper          (core)   →  #[derive(Component)]  WalletMapper          (DTO ↔ entity)
@Component       (core)   →  #[derive(Component)]  WalletNumberGenerator
@Repository      (models) →  #[derive(SqlxRepository)]  WalletRepository  (built from the Db @Bean)
   │ over
@Entity          (models) →  #[derive(Entity)]  Wallet                   (generates the SqlxEntity mapping)
   │ from
@Bean (DataSource)(models) →  #[bean] async fn data_source() -> Db
```

> **Note** **Término clave — autowiring entre crates.** El *autowiring* pide al
> contenedor un colaborador por tipo en lugar de construirlo tú mismo (el
> `@Autowired` de Spring). El descubrimiento es en tiempo de enlazado, no por
> crate, de modo que un campo `#[autowired]` en el controlador de `-web` se
> satisface con un bean `@Service` declarado en `-core`, que a su vez hace
> autowiring de un `@Repository` de `-models`. El cableado cruza las fronteras de
> los crates sin ceremonia adicional —una vez que los crates están enlazados
> (Paso 6), el grafo es un solo contenedor.

El `@Service` programa contra el trait **`ReactiveCrudRepository`** del
repositorio (`save`, `find_by_id`, `delete_by_id`, `count`, … que devuelven
`Mono` / `Flux`) más las consultas derivadas de `#[firefly::repository]` —
`find_by_owner`, `find_by_status(.., Pageable)` (paginada) y `count_by_status`—
la misma superficie de Spring Data, generada a partir de los nombres de los
métodos. Conociste todas ellas en [Persistencia](./07-persistence.md); aquí
simplemente viven un crate más abajo.

## Paso 3 — Declarar la entidad, al estilo de Spring Data

Empieza por abajo —el crate `-models`. Un repositorio de Spring Data es una
*declaración*: tú escribes la interfaz, el framework aporta la implementación.
`lumen-ledger` hace lo mismo con dos derives, y el primero está sobre la entidad.

> **Note** **Término clave — entidad.** Una *entidad* es la forma persistida de un
> objeto de dominio —una fila de una tabla. `#[derive(Entity)]` genera el mapeo
> `@Table` / `@Id` / `@Version` / `@Column` a partir de los campos del struct, la
> experiencia `@Entity` de JPA: las columnas escalares se mapean automáticamente,
> y los campos anotados se adhieren a los roles especiales (clave primaria,
> versión, marcas de tiempo de auditoría).

Crea `models/src/entities/wallet/v1/wallet.rs`:

```rust,ignore
use chrono::{DateTime, Utc};
use lumen_ledger_interfaces::WalletStatus;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, firefly::Entity)]
#[firefly(table = "wallets")]
pub struct Wallet {
    #[firefly(id)]
    pub id: Uuid,
    pub account_number: String,
    pub owner: String,
    pub balance: i64,            // minor units (cents)
    pub currency: String,       // ISO-4217 code
    // The typed enum maps via an explicit converter — the @Enumerated(STRING) boundary.
    #[firefly(with(read = "WalletStatus::from_token", write = "WalletStatus::as_str"))]
    pub status: WalletStatus,
    #[firefly(version)]
    pub version: i64,           // @Version — bumped by the store on update
    pub created_at: DateTime<Utc>,  // @CreatedDate, stamped on insert
    pub updated_at: DateTime<Utc>,  // @LastModifiedDate, stamped on every write
}
```

Lo que acaba de ocurrir: el derive leyó el struct y produjo el mapeo de la tabla.
Las columnas escalares (`String`, `i64` / `i32`, `bool`, `f64`, `Uuid`,
`DateTime<Utc>`) se mapean automáticamente, con `Uuid` y `DateTime<Utc>`
persistidos como texto; `#[firefly(column = "name")]` renombraría una. El enum
`WalletStatus` *no* es escalar, así que lleva un convertidor explícito
`with(read = …, write = …)` —la dirección de lectura (`from_token`) y la de
escritura (`as_str`)— que es la frontera `@Enumerated(STRING)` de JPA hecha
explícita. Los campos `#[firefly(id)]`, `#[firefly(version)]` y los dos campos de
marca de tiempo se adhieren a los roles especiales: el almacén estampa la versión
y las marcas de tiempo por ti, de modo que el servicio nunca las toca.

> **Note** **Término clave — bloqueo optimista.** El *bloqueo optimista* deja que
> dos lectores carguen la misma fila, y luego hace que el *segundo* escritor
> falle si el primero ya la cambió —detectado comparando la columna `@Version`.
> Ninguna fila se bloquea jamás para lectura; el conflicto se detecta en el
> momento de la escritura. Una escritura obsoleta aflora como la
> `OptimisticLockingFailureException` de Spring, que este servicio convierte en un
> `409`.

## Paso 4 — Declarar el repositorio con un solo derive

El repositorio es **una sola anotación** —`#[derive(SqlxRepository)]` sobre un
struct cuyo único campo es el repositorio reactivo del framework— más un bloque
opcional de *consultas derivadas*.

> **Note** **Término clave — consulta derivada.** Una *consulta derivada* es un
> buscador cuyo SQL genera el framework a partir del *nombre* del método —
> `find_by_owner`, `count_by_status`, `find_by_status(.., Pageable)`— exactamente
> como el `findByOwner` de Spring Data. Tú escribes la firma y dejas el cuerpo sin
> implementar; la macro `#[firefly::repository]` lo reemplaza.

Crea `models/src/repositories/wallet/v1/wallet_repository.rs`:

```rust,ignore
use firefly::data::{DataError, Pageable};
use firefly::data_sqlx::SqlxReactiveRepository;
use uuid::Uuid;

use crate::entities::wallet::v1::Wallet;

#[derive(firefly::SqlxRepository)]
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}

#[firefly::repository] // the derived queries, on top
impl WalletRepository {
    /// `SELECT … WHERE owner = ?`
    pub async fn find_by_owner(&self, owner: &str) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }

    /// `SELECT COUNT(*) WHERE status = ?`
    pub async fn count_by_status(&self, status: &str) -> Result<i64, DataError> {
        unimplemented!()
    }

    /// Paged `SELECT … WHERE status = ?` — ORDER BY / LIMIT / OFFSET come from
    /// the trailing `Pageable`.
    pub async fn find_by_status(
        &self,
        status: &str,
        page: Pageable,
    ) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }
}
```

Lo que acaba de ocurrir, y por qué importa: ese único derive hace tres cosas a la
vez. **Registra `WalletRepository` como un bean `@Repository`** (descubierto por
el scan, clasificado correctamente en `/actuator/beans`); **construye el
`SqlxReactiveRepository` interno a partir del datasource `Db` cableado por
autowiring** —enlazando el bloqueo optimista `@Version` de la entidad y la
auditoría `@CreatedDate` / `@LastModifiedDate` del mapeo `SqlxEntity` que emitió
el derive de la entidad—; e **implementa `ReactiveCrudRepository` *y*
`ReactiveSpecificationRepository` por delegación**. Ese último punto es lo que
permite que el servicio del Paso 5 llame a `save`, `find_by_id`, `delete_by_id`
y `find_by_spec` sin que escribas ninguno de ellos.

El bloque `#[firefly::repository]` añade las consultas derivadas encima: el
cuerpo de cada método es `unimplemented!()` en tu fuente, y la macro lo reemplaza
con SQL generado a partir del nombre del método. Sin factory `#[bean]`, sin CRUD
escrito a mano —el único estado del struct es el repositorio interno, y el derive
lo construye.

> **Tip** **Punto de control.** Dos derives, cero cuerpos de CRUD, y el único
> campo es `repo: SqlxReactiveRepository<Wallet, Uuid>`. Si te encuentras
> escribiendo un `save` o un `SELECT` a mano aquí, da un paso atrás —el derive ya
> aporta la superficie canónica.

### La clave es genérica, como en Java

El `CrudRepository<T, ID>` de Spring Data deja `ID` sin acotar. El repositorio
sqlx de Firefly acepta **cualquier clave `Serialize`** a través del trait
`SqlKey` (implementado de forma general), de modo que el repositorio del monedero
se indexa directamente por `Uuid`:

```rust,ignore
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}
```

`Uuid`, `i64`, `String`, un enum o un struct de clave compuesta funcionan todos
—la clave se enlaza en su forma serde-JSON contra la columna id. Nada en el
repositorio está cableado de forma rígida a UUIDs.

## Paso 5 — Abrir el datasource como un bean asíncrono

El repositorio es un bean *síncrono*: construirlo es solo envolver el manejador
`Db`. Lo que realmente realiza E/S al arrancar es el **datasource** —el
`DataSource` autoconfigurado de Spring Boot. En `lumen-ledger` es un **`@Bean`
asíncrono** en el `@Configuration` de `-models`.

> **Note** **Término clave — bean asíncrono.** Un *bean asíncrono* es un bean
> cuya factory es una `async fn` —debe hacer `await` de trabajo (abrir un pool,
> conectar a un broker) antes de que el bean exista. El framework aparca tal
> factory durante el `container.scan()` síncrono y le hace `await` durante
> `Container::init_async_beans()`, ejecutado por el bootstrap justo después del
> scan. Este es el patrón de Spring Boot de un `@Bean` que realiza E/S en el
> momento del refresco del contexto, salvo que la E/S se espera con `await` en
> lugar de bloquear un hilo.

Crea `models/src/config/wallet_persistence_config.rs`:

```rust,ignore
use firefly::data_sqlx::Db;
use firefly::prelude::*;

#[derive(Configuration, Default)]
pub struct WalletPersistenceConfig;

#[firefly::bean]
impl WalletPersistenceConfig {
    /// The `Db` datasource bean — an async factory that opens the pool and
    /// applies the schema with `await`.
    #[bean]
    async fn data_source(&self) -> Db {
        connect_and_migrate().await // open pool + apply schema
    }
}
```

Lo que acaba de ocurrir: el `#[bean] async fn data_source` se aparca durante el
scan, y luego se le hace `await` durante `init_async_beans()`. Como el datasource
está *listo* antes de que se resuelva cualquier bean síncrono, el repositorio
`#[derive(SqlxRepository)]` (un bean síncrono que hace autowiring de `Db`)
encuentra un pool vivo cuando el framework lo construye. Un error de construcción
aquí aborta el arranque —fail-fast, aflorado a través de
`Container::init_async_beans` como un error `BeanCreation`.

Por defecto, `connect_and_migrate()` abre una **base de datos SQLite en memoria**,
de modo que la muestra se ejecuta y se prueba sin servidor externo. Establece
`DATABASE_URL=postgres://…` y apuntará a PostgreSQL real en su lugar —la única
dependencia de entorno de toda la muestra, y es opcional.

> **Design note.** ¿Por qué el servicio posee su gestor de transacciones (Paso 7)
> pero el datasource vive aquí? Porque el registro de gestores de transacciones
> globales al proceso es *gana-el-primero*, y la batería de pruebas de esta
> muestra arranca una base de datos en memoria aislada **por prueba**. Un único
> gestor global las contaminaría entre sí. El bean del datasource se puede
> compartir sin problema —cada consumidor resuelve el mismo `Db`— pero la
> frontera transaccional se enlaza a un gestor por instancia para que cada prueba
> siga siendo hermética. Un servicio de producción con un único datasource podría
> igualmente registrar un gestor al arrancar y usar un `#[firefly::transactional]`
> a secas.

> **Tip** **Punto de control.** El crate `-models` tiene ahora tres archivos de
> sustancia: la entidad, el repositorio y la configuración. `cargo test -p
> firefly-sample-lumen-ledger-models` ejercita el repositorio directamente contra
> una base de datos en memoria aislada —incluidas las consultas derivadas y un
> conflicto real de bloqueo optimista `@Version` (una escritura obsoleta
> detectada con `firefly::data_sqlx::is_optimistic_lock`).

## Paso 6 — Escribir el servicio contra el trait del repositorio

Sube a `-core`. El `@Service` es la capa de negocio: hace autowiring del
repositorio, el mapper y el generador de números, y programa contra la superficie
del *trait* del repositorio —nunca su SQL concreto.

El servicio se publica como un **puerto** para que el controlador dependa de una
interfaz, no de un struct:

```rust,ignore
use std::sync::Arc;

use firefly::prelude::*;
use firefly::data_sqlx::Db;

#[derive(Service)]
#[firefly(provides = "dyn WalletService")]
pub struct WalletServiceImpl {
    #[autowired] repository: Arc<WalletRepository>,
    #[autowired] mapper: Arc<WalletMapper>,
    #[autowired] numbers: Arc<WalletNumberGenerator>,
    #[autowired] db: Arc<Db>,   // for the service's own transaction manager
}
```

> **Note** **Término clave — puerto provisto.** `#[firefly(provides = "dyn WalletService")]`
> registra la implementación bajo el tipo de *objeto trait*, de modo que
> cualquiera que haga autowiring de `Arc<dyn WalletService>` (el controlador, una
> prueba) recibe este bean. El trait es el puerto publicado; el struct es un
> adaptador oculto —el "programa contra una interfaz, inyecta la implementación"
> de Spring.

Las rutas de lectura simples simplemente delegan en el trait
`ReactiveCrudRepository` del repositorio y mapean el resultado a través del
`@Mapper`:

```rust,ignore
async fn get(&self, id: Uuid) -> Result<WalletResponse, ServiceError> {
    let wallet = self
        .repository
        .find_by_id(id)
        .await
        .map_err(|e| ServiceError::Backend(e.to_string()))?
        .ok_or(ServiceError::NotFound)?;
    Ok(self.mapper.to_response(&wallet))
}

async fn list_by_owner(&self, owner: &str) -> Result<Vec<WalletResponse>, ServiceError> {
    let wallets = self
        .repository
        .find_by_owner(owner)            // the derived query
        .await
        .map_err(|e| ServiceError::Backend(e.to_string()))?;
    Ok(wallets.iter().map(|w| self.mapper.to_response(w)).collect())
}
```

Lo que acaba de ocurrir: `find_by_id` proviene del trait `ReactiveCrudRepository`
que implementó el derive; `find_by_owner` es la consulta derivada del bloque
`#[firefly::repository]`. El servicio nunca ve SQL —ve un repositorio que ya habla
su dominio.

> **Note** **Término clave — mapper.** Un *mapper* traduce entre capas —aquí la
> entidad `Wallet` de `-models` y el DTO `WalletResponse` de `-interfaces`. Como
> los dos tipos viven en crates *diferentes*, la regla del huérfano de Rust
> prohíbe `impl From<Wallet> for WalletResponse` en `-core`. Por eso `WalletMapper`
> es un bean `#[derive(Component)]` escrito a mano con un método
> `to_response(&self, &Wallet) -> WalletResponse` —exactamente la forma que genera
> el `@Mapper` de MapStruct.

### Filtrar con una Specification en tiempo de ejecución

El caso de uso `search` muestra la `Specification` del framework —el análogo del
`JpaSpecificationExecutor` de Spring Data. El servicio convierte cada campo de
filtro *presente* en un predicado combinado con AND, y luego ejecuta la
specification compuesta:

```rust,ignore
use firefly::data::{Op, Predicate, ReactiveSpecificationRepository, Specification};

async fn search(&self, filter: WalletFilter) -> Result<Vec<WalletResponse>, ServiceError> {
    // At least one criterion is required — a no-filter search would be an
    // unscoped list-every-wallet enumeration.
    if filter.owner.is_none()
        && filter.currency.is_none()
        && filter.status.is_none()
        && filter.min_balance.is_none()
        && filter.max_balance.is_none()
    {
        return Err(ServiceError::Validation("provide at least one filter criterion".into()));
    }

    let mut spec = Specification::all();
    if let Some(owner) = filter.owner {
        spec = spec.and(Specification::eq("owner", owner));
    }
    if let Some(min) = filter.min_balance {
        spec = spec.and(Specification::pred(Predicate::new("balance", Op::Gte, min)));
    }
    // …currency, status, max_balance the same way…

    let wallets = self
        .repository
        .find_by_spec(spec)         // from ReactiveSpecificationRepository
        .collect_list()
        .block()
        .await
        .map_err(|e| ServiceError::Backend(e.to_string()))?
        .unwrap_or_default();
    Ok(wallets.iter().map(|w| self.mapper.to_response(w)).collect())
}
```

Lo que acaba de ocurrir: `find_by_spec` proviene de
`ReactiveSpecificationRepository` (el *otro* trait que implementó el derive
`SqlxRepository`). Devuelve un `Flux`, así que `.collect_list().block().await` lo
recopila. `block()` devuelve `Result<Option<Vec<Wallet>>, _>`, de modo que
`.unwrap_or_default()` convierte el `None` de "sin filas" en un `Vec` vacío. El
framework compila la `Specification` a un `WHERE` consciente del dialecto, de modo
que el mismo código de servicio se ejecuta sin cambios sobre SQLite o PostgreSQL.

### La transferencia atómica, bajo una sola transacción

La transferencia es el corazón de un libro mayor: debitar el origen y abonar el
destino, ambos o ninguno. Eso exige una transacción.

> **Note** **Término clave — frontera transaccional.** `#[firefly::transactional]`
> envuelve un método de modo que toda escritura dentro de él se confirma junta o
> revierte junta —el `@Transactional` de Spring. El argumento
> `manager = "self.tx_manager()"` enlaza la frontera a un gestor que posee el
> *servicio* (evaluado por llamada) en lugar del registro global al proceso. El
> atributo vive en un método inherente (`transfer_tx`), porque un método
> `async-trait` no puede llevarlo de forma limpia; el método del trait
> simplemente delega.

```rust,ignore
use firefly::data_sqlx::SqlxTransactionManager;
use firefly::transactional::TransactionManager;

impl WalletServiceImpl {
    fn tx_manager(&self) -> Arc<dyn TransactionManager> {
        Arc::new(SqlxTransactionManager::new((*self.db).clone()))
    }

    #[firefly::transactional(manager = "self.tx_manager()")]
    async fn transfer_tx(&self, from: Uuid, to: Uuid, amount: i64)
        -> Result<WalletResponse, ServiceError>
    {
        if amount <= 0 { return Err(ServiceError::Validation("transfer amount must be positive".into())); }
        if from == to { return Err(ServiceError::Validation("cannot transfer to the same wallet".into())); }

        let mut source = self.load_active(from).await?;  // 404 if absent, 422 if not active
        let mut dest = self.load_active(to).await?;
        if source.currency != dest.currency {
            return Err(ServiceError::Validation("currency mismatch".into()));
        }
        if source.balance < amount {
            return Err(ServiceError::Validation("insufficient funds".into()));
        }

        // Every precondition is checked BEFORE the source is debited, so a
        // rejected transfer moves no money. If the credit fails after the debit,
        // the transaction rolls the debit back.
        source.balance = source.balance.checked_sub(amount)
            .ok_or_else(|| ServiceError::Validation("balance underflow".into()))?;
        let saved_source = self.persist(source).await?;
        dest.balance = dest.balance.checked_add(amount)
            .ok_or_else(|| ServiceError::Validation("balance overflow".into()))?;
        self.persist(dest).await?;
        Ok(saved_source)  // the updated source
    }
}
```

Lo que acaba de ocurrir, y por qué importa: `transfer_tx` se ejecuta dentro de una
única transacción enlazada a `self.tx_manager()`. Cada guarda (importe positivo,
monederos activos distintos, moneda coincidente, fondos suficientes) se dispara
*antes* de la primera escritura, de modo que una transferencia rechazada nunca
toca un saldo. La aritmética es `checked_*`, así que un desbordamiento del libro
mayor es un error de dominio, no un wrap silencioso. Y si el abono fallara alguna
vez tras el débito, la frontera revierte el débito —la garantía de ambos-o-ninguno
de la que vive un libro mayor.

> **Note** Como `#[transactional]` exige que el tipo de error sea
> `From<firefly::transactional::TxError>`, `ServiceError` implementa esa conversión
> —un fallo de la infraestructura transaccional (begin / commit / rollback) aflora
> como `ServiceError::Backend`. Los argumentos `no_rollback_for` /
> `rollback_only_for` de `#[transactional]` (no mostrados aquí) te permiten ajustar
> qué variantes de error desencadenan un rollback; por defecto se revierte ante
> cualquier `Err`.

El ayudante `persist` centraliza el guardar-y-mapear, y mapea una escritura
`@Version` obsoleta a un `Conflict`:

```rust,ignore
async fn persist(&self, wallet: Wallet) -> Result<WalletResponse, ServiceError> {
    let saved = self
        .repository
        .save(wallet)
        .await
        .map_err(|e| {
            if is_optimistic_lock(&e) {
                ServiceError::Conflict("wallet was modified concurrently; retry".into())
            } else {
                ServiceError::Backend(e.to_string())
            }
        })?
        .ok_or_else(|| ServiceError::Backend("save returned no row".into()))?;
    Ok(self.mapper.to_response(&saved))
}
```

`deposit` y `withdraw` son lecturas-modificación-escrituras simples que se apoyan
en el mismo par `load_active` + `persist`; su seguridad ante la concurrencia
proviene del bloqueo optimista `@Version` del repositorio (una escritura obsoleta
→ `409`), no de una transacción.

> **Tip** **Punto de control.** El crate `-core` contiene ahora el servicio (con
> su trait, implementación y `ServiceError`), el mapper y el generador de números
> —cada uno un bean de DI, ninguno construido a mano. El servicio compila contra
> `-models` y `-interfaces` pero no sabe nada de `-web`.

## Paso 7 — Cablear los crates con `firefly::link!`

Ahora el binario. El crate `-web` contiene el `@RestController` (Paso 8) y el
arranque `FireflyApplication` de una línea —pero una dependencia de Cargo en los
crates de capa **no basta**. Como el descubrimiento es en tiempo de enlazado, el
enlazador eliminará por dead-strip los registros de bean / controlador / esquema
de un crate de capa a menos que el binario realmente *referencie* ese crate. La
macro `firefly::link!` es esa referencia.

Crea `web/src/main.rs`:

```rust,ignore
// LINK-TIME WIRING — DO NOT REMOVE. Force-links each layer crate so its beans,
// controllers, and schemas survive dead-code elimination into the binary.
firefly::link!(
    lumen_ledger_core,
    lumen_ledger_models,
    lumen_ledger_interfaces
);

mod controllers;

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen-ledger")
        .version(firefly::VERSION)
        .run()
        .await
}
```

Lo que acaba de ocurrir: `firefly::link!(a, b, c)` se expande a
`extern crate a as _;` por cada crate, que es exactamente la referencia que el
enlazador necesita para conservar los registros `inventory` de ese crate. Sin
ella obtienes el clásico síntoma de "6 de 16 beans" —el binario compila, enlaza,
se ejecuta y descarta silenciosamente la mitad de sus beans. El propio crate
`-web` está referenciado (él *es* el binario, y declara `mod controllers`), así
que no aparece en la lista de `link!`; las tres capas de biblioteca sí.

Observa que `main` en sí mismo es la misma única línea que escribiste en
[Inicio rápido](./02-quickstart.md) —`FireflyApplication::new(name).run().await`—
solo que con `.version(firefly::VERSION)` establecido para que `/actuator/info`
informe de la versión del framework. Un servicio por capas necesita exactamente
**una** línea extra de cableado (`link!`); todo lo demás se descubre.

Para convertir un crate `link!` olvidado de un bug silencioso en un fallo ruidoso,
protege el arranque con `assert_discovered`. Lo llamas justo después de que
`bootstrap()` retorne (la costura de prueba de Inicio rápido), usando el
`Bootstrapped::container` devuelto:

```rust,ignore
let app = firefly::FireflyApplication::new("lumen-ledger")
    .bootstrap()
    .await
    .expect("bootstrap");

// At least 8 beans (repository, service, mapper, component, config, …) and at
// least 1 controller were discovered — across all three layer crates.
firefly::assert_discovered(&app.container, 8, 1);
```

`assert_discovered(&container, min_beans, min_controllers)` entra en pánico al
arrancar si el número de beans o controladores descubiertos cae por debajo del
suelo que afirmas —la comprobación más útil de un servicio por capas.

> **Tip** **Punto de control.** `cargo run -p firefly-sample-lumen-ledger-web`
> arranca en `:8080` (público) con la superficie de gestión en `:8081`, y el
> informe de arranque lista los beans extraídos de los cuatro crates de código. Si
> el recuento de beans parece demasiado pequeño, falta un crate en
> `firefly::link!`.

## Paso 8 — La superficie web de calidad de producción

El `@RestController` es la última capa, y es más que CRUD —lleva la disciplina de
errores y validación que se espera de un servicio Spring Boot, cada fallo
renderizado como `application/problem+json` de RFC 9457. Conociste cada una de
estas herramientas en [Tu primera API HTTP](./06-first-http-api.md) y
[OpenAPI](./06a-openapi.md); aquí se componen sobre el servicio por capas.

El controlador es un bean `#[derive(Controller)]` que hace autowiring del puerto
`dyn WalletService` de `-core` y es auto-montado por `#[rest_controller]`:

```rust,ignore
use std::sync::Arc;
use firefly::prelude::*;
use firefly::web::{PageRequest, Path, Query, Valid, WebError, WebResult};
use lumen_ledger_core::{ServiceError, WalletService};

#[derive(Clone, Controller)]
pub struct WalletController {
    #[autowired]
    service: Arc<dyn WalletService>,
}

#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletController {
    #[post("/wallets", summary = "Open a wallet", status = 201,
        header("Idempotency-Key", description = "optional client-supplied key to make retries safe"))]
    async fn open(
        State(api): State<WalletController>,
        headers: axum::http::HeaderMap,
        Valid(body): Valid<CreateWalletRequest>,   // 422 on a blank owner / bad currency
    ) -> WebResult<(StatusCode, Json<WalletResponse>)> {
        let view = api.service.create(body).await.map_err(service_to_web)?;
        Ok((StatusCode::CREATED, Json(view)))
    }

    #[get("/wallets/:id", summary = "Fetch a wallet")]
    async fn get(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,                       // 400 on a non-UUID id
    ) -> WebResult<Json<WalletResponse>> {
        let view = api.service.get(id).await.map_err(service_to_web)?;
        Ok(Json(view))
    }

    #[get("/wallets/page", summary = "List wallets by status (paged)")]
    async fn list_paged(
        State(api): State<WalletController>,
        Query(query): Query<StatusQuery>,
        PageRequest(pageable): PageRequest,         // binds page/size/sort
    ) -> WebResult<Json<Page<WalletResponse>>> {
        let page = api.service.list_by_status(query.status, pageable).await.map_err(service_to_web)?;
        Ok(Json(page))
    }
    // …deposit, withdraw, transfer, search, set_status, delete…
}
```

Lo que acaba de ocurrir, preocupación por preocupación:

| Preocupación | Cómo se gestiona |
|---|---|
| Bean validation en el borde | `Valid<CreateWalletRequest>` / `Valid<AmountRequest>` / `Valid<TransferRequest>` — un owner en blanco, una moneda no ISO (`#[validate(pattern = "[A-Z]{3}")]`), o un importe no positivo (`#[validate(range(min = 1))]`) es un **422** antes de que el servicio se ejecute |
| Path / query malformado | los extractores `firefly::web::{Path, Query}` del framework — un id que no es UUID o un `?owner=` ausente es un problem **400**, no el texto plano por defecto de axum |
| Transferencia atómica | `POST /api/v1/wallets/:id/transfer` debita el origen y abona el destino dentro de **una transacción** (Paso 6). Una transferencia rechazada no mueve dinero |
| Conflicto de bloqueo optimista | una escritura `@Version` obsoleta → `ServiceError::Conflict` → **409** |
| Monedero desconocido | `ServiceError::NotFound` → **404** |
| Ciclo de vida de estado | `PATCH /api/v1/wallets/:id/status` transiciona `active → frozen → closed`; un monedero congelado rechaza un débito con **422** |
| Eliminación | `DELETE /api/v1/wallets/:id` → **204**, delegando en `delete_by_id` |
| Paginación | `GET /api/v1/wallets/page?status=active&page=1&size=20&sort=balance,desc` devuelve un `Page<T>` al estilo de Spring Data (`content` + `totalElements`). El resolver `PageRequest` enlaza `page` / `size` / `sort` en un `Pageable` (exactamente como un parámetro `Pageable` de Spring), que el servicio pasa a la consulta derivada paginada `find_by_status` |
| Filtrado | `GET /api/v1/wallets/search?owner=&currency=&status=&minBalance=&maxBalance=` enlaza un DTO de query `WalletFilter` (cada campo un parámetro de query de OpenAPI); el servicio convierte los criterios presentes en una `Specification` que el repositorio compila a un `WHERE` consciente del dialecto. Se requiere al menos un criterio |

> **Note** **Término clave — regla del huérfano.** La *regla del huérfano* de Rust
> prohíbe implementar un trait para un tipo cuando *ambos* son foráneos al crate
> actual. `WebError` (de `firefly`) y `ServiceError` (de `-core`) son ambos
> foráneos a `-web`, así que `impl From<ServiceError> for WebError` es ilegal aquí.
> El controlador los mapea con una pequeña función libre en su lugar —la misma
> restricción que hizo del `@Mapper` un bean en lugar de un `impl From`:

```rust,ignore
fn service_to_web(err: ServiceError) -> WebError {
    match err {
        ServiceError::NotFound => WebError::from(FireflyError::not_found("wallet not found")),
        ServiceError::Validation(d) => WebError::from(FireflyError::validation(d)),
        ServiceError::Conflict(d) => WebError::from(FireflyError::conflict(d)),
        ServiceError::Backend(d) => WebError::from(FireflyError::internal(d)),
    }
}
```

> **Tip** **Punto de control.** Cada handler del controlador devuelve
> `WebResult<T>`, y cada fallo de dominio fluye a través de `service_to_web` hacia
> un estado de problem preciso. Abre `http://localhost:8081/swagger-ui` tras
> `cargo run` para ver toda la superficie del monedero —cuerpos, parámetros de
> query y la cabecera `Idempotency-Key` declarada— renderizada a partir del
> inventario. La documentación OpenAPI está en el puerto de **gestión**, junto a
> actuator y admin, nunca en la API pública.

## Paso 9 — Entregar a los llamadores un SDK tipado

El quinto crate, `-sdk`, es un cliente saliente tipado sobre
`firefly_client::RestClient`, que reutiliza los DTOs de `-interfaces` para que un
llamador nunca redeclare el contrato. Como `-sdk` depende solo de `-interfaces`,
importarlo arrastra los DTOs y nada más —sin persistencia, sin pila web.

```rust,ignore
use firefly_client::{ClientError, RestBuilder, RestClient, NO_BODY};
use http::Method;
use lumen_ledger_interfaces::{AmountRequest, CreateWalletRequest, WalletResponse};

pub struct WalletClient {
    inner: RestClient,
}

impl WalletClient {
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self { inner: RestBuilder::new(base_url).build() }
    }

    /// `POST /api/v1/wallets` — open a wallet.
    pub async fn create_wallet(
        &self,
        request: &CreateWalletRequest,
    ) -> Result<WalletResponse, ClientError> {
        self.inner
            .request::<_, WalletResponse>(Method::POST, "/api/v1/wallets", Some(request))
            .await
    }

    /// `GET /api/v1/wallets/{id}` — fetch one wallet.
    pub async fn get_wallet(
        &self,
        id: impl std::fmt::Display,
    ) -> Result<WalletResponse, ClientError> {
        let path = format!("/api/v1/wallets/{id}");
        self.inner
            .request::<(), WalletResponse>(Method::GET, &path, NO_BODY)
            .await
    }
    // …list_wallets, deposit, withdraw…
}
```

Lo que acaba de ocurrir: cada método se asigna a un endpoint y (de)serializa los
DTOs compartidos, de modo que el llamador programa contra *los mismos tipos* que
el servidor impone —una deriva del contrato no compila. Cada método devuelve
`Result<T, ClientError>`; un cuerpo RFC 9457 que no sea 2xx se decodifica en un
`FireflyError` tipado accesible vía `ClientError::as_firefly`. El constructor
`with_client` envuelve un `RestClient` ya configurado (cabeceras personalizadas,
reintentos, timeouts, un token bearer), la vía trae-tu-propio-cliente. La
superficie completa de `RestClient` se cubre en
[Clientes HTTP](./13-http-clients.md).

### Generar el SDK en su lugar

También puedes **generar** un cliente equivalente a partir del documento OpenAPI
del servicio en ejecución, para que nunca vuelvas a escribir un método a mano:

```bash
firefly openapi-client --spec wallet-openapi.json -o src/generated.rs --client-name WalletClient
```

`firefly openapi-client` recorre la spec y emite un cliente autocontenido —un
`struct` / `enum` de modelo por cada entrada de `components.schemas` y una
`async fn` por operación, con parámetros de path / query tipados y cuerpos JSON.
El archivo generado va encabezado con
`// Code generated by \`firefly openapi-client\`. DO NOT EDIT.`. El catálogo
completo del generador está en [La CLI](./19-cli.md).

> **Tip** **Punto de control.** `cargo test -p firefly-sample-lumen-ledger-sdk`
> compila el cliente y ejecuta sus comprobaciones de contrato —el resultado
> tipado de cada método encaja con un DTO compartido de `-interfaces`. (La propia
> ida y vuelta por red la ejercita la prueba de integración de `-web` en el
> Paso 10.)

## Paso 10 — Ejecutar y probar todo el grafo

Con los cinco crates en su sitio, ejecuta y prueba el servicio:

```bash
cargo run  -p firefly-sample-lumen-ledger-web   # boots on :8080, management on :8081
cargo test -p firefly-sample-lumen-ledger-web   # in-process cross-crate round-trip
```

La prueba de integración arranca todo el grafo en proceso con `bootstrap()` (sin
socket enlazado), afirma el descubrimiento con `assert_discovered(&app.container,
8, 1)`, y conduce toda la superficie pública a través del `api_router` devuelto
—crear / obtener / depositar / retirar, la consulta de estado paginada, la
specification de búsqueda, la transición de estado, eliminar, la transferencia
atómica (incluida cada vía de rechazo), y cada vía de problem (404, los fallos de
validación 422, el 400 de path-malformado / query-ausente). También comprueba el
router de **gestión**: que el documento OpenAPI se sirve ahí (y *ausente* de la
API pública), y que una ruta de gestión desconocida responde un problem 404 de
RFC 9457 —el mismo contrato que la API pública.

El propósito de la prueba es la arquitectura, no solo las aserciones: demuestra
que cada capa se cablea junta solo mediante DI. El `@RestController` en `-web`
alcanza el `@Service` en `-core`, que alcanza el `@Repository` en `-models`, que
alcanza el datasource `@Bean` —todo descubierto, nada ensamblado a mano, a través
de cuatro fronteras de crate.

> **Tip** **Punto de control.** Ambos comandos tienen éxito. `cargo run` imprime
> un informe de arranque cuya línea `:: beans ::` se extrae de cada crate de
> código, y la batería de pruebas está en verde —el servicio por capas se comporta
> como una sola aplicación.

## Resumen — lo que construiste

Convertiste una muestra de un solo crate en un microservicio por capas de cinco
crates sin añadir una raíz de composición:

| Capa | Crate | Lo que aporta |
|---|---|---|
| contrato | `…-interfaces` | DTOs + el enum `WalletStatus` — `#[derive(Schema, Validate)]`, no depende de nadie |
| persistencia | `…-models` | la `@Entity` `Wallet`, el `@Repository` de dos derives, el `@Bean` del datasource asíncrono |
| negocio | `…-core` | el puerto `@Service`, el `@Mapper`, un `@Component`; la transferencia atómica `#[transactional]` |
| web | `…-web` | el `@RestController`, el cableado `firefly::link!`, el `FireflyApplication` de una línea |
| cliente | `…-sdk` | un `RestClient` tipado sobre los DTOs compartidos (o generado a partir de OpenAPI) |

Ahora también sabes:

- Por qué las flechas de dependencia deben apuntar estrictamente hacia adentro, y
  cómo cada capa aporta exactamente los estereotipos que le pertenecen.
- Que un repositorio al estilo de Spring Data son dos derives —`#[derive(Entity)]`
  y `#[derive(SqlxRepository)]`— construidos a partir de un bean de datasource
  asíncrono, dándote `ReactiveCrudRepository` + `ReactiveSpecificationRepository`
  + consultas derivadas gratis.
- Que `firefly::link!` es la *única* línea de cableado que necesita un servicio por
  capas, que existe porque el descubrimiento es en tiempo de enlazado, y que
  `assert_discovered` convierte un crate olvidado en un fallo de arranque ruidoso.
- Cómo una transferencia atómica compone `#[transactional]`, el bloqueo optimista
  y un diseño de precondiciones-primero para que una transferencia rechazada no
  mueva dinero.
- Cómo entregar a los llamadores un SDK tipado que reutiliza el crate del contrato
  —o generar uno a partir del documento OpenAPI en vivo.

## Ejercicios

1. **Provoca el dead-strip.** Comenta un crate en la línea `firefly::link!` (por
   ejemplo `lumen_ledger_models`), y luego `cargo run -p
   firefly-sample-lumen-ledger-web`. Observa cómo `assert_discovered` falla al
   arrancar con el pánico "discovered N beans but expected at least 8" —ese es
   exactamente el bug que `link!` previene. Restaura la línea.
2. **Traza una petición a través de cuatro crates.** Con el servicio en ejecución,
   `curl -X POST localhost:8080/api/v1/wallets -H 'content-type: application/json'
   -d '{"owner":"ada","currency":"EUR","openingBalance":1000}'`. Nombra, en orden,
   qué crate gestiona cada salto: el controlador (`-web`), el servicio (`-core`),
   el mapper (`-core`), el repositorio (`-models`), el datasource (`-models`).
3. **Rompe la afirmación de atomicidad de la transferencia.** Lee `transfer_tx` y
   confirma que las comprobaciones de moneda / fondos / actividad se ejecutan todas
   *antes* del primer `persist`. Luego haz `curl` de una transferencia con
   `amount` mayor que el saldo del origen y verifica que el saldo del origen queda
   inalterado después (`GET` de él) —una transferencia rechazada no mueve dinero.
4. **Añade una consulta derivada.** Añade `find_by_currency(&self, currency: &str)
   -> Result<Vec<Wallet>, DataError>` al bloque `#[firefly::repository]` (cuerpo
   `unimplemented!()`), exponla a través del servicio y una ruta de controlador, y
   confirma que funciona —sin escribir nada de SQL.
5. **Genera el SDK.** Ejecuta el servicio, obtén la spec con `curl
   localhost:8081/v3/api-docs > wallet-openapi.json`, y luego `firefly
   openapi-client --spec wallet-openapi.json -o /tmp/generated.rs --client-name
   WalletClient`. Compara los métodos generados con el cliente `-sdk` escrito a
   mano.

## Adónde ir después

- Conduce una aplicación plenamente cableada en proceso —la costura `bootstrap()`,
  el `api_router` / `management_router`, y las pruebas de ida y vuelta entre
  crates como la del Paso 10— en **[Pruebas](./18-testing.md)**.
- Revisa la maquinaria de persistencia que este capítulo dispuso por capas
  (entidades, consultas derivadas, specifications, bloqueo optimista) en
  **[Persistencia y repositorios reactivos](./07-persistence.md)**.
- Lleva el servicio por capas a producción —PostgreSQL real vía `DATABASE_URL`,
  contenedores y la superficie de gestión— en
  **[Producción y despliegue](./20-production.md)**.
