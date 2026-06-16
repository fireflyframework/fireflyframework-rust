# Seguridad

En [Clientes HTTP](./13-http-clients.md) viste cómo Lumen *llamaría* a un
proveedor externo de pagos o de divisas. Sin embargo, Lumen sigue estando
abierto de par en par: cualquier llamante puede abrir un monedero, ingresar,
retirar o mover dinero entre monederos. Antes de que la Parte V pueda llevar
Lumen a producción, tienes que cerrar esa puerta, y lo harás sin añadir ninguna
dependencia, sin escribir criptografía a mano y sin reescribir un solo handler.

Al terminar este capítulo, Lumen **autenticará** cada petición con un JWT
firmado, **autorizará** las rutas mutadoras con una cadena de filtros RBAC
basada en rutas, y dejará abiertas las lecturas públicas y la superficie de
gestión. Todo ello se construye sobre la capa de seguridad del framework, a la
que se llega a través de la única fachada `firefly` de la que dependes desde el
[Arranque rápido](./02-quickstart.md).

Al terminar este capítulo, serás capaz de:

- Emitir y verificar tokens HS256 firmados con `JwtService`, y entender por qué
  cada token emitido lleva un `exp` acotado.
- Adaptar ese servicio a un `Verifier` y convertir el bearer token de una
  petición en una `Authentication` (principal, roles, claims).
- Componer un `BearerLayer` y una `FilterChain` RBAC ordenada por rutas, y
  entender el orden fail-closed (denegar por defecto).
- Cablear ambos como `#[bean]`s y ver cómo `FireflyApplication` los autodescubre
  y los aplica como capas, sin ninguna llamada a `.with_security(...)`.
- Empujar la autorización hasta un método de servicio con
  `#[firefly::pre_authorize]` / `#[firefly::post_authorize]` sobre un contexto
  de seguridad ambiental.
- Mover esa misma postura a la configuración para que producción intercambie la
  clave de demostración por un IdP real sin tocar el código.

## Conceptos que conocerás

Antes de la primera línea de código, aquí tienes las ideas en las que se apoya
este capítulo. Cada una se reintroduce en contexto donde se usa por primera vez;
esta es la versión breve.

> **Note** **Término clave — autenticación frente a autorización.** La
> *autenticación* responde a "¿quién es este llamante?": valida una credencial y
> resuelve un principal. La *autorización* responde a "¿puede hacer esto?":
> comprueba que el principal resuelto tiene permiso para realizar la operación.
> Son dos etapas distintas, y Firefly las mantiene en dos componentes distintos.

> **Note** **Término clave — JWT (JSON Web Token).** Un *JWT* es un token
> compacto, URL-safe y firmado que transporta una carga útil JSON de *claims*
> (`sub`, `roles`, `exp`, …). Como la firma demuestra que la carga útil no fue
> manipulada, un servicio sin estado puede confiar en los claims de un token sin
> una sesión del lado del servidor ni un viaje de ida y vuelta a una base de
> datos. El análogo en Spring es un servidor de recursos de Spring Security que
> valida un token `Bearer`.

> **Note** **Término clave — RBAC (Role-Based Access Control).** El *RBAC*
> concede acceso según los *roles* que ostenta un llamante (aquí `CUSTOMER`) en
> lugar de según su identidad. Una regla dice "esta ruta requiere el rol X"; un
> llamante pasa si su token lleva X. Es el modelo que usan las reglas de
> autorización por URL de Spring Security.

> **Note** **Término clave — servidor de recursos.** Un *servidor de recursos*
> es un servicio que protege sus propios endpoints validando un token de acceso
> emitido en otro lugar (un proveedor de identidad). Nunca autentica a nadie;
> solo *verifica* la credencial que se le entrega. Lumen es un servidor de
> recursos: en la demostración también emite sus propios tokens para pruebas,
> pero la ruta de verificación es idéntica a la de un IdP de producción.

La capa de seguridad del framework lleva mucho más de lo que usa Lumen: JWKS,
OAuth2 (cliente + servidor de autorización), jerarquía de roles, guardas de
métodos, CSRF y codificadores de contraseñas. Haremos un recorrido por todo eso
al final; primero, las cuatro piezas que Lumen realmente cablea, en el orden en
que una petición se las encuentra.

## La canalización de la petición de un vistazo

Cada petición recorre dos etapas de seguridad antes de llegar a un handler:

```text
            incoming request
                   │
                   ▼
   ┌───────────────────────────────────────┐
   │              BearerLayer               │  (authentication)
   │  • reads Authorization: Bearer <tok>   │
   │  • calls the Verifier (JwtService)     │
   │  • stores Authentication on the request│
   │  • allow_anonymous → pass empty ctx    │
   └───────────────────────────────────────┘
                   │
                   ▼
   ┌───────────────────────────────────────┐
   │           FilterChain (RBAC)           │  (authorization)
   │  permit_method("GET", "/api/v1/...")   │
   │  permit("/actuator/")                  │
   │  require("/api/v1/wallets", CUSTOMER)  │
   │  401 / 403 problem+json on a miss      │
   └───────────────────────────────────────┘
                   │
                   ▼
            WalletApi handlers
```

El `BearerLayer` autentica (¿quién?), la `FilterChain` autoriza (¿puede?), y
solo una petición que supera ambas alcanza los handlers de Lumen. Los fallos de
autenticación/autorización se renderizan como `application/problem+json` según
la RFC 9457 sobre un 401 o un 403: un contrato de cable estable y basado en
estándares que los clientes y pasarelas comerciales ya entienden.

