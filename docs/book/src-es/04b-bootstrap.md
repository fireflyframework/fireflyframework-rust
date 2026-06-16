# Arranque con FireflyApplication

El `main` de Lumen es una sola línea, y esa línea es todo el servicio. En el
[Inicio rápido](./02-quickstart.md) lo ejecutaste y viste un banner, un informe
de arranque y dos puertos en vivo, pero diste por bueno `run()` sin más. Este
capítulo levanta la tapa. Aquí no se *añade* nada nuevo a Lumen; en su lugar
aprenderás exactamente qué hace `FireflyApplication::new("lumen").run().await`
entre el momento en que pulsas Intro y el momento en que los dos servidores
aceptan conexiones. Conocer el pipeline rinde frutos en cada capítulo posterior,
porque cada uno declara un bean, un controlador, un handler, un listener o una
tarea programada que *una de estas etapas* descubre y conecta por ti.

Al terminar este capítulo, serás capaz de:

- Explicar la diferencia entre `new`, `run` y `bootstrap`, y saber cuál debería
  llamar tu código de pruebas.
- Recorrer el pipeline de arranque de doce etapas que ejecuta `bootstrap()`, y
  nombrar lo que descubre cada etapa: el stack web, el escaneo de DI, la
  autoconfiguración de CQRS, el descubrimiento de seguridad, el automontaje de
  controladores, el vaciado de handlers, OpenAPI y el router de gestión.
- Usar las palancas del builder de `FireflyApplication` (`version`, `configure`,
  `security`, `on_ready`, `extra_routes`, los overrides de dirección) y saber
  cuándo se prefiere la vía del *bean declarativo* sobre la palanca imperativa.
- Sobrescribir las direcciones de enlace pública y de gestión desde el entorno
  con `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`.
- Leer el informe de arranque línea a línea y usarlo como comprobación de
  cordura sobre lo que conectó el framework.
- Entender el apagado ordenado y el 404 RFC 9457 por defecto que obtienes gratis.

## Conceptos que conocerás

Antes de recorrer el pipeline, aquí están las ideas en las que se apoya este
capítulo. Cada una se reintroduce en contexto allí donde se usa por primera vez;
esta es la versión corta.

> **Note** **Término clave — bootstrap.** El *arranque* (bootstrapping) es el
> acto único de ensamblar una aplicación en ejecución a partir de sus
> declaraciones: construir la infraestructura, descubrir componentes,
> conectarlos entre sí y producir algo servible. En Firefly, todo el arranque es
> el cuerpo de `FireflyApplication::bootstrap()`. El análogo en Spring es todo lo
> que `SpringApplication.run(...)` hace antes de que el servidor embebido empiece
> a aceptar peticiones.

> **Note** **Término clave — raíz de composición.** La *raíz de composición*
> (composition root) es el único lugar de un programa donde se ensambla el grafo
> de objetos, donde se construye y conecta cada componente. Muchos frameworks te
> obligan a escribirla a mano. En Firefly el framework *es* la raíz de
> composición: escanea tus beans y los conecta, así que nunca deletreas el grafo
> en una función. Por eso Lumen no tiene `build_app`, ni router escrito a mano,
> ni ningún punto de llamada `register_*`.

> **Note** **Término clave — inventario.** El *inventario* (inventory) es un
> conjunto de registros de tiempo de enlace que las macros de Firefly rellenan en
> tiempo de compilación. Cuando escribes `#[command_handler]`, `#[event_listener]`,
> `#[scheduled]` o `#[rest_controller]`, la macro registra el elemento en una
> tabla global que el framework *vacía* en el arranque. No hay reflexión ni
> escaneo del sistema de archivos en tiempo de ejecución: las declaraciones
> mismas *son* el registro. Así es como `main` nunca cambia a medida que Lumen
> crece.

> **Note** **Término clave — superficie de gestión.** La *superficie de gestión*
> (management surface) es el conjunto de endpoints HTTP operativos —salud,
> información, métricas, introspección de configuración— más el panel de
> administración autoalojado y la documentación de la API. Firefly los sirve en
> un puerto separado (`8081` por defecto) de tu API de negocio (`8080`), de modo
> que los endpoints operativos nunca se filtran a la red pública. Esto refleja
> Spring Boot Actuator.

## Paso 1 — Mira la única línea que estás a punto de descifrar

