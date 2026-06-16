# Inicio rápido

Aquí es donde **Lumen** —el servicio de monedero digital y libro mayor que harás
crecer a lo largo del resto del libro— cobra vida por primera vez. Al terminar
este capítulo, Lumen existe como un crate real: compila, imprime un banner, sirve
una superficie de gestión en vivo y se apaga de forma ordenada. Todavía no hace
casi nada más, y eso es deliberado. Todo a partir de aquí es *aditivo*: cada
capítulo posterior recorta un poco más del crate terminado
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen)
y lo reincorpora a la narrativa, y nada de lo que escribas ahora se descarta.

Abordaremos el mismo objetivo en dos pasadas. Primero generaremos el andamiaje del
crate con la CLI `firefly` (la vía rápida), y luego construiremos el crate idéntico
a mano para que cada línea sea algo que hayas tecleado y comprendas. Ambas pasadas
llegan a la misma forma de binario único que el resto del libro da por supuesta.

Al terminar este capítulo, serás capaz de:

- Generar el andamiaje de un proyecto Firefly de dos maneras: con la CLI
  `firefly new` y a partir de un `cargo new` desnudo.
- Comprender por qué un servicio Firefly depende de un *único* crate, la fachada
  `firefly`, en lugar de una constelación de artefactos de arranque.
- Escribir el `main` de una sola línea que arranca y sirve el servicio completo, y
  explicar qué hace cada etapa de `run()`.
- Ejecutar Lumen y alcanzar sus dos puertos: la API pública en el `8080` y la
  superficie de gestión (actuator, panel de administración, documentación de la
  API) en el `8081`.
- Leer el informe de arranque y confirmar el estado de salud y los metadatos de
  compilación de Lumen con `curl`.

## Conceptos que conocerás

Antes del primer comando, aquí tienes las tres ideas en las que se apoya este
capítulo. Cada una se reintroduce en contexto donde se usa por primera vez; esta
es la versión breve.

> **Note** **Término clave — facade crate.** Una *facade* es un único crate que
> reexporta toda una familia de crates (y sus macros) para que dependas de un
> solo nombre en lugar de muchos. Firefly distribuye todo su framework detrás de
> la fachada `firefly`. El equivalente en Spring es un *starter* de Spring Boot,
> salvo que aquí hay exactamente uno y lo cubre todo.

> **Note** **Término clave — bean.** Un *bean* es un objeto que el framework
> construye y gestiona por ti, y luego entrega a quien lo necesite. Tú declaras
> los beans; el framework los descubre en el arranque y los conecta entre sí.
> Esto es exactamente la noción de Spring de un bean gestionado por el contexto de
> la aplicación.

> **Note** **Término clave — actuator / superficie de gestión.** La *superficie
> de gestión* es un conjunto de endpoints HTTP operativos —comprobaciones de
> salud, información de compilación, métricas, introspección de configuración— que
> existen para operadores y herramientas, no para usuarios finales. Firefly los
> sirve en un puerto distinto del de tu API de negocio. Esto refleja Spring Boot
> Actuator.

## Paso 1 — Comprueba tu toolchain

Necesitas un toolchain estable y reciente de Rust, y nada más. La pila por
defecto de Lumen **no requiere infraestructura externa**: su almacén de eventos,
su event broker y su read model son todos Rust puro ejecutándose en proceso.

```bash
rustc --version   # 1.88 or later
cargo --version
```

> **Tip** **Punto de control.** Ambos comandos imprimen una versión. Si `rustc`
> reporta algo por debajo de 1.88, actualiza con `rustup update stable` antes de
> continuar.

Más adelante intercambiarás las piezas en proceso por infraestructura real
(Postgres, Kafka) en [Producción y despliegue](./20-production.md), pero nunca
antes de estar preparado: todo el libro se ejecuta contra los valores por defecto
en proceso.

## Paso 2 — Genera el andamiaje con la CLI `firefly` (Vía A)

La forma más rápida de llegar a un servicio en ejecución es la CLI de desarrollo.
Instálala una vez y luego pídele que genere el proyecto.

