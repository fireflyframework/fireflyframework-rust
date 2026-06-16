# Pruebas

Cada capítulo hasta ahora ha mostrado los listados de Lumen *y* las pruebas que
los mantienen honestos: ese es justamente el propósito del libro, la prosa se
verifica contra un crate que compila y supera su batería de pruebas. Este
capítulo da un paso atrás respecto de cualquier funcionalidad concreta y
contempla la estrategia de pruebas en su conjunto, tal como la diseñarías para tu
propio servicio. Aquí no aprenderás ni una sola regla de negocio nueva;
aprenderás a demostrar las que ya escribiste, en tres niveles, sin arrancar un
servidor ni iniciar una base de datos.

La buena noticia es que el stack «primero en memoria» de Firefly convierte casi
toda prueba en una simple llamada a función. La infraestructura por defecto de
Lumen —su event store, su event broker y su read model— es Rust puro ejecutándose
en el propio proceso, así que una prueba nunca enlaza un socket, nunca abre una
conexión y nunca espera a un contenedor. El resultado es una batería de pruebas
rápida, determinista y verde en un portátil sin nada instalado.

Al terminar este capítulo, serás capaz de:

- Comprender los tres niveles de pruebas de Firefly —pruebas unitarias puras,
  pruebas HTTP en proceso que ejercitan el router *real*, y pruebas de
  integración condicionadas por variables de entorno contra infraestructura
  real— y cuándo recurrir a cada uno.
- Ejercitar en proceso un router de aplicación completamente cableado con
  `bootstrap()` y `tower::oneshot`, sin enlazar ningún socket y sin mocks.
- Usar los ayudantes de `firefly-testkit` —`TestClient`, `Slice`,
  `assert_event_published` y los firmantes de webhooks— para escribir las mismas
  pruebas de forma mucho más concisa.
- Construir una porción de inyección de dependencias acotada a una única unidad,
  instalar un colaborador falso (el análogo de `@MockBean`) y ejercitar un
  controlador sobre mocks (el análogo de `@WebMvcTest`).
- Escribir una prueba de integración que use Postgres o Kafka reales cuando estén
  presentes y que **se omita limpiamente** cuando no lo estén, de modo que
  `cargo test` se mantenga verde en todas partes.

## Conceptos que conocerás

Antes de la primera prueba, estas son las ideas en las que se apoya este
capítulo. Cada una se reintroduce en su contexto la primera vez que se usa; esta
es la versión breve.

> **Note** **Término clave — nivel de pruebas (testing tier).** Un *nivel* es una
> capa de la pirámide de pruebas: pruebas unitarias puras en la base (las más
> rápidas y numerosas), pruebas HTTP/de porción en proceso en el medio, y pruebas
> de integración contra infraestructura real en la cúspide (las más lentas y
> escasas). Firefly te ofrece un ayudante conciso por nivel. La división refleja
> el stack de pruebas de JUnit + Spring Boot: `@Test` simple, `@SpringBootTest` /
> `@WebMvcTest`, y `@Testcontainers`.

> **Note** **Término clave — prueba HTTP en proceso.** Una prueba *en proceso*
> ejercita el router HTTP real entregándole un `Request` y haciendo `await`
> directamente sobre el `Response`; no se abre ningún puerto ni se lanza ninguna
> tarea de servidor. Tiene la velocidad de una prueba unitaria con la cobertura de
> una prueba de extremo a extremo. El análogo en Spring es `MockMvc` (y el
> `WebTestClient` de Spring en modo `MOCK`).

> **Note** **Término clave — costura de pruebas (test seam).** Una *costura* es un
> punto que el framework expone específicamente para que las pruebas puedan llegar
> a su interior. La costura de Firefly es `bootstrap()`: ensambla la misma
> aplicación completamente cableada que serviría `run()`, pero la devuelve como un
> valor *sin* enlazar un socket. El `@SpringBootTest` de Spring arranca el mismo
> contexto que el `main` de producción; `bootstrap()` es su análogo en Rust.

> **Note** **Término clave — mock / fake.** Un *fake* es un colaborador
> sustituto que instalas en lugar del real: un repositorio en memoria en vez de
> una base de datos, un servicio predefinido en vez de una llamada de red.
> Instalar uno es la jugada del `@MockBean` de Spring: sobrescribir un bean bajo
> su port para que la unidad bajo prueba cablee el fake en lugar de la
> implementación real.

## El modelo de pruebas en proceso

El stack por defecto de Lumen es enteramente en memoria —un `MemoryEventStore`,
un `InMemoryBroker` y un read model basado en `Mutex<HashMap>`— de modo que casi
toda prueba se ejecuta como un simple `#[tokio::test]` **sin socket ni servicio
externo**. Incluso las pruebas HTTP no enlazan un puerto: entregan un `Request` al
router y hacen `await` del `Response`. Ese único hecho es lo que hace que la
batería sea rápida y amigable con CI, y conviene enunciarlo desde el principio
porque cada nivel descrito más abajo se construye en torno a él.

El modelo tiene una regla organizadora: cada prueba arranca **un** contexto de
aplicación y ejercita cada petición contra él. Es exactamente el modelo de
`@SpringBootTest` de Spring Boot —un contexto cableado por método de prueba— y en
Lumen el ayudante que te lo proporciona es `build_router()`:

```rust,ignore
// src/web.rs — the test seam, compiled only under #[cfg(test)].
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

Lo que acaba de ocurrir: `bootstrap()` ejecuta la misma tubería de arranque que
`run()` —escanea los componentes del contenedor de DI, automonta cada
`#[rest_controller]`, autodescubre la seguridad y el middleware, vacía los
handlers de CQRS / listeners de EDA / tareas `#[scheduled]` registrados en el
inventario— y devuelve un valor `Bootstrapped` en lugar de servirlo. Su campo
`.api_router` es el `axum::Router` público, completamente cableado, sin ningún
listener enlazado. `build_router()` es simplemente `main()` menos el paso de
servicio `.run()`.

> **Note** **Término clave — costura de bootstrap.** `bootstrap()` es el hermano
> de `run()` que conociste en [Quickstart](./02-quickstart.md): `run()` ensambla
> la aplicación *y la sirve*; `bootstrap()` ensambla la aplicación idéntica y
> devuelve el handle `Bootstrapped` para que una prueba pueda ejercitar
> `Bootstrapped::api_router` en proceso. Los mismos beans, el mismo cableado, sin
> socket.

Como los handlers de CQRS (`WalletHandlers`) y la proyección del read model
(`WalletProjection`) son **beans de DI autocableados** —no funciones libres sobre
un estado global de proceso— el contenedor de cada prueba es autoconsistente. Los
singletons `Ledger`, `ReadModel` y `QueryCache` que un contenedor resuelve son las
*mismas* instancias que comparten cada handler y la proyección. Así, la cartera
que abre un comando es la cartera que lee una consulta posterior, porque ambos se
ejecutan contra el único contenedor que la prueba arrancó. Y como un
`axum::Router` es barato de clonar (`clone`) (está respaldado por `Arc`), cada
petición clona la aplicación compartida en lugar de reconstruirla.

> **Tip** **Punto de control.** Ya puedes ejecutar toda la batería de pruebas.
> Desde la raíz del workspace, `cargo test -p firefly-sample-lumen` compila Lumen y
> ejecuta sus pruebas; deberías ver pasar `42 unit + 12 HTTP + 1 doctest`. El resto
> de este capítulo explica qué *son* esas pruebas.

## Nivel 1 — Pruebas unitarias sin infraestructura

El nivel inferior no necesita nada: ni router, ni contenedor, ni E/S. El value
object y el agregado de Lumen son Rust puro, así que sus pruebas construyen un
valor y comprueban un invariante directamente. `money.rs` y `domain.rs`
verifican la aritmética exacta en céntimos, los importes positivos, los fondos
suficientes y la regla de «propietario obligatorio» con simples `assert!`.

La capa de CQRS es igual de directa. Los handlers viven en un bean
`#[derive(Service)]` (`WalletHandlers`) cuyos colaboradores —el `Ledger` del lado
de escritura y el `ReadModel` del lado de lectura— se `#[autowired]` desde el
contenedor en el arranque. Pero nada te impide construir el bean tú mismo con esos
colaboradores en mano y llamar a un método directamente. Este es el núcleo del
módulo de pruebas de `commands.rs`:

```rust,ignore
use firefly::eda::InMemoryBroker;
use firefly::eventsourcing::MemoryEventStore;

#[tokio::test]
async fn handler_bean_operates_on_its_autowired_collaborators() {
    // Build the handler bean with the same Ledger + ReadModel the container
    // would inject — no bus, no process-global, no boot.
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
}
```

Lo que acaba de ocurrir, y por qué importa: construiste el bean de handlers a mano
con un `Ledger` en memoria y un `ReadModel` recién creado, luego llamaste a
`open_wallet` y `deposit` directamente y comprobaste los balances devueltos. Sin
despacho por el bus, sin contenedor de DI, sin HTTP. El arranque completo de la
aplicación instala el *mismo* bean en el bus vaciando el registro de inventario
(`register_discovered_handlers`), así que esta prueba ejercita la lógica real del
handler sin levantar nada de eso. Cuando quieras saber «¿hace el handler la
aritmética correcta?», este es el lugar más barato para averiguarlo.

La validación se prueba del mismo modo, sin tocar HTTP en ningún momento.
`OpenWallet` lleva `#[derive(Command)]`, que generó un `.validate()` a partir de
sus campos `#[firefly(validate)]`, así que lo invocas sobre el comando
directamente:

```rust,ignore
#[test]
fn open_wallet_validates_owner() {
    assert!(OpenWallet::default().validate().is_err());      // empty owner fails
    assert!(OpenWallet { owner: "alice".into(), opening_balance: 0 }.validate().is_ok());
}
```

Lo que acaba de ocurrir: el valor por defecto vacío falla la validación (sin
propietario), y un comando bien formado pasa, todo antes de que se ejecute ningún
handler. La capa web nunca ve un comando inválido porque el bus lo rechaza antes;
esta prueba fija ese rechazo al nivel más barato posible.