El `main` de Lumen es la misma línea que escribiste en el inicio rápido, que vive
en `src/main.rs` junto a las declaraciones `mod` del crate:

```rust,ignore
// src/main.rs
#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
```

Lo que acaba de ocurrir: no hay `build_app`, ni router escrito a mano, ni ningún
punto de llamada `register_*` / `subscribe_*` / `schedule_*` en ninguna parte de
Lumen. `main` solo nombra la aplicación y entrega el control al framework. Todo
lo demás —los beans de [Inyección de dependencias](./04a-dependency-injection.md),
los controladores de [Tu primera API HTTP](./06-first-http-api.md), los handlers
de [CQRS](./09-cqrs.md), los listeners de
[Arquitectura dirigida por eventos](./10-eda-messaging.md) y las tareas
programadas de [Planificación y notificaciones](./16-scheduling-notifications.md)—
lo descubre el framework a partir de declaraciones que se sitúan junto al código.

> **Note** **Término clave — `BoxError`.** `firefly::BoxError` es el tipo de error
> en caja del framework, `Box<dyn std::error::Error + Send + Sync>`. Devolverlo
> desde `main` te permite usar `?` sobre el arranque y hace que cualquier fallo de
> arranque aflore como una salida del proceso distinta de cero. Se reexporta desde
> la fachada `firefly`, así que nunca nombras el crate subyacente.

> **Design note.** `FireflyApplication::new(name).run()` es el análogo en Rust de
> `SpringApplication.run(App.class, args)` de Spring Boot y de
> `FireflyApplication("lumen").run()` de pyfly. Esa única llamada *es* la raíz de
> composición. Nada es reflexivo ni está oculto: el informe de arranque (Paso 10)
> registra exactamente qué se conectó, de modo que "qué está corriendo" se imprime
> línea a línea en el arranque.

> **Tip** **Punto de control.** Abre `samples/lumen/src/main.rs` (o el `main.rs`
> de tu propio crate). Confirma que `main` es una sola sentencia:
> `new("lumen").run().await`. Si ves un `build_app`, un router o cualquier llamada
> `register_*`, estás leyendo una forma más antigua: el framework actual conecta
> todo eso por ti.

## Paso 2 — Distingue entre `new`, `run` y `bootstrap`

Tres métodos gobiernan el ciclo de vida, y elegir el correcto marca la diferencia
entre un servidor de producción y una prueba rápida en proceso.

> **Note** **Término clave — `bootstrap` frente a `run`.** `bootstrap()` ensambla
> la aplicación entera —cada etapa del Paso 4— y devuelve un valor `Bootstrapped`
> **sin enlazar un socket ni servir**. `run()` llama a `bootstrap()` y luego a
> `serve()`. Así que ambas vías ensamblan la *misma* aplicación; solo difiere el
> último movimiento (enlace + servir).

- **`FireflyApplication::new(name)`** construye el builder. Lee las direcciones de
  enlace por defecto del entorno y siembra el nombre de la aplicación. Todavía no
  pasa nada: ni escaneo, ni servidor.
- **`.run().await`** arranca y sirve hasta que el proceso recibe
  `SIGINT`/`SIGTERM`. Esto es lo que llama `main`.
- **`.bootstrap().await`** hace todo lo que hace `run` *salvo* servir, y devuelve
  un `Bootstrapped` cuyo `api_router` puedes manejar en proceso. Esta es la
  costura para pruebas.

Las pruebas HTTP de Lumen usan exactamente la vía `bootstrap`. Aquí está el helper
real de `src/web.rs` que llaman los módulos de prueba:

```rust,ignore
// src/web.rs — the testable in-process router, no socket bound
#[cfg(test)]
pub(crate) async fn build_router() -> axum::Router {
    firefly::FireflyApplication::new(APP_NAME)
        .version(VERSION)
        .bootstrap()
        .await
        .expect("lumen bootstrap")
        .api_router
}
```

Lo que acaba de ocurrir: `bootstrap()` devuelve un `Bootstrapped`, y `.api_router`
es su router público completamente ensamblado, con controladores, middleware y
seguridad aplicados. Una prueba maneja después ese router con
`tower::ServiceExt::oneshot`, enviando una petición directamente al router sin
socket TCP de por medio. Como la vía de producción (`run`) y la vía de prueba
(`bootstrap` → `oneshot`) ensamblan la **misma** aplicación, las pruebas de
[Pruebas](./18-testing.md) ejercitan exactamente el cableado que sirve `main`.