> **Note** **Término clave — archetype.** Un *archetype* es una plantilla de
> proyecto que decide la forma inicial de tu crate: qué módulos existen, qué
> características de Firefly están activadas y cómo es el código de ejemplo. La CLI
> distribuye varios (`core`, `web-api`, `web`, `hexagonal`, `library`, `cli`). El
> equivalente en Spring es un «tipo de proyecto» de Spring Initializr más sus
> dependencias preseleccionadas.

```bash
cargo install --path crates/cli      # from a checkout of the framework
# or, once published: cargo install firefly-cli

firefly new lumen --archetype web-api --features web,cqrs --git
cd lumen
cargo run
```

Qué acaba de pasar: `firefly new` escribió un crate de Cargo con un árbol `src/`,
un `firefly.yaml`, un `.gitignore`, un `README.md`, un `Dockerfile` y un
directorio `tests/`, y luego (por el `--git`) inicializó un repositorio Git con un
primer commit. El archetype `web-api` es la forma de partida adecuada para Lumen
—un servicio web con el bus de CQRS ya conectado— y `--features web,cqrs` activa
exactamente esos dos subsistemas. `cargo run` compila y arranca el servicio.

> **Note** **Término clave — CQRS.** *Command/Query Responsibility Segregation* es
> un patrón que enruta los **comandos** que modifican estado y las **consultas**
> de solo lectura a través de manejadores separados sobre un *bus* compartido.
> Construirás los manejadores de comandos y consultas de Lumen en capítulos
> posteriores; por ahora basta con que la característica `cqrs` reserve el
> cableado.

> **Tip** Ejecuta `firefly new --list` para imprimir cada archetype y feature
> flag, o `firefly new lumen --dry-run` para previsualizar el plan exacto de
> archivos sin escribir ni un solo archivo. Consulta [La CLI](./19-cli.md) para el
> catálogo completo del generador.