> **Note** La seguridad (`security.rs`), la saga de transferencia (`transfer.rs`),
> el flujo de cumplimiento (`compliance.rs`), la transferencia en dos fases
> (`tcc_transfer.rs`) y la tarea programada (`housekeeping.rs`) llevan cada una su
> propio `#[cfg(test)] mod tests` en el mismo espíritu: acuñar y luego verificar un
> token, ejecutar el camino feliz de la saga *y* su camino de compensación,
> ejecutar las ramas de aprobación/rechazo del flujo, y registrar el latido y
> comprobar que «tictaquea». Estos son los capítulos
> [Seguridad](./14-security.md), [Sagas, Workflows y TCC](./12-sagas.md) y
> [Programación y Notificaciones](./16-scheduling-notifications.md) demostrándose a
> sí mismos.

> **Tip** **Punto de control.** En conjunto, estas pruebas suman las **42 pruebas
> unitarias** de Lumen: los invariantes de `money` y `domain`, la validación de
> `commands` más el bean de handlers, el acuñar/verificar/rechazar de `security`,
> el camino feliz + compensación de `transfer`/`tcc_transfer`, el
> aprobar/rechazar de `compliance`, y el registro + tick de `housekeeping`.
> Ejecuta `cargo test -p firefly-sample-lumen --lib` para ver solo estas.

## Nivel 2 — Pruebas HTTP en proceso con `tower::oneshot`

El nivel intermedio demuestra que todo el stack se compone. La batería de extremo
a extremo de Lumen vive en `src/http_test.rs` —un `#[cfg(test)] mod http_test`
declarado en `main.rs`, de modo que se ejecuta como parte del propio target de
pruebas del binario— y ejercita el `build_router()` **completamente cableado**:
las rutas `#[rest_controller]` automontadas, el bean de handlers de CQRS, el
ledger con event sourcing, el bean de proyección del read model, la saga de
transferencia *y* la aplicación de JWT/RBAC autodescubierta de
[Seguridad](./14-security.md). Sin mocks: cada capa es la capa de producción, solo
que sobre infraestructura en memoria.

> **Note** **Término clave — `tower::oneshot`.** `oneshot` (de
> `tower::ServiceExt`) envía exactamente una petición a través de un `Service`
> —aquí un `axum::Router`— y se resuelve a su `Response`, luego descarta el
> servicio. Es la forma de llamar a un router como una simple función asíncrona. El
> tipo de cuerpo del router proviene de `http_body_util::BodyExt`, que usas para
> recopilar los bytes de la respuesta.

### Paso 1 — Escribir los ayudantes de petición/respuesta

El patrón es un `Router` por prueba más `oneshot` por petición. Una prueba arranca
la aplicación una vez con `let app = build_router().await` y ejercita cada
petición contra ella; un pequeño ayudante `send` clona el `&Router` compartido por
petición de modo que todas comparten el único contenedor. Estos son los ayudantes
que `http_test.rs` define una sola vez al inicio del archivo:

```rust,ignore
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Sends one request against the (cloned) shared app and returns the response.
async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.unwrap()
}

/// Builds a POST with a JSON body, optionally carrying a bearer token.
fn post(path: &str, body: serde_json::Value, auth: bool) -> Request<Body> {
    let mut b = Request::post(path).header("content-type", "application/json");
    if auth {
        b = b.header("authorization", bearer()); // "Bearer <minted CUSTOMER token>"
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()
}

/// Buffers the response body and decodes it as JSON into `T`.
async fn body_json<T: serde::de::DeserializeOwned>(res: Response) -> T {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
```

Lo que acaba de ocurrir: `send` es todo el mecanismo —`app.clone().oneshot(req)`
ejecuta la petición a través del router real en proceso—. `post` ensambla una
petición JSON y, cuando `auth` es verdadero, añade una cabecera
`Authorization: Bearer …` acuñada por `bearer()` (que llama al
`mint_token("u-alice", &[CUSTOMER_ROLE])` de Lumen del módulo de seguridad).
`body_json` drena el cuerpo de la respuesta con `BodyExt::collect` y lo
deserializa. Tres ayudantes, y cada prueba de más abajo se lee como un guion.

### Paso 2 — Ejercitar un viaje de ida y vuelta a través de CQRS

Con los ayudantes en su sitio, una prueba arranca la aplicación, abre una cartera
a través de la API pública y comprueba que la lectura proyectada vuelve a través
de CQRS, todo contra el único contexto de aplicación:

```rust,ignore
#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    let app = build_router().await;                       // one app context per test
    let opened = open_wallet(&app, "alice", 1_000).await; // POST /api/v1/wallets, asserts 201
    assert_eq!(opened.owner, "alice");
    assert_eq!(opened.balance, 1_000);

    // GET dispatches the #[query_handler] on the handler bean; it reads the
    // projection (or repairs from the event stream) — both resolved from the
    // SAME container as the command that opened the wallet.
    let fetched = get_wallet(&app, &opened.id).await;
    assert_eq!(fetched.id, opened.id);
    assert_eq!(fetched.balance, 1_000);
}
```

Lo que acaba de ocurrir, y por qué importa: el `POST` ejecutó un comando a través
del bus, que añadió eventos al ledger en memoria; el `GET` ejecutó una consulta
que leyó la proyección que esos eventos alimentaron. Ambos resolvieron el *mismo*
`Ledger` y `ReadModel` desde el único contenedor que la prueba arrancó, así que la
lectura ve la escritura. Esta única prueba demuestra que el lado de comandos, el
lado de consultas, la proyección y su cableado compartido encajan todos juntos
—algo que ninguna prueba unitaria puede mostrar, porque la costura que se prueba
*es* el cableado.