> **Note** **Término clave — problem+json de la RFC 9457.** La RFC 9457 (que
> deja obsoleta la RFC 7807) estandariza cuerpos de error legibles por máquina
> bajo el tipo de medio `application/problem+json`: un objeto JSON con los
> miembros `type`, `title`, `status` y `detail`. Firefly renderiza así cada
> rechazo de seguridad, de modo que un 401 o un 403 es un documento
> estructurado, no un cuerpo en blanco. Conociste este renderizador en [Tu
> primera API HTTP](./06-first-http-api.md); aquí también transporta los fallos
> de autenticación/autorización.

## Paso 1 — Emitir y verificar tokens con `JwtService`

Lumen es una API sin estado. Las sesiones requerirían enrutamiento adherente
(sticky) o un almacén compartido en cada réplica; un JWT firmado permite que
cada petición transporte su propia credencial, de modo que el servicio escala
horizontalmente sin estado compartido. El `JwtService` del framework tanto
**emite** los tokens de demostración como **verifica** los entrantes, usando una
clave simétrica HS256, que es exactamente lo que hace que Lumen sea ejecutable y
testeable sin un IdP externo.

> **Note** **Término clave — firma simétrica (HS256).** HS256 firma y verifica
> con el *mismo* secreto compartido. Es el esquema más sencillo de operar (una
> clave, sin servidor de claves) y el adecuado para un ejemplo autocontenido.
> (Un despliegue de producción suele pasar a RS256 *asimétrico*, donde un IdP
> firma con una clave privada y tu servicio verifica con la clave pública
> correspondiente obtenida desde un endpoint JWKS. El Paso 7 muestra ese
> intercambio.)

Crea `src/security.rs` y empieza con la clave de firma, la constante de rol y el
servicio compartido:

```rust,ignore
// src/security.rs
use firefly::security::{
    BearerConfig, BearerLayer, FilterChain, JwtService, SecurityError, Verifier, VerifierFn,
};
use serde_json::json;

/// The demo signing key. A real service reads this from configuration / a
/// secret store; it is inlined here so the sample is runnable as-is.
pub const DEMO_SIGNING_KEY: &[u8] = b"lumen-demo-signing-key-change-me";

/// The role every mutating wallet command requires.
pub const CUSTOMER_ROLE: &str = "CUSTOMER";

/// The shared HS256 service that both signs the demo tokens and verifies
/// incoming bearer tokens.
fn jwt_service() -> JwtService {
    JwtService::new(DEMO_SIGNING_KEY)
}

/// Mints a signed HS256 access token for `subject` with `roles`, valid for the
/// service's default lifetime (one hour).
pub fn mint_token(subject: &str, roles: &[&str]) -> String {
    jwt_service()
        .encode(json!({ "sub": subject, "roles": roles }))
        .expect("mint_token: HS256 encode")
}
```

Qué acaba de pasar, bloque a bloque:

- La línea `use` extrae toda la superficie de tokens de `firefly::security`: la
  fachada reexporta el crate de seguridad del framework, así que no hay ninguna
  dependencia nueva que añadir a `Cargo.toml`.
- `JwtService::new(secret)` construye un servicio HS256 sobre el secreto. La
  construcción acepta cualquier `AsRef<[u8]>`, así que la clave en línea (una
  cadena de bytes) funciona directamente.
- `encode` firma una carga útil JSON y —este es el detalle de peso—
  **inyecta un claim `exp`** cuando la carga útil no lo tiene, con un valor por
  defecto de una hora en el futuro (`DEFAULT_EXPIRATION_SECONDS = 3600`). Cada
  token que Lumen emite tiene, por tanto, una vida útil acotada. Un token
  emitido *sin* `exp` (uno que nunca expiraría) se rechaza en el momento del
  decode, porque `decode` lista `exp` como claim obligatorio.
- `mint_token` es el helper que llaman las pruebas HTTP para obtener una
  credencial que el verificador aceptará. El `.expect(...)` es seguro aquí:
  firmar una forma de claim fija y bien formada no puede fallar.

> **Tip** **Punto de control.** Una rápida prueba mental en seco:
> `mint_token("u-alice", &["CUSTOMER"])` devuelve una cadena de tres segmentos
> `header.payload.signature`. Decodificar su segmento intermedio (base64)
> mostraría `sub`, `roles` y un `exp` autoestampado aproximadamente una hora en
> el futuro.

## Paso 2 — Convertir el servicio en un `Verifier`

`JwtService` ya puede verificar: implementa directamente el trait `Verifier`.
Pero Lumen lo envuelve en un pequeño adaptador para que la *forma del error* sea
exactamente la que el `BearerLayer` quiere renderizar.

> **Note** **Término clave — `Verifier` (el puerto de autenticación).** Un
> `Verifier` es el *puerto* del servidor de recursos: dado un token en bruto, lo
> valida y devuelve una `Authentication` (el principal, el nombre de usuario,
> los roles y los claims en bruto), o un `SecurityError` en caso de fallo. Es un
> trait, de modo que cualquier validador de tokens —el servicio HS256 de
> demostración, un verificador JWKS, tu propio closure— satisface el mismo
> contrato. `VerifierFn` adapta un simple closure asíncrono a uno de ellos.

Añade el constructor del verificador:

```rust,ignore
/// Builds the resource-server Verifier: validates the token's HS256
/// signature + expiry, then maps `sub` → principal and `roles` → roles onto an
/// Authentication. A bad signature / expired token surfaces as a
/// SecurityError::Verification, which the BearerLayer renders as a
/// `401 application/problem+json`.
pub fn build_verifier() -> impl Verifier {
    VerifierFn(|token: String| async move {
        jwt_service()
            .to_authentication(&token)
            .map_err(|e: SecurityError| SecurityError::verification(format!("invalid token: {e}")))
    })
}
```