> **Note** Observa que este helper llama a `.version(VERSION)` mientras que `main`
> no lo hace. La versión es puramente cosmética —aparece en el banner y en
> `/actuator/info`—, así que `main` puede omitirla y dejar que tome su valor por
> defecto. La prueba la fija explícitamente solo para que las aserciones sobre
> `/actuator/info` sean estables.

> **Tip** **Punto de control.** Ya puedes responder: *¿qué método llama una
> prueba, y por qué?* Una prueba llama a `bootstrap()` porque quiere el router
> conectado sin enlazar un puerto; `main` llama a `run()` porque quiere servir.

## Paso 3 — Conoce el valor `Bootstrapped`

`bootstrap()` devuelve una struct `Bootstrapped`. Rara vez construirás una tú
mismo, pero conocer sus campos desmitifica qué significa "ensamblada". La forma
real del framework:

```rust,ignore
pub struct Bootstrapped {
    /// The web stack (kept so `serve` can run the lifecycle).
    pub web: WebStack,
    /// The scanned DI container.
    pub container: Arc<Container>,
    /// The fully-assembled public API router (controllers + middleware + security).
    pub api_router: Router,
    /// The management router (`/actuator/*` + the self-hosted `/admin` dashboard).
    pub management_router: Router,
    /// The task scheduler (started by `serve`).
    pub scheduler: Arc<Scheduler>,
    /// The public bind address.
    pub api_addr: String,
    /// The management bind address.
    pub management_addr: String,
}
```

Lo que acaba de ocurrir: un `Bootstrapped` lleva ambos routers (público + de
gestión), el contenedor de DI escaneado, el scheduler que aún no ha arrancado y
las dos direcciones a las que enlazar. El único trabajo que le queda a `run()` es
llamar a `serve()` sobre este valor, que arranca el scheduler y enlaza ambos
routers. Una prueba ignora todo salvo `api_router`.

> **Note** **Término clave — contenedor de DI.** El *contenedor* (container) es el
> registro que guarda cada bean que construyó el framework, indexado por tipo, de
> modo que cualquier componente puede pedir un colaborador por tipo y obtener la
> instancia gestionada. Es la mitad de tiempo de ejecución de la inyección de
> dependencias que conociste en
> [Inyección de dependencias](./04a-dependency-injection.md).
> `Bootstrapped.container` es ese registro, completamente escaneado.

## Paso 4 — Recorre el pipeline de arranque, etapa por etapa

Este es el corazón del capítulo. `bootstrap()` ejecuta un pipeline fijo, y cada
etapa se vincula a algo que el framework *descubre y conecta* por ti. Léelo una
vez de arriba abajo; volverás a etapas individuales a medida que los capítulos
posteriores añadan los beans que cada etapa encuentra. La numeración de abajo
sigue el código fuente del framework (`crates/firefly/src/application.rs`).

**1. Construir el stack web.** `WebStack::new(config)` levanta axum, el `Bus` de
CQRS, el `Broker` de EDA, el `Scheduler`, el registro de métricas, el compuesto de
salud y el middleware por defecto (id de correlación, métricas de petición,
idempotencia, CORS, cabeceras de seguridad). La seguridad *no* se aplica aquí
—viene de un bean tras el escaneo—, así que el stack se construye en bruto y
mutable.

> **Note** **Término clave — bus / broker / scheduler.** El *bus* enruta los
> comandos y consultas de CQRS (Command/Query Responsibility Segregation) hacia
> sus handlers; el *broker* entrega los eventos a los listeners; el *scheduler*
> ejecuta las tareas `#[scheduled]` con un temporizador. Los tres son beans de
> infraestructura del framework, construidos aquí y registrados en el contenedor en
> la etapa 3 para que tu código pueda autoconectarlos (autowire).

**2. Inicializar el logging.** Se instala el subscriber de logging estructurado.
Cuando la feature `admin` está activa, los logs también se derivan al búfer de
captura en memoria del panel de administración, de modo que `/admin` puede mostrar
una cola de logs en vivo.

**3. Escaneo de componentes del contenedor.** El framework primero
**autorregistra sus propios beans de infraestructura**
(`web.register_beans(&container)`: el `Bus`, el `Broker`, el `Scheduler`, los
registros) y luego `container.scan()` descubre, registra y autoconecta **tus**
beans: cada
`#[derive(Component/Service/Repository/Configuration/Controller)]` y cada factoría
`#[bean]` enlazada al binario. Inmediatamente después del escaneo síncrono,
`container.init_async_beans().await` espera a cada factoría `#[bean]` `async fn`
(un pool de BD, una conexión al broker) para que los beans asíncronos estén vivos
antes de que cualquier cosa los resuelva, y un error de construcción aborta el
arranque (fail-fast). En el caso de Lumen, ese escaneo encuentra el
`#[derive(Configuration)]` de `LumenBeans` y sus factorías `#[bean]` (el event
store, la caché de consultas, el servicio JWT, la `FilterChain`, el `BearerLayer`,
el ledger) más el bean del controlador `WalletApi`. Esta es la DI de tiempo de
enlace de [Inyección de dependencias](./04a-dependency-injection.md).

**4. Autoconfigurar el bus de CQRS.** La propagación de la correlación siempre se
añade como capa (`bus.use_middleware(CorrelationMiddleware::new())`). Si hay un
bean `QueryCache` presente en el contenedor, su middleware de caché de lectura
también se añade como capa, de modo que la caché de 30 segundos de `GetWallet` en
Lumen ([Caché](./17-caching.md)) se conecta sin código de aplicación, solo por
*declarar el bean `QueryCache`*. El middleware de validación ya está instalado por
el core.

**5. Ejecutar el hook de preparación opcional.** La mayoría de las aplicaciones
—Lumen incluida— no necesitan ninguno; los beans y el pipeline cubren el cableado.
`on_ready` existe para el caso raro que quiere los colaboradores en vivo
(contenedor, bus, broker, scheduler) después del escaneo pero antes de servir. Lo
cubrimos en el Paso 5, más abajo.

**6. Autodescubrir la seguridad.** Sin una llamada explícita a `.security(...)`,
el framework resuelve el bean `FilterChain` (RBAC basado en rutas) y el bean
`BearerLayer` (extracción de token) desde el contenedor y los aplica: el bean
`SecurityFilterChain` de Spring, descubierto. Lumen declara ambos como `#[bean]`
en `LumenBeans`, de modo que las rutas protegidas de [Seguridad](./14-security.md)
se activan automáticamente. (Si hay un bean `ExceptionHandlerRegistry` presente, se
instala como la capa de advice más externa: el análogo de `@ControllerAdvice`).

**7. Automontar las rutas.** `mount_controllers(&container)` resuelve cada
`#[rest_controller]` y construye su router a partir del bean de estado
autoconectado del controlador; `mount_route_contributors(&container)` fusiona cada
bean `RouteContributor`. Así es como se añade el endpoint de streaming
condicionado por feature de Lumen: declarando un bean, no editando una raíz de
composición. Resolver los controladores aquí también construye sus colaboradores,
incluido el `#[bean]` `ledger`.

> **Note** **Término clave — `RouteContributor`.** Un `RouteContributor` es un
> bean que aporta rutas axum en bruto que el framework fusiona en el router
> público. Es la vía de escape para endpoints que no encajan en la forma de
> `#[rest_controller]`, como el flujo reactivo de eventos de Lumen. Lo declaras
> como un bean (`#[firefly(provides = "dyn firefly::web::RouteContributor")]`) y el
> framework lo encuentra; sigue sin haber una raíz de composición que editar.

**8. Vaciar el inventario.** El framework vacía los registros de tiempo de enlace
que las macros rellenaron en tiempo de compilación.
`register_discovered_handlers(&bus)` más
`register_discovered_handler_beans(&bus, &container)` instalan cada
`#[command_handler]` / `#[query_handler]`;
`subscribe_discovered_listeners(broker)` más la variante de bean suscriben cada
`#[event_listener]`; y `register_discovered_scheduled(&scheduler)` más la variante
de bean programan cada tarea `#[scheduled]`. No hay puntos de llamada
`register(&bus)` / `subscribe(&broker)`: las declaraciones *son* el registro.

**9. Aplicar la cadena de middleware.** El middleware descubierto se aplica sobre
las rutas montadas, se añade la capa de autenticación bearer, se fija el fallback
404 por defecto y `web.apply_middleware(...)` envuelve todo el router en el borde
de observabilidad heredado: idempotencia, el log de acceso, métricas de petición,
correlación, trazas W3C, cabeceras de seguridad, renderizado de problemas, CORS y
el advice global de excepciones. Con la feature `admin`, una capa de trazas más
externa **origina y reenvía** `traceparent` para que cada petición sea
correlacionable entre servicios.

**10. Servir la documentación OpenAPI.** La especificación se construye a partir
del **inventario en vivo** —cada ruta `#[rest_controller]` más cada DTO
`#[derive(Schema)]`— y se sirve en `/v3/api-docs` (más `/openapi.json`), con
Swagger UI en `/swagger-ui` y ReDoc en `/redoc`. Estas se montan en el router de
**gestión** (junto a actuator y admin), *no* en la API pública, ya que exponen
toda la superficie de la API. Esto se conecta automáticamente sin código de
aplicación; [OpenAPI, Swagger UI y ReDoc](./06a-openapi.md) lo cubre por completo.

> **Note** La especificación OpenAPI anuncia la URL base de la API *pública* como
> su `server` aunque la documentación se sirva en el puerto de gestión, de modo
> que el "Try it out" de Swagger UI envía las peticiones al `8080`, no al origen
> `8081` desde el que se cargó. `FIREFLY_OPENAPI_SERVER_URL` sobrescribe esa URL
> base (por ejemplo, una URL pública detrás de un proxy inverso).

**11. Instalar el 404 por defecto.** Una ruta no coincidente obtiene un 404 RFC
9457 `application/problem+json` en condiciones, en lugar del cuerpo vacío y desnudo
de axum (véase [Paso 8](#step-8--understand-the-default-404)).

**12. Construir el router de gestión.** Los endpoints de actuator
(`/actuator/health|info|metrics|loggers|mappings|beans|conditions|env`) se
ensamblan y —con la feature `admin`— el **panel de administración autoalojado** se
monta en `/admin/`, conectado a los componentes en vivo (salud, métricas, el bus,
el scheduler, el contenedor, la instantánea del entorno, el búfer de trazas, el
búfer de logs). El router de documentación OpenAPI de la etapa 10 se fusiona aquí,
y se fija un único fallback 404 RFC 9457 para toda la superficie de gestión.
[Observabilidad](./15-observability.md) cubre la superficie de administración en
profundidad.

`bootstrap()` devuelve el `Bootstrapped` ensamblado; `run()` llama después a
`serve()`.

> **Tip** **Punto de control.** Sin releer, nombra qué etapa descubre un handler
> de comando de CQRS (etapa 8: vaciar el inventario), un controlador (etapa 7:
> automontaje), una cadena de filtros de seguridad (etapa 6: descubrimiento de
> seguridad) y la caché de lectura de `GetWallet` (etapa 4: autoconfiguración de
> CQRS, porque hay un bean `QueryCache` presente). Si sabes ubicar cada una,
> entiendes por qué `main` nunca cambia a medida que Lumen crece.

## Paso 5 — Recurre a una palanca del builder (solo cuando un bean no sirva)

`FireflyApplication` es un builder, y cada palanca es opcional. Lumen usa solo
`new` (en `main`) y `version` (en `build_router`). Aquí está el conjunto completo,
extraído del código fuente del framework para que las firmas sean exactas:

| Método | Qué hace |
|--------|--------------|
| `new(name)` | Nombra la aplicación (banner + `/actuator/info`). Toma los enlaces por defecto de `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`. |
| `version(v)` | Fija la versión (banner + `/actuator/info`). |
| `configure(\|cfg\| { … })` | Ajusta el `CoreConfig` in situ: CORS, cabeceras de seguridad, idempotencia, las palancas de [Configuración](./03-configuration.md). |
| `security(chain, bearer)` | Instala una `FilterChain` + `BearerLayer` **explícitamente**, en lugar de descubrirlos desde beans. |
| `on_ready(\|ctx\| async { … })` | Un hook de preparación sobre los `container` / `bus` / `broker` / `scheduler` en vivo, ejecutado tras el escaneo y antes de servir. |
| `extra_routes(\|container\| router)` | Fusiona rutas extra que no son `#[rest_controller]`, construidas a partir del contenedor escaneado. |
| `info_contributor(c)` | Añade un contribuidor a `/actuator/info`. |
| `api_addr(addr)` | Sobrescribe la dirección de enlace de la API pública. |
| `management_addr(addr)` | Sobrescribe la dirección de enlace de gestión (actuator + admin). |
| `bootstrap()` | Ensambla la aplicación **sin servir** (pruebas). |
| `run()` | Arranca y sirve. |

Las palancas son encadenables. Un hipotético servicio `orders` que quiera un toque
de cableado imperativo podría escribir:

```rust,ignore
// the knobs are chainable; Lumen needs almost none of them
firefly::FireflyApplication::new("orders")
    .version("1.0.0")
    .configure(|cfg| { /* tune the CoreConfig: CORS, security headers, … */ })
    .management_addr("127.0.0.1:9091")
    .run()
    .await