### Paso 3 — Demostrar que los modos de fallo se renderizan como problemas

El mismo archivo demuestra el camino feliz de la saga
(`transfer_saga_happy_path_moves_funds_between_wallets`), el camino de
compensación (`transfer_saga_overdraft_compensates_and_is_422`) y la
renderización como problema de los modos de fallo. Un token ausente es un 401, un
propietario vacío es un 422, y un id desconocido es un 404, cada uno comprobando
el tipo de contenido `application/problem+json`:

```rust,ignore
#[tokio::test]
async fn missing_token_is_401_problem_on_mutations() {
    let app = build_router().await;
    let res = send(
        &app,
        post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "mallory", "openingBalance": 10 }),
            false, // no Authorization header
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(content_type(&res).contains("application/problem+json"));
}
```

Lo que acaba de ocurrir: el `POST` sin autenticar fue rechazado por la capa de
seguridad autodescubierta con un 401, *y* el cuerpo volvió como un documento
`application/problem+json` conforme a la RFC 9457, no como un 401 en blanco. La
misma forma se mantiene para las pruebas del 422 (validación) y el 404 (cartera
desconocida). Esa única batería es la prueba de que todo el stack —enrutado,
seguridad, CQRS, event sourcing, sagas y renderización de problemas— se compone
correctamente.

> **Note** **Término clave — respuesta de problema RFC 9457.** La RFC 9457 (que
> deja obsoleta la antigua RFC 7807) define `application/problem+json`: un cuerpo
> de error estructurado con un `type`, un `title`, un `status` y un `detail`.
> Firefly renderiza automáticamente todo error de handler y toda ruta no
> emparejada como uno de estos, razón por la cual las pruebas pueden comprobar el
> tipo de contenido. Conociste esto en [Tu primera API
> HTTP](./06-first-http-api.md).

> **Tip** **Punto de control.** Estos doce escenarios —abrir → consultar →
> depositar/retirar → transferir (feliz + compensado) → flujo de cumplimiento →
> transferencia en dos fases → problemas 401/422/404— son las **12 pruebas HTTP**
> de Lumen. Ejecuta `cargo test -p firefly-sample-lumen --test '*' 2>/dev/null ||
> cargo test -p firefly-sample-lumen` y observa pasar el módulo `http_test`.

## El Nivel 2, a la manera concisa — el `firefly-testkit`

Las propias pruebas HTTP de Lumen usan la forma cruda de `tower::oneshot` a
propósito, para mostrar el mecanismo sin magia. En *tu* servicio recurrirías a
`firefly-testkit`, que empaqueta exactamente ese código repetitivo en ayudantes
reutilizables. Es un crate aparte con niveles condicionados por features, así que
solo traes lo que usas:

```toml
# Cargo.toml — add as a dev-dependency, switching on the helpers you need.
[dev-dependencies]
firefly-testkit = { version = "26.6.28", features = ["web", "container"] }
```

> **Note** La superficie por defecto (los firmantes de webhooks, `SpyBroker` y los
> ayudantes JSON) no acarrea dependencias pesadas. La feature `web` añade el
> `TestClient` en proceso; `container` añade el `Slice` de DI; y `testcontainers`
> añade los fixtures de pruebas de integración. Un servicio que solo firma webhooks
> obtiene una compilación ligera.

Tres piezas son las que más importan.

### TestClient — un cliente HTTP en proceso (feature `web`)

`TestClient::new(router)` envuelve cualquier `Router` de axum y te ofrece
`get` / `post` / `put` / `patch` / `delete` (asíncronos) más una API de aserciones
fluida sobre el `TestResponse` que devuelve. La prueba `open_then_get` de más
arriba, reescrita con `TestClient`:

```rust,ignore
use firefly_testkit::TestClient;

#[tokio::test]
async fn open_then_get_with_testclient() {
    let client = TestClient::new(build_router().await);

    let created = client
        .post("/api/v1/wallets", &serde_json::json!({ "owner": "alice", "openingBalance": 1000 }))
        .await;
    created.assert_status(201);
    let id = created.json_path("$.id").unwrap();

    client
        .get(&format!("/api/v1/wallets/{}", id.as_str().unwrap()))
        .await
        .assert_status(200)
        .assert_json_path("$.balance", 1000);
}
```

Lo que acaba de ocurrir: `TestClient` se encargó por ti de construir la petición y
de bufferizar el cuerpo. `post(path, &body)` serializa el JSON y fija el
`content-type`; `assert_status` comprueba el código; `json_path("$.id")`
selecciona un único campo; y `assert_json_path("$.balance", 1000)` comprueba un
valor en lo profundo del cuerpo sin deletrear el documento entero. Cada aserción
devuelve `&Self`, así que se encadenan.