Qué acaba de pasar: `VerifierFn(closure)` envuelve un simple closure `async`
como un `Verifier`. El closure delega en `JwtService::to_authentication`, que
decodifica el token y mapea sus claims sobre una `Authentication`: `sub` se
convierte en el principal, el array `roles` se convierte en los roles y se
conserva cada claim decodificado. Cualquier fallo (firma incorrecta, expirado,
`exp` ausente) se reenvuelve como `SecurityError::Verification(..)`; el
`BearerLayer` lo convierte en el problem 401 canónico.

> **Note** **Término clave — `Authentication`.** `Authentication` es el llamante
> resuelto que inspecciona el resto de la pila. Es el análogo en Rust del objeto
> `Authentication` de Spring Security. Sus campos:
>
> | Field       | Type                                  | From the claim                |
> |-------------|---------------------------------------|-------------------------------|
> | `principal` | `String`                              | `sub`                         |
> | `username`  | `String`                              | `preferred_username` / `name`, else `sub` |
> | `roles`     | `Vec<String>`                         | `roles`                       |
> | `authorities` | `Vec<String>`                       | `permissions` (and OAuth2 scopes) |
> | `claims`    | `HashMap<String, serde_json::Value>`  | every decoded claim           |
>
> Sus helpers cubren las comprobaciones habituales: `has_role(r)`,
> `has_any_role(&[..])`, `has_authority(a)` (que casa con un rol *o* con un
> permiso/scope de grano fino), `has_any_authority(&[..])`, y el constructor
> `Authentication::anonymous()`.

Una prueba unitaria afirma el viaje de ida y vuelta directamente: emite un
token, lo verifica y confirma que el principal y el rol sobrevivieron:

```rust,ignore
#[tokio::test]
async fn mint_then_verify_roundtrips_claims() {
    use firefly::security::Authentication;
    let token = mint_token("u-alice", &[CUSTOMER_ROLE]);
    let auth: Authentication = build_verifier().verify(&token).await.unwrap();
    assert_eq!(auth.principal, "u-alice");
    assert!(auth.has_role(CUSTOMER_ROLE));
}
```

Un token manipulado (`"not.a.jwt"`) o uno firmado con la clave incorrecta se
rechaza con `SecurityError::Verification`: dos pruebas negativas en `security.rs`
lo demuestran:

```rust,ignore
#[tokio::test]
async fn tampered_token_is_rejected() {
    let err = build_verifier().verify("not.a.jwt").await.unwrap_err();
    assert!(matches!(err, SecurityError::Verification(_)));
}
```

> **Tip** **Punto de control.** Ejecuta `cargo test mint_then_verify` (o el
> módulo `security` completo). La prueba de ida y vuelta pasa, y las dos pruebas
> de rechazo confirman que una credencial incorrecta nunca se resuelve a una
> `Authentication`. La autenticación ya funciona de extremo a extremo, antes de
> cualquier cableado HTTP.

## Paso 3 — Componer el `BearerLayer` y la `FilterChain` RBAC

`JwtService` responde a *¿quién es este llamante?*; la `FilterChain` responde a
*¿puede hacer esto?* La cadena casa las rutas de la petición con reglas en orden
de declaración —**gana la primera coincidencia**— y renderiza un 401 (sin
credencial o con credencial inválida) o un 403 (autenticado pero con privilegios
insuficientes). Lumen compone la capa bearer y la cadena en una sola función.

> **Note** **Término clave — `BearerLayer`.** El `BearerLayer` es el middleware
> tower que realiza la autenticación en el cable: lee la cabecera
> `Authorization: Bearer <token>`, llama al `Verifier` y almacena la
> `Authentication` resultante en la petición antes de que se ejecute la cadena.
> Es el análogo en Rust del filtro de autenticación por bearer token de Spring
> Security.

> **Note** **Término clave — `FilterChain`.** La `FilterChain` es el matcher de
> autorización basado en rutas, el análogo en Rust de las reglas de autorización
> por URL de Spring Security (`authorizeHttpRequests`). La construyes con
> llamadas `permit` / `require` / `permit_method`; cada una añade una regla
> ordenada.

Añade la función de composición:

```rust,ignore
/// Builds the BearerLayer + FilterChain that protect the service.
///
/// | Route                                          | Rule                  |
/// |------------------------------------------------|-----------------------|
/// | `GET  /api/v1/wallets/:id`                      | permit (public read)  |
/// | `GET  /actuator/*`                              | permit (management)   |
/// | `POST /api/v1/wallets`                          | require `CUSTOMER`    |
/// | `POST /api/v1/wallets/:id/deposit` / `withdraw` | require `CUSTOMER`    |
/// | `POST /api/v1/transfers`                        | require `CUSTOMER`    |
pub fn security_layers() -> (BearerLayer, FilterChain) {
    // `allow_anonymous` lets an unauthenticated request reach the chain; the
    // chain (not the bearer layer) then decides — a 401 on a `require` route
    // without a valid token, a pass on a permitted route.
    let bearer = BearerLayer::new(BearerConfig::new(build_verifier()).allow_anonymous(true));
    let chain = FilterChain::new()
        .permit_method("GET", "/api/v1/wallets")
        .permit("/actuator/")
        .require("/api/v1/wallets", &[CUSTOMER_ROLE])
        .require("/api/v1/transfers", &[CUSTOMER_ROLE])
        .any_request_permit();
    (bearer, chain)
}
```

Dos decisiones de diseño merecen detenerse en ellas, porque deciden quién recibe
un 401 frente a quién se cuela:

- **`allow_anonymous(true)` en la capa bearer.** Con ese ajuste, una petición
  sin cabecera `Authorization` *no* se rechaza en la capa bearer: llega a la
  cadena llevando una `Authentication` anónima. Eso mantiene un único tomador de
  decisiones: la `FilterChain` decide en cada ruta. Un `GET` público pasa; una
  ruta `require` sin token válido se convierte en un 401. Sin `allow_anonymous`,
  la capa bearer rechazaría el tráfico anónimo *antes* de que la cadena pudiera
  permitir las lecturas públicas, así que la lectura pública de monedero se
  rompería.
- **El orden importa.** `permit_method("GET", "/api/v1/wallets")` y
  `permit("/actuator/")` van *primero*, de modo que las lecturas públicas y la
  superficie de gestión se deciden antes de que el `require("/api/v1/wallets",
  ...)` más amplio pudiera capturarlas. Gana la primera coincidencia, así que un
  permit más específico debe preceder a un require más amplio.
  `any_request_permit()` reabre entonces la cola sin coincidencias (consulta la
  advertencia de abajo).

> **Warning** En cuanto se declara cualquier regla, una `FilterChain` es
> **fail-closed**: una petición que no casa con ninguna regla se rechaza con un
> 403 (denegar por defecto, igual que Spring Security 6). Reabre la cola sin
> coincidencias explícitamente con `any_request_permit()` /
> `any_request_authenticated()` / `any_request_deny()`. Una cadena *sin* reglas
> en absoluto es un no-op y deja pasar todo, de modo que una cadena vacía nunca
> es un bloqueo total sorpresa, pero en el momento en que añades tu primera
> regla, todo lo que no nombraste queda denegado a menos que un catch-all lo
> reabra.

> **Tip** **Punto de control.** Recorre a mano cada ruta a través de la lista de
> reglas: `GET /api/v1/wallets/w-1` toca el primer `permit_method` y pasa; `GET
> /actuator/health` toca `permit("/actuator/")` y pasa; `POST /api/v1/wallets`
> cae más allá de ambos permits hasta `require("/api/v1/wallets", [CUSTOMER])`;
> una ruta sin coincidencia como `GET /favicon.ico` alcanza
> `any_request_permit()` y pasa. Si reordenaras mentalmente los requires por
> encima de los permits, la lectura pública exigiría ahora un token: esa es la
> trampa de "gana la primera coincidencia" en acción.

## Paso 4 — Cablear las capas como beans

Lumen **no** aplica las capas de seguridad a mano. La `FilterChain` y el
`BearerLayer` se declaran cada uno como un `#[bean]` en `LumenBeans` —el
contenedor `#[derive(Configuration)]` de `src/web.rs` que has ido haciendo
crecer desde el capítulo de DI—, y `FireflyApplication` los autodescubre y los
aplica. Este es el análogo en Rust del bean `SecurityFilterChain` de Spring:
declarar el bean *es* el cableado.

> **Note** **Término clave — la seguridad como beans descubiertos.** En Spring
> Boot registras un `@Bean` `SecurityFilterChain` y el framework lo aplica;
> nunca llamas a un método `with_security(...)`. Firefly funciona igual: un bean
> `FilterChain` y un bean `BearerLayer` se autodescubren en el arranque y se
> aplican como capas sobre el router. No hay ninguna llamada a
> `.with_security(...)` ni un `.layer(bearer)` manual en el código de la app.

Añade los dos métodos bean al bloque existente `#[bean] impl LumenBeans`:

```rust,ignore
// samples/lumen/src/web.rs — inside #[bean] impl LumenBeans { ... }
use firefly::security::{BearerLayer, FilterChain};

/// The HTTP security filter chain (path-based RBAC) — the Spring
/// `SecurityFilterChain` bean. `FireflyApplication` auto-discovers + applies it.
#[bean]
fn security_filter_chain(&self) -> FilterChain {
    crate::security::security_layers().1
}

/// The bearer-token authentication layer — auto-discovered + layered onto
/// the API by `FireflyApplication`.
#[bean]
fn bearer_layer(&self) -> BearerLayer {
    crate::security::security_layers().0
}
```

Qué acaba de pasar, y qué hace el framework con ello en el arranque:

- Cada método `#[bean]` declara un componente para que el contenedor lo
  construya. `security_layers()` devuelve la tupla `(BearerLayer, FilterChain)`;
  un bean devuelve `.0` y el otro `.1`.
- En el arranque, `run()` resuelve el bean `FilterChain` y lo asigna en la pila
  web, luego resuelve el bean `BearerLayer` y lo aplica como capa alrededor de
  todo el router para que la cadena siempre vea una `Authentication` poblada.
- La cadena se ejecuta *dentro* del borde heredado de correlación /
  security-headers / CORS, de modo que incluso una respuesta 401 lleva esas
  cabeceras y un identificador de correlación. La capa bearer va por *fuera*:
  axum ejecuta primero la última capa añadida, así que el orden es **autenticar
  y luego autorizar**.

Declarar los dos beans es el cableado *completo*: nada de `with_security`,
ninguna llamada a `apply_middleware`, ninguna edición en `main`. Esta es la
propiedad de "sin cambios en `main`" del [Arranque rápido](./02-quickstart.md)
en acción: la seguridad no es más que más beans para que el framework los
descubra.

> **Tip** **Punto de control.** Ejecuta `cargo run` y lee la línea
> `:: beans ::` del informe de arranque: `security_filter_chain` y
> `bearer_layer` aparecen ahora en el inventario de beans descubiertos. Luego
> `curl -i -X POST localhost:8080/api/v1/wallets
> -H 'content-type: application/json' -d '{"owner":"mallory","openingBalance":10}'`:
> obtienes un `401` con `content-type: application/problem+json`, porque la
> mutación requiere ahora un token `CUSTOMER`. La lectura pública sigue
> funcionando: `curl localhost:8080/api/v1/wallets/anything` ya no se rechaza por
> falta de token (devuelve un 404 por un id desconocido, que es un resultado
> distinto, a nivel de negocio).

## Paso 5 — Empujar la autorización hasta un método

La `FilterChain` protege *rutas*. Pero la autorización suele ser una propiedad de
un *método de servicio*: una operación de dominio que llaman varios handlers, un
trabajo programado y un handler CQRS. Empujar la comprobación hasta el método
significa que se mantiene sin importar cómo se alcance la operación, y la tabla
de rutas queda centrada en las rutas.

Firefly hace esto con dos macros de atributo y un **contexto de seguridad
ambiental**. Las macros declaran la regla; el contexto transporta la
`Authentication` del llamante a través de la pila de llamadas, de modo que el
método nunca tiene que pasar un argumento ni tocar la `Request`.

> **Note** **Término clave — contexto de seguridad ambiental.** El *contexto
> ambiental* es una ranura task-local que contiene la `Authentication` actual,
> el análogo en Rust del `SecurityContextHolder` de Spring y su thread-local. El
> `BearerLayer` lo instala durante la duración de cada petición, de modo que
> cualquier método alcanzado aguas abajo puede leer al llamante sin que este
> viaje en cada firma de función. Como la ranura es task-local, anida
> limpiamente y nunca se filtra entre tareas lanzadas (spawned).

### `#[firefly::pre_authorize(...)]`

`#[firefly::pre_authorize(...)]` protege una función *antes* de que se ejecute su
cuerpo. Se adjunta a cualquier función que devuelva `Result<T, E>` donde `E:
From<firefly_security::SecurityError>`, y lee la `Authentication` ambiental para
decidir. Las reglas:

| Rule                          | Passes when                                         |
|-------------------------------|-----------------------------------------------------|
| *(empty)* / `authenticated`   | a real (non-anonymous) caller is in scope (default) |
| `role = "ADMIN"`              | the caller has role `ADMIN`                         |
| `any_role = ["A", "B"]`       | the caller has *any* of the listed roles            |
| `authority = "wallet:write"`  | the caller holds that authority (role or scope)     |
| `any_authority = ["a", "b"]`  | the caller holds *any* of the listed authorities    |

En caso de denegación, la macro retorna de forma temprana con `Err(..)`: un
`SecurityError` `Unauthenticated` cuando no hay ningún llamante en el ámbito, y
uno `Forbidden` cuando hay un llamante presente pero las autoridades no coinciden.
El `?` dentro del código generado propaga ese error a través de tu impl
`From<SecurityError>`.

```rust,ignore
use firefly_security::SecurityError;

/// Only a CUSTOMER may withdraw. The check runs before any balance logic.
#[firefly::pre_authorize(role = "CUSTOMER")]
pub async fn withdraw(wallet: WalletId, amount: Money) -> Result<Wallet, WalletError>
where
    WalletError: From<SecurityError>,
{
    // ... domain logic; reached only for an authenticated CUSTOMER ...
}

/// A coarse "must be logged in" gate — the empty form is `authenticated`.
#[firefly::pre_authorize]
pub fn current_balance(wallet: WalletId) -> Result<Money, WalletError> {
    // ...
}

/// A fine-grained scope check rather than a role.
#[firefly::pre_authorize(authority = "wallet:approve")]
pub async fn approve(wallet: WalletId) -> Result<(), WalletError> {
    // ...
}
```

### `#[firefly::post_authorize(<bool expr>)]`

A veces solo puedes decidir *después* de tener el valor: "puedes leer este
monedero solo si es tuyo". `#[firefly::post_authorize(...)]` se adjunta a una
`async fn` que devuelve `Result<T, E>` y evalúa una expresión booleana una vez
que el cuerpo ha producido su `Ok(T)`. La expresión ve dos enlaces (bindings):

- `result`: un `&T`, el valor que la función está a punto de devolver (el
  *return object* de Spring).
- `auth`: un `&Authentication`, el llamante ambiental.

Si la expresión es `false`, el valor se **descarta** y la llamada se resuelve a
un error `Forbidden` en su lugar; si no hay ningún contexto activo en absoluto,
se resuelve a `Unauthenticated`:

```rust,ignore
/// A caller may fetch a wallet only if they own it.
#[firefly::post_authorize(result.owner == auth.principal)]
pub async fn get_wallet(id: WalletId) -> Result<Wallet, WalletError> {
    repo().load(id).await // produces Ok(Wallet); the rule then vets the owner
}
```

### Las funciones del contexto ambiental

Ambas macros leen la `Authentication` ambiental en lugar de un argumento. Ese
ámbito lo gestiona un pequeño conjunto de funciones en `firefly_security`, al que
se llega a través de la fachada como `firefly::security`:

```rust,ignore
use firefly::security::{
    with_authentication_scope, current_authentication, check_access,
    AccessRule, Authentication, SecurityError,
};

// Run `fut` with `auth` installed as the ambient caller for its whole duration.
let wallet = with_authentication_scope(auth, async {
    withdraw(id, amount).await // #[pre_authorize] inside sees `auth`
}).await?;

// Read the current caller anywhere downstream (None if no scope is active).
let who: Option<Authentication> = current_authentication();

// Imperative check when a macro doesn't fit — returns the Authentication on
// success, a SecurityError on failure.
let auth: Authentication = check_access(&AccessRule::Role("CUSTOMER"))?;
```

`AccessRule` es la forma en tiempo de ejecución de las reglas de las macros:
`AccessRule::Authenticated`, `Role(&str)`, `AnyRole(&[&str])`, `Authority(&str)`
y `AnyAuthority(&[&str])`.

La recompensa es que **el `BearerLayer` instala el ámbito por ti**. En cada
petición —tanto la ruta verificada *como* la ruta anónima (`allow_anonymous`)— la
capa bearer envuelve la llamada aguas abajo en `with_authentication_scope`, de
modo que un método de servicio decorado con `#[pre_authorize]` funciona
correctamente aunque nunca vea la `Request`. Las reglas de URL y las reglas de
método se componen entonces: la `FilterChain` es tu perímetro grueso, las macros
de método son tu defensa en profundidad.

> **Tip** **Punto de control.** El invariante clave que debes tener en mente: un
> método con `#[pre_authorize]` llamado *fuera* de cualquier ámbito (por ejemplo,
> directamente desde un simple `#[test]` sin `with_authentication_scope_sync`)
> devuelve `Unauthenticated`: la macro falla en cerrado cuando no hay ningún
> llamante, exactamente igual que la cadena de rutas.

## Paso 6 — Demostrarlo de extremo a extremo sobre HTTP

La suite HTTP (`tests/http.rs`, en el ejemplo en `src/http_test.rs`) dirige el
router completamente cableado con `tower::ServiceExt::oneshot` y afirma el
comportamiento de seguridad directamente, sin enlazar ningún socket. El router
viene de `build_router`, que arranca la misma app que arranca `main()`:

```rust,ignore
// The testable in-process public router — every bean (including the
// FilterChain + BearerLayer) is auto-discovered, exactly as in `main`.
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

> **Note** **Costura de pruebas.** `bootstrap()` es el hermano de `run()` del
> [Arranque rápido](./02-quickstart.md): ensambla la misma app —beans de
> seguridad incluidos— pero devuelve un valor `Bootstrapped` *sin* servir, de
> modo que una prueba puede dirigir el router público cableado
> (`Bootstrapped::api_router`) en proceso. Conociste esto en [Tu primera API
> HTTP](./06-first-http-api.md); aquí permite que la suite ejercite el
> `BearerLayer` + `FilterChain` reales.

Un helper de peticiones construye la cabecera `Authorization` a partir de
`mint_token`, de modo que una petición autenticada es simplemente
`post(path, body, true)` y una no autenticada es `post(path, body, false)`:

```rust,ignore
fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &[CUSTOMER_ROLE]))
}

fn post(path: &str, body: serde_json::Value, auth: bool) -> Request<Body> {
    let mut b = Request::post(path).header("content-type", "application/json");
    if auth {
        b = b.header("authorization", bearer());
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()
}
```

Una mutación **sin** token es un problem 401:

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

Todas las demás pruebas de la suite —abrir, obtener, ingresar/retirar,
transferir— se ejecutan como el `CUSTOMER` emitido (pasan `true`), de modo que la
autenticación también se ejercita en la ruta feliz. Un 401 demuestra que el
perímetro está cerrado; las pruebas verdes de la ruta feliz demuestran que un
token válido sigue pasando.

> **Tip** **Punto de control.** Ejecuta `cargo test` para Lumen. La prueba
> `missing_token_is_401` es la prueba rojo-luego-verde de que la puerta de
> entrada está cerrada, y las pruebas de ida y vuelta del monedero confirman que
> un token `CUSTOMER` sigue abriéndola. Si la prueba del 401 falla con un
> `201 Created`, el bean `FilterChain` no se está descubriendo: confirma que
> ambos métodos `#[bean]` compilan dentro del bloque `LumenBeans`.

## Paso 7 — Mover la postura a la configuración

Lumen incrusta en línea su clave de firma y construye la capa bearer a mano
porque eso hace que el ejemplo sea ejecutable tal cual. Un servicio desplegado, en
cambio, lee su postura de seguridad desde la configuración, y `firefly_security`
la enlaza directamente: sin contenedor de DI, sin callback del framework. Las
propiedades viven bajo `firefly.security.*` y se enlazan mediante `serde`:

```rust,ignore
use firefly::security::{
    SecurityProperties, JwtProperties, BearerProperties,
    verifier_from_config, bearer_layer_from_config,
};
```

Una postura de servidor de recursos JWKS en `firefly.yaml`:

```yaml
firefly:
  security:
    jwt:
      jwk-set-uri: "https://idp.example.com/.well-known/jwks.json"
      issuer-uri: "https://idp.example.com/"
      audience: "lumen"
      algorithm: "RS256"
    bearer:
      header-name: "Authorization"
      allow-anonymous: true
```

Los structs reflejan esa forma; cada uno deriva `Default` + `Deserialize` con
`#[serde(default)]`, de modo que un campo ausente cae a su valor cero:

```rust,ignore
pub struct SecurityProperties {
    pub jwt: JwtProperties,
    pub bearer: BearerProperties,
}

pub struct JwtProperties {
    pub jwk_set_uri: String,
    pub issuer_uri: String,
    pub audience: String,
    pub secret: String,
    pub algorithm: String,
    pub expiration_seconds: u64,
}

pub struct BearerProperties {
    pub header_name: String,
    pub allow_anonymous: bool,
}
```

Dos funciones constructoras convierten las propiedades enlazadas en componentes
listos:

```rust,ignore
use std::sync::Arc;
use firefly::security::{Verifier, BearerLayer, SecurityError};

// Pick a verifier by what configuration provides — JWKS first, then HMAC,
// then nothing.
let verifier: Option<Arc<dyn Verifier>> = verifier_from_config(&props.jwt)?;

// The fully-assembled bearer layer (header name + anonymous policy applied),
// or None when no verifier is configured.
let bearer: Option<BearerLayer> = bearer_layer_from_config(&props)?;
```

Qué acaba de pasar: `verifier_from_config(&JwtProperties)` resuelve el
verificador por precedencia: un `jwk_set_uri` no vacío construye un verificador
de servidor de recursos JWKS (RS256); en su defecto, un `secret` no vacío
construye un verificador HMAC (HS256/384/512); en su defecto, devuelve `None`.
`bearer_layer_from_config(&SecurityProperties)` construye el verificador de la
misma manera y, si hay uno, lo envuelve en un `BearerLayer` con el nombre de
cabecera configurado y la política anónima ya aplicada: la misma capa que
`security_layers` construye a mano, pero obtenida desde la configuración en lugar
de a mano. Cambiar Lumen de la clave HMAC de demostración a un IdP de producción
se convierte en un cambio de configuración, sin tocar `security.rs`.

> **Note** **Término clave — JWKS (JSON Web Key Set).** Un *JWKS* es el conjunto
> de claves públicas que un proveedor de identidad publica en una URL conocida.
> Un servidor de recursos lo obtiene para verificar tokens RS256, indexando por
> el `kid` (id de clave) de cada token y cacheando el resultado. `JwksVerifier`
> es el `Verifier` plug-and-play del framework para esto: el mismo puerto
> `Verifier`, así que `security_layers` —y cada handler— queda intacto cuando
> intercambias el verificador HMAC de demostración por él. Esa es la promesa de
> "intercambia el adaptador, conserva el código" aplicada a la identidad.

## El resto de la capa de seguridad — el margen de crecimiento de Lumen

Lumen usa la vía rápida de clave simétrica. El mismo crate lleva la superficie de
producción a la que recurres a medida que madura un servicio de monedero real:

- **Verificación JWKS.** `JwksVerifier::new("https://idp.example.com/.well-known/jwks.json")`
  es un `Verifier` plug-and-play para tokens RS256 de un IdP externo (Keycloak,
  Auth0, Cognito): caché de `kid`, comprobaciones de `iss`/`aud` vía
  `.issuer(..)` / `.audience(..)`, `exp` obligatorio, y el mismo mapeo de claims
  `sub`/`roles`/`permissions`.
- **Guardas de método.** Para comprobaciones imperativas por handler, el módulo
  `guards` compone predicados tipados:
  `guards::has_role("CUSTOMER").or(guards::has_authority("wallet:approve"))`,
  y luego `guard.authorize(Some(&auth))?`: `Unauthenticated` sin principal,
  `Forbidden` si el predicado es falso. Para una grafía declarativa, prefiere las
  [macros de method-security](#step-5--push-authorization-down-to-a-method) de
  arriba.
- **Jerarquía de roles.** `RoleHierarchy::from_string("ADMIN > CUSTOMER")` parsea
  la especificación; adjúntala con `chain.with_role_hierarchy(..)` para que
  conceder `ADMIN` implique `CUSTOMER` en todas partes donde la cadena comprueba
  un rol.
- **Reglas por patrón.** Junto a las reglas por prefijo que usa Lumen, la cadena
  ofrece un DSL de globs al estilo fnmatch: `permit_pattern("/public/**")`,
  `require_pattern("/api/admin/**", &["ADMIN"])`,
  `require_authority("/api/reports/**", &["reports:read"])` y
  `authenticated("/api/**")`.
- **Sesiones.** Para flujos de navegador donde cerrar sesión debe significar
  cerrar sesión, el crate `firefly-session` añade un `SessionLayer` sobre un
  `SessionStore` (`MemorySessionStore` para desarrollo, un almacén respaldado por
  Redis para escalar). Un handler obtiene la `Session` de la petición con el
  extractor `SessionExt` y llama a `session.rotate_id().await` tras el login
  (defensa contra fijación de sesión), `session.set_attribute("user_id",
  &id).await`, y `session.invalidate().await` al cerrar sesión.
- **OAuth2.** El módulo `oauth2` cubre ambos lados: `ClientRegistration` (con
  presets `google` / `github` / `keycloak`) + `OAuth2LoginHandler` para el flujo
  de login con código de autorización (state + nonce + PKCE S256, validación del
  id-token OIDC), y un `AuthorizationServer` que emite tokens para
  `client_credentials` / `refresh_token`.
- **CSRF y contraseñas.** `CsrfLayer` implementa el patrón double-submit-cookie
  para flujos de sesión por cookie; `BcryptPasswordEncoder` (factor de trabajo
  por defecto 12) hashea credenciales, y `Argon2PasswordEncoder` (Argon2id, con
  los valores por defecto de OWASP vía `new()` —`m=19456` KiB, `t=2`, `p=1`— o
  `with_params(m, t, p)`) es la alternativa memory-hard detrás del *mismo* puerto
  `PasswordEncoder`. Tanto los hashes bcrypt `$2b$` como las cadenas PHC
  autodescriptivas `$argon2id$` son intercambiables con el adaptador
  `firefly-idp-internal-db` y con cualquier otro puerto.

Ambos codificadores comparten un mismo trait, así que son intercambiables:

```rust
use firefly_security::{Argon2PasswordEncoder, BcryptPasswordEncoder, PasswordEncoder};

let enc = BcryptPasswordEncoder::new(); // work factor 12 (the default)
let hash = enc.hash("s3cret").unwrap();
assert!(enc.verify("s3cret", &hash).unwrap());
assert!(!enc.verify("wrong", &hash).unwrap());

// Argon2id — the OWASP-preferred encoder, same PasswordEncoder port.
let argon = Argon2PasswordEncoder::new(); // OWASP defaults (m=19456, t=2, p=1)
let argon_hash = argon.hash("s3cret").unwrap();
assert!(argon_hash.starts_with("$argon2id$"));
assert!(argon.verify("s3cret", &argon_hash).unwrap());
```

## Resumen — qué cambió en Lumen

Este capítulo cerró la puerta de entrada abierta de Lumen sin añadir una
dependencia ni una línea de lógica de negocio a los handlers:

| Before | After this chapter |
|--------|--------------------|
| any caller could open/deposit/withdraw/transfer | mutating routes require a `CUSTOMER` JWT; reads and `/actuator/*` stay public |
| no token machinery | one HS256 `JwtService` mints and verifies, auto-stamping a one-hour `exp` |
| no authorization | a path-ordered, fail-closed RBAC `FilterChain` plus method-level `#[pre_authorize]` / `#[post_authorize]` |
| — | the `FilterChain` + `BearerLayer` `#[bean]`s, auto-discovered and layered by `FireflyApplication` — no `with_security` call |

También sabes ahora:

- Que `JwtService::encode` autoestampa un `exp` de una hora y `decode` rechaza
  cualquier token que no lo tenga, de modo que cada credencial está acotada.
- Que `build_verifier` convierte el servicio en un `Verifier` vía `VerifierFn`,
  mapeando `sub` → principal y `roles` → roles, y haciendo que un token
  incorrecto aflore como `SecurityError::Verification` → un problem 401.
- Que `security_layers` compone un `BearerLayer` (con `allow_anonymous(true)`) y
  una `FilterChain` de gana-la-primera-coincidencia, donde el orden de las reglas
  decide quién queda permitido.
- Que declarar la cadena y la capa como `#[bean]`s es el cableado *completo*: el
  framework asigna la cadena dentro del borde de correlación/cabeceras y aplica
  la autenticación bearer por fuera (autenticar y luego autorizar).
- Que la seguridad de método empuja la autorización a las operaciones de dominio
  a través de un contexto ambiental que el `BearerLayer` instala, de modo que un
  método de servicio aplica la regla sin ver nunca la `Request`.
- Que `verifier_from_config` / `bearer_layer_from_config` mueven toda la postura
  a `firefly.security.*`, de modo que la clave de demostración se convierte en un
  IdP de producción sin tocar el código.

## Ejercicios

1. **Añade una ruta solo para ADMIN.** Dale a Lumen una hipotética lista de
   colección `GET /api/v1/wallets` y protégela con
   `require_pattern("/api/v1/wallets", &["ADMIN"])` para que solo un `ADMIN`
   pueda listar todos los monederos, mientras `CUSTOMER` conserva el acceso a la
   lectura de un único monedero. Emite un token `ADMIN` en una prueba y afirma
   que un token `CUSTOMER` obtiene un 403.
2. **Jerarquía de roles.** Introduce un rol `SUPER` que implique `CUSTOMER`.
   Construye un `RoleHierarchy::from_string("SUPER > CUSTOMER")`, adjúntalo con
   `chain.with_role_hierarchy(..)`, emite un token solo con `SUPER`, y afirma que
   pasa la regla `require("/api/v1/wallets", &["CUSTOMER"])`.
3. **Intercambia JWKS.** Esboza un `build_verifier_jwks()` que devuelva un
   `JwksVerifier::new("https://idp.example.com/.well-known/jwks.json")` y confirma
   (leyendo el trait `Verifier`) que `security_layers` no necesita ningún otro
   cambio. ¿Por qué al resto de Lumen no le importa qué verificador recibió?
4. **Expiración.** Baja la vida útil del token con
   `JwtService::new(KEY).expiration_seconds(1)`, emite un token, espera dos
   segundos y afirma que el verificador devuelve ahora
   `SecurityError::Verification`.
5. **Seguridad de método en aislamiento.** Decora una función simple con
   `#[firefly::pre_authorize(role = "CUSTOMER")]`, llámala desde un `#[test]`
   *sin* un ámbito y afirma `Unauthenticated`, luego envuelve la llamada en
   `firefly::security::with_authentication_scope_sync(auth, || ...)` con una auth
   `CUSTOMER` y afirma que pasa.

## Adónde ir después

Un servicio seguro solo es de fiar si puedes *ver* lo que está haciendo. El
siguiente capítulo le da a Lumen ojos y oídos: logs estructurados, salud,
métricas y el panel de administración.

- Haz Lumen observable en **[Observabilidad](./15-observability.md)**: la
  superficie de gestión junto al perímetro de seguridad que acabas de construir.
- Revisita cómo el framework descubre y cablea beans como la `FilterChain` en
  **[Cableado de dependencias](./04-dependency-wiring.md)**.
- Dirige el router cableado en pruebas con `bootstrap()` en
  **[Pruebas](./18-testing.md)**.
