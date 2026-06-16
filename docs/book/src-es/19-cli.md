# La CLI

Hasta ahora has construido **Lumen** — el servicio de monedero digital y libro
mayor desde [Inicio rápido](./02-quickstart.md) en adelante — a mano: un archivo
cada vez, un `cargo build` después de cada capítulo. Eso fue deliberado, para que
cada línea sea algo que tecleaste y comprendes. Este capítulo enseña la *otra*
forma de hacer el mismo trabajo: la CLI de desarrollo `firefly`. Es un único
binario compilado que andamia un proyecto, genera los mismos artefactos que los
capítulos anteriores escribieron a mano, ejecuta el binario con perfiles y
sobrescrituras de configuración, sella metadatos de compilación, gestiona
migraciones, exporta un documento OpenAPI e introspecciona una Lumen *en
ejecución* a través de su superficie de actuator — el bucle cotidiano del
desarrollador en una sola herramienta.

Nada en este capítulo cambia el propio `samples/lumen`; es puramente operativo.
Pero al terminar serás capaz de pilotar todo el ciclo de vida desde la línea de
comandos y — algo igual de importante — sabrás exactamente con qué crate del
framework habla cada comando, porque la CLI nunca inventa una API. Llama a los
mismos `firefly-migrations`, `firefly-openapi` y endpoints de actuator que ya
conoces.

Al terminar este capítulo, serás capaz de:

- Instalar el binario `firefly` y leer su catálogo de comandos.
- Andamiar un nuevo servicio de dos maneras — eligiendo un *archetype* y activando
  *features* — y previsualizar el plan exacto de archivos con `--dry-run`.
- Generar artefactos de código individuales (un comando CQRS, una consulta, un
  agregado, una saga, una migración) dentro de un proyecto existente, y leer lo
  que los generadores emiten realmente.
- Ejecutar una aplicación Firefly a través de la CLI, mapeando los flags de perfil
  y sobrescritura a las variables de entorno `FIREFLY_*` que el framework lee al
  arrancar.
- Introspeccionar una Lumen *en ejecución* — su salud, rutas, beans y métricas — a
  través del puerto de actuator, y entender por qué un binario compilado requiere
  `--url`.

## Conceptos que conocerás

Antes del primer comando, aquí están las ideas en las que se apoya este capítulo.
Cada una se reintroduce en contexto donde se usa por primera vez; esta es la
versión breve.

> **Note** **Término clave — archetype.** Un *archetype* es una plantilla de
> proyecto que decide la forma inicial de tu crate: qué módulos existen, qué
> features de Firefly están activadas y qué aspecto tiene el código de ejemplo. La
> CLI incluye seis (`core`, `web-api`, `web`, `hexagonal`, `library`, `cli`). El
> análogo en Spring es un "tipo de proyecto" de Spring Initializr más sus
> dependencias preseleccionadas.

> **Note** **Término clave — feature.** Una *feature* es un subsistema opcional que
> el andamiaje conecta — `web`, `data`, `cqrs`, `eda`, `cache`, `security`, etc.
> Cada una se corresponde con uno o más crates `firefly-*` añadidos al `Cargo.toml`
> generado. En términos de Spring, elegir una feature es como marcar un starter en
> el Initializr.

> **Note** **Término clave — superficie de actuator.** La *superficie de actuator*
> es el conjunto de endpoints HTTP operativos — `/actuator/health`,
> `/actuator/info`, `/actuator/metrics`, `/actuator/mappings`, `/actuator/beans`,
> `/actuator/conditions`, `/actuator/env` — que una aplicación Firefly en ejecución
> sirve en su puerto de **gestión** (`8081` por defecto), separado de la API
> pública en `8080`. Esto refleja Spring Boot Actuator. Los comandos de
> introspección de la CLI son clientes ligeros sobre estos endpoints.