La superficie de aserciones es: `assert_status`, `assert_success`,
`assert_header` / `assert_header_present`, `assert_body_contains`,
`assert_json_eq`, `assert_json_path` / `assert_json_path_exists` /
`assert_json_path_absent`, más los extractores `json::<T>()`,
`json_path("$.field")`, `text()`, `header(name)` y `body_bytes()`. La gramática de
rutas es un subconjunto de JSONPath de resultado único: un `$` inicial, acceso a
miembros con punto (`$.user.name`) o con corchetes (`$['user']['name']`), e
indexado de arrays (`$[0]`, `$.items[2].id`); sin comodines, filtros ni descenso
recursivo.

> **Note** Cada verbo tiene además una variante bloqueante —`get_blocking`,
> `post_blocking`, …— que ejecuta la petición sobre un runtime interno de hilo
> actual, de modo que un simple `#[test]` (sin `#[tokio::test]`) se lee
> exactamente como un cliente HTTP síncrono. Usa la forma bloqueante fuera de un
> runtime de Tokio y la forma asíncrona dentro de uno.

### Slice — un contenedor de DI acotado para una prueba (feature `container`)

Las pruebas HTTP arrancan la aplicación *completa*. A veces quieres lo contrario:
el cableado de una única unidad y nada más, sin router, sin datasource. `Slice`
construye un `firefly-container` mínimo exactamente para eso. Registras solo los
colaboradores que la unidad bajo prueba necesita, y luego los resuelves.

> **Note** **Término clave — prueba de porción (slice test).** Una *porción* carga
> un subconjunto acotado del grafo de objetos en lugar de todo el contexto de
> aplicación. Es más rápida que un arranque completo y aísla la unidad bajo prueba.
> Las anotaciones de porción de Spring (`@WebMvcTest`, `@DataJpaTest`) son el
> análogo directo; `Slice` es el constructor explícito que Rust necesita en su
> lugar, ya que no hay escaneo de paquetes.

```rust,ignore
use firefly_testkit::Slice;
use firefly_container::{Container, ContainerError, Scope};

let slice = Slice::new()
    .instance(ReadModel::default())                       // a ready instance (the mock/override path)
    .register::<MyService, _>(Scope::Singleton, |c: &Container| {
        Ok(MyService::new())                              // a factory; resolve deps from `c`
    })
    .build();

let read_model: std::sync::Arc<ReadModel> = slice.get();
```

Lo que acaba de ocurrir: `instance(value)` instala un singleton listo;
`register::<T, _>(scope, factory)` registra un bean construido por una factoría
que puede resolver sus propias dependencias desde el contenedor `c`; y `build()`
devuelve un `BuiltSlice` desde el que resuelves con `get::<T>()` (o
`get_named::<T>(name)`). También existe `eager::<T>()`, que fuerza la construcción
de un bean en tiempo de `build()`, de modo que un colaborador ausente falle *ahí*
(la barrera de fallo rápido que refleja el arranque de las porciones de Spring) en
lugar de perezosamente en el primer uso.

El par `instance` + `bind` **es** el `@MockBean`. Instala un fake bajo un port y
el bean bajo prueba lo cablea en lugar del colaborador real:

```rust,ignore
let slice = Slice::new()
    .instance(FakeRepo::default())             // the fake (a "mock_bean")
    .bind::<dyn Repo, FakeRepo>(|a| a)         // expose it as the `dyn Repo` port
    .register::<Service, _>(Scope::Singleton, |c| {
        Ok(Service { repo: c.resolve::<dyn Repo>()? })  // wires the fake
    })
    .eager::<Service>()                        // fail fast if `Repo` is missing
    .build();
```

Como el fake lo retiene el contenedor, `get::<FakeRepo>()` después de `build()` te
devuelve la *misma* instancia que el servicio cableó. Así puedes configurarlo y
comprobar contra él mediante mutabilidad interior —la jugada de verificación de
mocks de Spring, sin un framework de mocking.

### `@WebMvcTest` — un controlador sobre servicios mockeados con `web_client`

Combina ambos: registra un bean de controlador más sus colaboradores
**mockeados**, luego llama a `built.web_client::<C, _>(C::routes)` para resolver
ese controlador y envolver su router generado por `#[rest_controller]` en un
`TestClient`. Esto es el `@WebMvcTest(Controller.class)` + `@MockBean(Service.class)`
de Spring: la capa web de un único controlador ejercitada sobre fakes, sin arranque
de la aplicación completa y sin datasource:

```rust,ignore
use firefly_testkit::Slice;
use firefly_container::Scope;

// @WebMvcTest(WalletController) + @MockBean(WalletService)
let client = Slice::new()
    .instance(FakeWalletService::default())               // the mock
    .bind::<dyn WalletService, FakeWalletService>(|a| a)
    .register::<WalletController, _>(Scope::Singleton, |c| {
        Ok(WalletController { service: c.resolve::<dyn WalletService>()? })
    })
    .eager::<WalletController>()
    .build()
    .web_client::<WalletController, _>(WalletController::routes);

client.get_blocking("/api/v1/wallets/unknown").assert_status(404);
```

Lo que acaba de ocurrir: `web_client` (feature `web`) toma la
`fn routes(state: C) -> Router` generada del controlador, clona el bean resuelto al
estado del router, y envuelve el resultado en un `TestClient`. Toda la capa web de
un controlador se ejercita ahora sobre fakes. (`FakeWalletService` /
`WalletController` aquí son formas ilustrativas para *tu* servicio; el propio
controlador de Lumen autocablea el bus real, así que su cobertura web proviene de
las pruebas HTTP de Nivel 2 de más arriba.)