> **Tip** **Punto de control.** Tras `cargo run` deberías ver el banner de Firefly
> seguido de un informe de arranque con prefijo `::` y dos URLs (el panel de
> administración y la documentación de la API). Si has llegado hasta ahí, salta al
> [Paso 7](#step-7--run-it). Si quieres entender cada línea generada, haz en su
> lugar los Pasos 3 a 6 a mano.

## Paso 3 — Construye el crate a mano (Vía B)

La CLI es cómoda, pero el resto del libro se corresponde con `samples/lumen`
listado por listado, y la forma más segura de seguirlo es teclear el crate tú
mismo. Parte de un binario Cargo desnudo.

```bash
cargo new lumen
cd lumen
```

Qué acaba de pasar: `cargo new` creó un crate binario —un `Cargo.toml` y un
`src/main.rs` de relleno—. A lo largo de los tres pasos siguientes reemplazarás
ambos por el contenido real de Lumen.

> **Tip** **Punto de control.** `ls` muestra un `Cargo.toml` y un directorio
> `src/`. `cargo run` imprime `Hello, world!`. Ese relleno es el último código de
> este libro que Firefly *no* gestiona por ti.

## Paso 4 — Depende del único crate que es el framework

Abre `Cargo.toml`. Aquí es donde la historia de la dependencia única se vuelve
concreta. Todo el framework —CQRS, inyección de dependencias, la pila web
reactiva, event sourcing, orquestación de sagas, planificación, seguridad,
observabilidad— y *cada* macro `#[derive(...)]` / `#[...]` llegan a través de un
único crate.

```toml
# Cargo.toml
[dependencies]
# The one-dependency front door: the `firefly` facade re-exports the whole
# framework AND every macro. Generated code resolves runtime types through the
# facade, so Lumen never lists the underlying `firefly-*` crates. The `admin`
# feature pulls in the self-hosted admin dashboard the management port mounts.
firefly = { version = "26.6.28", features = ["admin"] }
```

Qué acaba de pasar: esa única línea es el framework entero. Cada capítulo
posterior añade *código*, no dependencias: no volverás a editar esta línea
`firefly`.

> **Design note.** Muchos frameworks te obligan a ensamblar una constelación de
> artefactos de starter o plugin y a mantener sus versiones alineadas a mano.
> Firefly colapsa todo eso en una sola línea `firefly`: no hay starter que olvidar
> ni desfase de versiones entre subsistemas como `firefly-web` y `firefly-cqrs`;
> cada crate `firefly-*` se distribuye como una única release versionada por
> calendario (aquí `26.6.28`), y tú dependes de la fachada.

Un servicio Firefly aún escribe directamente contra unos pocos crates del
ecosistema: `axum` (tú redactas los manejadores de los controladores), `serde` /
`serde_json` (tus mensajes y payloads de eventos son serializables), el runtime
asíncrono y los crates de id/reloj que usa el dominio. Añádelos, junto con la
feature flag que controla el endpoint de streaming:

```toml
# The ecosystem crates a Firefly service still uses directly.
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# The async runtime for `#[tokio::main]`, and the id/clock crates the domain
# uses for wallet ids and event timestamps.
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
async-trait = "0.1"

[features]
# The reactive streaming endpoint is feature-gated so the teaching baseline
# stays lean; the production chapter turns it on. It needs nothing beyond the
# `firefly` facade.
default = []
streaming = []
```

Qué acaba de pasar: declaraste el puñado de crates contra los que escribirás
código directamente, y una feature flag `streaming` que permanece desactivada por
defecto. Todo lo demás fluye a través de `firefly`.

> **Tip** **Punto de control.** Ejecuta `cargo build`. Descarga y compila el
> framework (la primera compilación es la lenta). Una compilación limpia aquí
> significa que la fachada y tus dependencias directas se resuelven todas
> correctamente.

## Paso 5 — Escribe el `main` de una sola línea

Un servicio Firefly tiene exactamente un punto de entrada: `main`. **No hay
composition root, ni `build_app`, ni una struct de aplicación** que ensamblar a
mano. Lumen es un crate de binario único, así que `src/main.rs` es la raíz del
crate: unas cuantas declaraciones `mod` y un `main` que entrega el servicio
completo al framework.

> **Note** **Término clave — composition root.** El *composition root* es el único
> lugar de un programa donde se ensambla el grafo de objetos: donde cada
> componente se construye y se conecta. En muchos frameworks escribes esto a mano.
> En Firefly el framework *es* el composition root: escanea tus beans y los
> conecta, de modo que nunca deletreas el grafo en una función.

Reemplaza el contenido de `src/main.rs` por la lista de módulos y el punto de
entrada:

```rust,ignore
// src/main.rs
#![allow(dead_code)]

mod commands;
mod compliance;
mod domain;
mod housekeeping;
mod ledger;
mod money;
mod security;
mod tcc_transfer;
mod transfer;
mod web;

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

Qué acaba de pasar, línea por línea:

- Las declaraciones `mod` nombran los módulos en los que Lumen irá creciendo. Se
  listan ahora para que `main.rs` no vuelva a cambiar nunca; irás rellenando cada
  uno a lo largo del libro. Hasta que exista el archivo de un módulo, esta lista no
  compilará, así que cuando lo sigas de verdad añades la línea `mod` en el mismo
  capítulo que añade el módulo. Para este inicio rápido, el único que necesitas es
  el que decidas conservar: lo importante es la forma de `main`.
- `#[tokio::main]` convierte `async fn main` en un `main` normal respaldado por el
  runtime de Tokio, que Firefly necesita porque toda la pila es asíncrona.
- `Result<(), firefly::BoxError>` es el tipo de retorno. `BoxError` es el tipo de
  error encajado de Firefly (`Box<dyn std::error::Error + Send + Sync>`);
  devolverlo te permite usar `?` en el arranque y hace que un fallo de arranque se
  manifieste como un código de salida distinto de cero.
- `firefly::FireflyApplication::new("lumen").run().await` es el servicio entero.
  `new("lumen")` nombra la aplicación (el nombre aparece en el banner y en
  `/actuator/info`); `.run().await` la arranca y la sirve.

> **Design note.** `FireflyApplication::new(name).run()` es el análogo en Rust de
> `SpringApplication.run(App.class, args)` de Spring Boot. Esa única llamada *es*
> el composition root: el framework ensambla el grafo de objetos a partir de los
> beans que escanea en lugar de que tú lo deletrees en una función. Nada es
> reflexivo ni oculto: el informe de arranque (Paso 7) registra exactamente qué se
> conectó, de modo que «qué está corriendo» se imprime línea por línea en el
> arranque.

Si quieres seguirlo con lo más pequeño que compile, elimina las líneas `mod` y
conserva solo la función `main` y el atributo `#![allow(dead_code)]`. La lista
completa de módulos de arriba es la forma real de Lumen que el resto del libro da
por supuesta.

> **Note** **Término clave — nombre y versión de la aplicación.** Lumen mantiene
> su nombre y su versión en dos constantes junto a su superficie HTTP, en
> `src/web.rs`. La versión procede del propio framework, de modo que sigue la
> release de la que dependes:
>
> ```rust,ignore
> // src/web.rs
> /// Lumen's application name (banner + `/actuator/info`).
> pub const APP_NAME: &str = "lumen";
>
> /// The released framework version, surfaced in the banner.
> pub const VERSION: &str = firefly::VERSION;
> ```

## Paso 6 — Entiende qué hace `run()`

`run()` es una línea en tu código y un pipeline de arranque completo por debajo:
el trabajo que un servicio solía cocinar a mano en un composition root. Conocer
las etapas rinde dividendos en cada capítulo posterior, porque cada capítulo añade
un bean que una de estas etapas descubre. En orden, `run()`:

- **Construye la pila web** — el renderizador de problemas RFC 9457, la propagación
  de correlation-id, la repetición de idempotencia, la caché en proceso, el bus de
  CQRS, el event broker, los registries de salud y métricas, el scheduler y las
  «pilas» web (CORS, cabeceras de seguridad, métricas de petición, el access log).
- **Hace component-scan del contenedor de DI** — autorregistra los beans de
  infraestructura del framework, y luego descubre y conecta cada bean de aplicación
  que declaraste: factorías `#[derive(Configuration)]` + `#[bean]`, controladores
  `#[derive(Controller)]` y campos `#[autowired]`. Cualquier factoría de bean
  `async fn` (un pool de BD, una conexión a un broker) se await aquí, de modo que
  los beans asíncronos están vivos antes de que algo los resuelva, y un error de
  construcción aborta el arranque (fail-fast).
- **Autoconfigura el bus de CQRS** — propagación de correlation siempre; el
  middleware de read-cache siempre que esté presente un bean `QueryCache`.
- **Autodescubre la seguridad** — los beans de DI `FilterChain` y `BearerLayer`
  (el `SecurityFilterChain` de Spring), superpuestos sobre la API sin necesidad de
  ninguna llamada `.security(...)`.
- **Automonta cada controlador** — cada `#[rest_controller]` se monta desde el
  contenedor con su estado resuelto automáticamente, y las rutas de cada bean
  `RouteContributor` se fusionan.
- **Drena los manejadores descubiertos** — los manejadores de comandos y consultas
  de CQRS registrados por inventario, los listeners de eventos de EDA y las tareas
  `#[scheduled]`, incluidas las declaradas como métodos de bean que autoconectan
  sus colaboradores.
- **Construye la documentación OpenAPI** a partir del inventario en vivo y aloja él
  mismo el panel de administración, ambos en el puerto de gestión, conectados a los
  componentes reales.
- **Imprime un informe de arranque al estilo Spring** — los perfiles activos, cada
  bean descubierto, la tabla de rutas montadas y los recuentos de
  manejadores/listeners/tareas programadas— y luego **sirve los puertos público +
  de gestión con apagado ordenado**.

Unas cuantas propiedades se repiten en cada capítulo, así que fíjate en ellas
ahora:

- **Sin trasiego en `main`.** A medida que Lumen adquiere un controlador, un bus de
  CQRS, un libro mayor basado en event sourcing y una cadena de seguridad, `main`
  nunca cambia: los nuevos beans se *descubren*, no se enhebran a través de un punto
  de entrada.
- **Dos puertos.** La API pública sirve en el `8080`; la superficie de gestión
  (`/actuator/*` más el panel `/admin` autoalojado más la documentación de la API)
  en el `8081` por defecto, de modo que los endpoints operativos nunca se filtran a
  la red pública.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 312" role="img"
     aria-label="Dual-port topology: the public API on port 8080 serves controllers, security and the RFC 9457 404 fallback; the management surface on port 8081 serves the actuator, the admin dashboard and the OpenAPI docs"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="24.0" y="18.5" width="248.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="16.0" width="248.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="148.0" y="33.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Public API  :8080</text><text x="148.0" y="47.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">client-facing</text>
<rect x="288.0" y="18.5" width="248.0" height="40.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="16.0" width="248.0" height="40.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="412.0" y="33.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Management  :8081</text><text x="412.0" y="47.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">operator-facing</text>
<rect x="24.0" y="80.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="78.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="148.0" y="101.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">#[rest_controller]</text><text x="148.0" y="115.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">your routes</text><rect x="24.0" y="150.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="148.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="148.0" y="171.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Security</text><text x="148.0" y="185.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">JWT · roles · sessions</text><rect x="24.0" y="220.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="218.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="148.0" y="241.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">RFC 9457 404</text><text x="148.0" y="255.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">problem+json fallback</text>
<rect x="288.0" y="80.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="78.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="412.0" y="101.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/actuator/*</text><text x="412.0" y="115.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">health · info · metrics</text><rect x="288.0" y="150.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="148.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="412.0" y="171.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/admin</text><text x="412.0" y="185.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">self-hosted dashboard</text><rect x="288.0" y="220.5" width="248.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="288.0" y="218.0" width="248.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="412.0" y="241.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/swagger-ui · /redoc</text><text x="412.0" y="255.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">/v3/api-docs</text>
<text x="280.0" y="300.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">FIREFLY_SERVER_ADDR  ·  FIREFLY_MANAGEMENT_ADDR  override the binds</text>
</svg>
<figcaption>Dos listeners, un proceso. La <strong>API pública</strong> (<code>:8080</code>) sirve tus controladores, la seguridad y el fallback <code>404</code> de RFC&nbsp;9457; la <strong>superficie de gestión</strong> (<code>:8081</code>) sirve el actuator, el panel <code>/admin</code> autoalojado y la documentación OpenAPI, de modo que los endpoints operativos nunca se filtran a la red pública.</figcaption>
</figure>
- **`FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`** sobrescriben las direcciones
  de bind desde el entorno (por defecto `0.0.0.0:8080` / `0.0.0.0:8081`). Ese es tu
  primer contacto con la historia de configuración tipada de
  [Configuración](./03-configuration.md).
- **El apagado ordenado viene de serie.** `run()` captura SIGINT/SIGTERM y drena
  las peticiones en vuelo antes de salir; una ejecución cancelada es un apagado
  limpio, no un error.

> **Note** **Costura de pruebas.** `bootstrap()` es el hermano de `run()`: ensambla
> la misma aplicación pero devuelve un valor `Bootstrapped` *sin servir*, de modo
> que las pruebas pueden manejar el router público totalmente conectado
> (`Bootstrapped::api_router`) en proceso sin ningún socket vinculado. Te apoyarás
> mucho en eso en [Tu primera API HTTP](./06-first-http-api.md) y
> [Pruebas](./18-testing.md).

## Paso 7 — Ejecútalo

```bash
cargo run
```

Verás el banner de Firefly (arte ASCII más la versión del framework, el nombre de
tu aplicación y el perfil activo), luego el informe de arranque línea por línea,
seguido de las URLs del panel de administración y de la documentación de la API:

```text
:: admin dashboard :: http://0.0.0.0:8081/admin/
:: api docs (management) :: swagger-ui http://0.0.0.0:8081/swagger-ui | redoc http://0.0.0.0:8081/redoc | spec http://0.0.0.0:8081/v3/api-docs
:: active profiles :: default
:: beans (…) ::
:: routes (…) ::
:: cqrs handlers: … | event listeners: … | scheduled tasks: … | controllers: … ::
:: openapi :: … operations | … component schemas (served at /v3/api-docs) ::
```

Qué acaba de pasar: el framework arrancó todo el pipeline del Paso 6 y ahora está
sirviendo ambos puertos. Las líneas `:: beans ::`, `:: routes ::` y los recuentos
son el inventario que el framework conectó: ahora mismo son pequeños porque Lumen
todavía no tiene lógica de negocio, y crecen a medida que añades capítulos.

> **Tip** **Punto de control.** El proceso permanece en ejecución y las últimas
> líneas muestran las dos URLs de arriba. Abre `http://localhost:8081/admin/` en un
> navegador para ver el panel autoalojado. Deja `cargo run` ejecutándose en este
> terminal y usa un segundo terminal para las comprobaciones con `curl` de abajo.

## Paso 8 — Confirma el estado de salud y los metadatos de compilación

Incluso sin rutas de negocio propias, el actuator está vivo en el puerto de
gestión. Desde un segundo terminal:

```bash
# Liveness / readiness — on the management port, never the public one.
curl localhost:8081/actuator/health
# {"status":"UP", ...}
```

Qué acaba de pasar: `/actuator/health` agrega cada indicador de salud que el
framework registró y reporta el `status` general. Con los valores por defecto en
proceso, todo está `"UP"`.

```bash
# Build metadata — the app name and version flow straight from
# `FireflyApplication::new("lumen")` and the framework version.
curl localhost:8081/actuator/info
# {"app":{"name":"lumen","version":"26.6.28"},"runtime":{...},"build":{...}}
```

Qué acaba de pasar: `/actuator/info` hace eco del nombre de aplicación que pasaste
a `new(...)` y de la versión, junto a los detalles de runtime y de compilación.
Cambia el nombre en `main` y este endpoint lo seguirá en la siguiente ejecución.

> **Tip** **Punto de control.** Ambos `curl` devuelven JSON: health reporta
> `"status":"UP"` e info reporta `"app":{"name":"lumen", ...}`. Si `curl` puede
> conectar pero a ninguna de las dos rutas, confirma que estás apuntando al `8081`
> (gestión), no al `8080` (público). El puerto público no tiene `/actuator/*`.

## Lo que obtuviste gratis

Sin escribir nada de ello tú mismo, Lumen ya tiene:

- **Respuestas de problema RFC 9457.** Cualquier error de un manejador se renderiza
  como `application/problem+json`, una ruta sin coincidencia devuelve un documento
  de problema 404 en condiciones (no un cuerpo en blanco) y un panic se captura y se
  renderiza como un problema 500. Usarás esto desde el primer endpoint del
  capítulo 6.
- **Correlation IDs.** Cada respuesta hace eco de un `X-Correlation-Id`; uno
  entrante se respeta y se mantiene en el ámbito de toda la petición.
- **Idempotencia.** Cada `POST`/`PUT`/`PATCH` que porte una cabecera
  `Idempotency-Key` se registra; repetir la petición reproduce la respuesta
  almacenada, y reutilizar la clave con un cuerpo distinto es un `409`.
- **Una superficie de gestión.** `/actuator/{health,info,metrics,env,beans,mappings,
  conditions,...}` (los informes `beans` / `mappings` / `conditions` reflejan la
  introspección de DI de Spring Boot Actuator) más un panel `/admin` autoalojado, en
  un listener aparte.
- **Documentación de la API autogenerada.** Swagger UI (`/swagger-ui`), ReDoc
  (`/redoc`) y la especificación OpenAPI 3.1 (`/v3/api-docs`) se sirven
  automáticamente en el puerto de **gestión** (junto al actuator y al admin, no en
  la API pública), con cero código de aplicación.
- **Apagado ordenado.** `run()` captura SIGINT/SIGTERM y drena las peticiones en
  vuelo.

> **Design note.** Salud, info y métricas en un puerto de gestión dedicado, un panel
> de administración autoalojado, documentación de la API autogenerada y middleware
> de petición de calidad de producción: todo levantado por una sola
> `FireflyApplication::new(...).run()`, sin ningún archivo de configuración que
> redactar primero y sin anotaciones que recordar. Esta es la superficie de actuator
> de Firefly, activada por defecto.

## Resumen — qué cambió en Lumen

| Antes | Después de este capítulo |
|--------|--------------------|
| directorio vacío | un crate que compila cuya única dependencia de Firefly es la fachada `firefly` |
| sin punto de entrada | un `main` de una sola línea sobre `FireflyApplication::new("lumen").run()` |
| nada que ejecutar | un actuator + admin vivo en el `:8081`, una API pública en el `:8080`, documentación autogenerada, apagado ordenado |
| — | constantes `APP_NAME` / `VERSION` que nombran el servicio y alimentan `/actuator/info` |

Ahora también sabes:

- Por qué un servicio Firefly depende de un solo crate —la fachada `firefly`— en
  lugar de muchos starters, y cómo eso evita el desfase de versiones.
- Que `run()` es un pipeline de arranque completo: construir la pila web, hacer
  component-scan del contenedor de DI, autoconfigurar CQRS, autodescubrir la
  seguridad, automontar controladores, drenar manejadores, autoalojar admin y
  documentación, y luego servir dos puertos.
- Que `bootstrap()` es la costura de pruebas que devuelve la aplicación conectada
  sin servir.

Lumen es ahora un servicio real y ejecutable que da la casualidad de que no tiene
lógica de negocio. Cada capítulo posterior rellena ese vacío, nunca reescribiendo
`main`, solo declarando más beans para que el framework los descubra.

## Ejercicios

1. **Mueve los puertos.** Arranca Lumen con `FIREFLY_SERVER_ADDR=127.0.0.1:9090
   FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091 cargo run`, luego
   `curl localhost:9091/actuator/health`. Confirma que las superficies pública y de
   gestión se movieron de forma independiente: esta es la costura sobre la que
   construye [Configuración](./03-configuration.md).
2. **Lee tus propios metadatos.** Ejecuta `curl localhost:8081/actuator/info` y
   encuentra los valores `app.name` / `app.version`. Cambia el nombre pasado a
   `FireflyApplication::new(...)`, vuelve a ejecutar y observa cómo el banner y
   `/actuator/info` lo siguen ambos.
3. **Lee el informe de arranque.** Ejecuta Lumen y lee el log de arranque línea por
   línea: los perfiles activos, los beans descubiertos, las rutas automontadas y los
   recuentos de manejadores/listeners/tareas programadas. Este es el inventario que
   el framework conectó: fíjate en lo corto que es hoy, y luego vuelve a visitarlo
   tras un capítulo posterior.
4. **Provoca un apagado ordenado.** Ejecuta Lumen y luego pulsa `Ctrl-C`. Observa
   que el proceso sale limpiamente sin ningún stack trace: `run()` trató la señal
   como un apagado, no como un fallo.
5. **Previsualiza el andamiaje.** Aunque hayas tomado la Vía B, ejecuta
   `firefly new lumen2 --archetype web-api --features web,cqrs --dry-run` y compara
   el plan generado con el `Cargo.toml` y el `main.rs` que escribiste a mano.

## Adónde ir después

- Añade configuración tipada, en capas y consciente de perfiles en
  **[Configuración](./03-configuration.md)**, y reemplaza esos overrides crudos de
  variables de entorno `FIREFLY_*` por propiedades reales.
- Aprende cómo el framework conecta el grafo de objetos que escanea en
  **[Cableado de dependencias](./04-dependency-wiring.md)**.
- Dale a Lumen sus primeros endpoints reales en
  **[Tu primera API HTTP](./06-first-http-api.md)**.