> **Note** **Término clave — Segregación de Responsabilidad entre Comandos y
> Consultas (CQRS).** Un patrón que enruta los **comandos** que cambian estado y
> las **consultas** de solo lectura a través de handlers separados sobre un *bus*
> compartido. Construiste los handlers de comando y consulta de Lumen en
> [CQRS](./09-cqrs.md); la CLI puede andamiar las mismas piezas para un proyecto
> nuevo con `firefly generate command` / `firefly generate query`.

## Paso 1 — Instalar el binario

La CLI vive en el `crates/cli` del framework. Instálala desde un checkout y luego
pídele que se describa a sí misma.

```bash
cargo install --path crates/cli   # installs the `firefly` binary
firefly --help                     # prints the banner + every command
firefly --version                  # 26.6.28
```

Lo que acaba de pasar: `cargo install` compiló el binario `firefly` y lo colocó en
tu `PATH`. `--version` imprime la versión de calendario del framework — la misma
`26.6.28` de la que depende Lumen, porque la CLI se versiona junto con el resto del
workspace.

> **Tip** **Punto de control.** `firefly --version` imprime `26.6.28` y
> `firefly --help` lista los subcomandos, incluyendo `new`, `generate`, `run`,
> `db`, `openapi`, `doctor` y `health`. Si `firefly` da "command not found",
> asegúrate de que `~/.cargo/bin` esté en tu `PATH`.