> **Note** Para un **`@DataJpaTest`** —una porción de persistencia sin stack web—
> el mismo `Slice` registra un repositorio sobre una base de datos SQLite en
> memoria. Construye el repositorio con
> `firefly::data_sqlx::repository_for::<Entity>(db)`, exactamente como hacen las
> pruebas `-models` de `lumen-ledger`: apuntan un `Db` a una URL de SQLite en
> memoria (`sqlite:file:…?mode=memory&cache=shared`) y ejercitan las consultas
> derivadas reales sin ningún Postgres a la vista. Conociste esos repositorios en
> [Persistencia y Repositorios Reactivos](./07-persistence.md).

### Comprobar los eventos emitidos con `SpyBroker`

El tercer ayudante de uso cotidiano demuestra que un handler *publicó* el evento
correcto. `SpyBroker` registra lo que un handler publicó, y los ayudantes de
aserción lo leen de vuelta:

- `assert_event_published(&spy, "Type")` comprueba que se registró un evento de
  ese tipo y lo devuelve.
- `assert_event_published_with(&spy, "Type", &json)` también comprueba que la
  carga útil (parseada como objeto JSON) contiene los pares clave/valor dados —una
  coincidencia de *subconjunto*, así que los campos extra se ignoran.
- `assert_no_events_published(&spy)` comprueba que no se registró ninguno.
- `must_encode` / `must_decode` son ayudantes JSON que entran en pánico al fallar,
  para construir y leer cargas útiles.

Un ejemplo con sabor a Lumen —demostrar que una apertura emite un `WalletOpened`:

```rust,ignore
use firefly_testkit::{assert_event_published, must_encode, SpyBroker};

#[test]
fn open_emits_wallet_opened() {
    let spy = SpyBroker::new();
    // The ledger publishes through the broker; here we record the envelope the
    // projection would consume.
    spy.record(
        "wallets.events",
        "WalletOpened",
        &must_encode(&serde_json::json!({ "id": "wlt_1", "owner": "alice" })),
    );

    let event = assert_event_published(&spy, "WalletOpened");
    assert_eq!(event.topic, "wallets.events");
}
```

Lo que acaba de ocurrir: `spy.record(topic, type, payload)` almacena el sobre de
un evento, y `assert_event_published` encuentra el primero del tipo nombrado (o
hace fallar la prueba, enumerando lo que *sí* se publicó). El `RecordedEvent`
devuelto lleva `topic`, `event_type` y los bytes crudos de `payload`, así que
puedes comprobar más cosas. Cablea un `SpyBroker` en un `Ledger` en una prueba
real y podrás demostrar que un depósito emite un `MoneyDeposited` con el importe
correcto.

### Firmantes de webhooks

Cuando Lumen incorpore un webhook entrante (el capítulo [Programación y
Notificaciones](./16-scheduling-notifications.md)), los firmantes HMAC del testkit
—`sign_hmac`, `sign_stripe`, `sign_github`, `sign_twilio`— producen valores de
cabecera idénticos byte a byte a lo que cada validador de `firefly-webhooks`
espera, de modo que una petición de prueba firmada se valida exactamente como lo
haría la de un proveedor real:

```rust,ignore
use firefly_testkit::sign_stripe;

let sig = sign_stripe(b"whsec_test", br#"{"type":"charge.succeeded"}"#, 1_700_000_000);
// Attach `sig` as the `Stripe-Signature` header on a TestClient POST and the
// validator accepts it exactly as it would a real Stripe delivery.
```

Lo que acaba de ocurrir: `sign_stripe(secret, body, unix_ts)` construye el valor
`t=<unix>,v1=<hex>` que Stripe envía en `Stripe-Signature`, firmando
`<unix>.<body>` con HMAC-SHA256. Como el firmante coincide exactamente con la
forma sobre el cable que espera el validador, una prueba que firma su propia carga
útil demuestra que tu receptor acepta una entrega genuina.

## Pruebas de pipelines reactivos

El endpoint de streaming (presentado en [Producción y
Despliegue](./20-production.md)) construye un `Flux`. Conociste `Mono` y `Flux` en
[El Modelo Reactivo](./05-reactive-model.md); aquí está cómo *probar* uno.

> **Note** **Término clave — operación terminal.** Un pipeline reactivo es perezoso:
> los operadores (`filter`, `map`, …) describen trabajo pero no ejecutan nada hasta
> que un *terminal* consume el stream. `collect_list()`, `count()` y `block()` son
> terminales: llevan el pipeline a su finalización y resuelven un valor. El
> `block()` / `collectList()` de Spring Reactor son el análogo directo.

Pruebas un pipeline llevándolo a un terminal y comprobando el valor resuelto:

```rust
use firefly_reactive::Flux;

#[tokio::test]
async fn pipeline_filters_and_maps() {
    let out = Flux::range(1, 5)          // emits 1, 2, 3, 4, 5 (start, count)
        .filter(|x| x % 2 == 1)          // keep the odds: 1, 3, 5
        .map(|x| x * 10)                 // scale: 10, 30, 50
        .collect_list()                  // Flux<i64> -> Mono<Vec<i64>>
        .block()                         // Result<Option<Vec<i64>>, FireflyError>
        .await
        .unwrap()                        // unwrap the Result
        .unwrap();                       // unwrap the Option (the stream was non-empty)
    assert_eq!(out, vec![10, 30, 50]);
}
```