```

Lo que acaba de ocurrir: cada palanca devuelve `Self`, así que encadenas tantas
como necesites y terminas con `run()` (o `bootstrap()`). La mayoría de los
servicios terminan con una cadena mucho más corta que esta; la de Lumen es la
cadena vacía, `new("lumen").run()`.

> **Design note.** Lumen declara la seguridad como `#[bean]` en lugar de llamar a
> `.security(...)`, declara su endpoint de streaming como un bean
> `RouteContributor` en lugar de llamar a `.extra_routes(...)`, y siembra su
> proyección dentro de los beans de `ledger`/proyección en lugar de en
> `.on_ready(...)`. Las palancas explícitas del builder existen para aplicaciones
> que prefieren un toque de cableado imperativo; la vía del *bean* es la senda
> preferida del framework, plenamente declarativa: declaración junto al código,
> descubierta en el arranque. Prefiere un bean; recurre a una palanca solo cuando
> ninguna forma de bean encaje.

> **Tip** **Punto de control.** Ya puedes justificar por qué el `main` de Lumen no
> tiene ninguna palanca del builder: todo lo que una palanca podría hacer, Lumen lo
> hace con un bean que el escaneo encuentra.

## Paso 6 — Sobrescribe las direcciones de enlace desde el entorno

Por defecto, la API pública enlaza `0.0.0.0:8080` y la superficie de gestión
enlaza `0.0.0.0:8081`. Puedes mover cualquiera de las dos sin tocar código, porque
`new` lee dos variables de entorno en el momento de la construcción:

```bash
FIREFLY_SERVER_ADDR=127.0.0.1:9090 \
FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091 \
cargo run --bin lumen
```

Lo que acaba de ocurrir: `new` leyó `FIREFLY_SERVER_ADDR` para el enlace público y
`FIREFLY_MANAGEMENT_ADDR` para el enlace de gestión, recurriendo cada uno a su
valor por defecto `0.0.0.0:808x` cuando no está definido. Las dos superficies se
mueven de forma *independiente*, prueba de que son listeners genuinamente
separados, no un único servidor con un prefijo de ruta. Si prefieres fijar las
direcciones en código, `api_addr(...)` / `management_addr(...)` sobrescriben el
entorno.

> **Tip** **Punto de control.** Arranca Lumen con los dos overrides de arriba y
> luego, en una segunda terminal, ejecuta `curl localhost:9091/actuator/health`. Un
> `{"status":"UP"}` desde `:9091` (y nada en `/actuator/*` de `:9090`) confirma que
> la superficie de gestión se movió por su cuenta. Este es el primer aperitivo de
> la historia de configuración tipada de [Configuración](./03-configuration.md).

## Paso 7 — Lee el informe de arranque

Justo antes de servir, `serve()` imprime el banner, las URLs de documentación y
luego `log_startup_report(&container)`: un **informe línea a línea** al estilo de
Spring Boot/pyfly, de modo que un log de arranque se lee como la consola de Spring
Boot. El formato es:

```text
:: active profiles :: default
:: beans (N) ::
     [stereotype   ] name                   scope      (TypeName)
     …one line per scanned bean, sorted by stereotype then name…
:: routes (N) ::
     METHOD path                             -> Controller::handler
     …one line per auto-mounted route, sorted by (path, method)…
:: cqrs handlers: H | event listeners: L | scheduled tasks: S | controllers: C ::
:: openapi :: N operations | K component schemas (served at /v3/api-docs) ::
```

Leyéndolo de arriba abajo:

- **`:: active profiles ::`** — los perfiles de configuración activos (`default`
  cuando no se fija ninguno).
- **`:: beans (N) ::`** — cada bean que escaneó el contenedor, uno por línea: el
  `[stereotype]` (`service`, `repository`, `controller`, `configuration`,
  `component` o `bean`), el nombre del bean, su scope y su nombre de tipo corto.
  Esta es la misma tabla que renderiza la vista `/beans` del panel de
  administración.
- **`:: routes (N) ::`** — la tabla de rutas automontadas: cada ruta
  `#[rest_controller]` como `METHOD path -> Controller::handler`. Se extrae del
  mismo registro `firefly_container::routes()` que alimenta `/admin/api/mappings` y
  el documento OpenAPI, así que los tres nunca divergen.