Si prefieres no instalar el binario, puedes pilotar la CLI a través de Cargo desde
un checkout del framework — consulta el [Paso 9](#step-9--run-the-cli-through-cargo).

## Paso 2 — Leer el catálogo de comandos

Todo el bucle del desarrollador cabe en una tabla. Échale un vistazo ahora; el
resto del capítulo recorre los comandos que más usarás.

| Command                                              | Purpose                                       |
|------------------------------------------------------|-----------------------------------------------|
| `firefly new <name>`                                 | andamia un nuevo proyecto firefly-rust        |
| `firefly generate <kind> <name>` (alias `g`)         | genera un artefacto de código                 |
| `firefly run`                                        | `cargo run` con flags de perfil / sobrescritura |
| `firefly build <info\|image>`                        | sella `build-info.json` / construye una imagen OCI |
| `firefly info`                                       | información del framework + del entorno        |
| `firefly doctor`                                     | comprobaciones de toolchain (rustc, cargo, git, …) |
| `firefly db <init\|migrate\|upgrade\|status>`        | gestión de migraciones                         |
| `firefly openapi --format json\|yaml [-o file]`      | exporta un documento OpenAPI 3.1               |
| `firefly openapi-client --spec <file>`               | genera un cliente Rust tipado a partir de una spec |
| `firefly actuator <endpoint> --url <base>`           | consulta el `/actuator/*` de una app en ejecución |
| `firefly routes\|env\|health\|metrics --url <base>`  | introspección remota de una app en ejecución   |
| `firefly beans\|conditions --url <base>`             | informe de DI / autoconfiguración de una app en ejecución |
| `firefly completion <shell>`                         | imprime un script de autocompletado de shell   |
| `firefly sbom [--json]`                              | lista de materiales de software desde `Cargo.lock` |
| `firefly license`                                    | informe de licencias del framework + dependencias |

Lo que acaba de pasar: esa es la superficie completa. Fíjate en la forma del bucle
— *andamiar* (`new`), *crecer* (`generate`), *ejecutar* (`run`), *empaquetar*
(`build`), *operar* (`db`, `openapi`), *introspeccionar* (`actuator`, `routes`,
`health`, …) y *auditar* (`doctor`, `sbom`, `license`). Cada comando se
corresponde con un crate del framework o un endpoint de actuator que ya conoces.

## Paso 3 — Andamiar un proyecto

`firefly new` genera un crate de Cargo sin workspace: un árbol `src/` con la forma
del archetype, un `firefly.yaml`, un `.gitignore`, un `README.md`, un `Dockerfile`
y un directorio `tests/`. Es la misma forma inicial que tenía Lumen tras el
[Inicio rápido](./02-quickstart.md).

```bash
firefly new lumen2 --archetype web-api --features web,data,cqrs --git
firefly new my-lib --archetype library --dep-path ../../             # local dev deps
firefly new --list                                                   # archetypes + features
firefly new svc --dry-run                                            # plan without writing
```

Lo que acaba de pasar, comando a comando:

- La primera línea andamia un proyecto `web-api` llamado `lumen2` con las features
  `web`, `data` y `cqrs` activadas y (por `--git`) inicializa un repositorio Git
  con un commit inicial.
- `--dep-path ../../` apunta las dependencias `firefly-*` generadas a un checkout
  de workspace local en lugar del repositorio canónico de GitHub. Cada crate se
  resuelve automáticamente en su propio `crates/<subdir>`.
- `--list` imprime los catálogos de archetypes y features, y luego sale sin crear
  nada.
- `--dry-run` imprime el plan exacto de archivos — cada ruta que *se* escribiría —
  sin tocar el sistema de archivos.

Los seis archetypes son `core`, `web-api`, `web`, `hexagonal`, `library` y `cli`.
El archetype `web-api` sella un punto de entrada, un controlador y el árbol por
capas `models/services/repositories` cableado contra el starter web real, de modo
que el primerísimo `cargo run` arranca. El origen de las dependencias `firefly-*`
generadas es configurable: `--dep-path <base>` para un checkout local,
`--dep-version <ver>` para una release publicada en crates.io y, en caso
contrario, el repositorio Git canónico. `--force` sobrescribe un directorio de
destino existente.

> **Note** Una feature que no selecciones simplemente está ausente del `Cargo.toml`
> generado — elegir `web,data,cqrs` añade `firefly-web`, `firefly-data` +
> `firefly-migrations`, y `firefly-cqrs`, y nada más. La lista completa de features
> (`web`, `data`, `mongodb`, `eda`, `cache`, `client`, `security`, `scheduling`,
> `observability`, `cqrs`, `shell`, `transactional`) la imprime
> `firefly new --list`, y los crates subyacentes son los que cataloga el
> [capítulo de macros](./21-declarative-macros.md).

> **Tip** **Punto de control.** Ejecuta
> `firefly new lumen2 --archetype web-api --dry-run`. Deberías ver un plan que
> lista `Cargo.toml`, `firefly.yaml`, `.gitignore`, `README.md`, `Dockerfile`,
> `src/main.rs`, `src/lib.rs`, `src/controllers.rs`, el árbol
> `models/services/repositories` y `tests/api.rs` — sin nada escrito en disco.
> Quita `--dry-run` y aparecerán los mismos archivos bajo `lumen2/`.

## Paso 4 — Generar artefactos individuales

Una vez que existe un proyecto, `firefly generate` (alias `g`) escribe un artefacto
cada vez dentro de él, detectando el paquete, el archetype y los flags de feature a
partir de `Cargo.toml` + `firefly.yaml`. Estas son exactamente las piezas que
escribiste a mano para Lumen — un comando y su handler, una consulta, un agregado,
una saga, una migración.

```bash
firefly generate command OpenWallet      # src/cqrs/open_wallet_command{,_handler}.rs
firefly generate query   GetWallet       # src/cqrs/get_wallet_query{,_handler}.rs
firefly generate aggregate Wallet        # src/domain/wallet.rs (embeds AggregateRoot)
firefly generate saga    MoneyTransfer --dry-run
firefly generate migration AddWallets    # migrations/V###__add_wallets.sql
firefly g handler Deposit                # `g` is the alias
```

Los tipos de artefacto son `handler`, `route`, `entity`, `repository`, `dto`,
`aggregate`, `command`, `query`, `saga` y `migration`. Los nombres se aceptan en
cualquier caja y se convierten según haga falta (`OpenWallet`, `open-wallet` y
`open_wallet` producen todos los mismos archivos). `--force` sobrescribe un archivo
existente; `--dry-run` planifica sin escribir.

Lo que acaba de pasar, con los dos generadores CQRS como ejemplo trabajado. Un
`generate command OpenWallet` escribe **dos** archivos dentro de `src/cqrs/`:

```rust,ignore
// src/cqrs/open_wallet_command.rs
use firefly_cqrs::{CqrsError, Message};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenWallet {
    /// Target aggregate identifier.
    pub id: String,
}

impl Message for OpenWallet {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.id.trim().is_empty() {
            return Err(CqrsError::validation("id is required"));
        }
        Ok(())
    }
}
```

```rust,ignore
// src/cqrs/open_wallet_command_handler.rs
use firefly_cqrs::{Bus, CqrsError};

use super::open_wallet_command::OpenWallet;

/// Register the `OpenWallet` command handler on `bus`. Call once at startup.
pub fn register_open_wallet_handler(bus: &Bus) {
    bus.register(|command: OpenWallet| async move {
        // Implement the OpenWallet command behaviour here.
        Ok::<_, CqrsError>(command.id)
    });
}
```

El comando es un struct de mensaje sencillo que implementa `firefly_cqrs::Message`
(su `validate` se ejecuta en el middleware de validación del bus antes del
handler). El handler es una *función registradora* `register_<name>_handler(bus:
&Bus)` que llama al `bus.register(...)` basado en closures — la misma forma de
registro que usaste en [CQRS](./09-cqrs.md). `generate query GetWallet` refleja
esto con un struct de consulta `GetWallet` y un `register_get_wallet_handler(bus:
&Bus)`.

> **Note** Los generadores apuntan a las APIs `firefly-*` reales, no a cuerpos
> marcador de posición. `generate aggregate Wallet` escribe `src/domain/wallet.rs`
> con un struct que embebe `firefly_eventsourcing::AggregateRoot` (el búfer de
> eventos sin confirmar), exponiendo `raise(...)` y `take_events(...)`. `generate
> saga MoneyTransfer` escribe `src/sagas/money_transfer_saga.rs` con una función
> `build_money_transfer_saga()` sobre el builder `firefly_orchestration::Saga` —
> `Saga::new("money-transfer")`, `Step::new(...)`, `.with_compensation(...)`. Estas
> son las mismas construcciones que conociste en
> [Event Sourcing](./11-event-sourcing.md) y [Sagas](./12-sagas.md).

> **Tip** **Punto de control.** Dentro de un proyecto andamiado, ejecuta
> `firefly generate command OpenWallet --dry-run`. Deberías ver un plan que nombra
> `src/cqrs/open_wallet_command.rs` y `src/cqrs/open_wallet_command_handler.rs` como
> acciones `create`, sin nada escrito.

## Paso 5 — Ejecutar la aplicación

`firefly run` es un envoltorio ligero sobre `cargo run`. Mapea los flags de perfil
y de sobrescritura de configuración a las variables de entorno `FIREFLY_*` que el
framework lee al arrancar, y luego hace exec de Cargo desde la raíz del proyecto
detectada.

> **Note** **Término clave — flag de sobrescritura de config.** Un flag
> `-D key=value` sobrescribe un único valor de configuración. La CLI lo mapea a una
> variable de entorno quitando un `firefly.` inicial, pasándolo a mayúsculas y
> reemplazando `.`/`-` por `_`, y anteponiendo `FIREFLY_`. Así, `-D
> logging.level-root=DEBUG` se convierte en `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`. Esta
> es la misma convención de entorno que describe
> [Configuración](./03-configuration.md).

```bash
firefly run                                  # cargo run
firefly run -p dev -p test                   # FIREFLY_PROFILES_ACTIVE=dev,test
firefly run -D logging.level-root=DEBUG      # FIREFLY_LOGGING_LEVEL_ROOT=DEBUG
firefly run --env FIREFLY_SERVER_ADDR=0.0.0.0:8080  # a raw env var for the process
firefly run --debug                          # FIREFLY_LOGGING_LEVEL_ROOT=DEBUG
firefly run --release --bin lumen            # cargo run --release --bin lumen
firefly run --dry-run                        # print the resolved env + cargo command
```

Lo que acaba de pasar: los flags se resuelven en un entorno que se aplica antes de
`cargo run`. `-p`/`--profile` es repetible o separado por comas y se aplana en un
único `FIREFLY_PROFILES_ACTIVE`; `-D key=value` se mapea a `FIREFLY_<KEY>`;
`--env KEY=VALUE` pasa una variable en bruto directamente; `--debug` es atajo de
`FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`; `--release` y `--bin <name>` se pasan tal cual
a Cargo. Un servicio Firefly es un único binario compilado, así que no hay recarga
en caliente ni selección de proceso de trabajo — recompilas y vuelves a ejecutar.
`--dry-run` imprime el entorno resuelto y el comando `cargo run` exacto sin
ejecutar, que es la forma más segura de aprender el mapeo.

> **Warning** Una sobrescritura `-D` solo surte efecto si el framework lee
> realmente esa clave. Lumen vincula sus dos puertos desde `FIREFLY_SERVER_ADDR` /
> `FIREFLY_MANAGEMENT_ADDR` (un `host:port` completo), no desde una clave
> `server.port` — así que, para mover los puertos de Lumen, establece directamente
> las variables de entorno de dirección. El equivalente del enlace de dos puertos
> es:

```bash
firefly run --bin lumen \
  --env FIREFLY_SERVER_ADDR=127.0.0.1:8080 \
  --env FIREFLY_MANAGEMENT_ADDR=127.0.0.1:8081
```

Esta es la misma costura que el [Inicio rápido](./02-quickstart.md) usó con
variables `FIREFLY_*` en bruto — `firefly run --env` simplemente las establece por
ti.

> **Tip** **Punto de control.** Ejecuta `firefly run -p dev -D
> logging.level-root=DEBUG --dry-run` desde dentro de un proyecto. La salida imprime
> `Would run: cargo run` y un bloque de entorno que lista
> `FIREFLY_PROFILES_ACTIVE=dev` y `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`. No se lanza
> nada.

## Paso 6 — Construir para release

La compilación simple es `cargo build`. El grupo `build` añade los dos artefactos
que un pipeline de release necesita por encima del binario compilado.

```bash
firefly build info                       # write build-info.json (git SHA + UTC time)
firefly build info -o target/build-info.json
firefly build image -t lumen:1.0.0       # OCI image via Cloud Native Buildpacks (`pack`)
firefly build image --builder docker     # or a plain Dockerfile build
```

Lo que acaba de pasar: `build info` escribe un `build-info.json` con la forma
`{"git": {"sha": …}, "build": {"time": …}}` (un SHA vacío cuando git no está
disponible). Ese archivo es la fuente de datos que lee el contribuidor de
compilación de `/actuator/info` cuando está presente, de modo que el SHA de git y
la hora de compilación aparecen junto al bloque `InfoContributor` que cableaste en
[Observabilidad](./15-observability.md). `build image` construye una imagen OCI —
por defecto vía Cloud Native Buildpacks (la herramienta `pack`), o con
`--builder docker` contra el `Dockerfile` andamiado.

> **Tip** **Punto de control.** Ejecuta `firefly build info -o /tmp/build-info.json`
> y abre el archivo. Es JSON válido con un objeto `git` y `build` de nivel
> superior, y `build.time` es una marca de tiempo UTC RFC 3339 que termina en `Z`.

## Paso 7 — Gestionar migraciones de base de datos

Lumen corre sobre un almacén de eventos en proceso, así que no incluye **ninguna**
migración SQL — su árbol `samples/lumen` no tiene directorio `migrations/` en
absoluto. Pero en el momento en que cambies al almacén de eventos de Postgres de
[Producción y despliegue](./20-production.md), `firefly db` gestiona el esquema.
Pilota el propio ejecutor de migraciones forward-only del framework, la misma
biblioteca [`firefly-migrations`](./07-persistence.md) que incluyen los proyectos
generados.

```bash
firefly db init                            # migrations/ + starter V001__init.sql
firefly db migrate -m "create wallets"     # writes V002__create_wallets.sql
firefly db upgrade --url sqlite://app.db   # apply pending migrations
firefly db status  --url sqlite://app.db   # show applied + pending
```

Lo que acaba de pasar: `db init` crea el directorio `migrations/` con un
`V001__init.sql` de arranque; `db migrate -m <msg>` escribe un nuevo
`V###__<slug>.sql` vacío con la versión autoincrementada a partir de la migración
existente más alta; `db upgrade` aplica todas las migraciones pendientes (de forma
idempotente — una reejecución aplica cero); `db status` informa de las migraciones
aplicadas y pendientes. La URL de la base de datos se resuelve desde `--url`, luego
`$DATABASE_URL`, luego `firefly.datasource.url` en `firefly.yaml`, con un valor por
defecto de `sqlite://firefly.db`.

> **Note** El ejecutor de migraciones es **forward-only** (un historial de solo
> adición, al estilo Flyway). Por eso no existe `firefly db downgrade` —
> ejecutarlo falla de forma ruidosa en lugar de hacer un no-op silencioso. Para
> deshacer un cambio, escribe en su lugar una nueva migración correctiva con
> `firefly db migrate`.

> **Warning** El backend de migraciones de la CLI es **SQLite vía `rusqlite`**. Una
> URL `postgres://` o `mysql://` devuelve un claro error de "not wired into the
> CLI". Para otro driver en producción, adapta el puerto
> `firefly_migrations::Database` y llama a `firefly_migrations::run` directamente
> desde tu build, en lugar de hacerlo a través de la CLI de conveniencia.

> **Tip** **Punto de control.** En un directorio temporal ejecuta `firefly db init`
> y luego `firefly db status --url ":memory:"`. Deberías ver una migración
> *pendiente* (`V001__init.sql`) y cero aplicadas, porque cada conexión `:memory:`
> empieza vacía.

## Paso 8 — Exportar OpenAPI y generar clientes

La CLI puede emitir un documento OpenAPI para el proyecto actual y, en sentido
inverso, generar un cliente Rust tipado a partir de cualquier spec.

```bash
firefly openapi                           # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml
firefly openapi-client --spec openapi.json -o client.rs --client-name WalletClient
```

Lo que acaba de pasar: `firefly openapi` lee los metadatos del documento
(`info.title` / `info.version` / `info.description`) desde `firefly.yaml`, luego
desde `Cargo.toml`, y emite un documento OpenAPI 3.1. Como un binario compilado no
puede arrancar una aplicación arbitraria para enumerar rutas en vivo, el documento
exportado es un **esqueleto** sellado con metadatos — un bloque `info` correcto y el
componente `ProblemDetail` estándar (Firefly renderiza los errores como
`application/problem+json` según RFC 9457), pero `paths` vacío. Para emitir las
rutas *reales* de Lumen, constrúyelas con `firefly_openapi::Builder` (que lee la
tabla de rutas de `#[rest_controller]`) y sírvelas con `Builder::router()` — la
spec en vivo que tu aplicación ya publica en `/v3/api-docs` en el puerto de
gestión.

`firefly openapi-client` es lo inverso: dado un documento OpenAPI 3.x, emite un
cliente tipado autocontenido sobre `firefly_client::RestClient` — un struct/enum de
modelo por cada entrada de `components.schemas` y una `async fn` por operación, con
parámetros de ruta tipados y cuerpos JSON. `--client-name` da nombre al struct
generado (por defecto `ApiClient`).

> **Tip** **Punto de control.** Ejecuta `firefly openapi --format yaml | head`. La
> primera línea es `openapi: 3.1.0`, seguida de un bloque `info:` que lleva el
> título y la versión de tu proyecto.

## Paso 9 — Introspeccionar una app en ejecución

Estos comandos consultan una Lumen *en ejecución* a través de HTTP. Un binario
compilado no tiene un contexto de DI offline que arrancar — no hay nada que
introspeccionar sin un proceso en vivo — así que `--url` es obligatorio, apuntado
al puerto de **gestión** de Lumen (la superficie de actuator de
[Observabilidad](./15-observability.md)).

```bash
firefly health  --url http://localhost:8081   # -> /actuator/health
firefly env     --url http://localhost:8081   # -> /actuator/env
firefly routes  --url http://localhost:8081   # -> /actuator/mappings
firefly metrics requests --url http://localhost:8081
firefly actuator info    --url http://localhost:8081 --json
firefly actuator metrics requests --url http://localhost:8081 --json
firefly beans      --url http://localhost:8081   # the DI container's bean table
firefly conditions --url http://localhost:8081   # the auto-configuration report
```

Lo que acaba de pasar: cada comando hace un GET a un endpoint de actuator mapeado e
imprime el JSON con formato legible. `routes` se mapea a `/actuator/mappings` (cada
ruta `#[rest_controller]`), `health`/`env`/`metrics`/`info` se mapean a sus
endpoints homónimos, y `beans`/`conditions` renderizan la tabla de beans de DI y el
informe de evaluación de beans condicionales — la introspección de DI de Spring
Boot Actuator. `firefly actuator <endpoint>` es la forma general;
`firefly health|env|routes|metrics|beans|conditions` son atajos de conveniencia.
`--json` emite el cuerpo en bruto para canalizar por tubería.

> **Note** **Término clave — bean.** Un *bean* es un objeto que el framework
> construye y gestiona por ti. `/actuator/beans` lista cada uno (tipo, scope,
> estereotipo), y `/actuator/conditions` informa de las guardas `@Profile` /
> `@ConditionalOn…` declaradas por cada bean condicional. Estos se leen a través de
> HTTP desde un servicio en ejecución, de la misma forma en que consultarías
> `/beans` y `/conditions` de Spring. Consulta
> [Inyección de dependencias](./04a-dependency-injection.md) para el propio
> contenedor de beans.

> **Tip** **Punto de control.** En una terminal ejecuta `cargo run --bin lumen`; en
> otra, ejecuta `firefly health --url http://localhost:8081`. Deberías ver un cuerpo
> JSON con `"status":"UP"`. Si `firefly routes --url …` devuelve un error sobre un
> contexto en proceso ausente, has omitido `--url` — estos comandos siempre lo
> requieren.

## Paso 10 — Diagnosticar, completar y auditar

Los comandos restantes informan sobre tu entorno y tus dependencias.

```bash
firefly info                # framework version + which optional adapters are built
firefly doctor              # checks rustc, cargo, git, clippy, rustfmt, docker
firefly completion zsh      # > ~/.zfunc/_firefly  (bash | zsh | fish | powershell)
firefly sbom                # a software bill of materials from Cargo.lock
firefly sbom --json         # machine-readable, for a compliance pipeline
firefly license             # the framework + dependency license report
```

Lo que acaba de pasar: `firefly doctor` es lo primero que ejecutar en una máquina
nueva. Informa de tus versiones de `rustc` y `cargo` (las dos herramientas
*requeridas*) y de si `git`, `clippy`, `rustfmt` y `docker` están en el `PATH` (las
*opcionales*), más el paquete, el archetype del proyecto detectado y si hay un
`firefly.yaml` y un `migrations/` presentes — terminando con "All required checks
passed!" o una lista de qué arreglar. `firefly completion <shell>` imprime un script
de autocompletado de shell generado a partir de la definición viva de la CLI, así
que siempre coincide con los subcomandos y flags disponibles. `firefly sbom` y
`firefly license` leen `Cargo.lock` para producir una lista de materiales de
software (SBOM) y un informe de licencias de dependencias para un pipeline de
cumplimiento.

> **Tip** **Punto de control.** Ejecuta `firefly doctor`. Dentro del workspace del
> framework informa de `rustc` y `cargo` como comprobaciones requeridas superadas e
> imprime un bloque `Project`. La línea final es "All required checks passed!".

## Paso 11 — Ejecutar la CLI a través de Cargo

Si no has instalado el binario, pilota la CLI a través de Cargo desde un checkout
del framework — útil en CI, o mientras iteras sobre la propia CLI.

```bash
make cli ARGS="doctor"
make cli ARGS="new orders --archetype web-api"
cargo run -p firefly-cli --bin firefly -- info
```

Lo que acaba de pasar: cada forma ejecuta el mismísimo binario `firefly`, solo que
sin instalarlo primero. El `--` separa los argumentos propios de Cargo de los que
se pasan a `firefly`.

## Resumen — la CLI se corresponde con crates que ya conoces

No cambiaste `samples/lumen` en este capítulo; es operativo. Pero viste el camino
de la CLI hacia cada artefacto que Lumen hizo crecer a mano:

- `firefly new --archetype web-api` andamia el esqueleto del
  [Inicio rápido](./02-quickstart.md) — punto de entrada, controlador, árbol por
  capas, `Cargo.toml`, `firefly.yaml`, `Dockerfile`, `tests/`.
- `firefly generate command/query/aggregate/saga/migration` escribe las piezas de
  CQRS, DDD, orquestación y esquema — como funciones registradoras y construcciones
  `firefly-*` reales, no marcadores de posición.
- `firefly run --bin lumen` lo lanza, mapeando los flags `-p`/`-D`/`--env` al
  entorno `FIREFLY_*`, y `--env FIREFLY_SERVER_ADDR/MANAGEMENT_ADDR` mueve los dos
  puertos.
- `firefly build info` sella el `build-info.json` que el contribuidor de
  compilación de `/actuator/info` expone; `firefly db` pilota el ejecutor
  forward-only `firefly-migrations` una vez que adoptas un almacén SQL.
- `firefly health/routes/beans/conditions --url http://localhost:8081`
  introspecciona la superficie de actuator a través de HTTP, que es por lo que
  `--url` es obligatorio: un binario compilado no tiene contexto offline que
  arrancar.

El hilo conductor: la CLI nunca inventa una API. Cada comando llama a un crate del
framework (`firefly-migrations`, `firefly-openapi`, `firefly-client`) o a un
endpoint de actuator que ya conoces, de modo que la línea de comandos no es más que
una puerta más rápida al mismo edificio.

## Ejercicios

1. **Andamia un gemelo de Lumen.** Ejecuta `firefly new lumen2 --archetype web-api
   --features web,cqrs --dry-run`, y luego de nuevo sin `--dry-run`. Compara el
   árbol `src/` generado con el de Lumen, y hazle un `cargo build`.
2. **Genera las piezas de CQRS.** En el proyecto andamiado, ejecuta `firefly generate
   command OpenWallet` y `firefly generate query GetWallet`. Abre los cuatro
   archivos generados y confirma que los handlers son funciones registradoras
   `register_<name>_handler(bus: &Bus)` que llaman a `bus.register(...)` — la forma
   de registro de [CQRS](./09-cqrs.md), no una macro.
3. **Aprende el mapeo de entorno.** Arranca la aplicación con `firefly run -p dev -D
   logging.level-root=DEBUG --dry-run` y lee el entorno `FIREFLY_*` resuelto que
   exportaría. Luego mueve los puertos de verdad con `firefly run
   --bin lumen --env FIREFLY_SERVER_ADDR=127.0.0.1:9090 --env
   FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091` y `curl localhost:9091/actuator/health`.
4. **Introspecciona la Lumen real.** `cargo run --bin lumen`, luego en otra shell
   ejecuta `firefly health --url http://localhost:8081`, `firefly routes --url
   http://localhost:8081` y `firefly beans --url http://localhost:8081`. Coteja la
   tabla de rutas con las constantes de endpoint en `src/web.rs`.
5. **Audita el toolchain.** Ejecuta `firefly doctor` en tu máquina y anota qué
   herramientas opcionales (`git`, `clippy`, `rustfmt`, `docker`) están presentes, y
   luego ejecuta `firefly sbom --json | head` para ver el manifiesto de
   dependencias resueltas que la CLI lee de `Cargo.lock`.

## Adónde ir después

Con un proyecto andamiado, generado, ejecutado e introspeccionado, el siguiente
capítulo lleva a Lumen hasta producción — cambiando el almacén de eventos en
proceso por Postgres y Kafka, donde `firefly db` y `firefly build` finalmente se
ganan el sueldo. Continúa en **[Producción y despliegue](./20-production.md)**.