Lo que acaba de ocurrir, y por qué el doble `unwrap`: `Flux::range(1, 5)` emite
cinco valores empezando en `1`. `filter` y `map` los transforman perezosamente.
`collect_list()` convierte el `Flux<i64>` en un `Mono<Vec<i64>>` —un único valor
que contiene la lista entera— y `block().await` lo lleva a su finalización.
`block()` devuelve `Result<Option<Vec<i64>>, FireflyError>`: el `Result` expone un
error del pipeline, y el `Option` es `None` solo para un stream vacío, así que una
ejecución exitosa no vacía necesita ambos `unwrap`. Esto son simples aserciones de
Rust asíncrono sobre un stream resuelto; sin runtime de pruebas especial.

> **Note** Las pruebas de streaming de Lumen (`src/streaming_test.rs`,
> condicionadas tras la feature `streaming`) toman la ruta HTTP en lugar de probar
> el `Flux` directamente: abren una cartera, depositan, luego hacen `GET /events`
> y comprueban dos líneas NDJSON (`WalletOpened` + `MoneyDeposited`) por defecto,
> `text/event-stream` con `?format=sse`, y un 404 para una cartera desconocida.
> Esas son las `+3 streaming tests` que activas con `--features streaming`.

## Nivel 3 — Pruebas de integración con infraestructura real

Lumen se ejecuta de forma hermética, pero los adaptadores de producción a los que
recurres en [Producción y Despliegue](./20-production.md) necesitan servicios
reales. El workspace incluye un `docker-compose.yml` con Postgres, Redis, RabbitMQ,
un Redpanda compatible con Kafka, Keycloak, emuladores de S3/Blob y una captura
SMTP.

La convención a lo largo de los crates de adaptadores mantiene el `cargo test` por
defecto verde en una máquina sin nada: una prueba lee una URL de conexión del
entorno y **se omite cuando está sin definir**. CI activa la batería completa
exportando la variable.

> **Note** **Término clave — prueba condicionada por entorno (env-gated).** Una
> prueba *condicionada por entorno* solo se ejecuta cuando una variable de entorno
> nombrada está presente (un `DATABASE_URL`, un `REDIS_URL`). Marcarla con
> `#[ignore]` la mantiene fuera de la ejecución por defecto; leer la variable y
> retornar pronto significa que incluso `--ignored` se omite limpiamente donde el
> servicio está ausente. Este es el análogo en Rust de las pruebas protegidas por
> `@Testcontainers` / `@EnabledIf` de Spring.

```rust,ignore
#[tokio::test]
#[ignore = "requires postgres (DATABASE_URL)"]
async fn postgres_event_store_round_trips() {
    // Skip on a bare machine: no DATABASE_URL -> return before touching the DB.
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    // ... drive the Postgres-backed EventStore against the live database at `url`.
}
```

Lo que acaba de ocurrir: el `#[ignore]` mantiene esta prueba enteramente fuera de
la ejecución por defecto de `cargo test`. Cuando optas por incluirla con
`--ignored`, la guarda `let … else { return }` sigue omitiéndola limpiamente si
`DATABASE_URL` está sin definir, así que la única forma de que toque Postgres de
verdad es cuando la apuntas a uno en vivo. Para ejecutar la batería condicionada
por entorno, arranca los servicios de respaldo y exporta las URL:

```bash
docker compose up -d                       # start the backing services
DATABASE_URL=postgres://firefly:firefly@localhost:5442/firefly \
REDIS_URL=redis://localhost:6379/0 \
  cargo test --workspace -- --ignored      # run the env-gated suite
docker compose down
```

> **Note** El archivo de compose mapea Postgres al puerto **5442** del host (no el
> 5432 por defecto) para evitar colisionar con un Postgres local que ya puedas
> tener en marcha, razón por la cual el `DATABASE_URL` de arriba dice
> `localhost:5442`.

El testkit también puede acortar este nivel. Con la feature `testcontainers`,
`firefly_testkit::containers` mapea el `(host, port)` de un servicio arrancado a
las claves de configuración canónicas `firefly.*` (`config_for(&container)`) y
ofrece una guarda de omisión `docker_available()` —el análogo en Rust del
`@ServiceConnection` de Spring—. Está desacoplado de cualquier biblioteca de
contenedores concreta: aliméntalo con los detalles de conexión que cualquier
herramienta ya te entregue.

## Ejecutar la batería de Lumen

Desde la raíz del workspace (con `export PATH="/opt/homebrew/bin:$PATH"` en macOS
para que la toolchain se resuelva):

```bash
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                      # 42 unit + 12 HTTP + 1 doctest
cargo test   -p firefly-sample-lumen --features streaming # + 3 streaming tests
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
cargo fmt    -p firefly-sample-lumen -- --check
```