- **`:: cqrs handlers … ::`** — los *recuentos* vaciados del inventario: cuántos
  `#[command_handler]`/`#[query_handler]`, `#[event_listener]`, `#[scheduled]` y
  controladores se descubrieron (cada recuento suma los registros de `fn` libre y
  los de bean).
- **`:: openapi ::`** — el recuento de operaciones (una por ruta) y el recuento de
  esquemas de componentes (uno por DTO `#[derive(Schema)]`), confirmando que la
  especificación está viva.

Lo que acaba de ocurrir: nada de tu aplicación imprimió esto; lo hizo el framework,
a partir del contenedor y el inventario en vivo. Los números son una comprobación
de cordura rápida: si esperabas cuatro handlers y el informe dice tres, falta un
`#[command_handler]` o su crate no está enlazado.

> **Tip** **Punto de control.** Ejecuta `cargo run` y lee el informe. Fíjate en lo
> cortas que son hoy las líneas de `beans`, `routes` y recuentos —Lumen aún tiene
> poca lógica de negocio— y luego vuelve a este informe después de
> [CQRS](./09-cqrs.md) y observa cómo crecen los números sin una sola edición en
> `main`.

## Paso 8 — Entiende el 404 por defecto

Como el framework instala un fallback en ambos routers (etapas 11 y 12), una ruta
no coincidente devuelve un 404 RFC 9457 `application/problem+json` en condiciones
—el mismo sobre de `type`/`title`/`status` y el mismo tipo de contenido
`application/problem+json` que cualquier otro error del framework— en lugar del
cuerpo vacío y desnudo de axum (que un navegador ofrecería descargar como un
archivo en blanco):

```text
GET /api/v1/nope

HTTP/1.1 404 Not Found
content-type: application/problem+json
{ "type": "...", "title": "Not Found", "status": 404,
  "detail": "No route matches GET /api/v1/nope" }
```

Lo que acaba de ocurrir: el fallback se conecta *dentro* del borde de
observabilidad, así que incluso un 404 de ruta no coincidente se registra, se traza
y se correlaciona; no hay laguna de observabilidad para "la ruta que no existía".
Este es el mismo renderizado de problemas que encuentras para los errores de
handler en [Tu primera API HTTP](./06-first-http-api.md) y para los errores de
seguridad en [Seguridad](./14-security.md): errores uniformes, de extremo a
extremo, sin trabajo por ruta.

> **Note** **Término clave — RFC 9457.** El RFC 9457 (que deja obsoleto al RFC
> 7807) define el tipo de medio `application/problem+json`: un sobre de error
> pequeño y legible por máquina con los campos `type`, `title`, `status` y
> `detail`. Firefly renderiza *todos* los errores —fallos de handler, validación,
> seguridad y rutas no coincidentes— a través de esta única forma, de modo que un
> cliente parsea los errores exactamente igual sin importar de dónde provengan.

## Paso 9 — Entiende el apagado ordenado

`serve()` arranca el scheduler en una tarea en segundo plano y luego sirve la API
pública en `api_addr` y la superficie de gestión en `management_addr` a través del
ciclo de vida del framework, cada una envuelta con `with_graceful_shutdown`. Ante
`SIGINT`/`SIGTERM`, ambos servidores dejan de aceptar nuevas conexiones, permiten
que las peticiones en vuelo terminen y `run()` devuelve `Ok(())`. Una parada
disparada por señal se trata como un *apagado limpio, no como un error*: el caso de
error de cancelación del ciclo de vida se mapea a `Ok(())`.

Lo que acaba de ocurrir: nunca escribiste un manejador de señales. El framework
atrapa la señal, drena ambos puertos y devuelve éxito, de modo que un `Ctrl-C` en
tu terminal sale sin traza de pila y con código de salida cero. Ese es el
comportamiento en el que confía un orquestador de contenedores (Kubernetes enviando
`SIGTERM`) para un reinicio progresivo.