> **Tip** **Punto de control.** Una ejecución limpia imprime `test result: ok` para
> los niveles unitario y HTTP y para el doctest, con cero advertencias de clippy y
> un `fmt --check` limpio. Si un fragmento de cualquier capítulo se desvía del
> archivo, esta barrera falla —que es precisamente cómo el libro se mantiene
> honesto.

## Resumen — cómo Lumen se demuestra a sí mismo

Nada cambió en `src/` este capítulo; es la retrospectiva sobre el código de
pruebas que creció junto a cada funcionalidad. Ahora sabes:

- **Los tres niveles, y un ayudante por nivel.** Pruebas unitarias `#[tokio::test]`
  puras sin E/S; pruebas HTTP/de porción en proceso que ejercitan el router real
  sin enlazar un socket; y pruebas de integración condicionadas por entorno contra
  infraestructura real.
- **`bootstrap()` es la costura de pruebas.** Ensambla la misma aplicación
  completamente cableada que serviría `run()` y devuelve `Bootstrapped::api_router`
  —sin socket— de modo que `build_router()` le da a cada prueba un contenedor
  autoconsistente donde una escritura es visible para una lectura posterior.
- **Nivel 1 — pruebas unitarias.** Construye un value object, un agregado o un bean
  de handlers con sus colaboradores en mano y comprueba directamente; llama a
  `.validate()` sobre un comando sin HTTP. Aquí viven las **42 pruebas unitarias**
  de Lumen.
- **Nivel 2 — HTTP en proceso.** `tower::oneshot` ejercita `build_router()` de
  extremo a extremo sobre infraestructura en memoria; las **12 pruebas HTTP** de
  Lumen cubren abrir → consultar → depositar/retirar → transferir (feliz +
  compensado) → flujo → 2PC → problemas RFC 9457 401/422/404. El `TestClient`, el
  `Slice` (`@MockBean` / `@WebMvcTest` / `@DataJpaTest`) y el `SpyBroker` de
  `firefly-testkit` hacen concisa la misma cobertura en tu propio servicio.
- **Los pipelines reactivos** se prueban llevando un `Flux` a un terminal
  (`collect_list().block()`) —el único **doctest** del capítulo.
- **Nivel 3 — integración.** Pruebas con `#[ignore]`, condicionadas por entorno,
  que leen una URL de conexión, se omiten limpiamente cuando está sin definir, y se
  ejecutan contra los servicios de `docker compose` (o los fixtures `containers` del
  testkit) cuando está definida.

## Ejercicios

1. **Reescribe una prueba con `TestClient`.** Toma las aserciones de lectura de
   `deposit_and_withdraw_update_the_balance` en `src/http_test.rs` y reescribe el
   viaje de ida y vuelta final del `GET` usando `TestClient` + `assert_json_path`.
   (Los ayudantes de petición de `TestClient` no llevan un argumento de cabecera por
   petición, así que arranca la aplicación una vez, mantén las mutaciones
   autenticadas en la forma cruda de `tower::oneshot` que acuña un token bearer
   contra ese `Router`, y luego envuelve el *mismo* `Router` en un `TestClient` para
   la lectura pública —un único contexto de aplicación, de modo que la lectura vea
   la mutación.)
2. **Una prueba de `Slice` para el read model.** Usa `Slice` para registrar una
   instancia `ReadModel::default()`, proyecta un `WalletOpened` en ella a mano, y
   comprueba que `find` devuelve la vista —todo sin el bus ni el router. Añade
   `.eager::<ReadModel>()` y confirma que `build()` tiene éxito, luego resuélvela con
   `slice.get::<ReadModel>()`.
3. **Aserción de eventos sobre el ledger.** Cablea un `SpyBroker` en un `Ledger` en
   una prueba, confirma un depósito, y usa `assert_event_published_with(&spy,
   "MoneyDeposited", &serde_json::json!({ "amount": 50 }))` para demostrar que el
   campo `amount` de la carga útil es igual a 50. Luego añade
   `assert_no_events_published` a un camino sin efecto y observa cómo pasa.
4. **Una porción al estilo `@WebMvcTest`.** Esboza un servicio falso tras un port,
   regístralo con `.instance(...)` + `.bind::<dyn Port, Fake>(|a| a)`, registra un
   controlador sobre él, y llama a `web_client::<C, _>(C::routes)` para ejercitar una
   ruta sobre el fake con `get_blocking`. Comprueba un 404 para un id desconocido.
5. **Una prueba de integración que se omite.** Escribe una prueba con `#[ignore]`
   que lea `DATABASE_URL`, retorne pronto cuando esté sin definir, y en otro caso
   abra una cartera contra un event store respaldado por Postgres. Confirma que se
   omite con un simple `cargo test`, que se omite con `--ignored` cuando la variable
   está sin definir, y que se ejecuta con la variable definida.

## Adónde ir después

- Anda Lumen, inspecciónalo y opéralo con las herramientas de desarrollo de **[La
  CLI](./19-cli.md)** —incluidos los comandos `firefly` que ejecutan estas mismas
  comprobaciones.
- Sustituye los valores por defecto en memoria por Postgres y Kafka reales, y luego
  despliega Lumen, en **[Producción y Despliegue](./20-production.md)** —donde las
  pruebas de integración de Nivel 3 por fin tienen infraestructura real contra la que
  ejecutarse.