> **Tip** **Punto de control.** Ejecuta Lumen y luego pulsa `Ctrl-C`. El proceso
> sale limpiamente, sin panic y sin traza de pila. Si viste un error, estás en una
> build más antigua: el `serve()` actual mapea la cancelación a `Ok(())`.

## Resumen

Este capítulo no añadió código a Lumen: descifró la línea que ha estado en
`main.rs` desde el inicio rápido. Ahora sabes:

- **`new` / `run` / `bootstrap`.** `new` construye el builder; `run` arranca y
  sirve; `bootstrap` ensambla la aplicación idéntica *sin* servir y devuelve un
  `Bootstrapped` cuyo `api_router` manejan tus pruebas en proceso.
- **El pipeline de doce etapas.** Construir el stack web, inicializar el logging,
  escanear los componentes del contenedor (esperando a los beans asíncronos),
  autoconfigurar el bus de CQRS, ejecutar el hook de preparación opcional,
  autodescubrir la seguridad, automontar controladores y route contributors, vaciar
  el inventario (handlers / listeners / tareas programadas), aplicar la cadena de
  middleware, servir OpenAPI en el puerto de gestión, instalar el 404 por defecto y
  construir el router de gestión con actuator + admin.
- **Palancas del builder frente a beans.** `version`, `configure`, `security`,
  `on_ready`, `extra_routes`, `info_contributor` y los overrides de dirección
  existen para el cableado imperativo, pero Lumen prefiere el bean declarativo para
  cada uno de ellos, así que su `main` es la cadena vacía.
- **Los valores operativos por defecto.** Dos puertos independientes
  sobrescribibles por `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`, un informe
  de arranque línea a línea, un 404 RFC 9457 para las rutas no coincidentes y un
  apagado ordenado ante SIGINT/SIGTERM, todo gratis.

`FireflyApplication` es la columna vertebral de la que cuelga el resto del libro.
Cada capítulo que declara un bean, un controlador, un handler, un listener o una
tarea programada está contribuyendo al pipeline de arriba, y nunca reescribiendo
`main`, solo dándole al framework una cosa más que descubrir.

## Ejercicios

1. **Rastrea una etapa hasta un capítulo.** Para cada una de las etapas 4, 6, 7 y
   8, nombra el capítulo posterior que añade el bean o la declaración que esa etapa
   descubre, y la única línea de código de Lumen que la hace activarse. (Pista: la
   etapa 4 es el bean `QueryCache` de [Caché](./17-caching.md)).
2. **Maneja la costura de pruebas.** En `samples/lumen`, lee `src/http_test.rs` y
   encuentra dónde llama a `build_router()`. Confirma que la prueba nunca enlaza un
   socket: maneja directamente el router ensamblado por `bootstrap()`. Luego explica
   por qué una prueba HTTP que pasa demuestra algo sobre la vía de *producción*
   `run()`.
3. **Mueve los puertos de forma independiente.** Arranca Lumen con
   `FIREFLY_SERVER_ADDR=127.0.0.1:9090 FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091
   cargo run`, luego `curl localhost:9091/actuator/health` y
   `curl localhost:9090/api/v1/wallets/none`. Confirma que la salud responde en
   `:9091` y que el 404 público (problem+json RFC 9457) responde en `:9090`.
4. **Lee el informe de arranque como una lista de comprobación.** Ejecuta Lumen y
   copia las líneas `:: cqrs handlers … ::` y `:: routes … ::`. Después de terminar
   [CQRS](./09-cqrs.md), ejecútalo de nuevo y compara las dos: cada número nuevo
   debería corresponder a un `#[command_handler]`, `#[query_handler]` o
   `#[rest_controller]` que añadiste, con `main` intacto.
5. **Provoca un apagado ordenado.** Ejecuta Lumen, lanza una petición lenta y pulsa
   `Ctrl-C` a mitad de vuelo. Confirma que la petición en vuelo aún se completa y que
   el proceso sale con código `0` y sin traza de pila: la señal fue un apagado, no
   un fallo.

## Adónde ir después

- Mira los beans que escanea este pipeline, declarados en
  **[Inyección de dependencias y autoconfiguración](./04a-dependency-injection.md)**.
- Escribe el `#[rest_controller]` que la etapa 7 automonta en
  **[Tu primera API HTTP](./06-first-http-api.md)**.
- Observa cómo cobra vida la especificación OpenAPI que construye la etapa 10 en
  **[OpenAPI, Swagger UI y ReDoc](./06a-openapi.md)**.
