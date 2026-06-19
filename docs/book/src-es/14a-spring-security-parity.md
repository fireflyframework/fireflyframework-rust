# Paridad con Spring Security

Este apéndice relaciona la capa de seguridad de Firefly con **Spring Security 6
/ Spring Boot 3**: qué está soportado hoy, los comportamientos fieles a Spring
que conviene conocer y la hoja de ruta para el resto. Complementa el capítulo de
[Seguridad](./14-security.md), que es el tutorial práctico.

La capa de seguridad de Firefly es un port idiomático a Rust — capas `tower` en
lugar de filtros de servlet, *traits* en lugar de interfaces, funciones
constructoras en lugar del DSL `HttpSecurity` — así que *la paridad es
semántica, no literal*. Una función está «presente» cuando ofrece el
comportamiento de Spring, sea cual sea su forma.

## Cobertura de un vistazo

En la columna **Estado**, :status-supported: indica una función soportada,
:status-partial: un módulo soportado pero opcional (activable por *feature*), y
:status-planned: un elemento de la hoja de ruta.

| Área | Estado | Notas |
|------|--------|-------|
| Autorización de peticiones HTTP (`FilterChain`, RBAC, jerarquía de roles) | :status-supported: | Coincidencia por segmentos de ruta, denegar por defecto, gana la primera regla |
| Servidor de recursos Bearer / OAuth2 (JWT) | :status-supported: | JWKS con RSA + **EC (ES256/384)** + **EdDSA**; validación de `iss`/`aud`/`exp`/`nbf`; tolerancia de reloj de 60 s; *challenge* `WWW-Authenticate` (RFC 6750) |
| JWT simétrico (`JwtService`) | :status-supported: | HS256/384/512, `exp` obligatorio, tolerancia de reloj |
| Seguridad de método (`#[pre_authorize]` / `#[post_authorize]`) | :status-supported: | Funciona igual con autenticación **bearer *y* de sesión/OAuth2-login**; reglas por palabra clave **y** expresiones estilo SpEL sobre argumentos + principal |
| Profundidad de seguridad de método (`@PreFilter`/`@PostFilter`, `PermissionEvaluator`) | :status-supported: | Filtrado de colecciones `#[pre_filter]` / `#[post_filter]`; `PermissionEvaluator` + `has_permission` (`hasPermission(...)`), utilizable dentro de las expresiones |
| Comprobación de roles (`hasRole`) | :status-supported: | Acepta el prefijo `ROLE_` de Spring *y* nombres de rol sin prefijo |
| CORS | :status-supported: | Rechaza la combinación insegura de origen comodín + credenciales |
| Cabeceras de respuesta de seguridad | :status-supported: | HSTS, CSP, X-Frame-Options, X-Content-Type-Options, Referrer-Policy, Permissions-Policy; **HSTS solo en peticiones seguras** por defecto |
| CSRF (cookie de doble envío) | :status-supported: | El atributo `Secure` sigue el esquema de la petición; *bypass* para Bearer |
| Gestión de sesiones | :status-supported: | Rotación anti-fijación, control de concurrencia, registros distribuidos (Redis / **Postgres, con purga por TTL** / Mongo) |
| Codificación de contraseñas | :status-supported: | BCrypt + Argon2id; login en tiempo constante (sin oráculo temporal de enumeración de usuarios) |
| Login OAuth2 / OIDC | :status-supported: | Código de autorización + PKCE + state/nonce; **el `id_token` siempre se valida** (nunca se omite en silencio) |
| Login con token de un solo uso (enlace mágico) | :status-supported: | `oneTimeTokenLogin()` de Spring 6.4 — `OneTimeTokenService` + manejador de entrega + `/ott/generate` + `/login/ott` |
| WebAuthn / passkeys | :status-partial: | `webAuthn()` de Spring 6.4 — módulo `webauthn` opcional (ceremonias de registro y autenticación) |
| Adaptadores de IdP | :status-supported: | Internal-DB, Keycloak, Azure AD / Entra, AWS Cognito |
| Arquitectura de autenticación | :status-supported: | `AuthenticationManager`/`ProviderManager`/`AuthenticationProvider`, `UserDetails`+`DaoAuthenticationProvider`, `SecurityContextRepository`, `AuthenticationEventPublisher`, `AuthenticationEntryPoint`/`AccessDeniedHandler` conectables |
| Codificador de contraseñas delegado (migración `{id}`) | :status-supported: | `DelegatingPasswordEncoder` (`{bcrypt}`/`{argon2}`/`{noop}`) con re-hash en login (`upgrade_encoding`) |
| HTTP Basic (`httpBasic()`) | :status-supported: | `HttpBasicLayer` sobre la columna de autenticación; cabecera ausente pasa de largo, inválida/malformada → `401` + `WWW-Authenticate: Basic realm=…` |
| Form login (`formLogin()`) | :status-supported: | `form_login_routes` (`POST /login`), rotación del id de sesión (anti-fijación), manejadores de éxito/fallo conectables, redirección consciente de la petición guardada |
| Remember-me (`rememberMe()`) | :status-supported: | `TokenBasedRememberMeServices` — token firmado, con caducidad y ligado al hash de la contraseña; niveles de confianza `is_remembered()` / `is_fully_authenticated()` |
| `RequestCache` / `SavedRequest` | :status-supported: | `HttpSessionRequestCache` — la página previa al login se restaura tras autenticarse (solo redirección del mismo origen) |
| `SessionCreationPolicy` | :status-supported: | `Always`/`IfRequired`/`Never`/`Stateless`; `Stateless` instala el repositorio de contexto nulo para APIs de tokens |
| Múltiples cadenas de filtros | :status-supported: | `SecurityFilterChains` — gana el primer `RequestMatcher` que coincide (el `FilterChainProxy` de Spring) |
| Cliente OAuth2 saliente (`AuthorizedClientManager`) | :status-supported: | `OAuth2AuthorizedClientManager` + `OAuth2AuthorizedClientService` — grants client-credentials / refresh-token, caché de tokens y auto-refresco para llamadas salientes |
| Introspección de tokens opacos (RFC 7662) | :status-supported: | `RemoteTokenIntrospector` (`OpaqueTokenIntrospector`) — un `Verifier` de servidor de recursos intercambiable |
| Logout iniciado por RP (OIDC) | :status-supported: | `oidc_logout_url` — el logout redirige al `end_session_endpoint` del proveedor (`OidcClientInitiatedLogoutSuccessHandler`) |
| Servidor de autorización | :status-partial: | `AuthorizationServer` (client-credentials + refresh-token) montado vía `AuthorizationServerRouter` (`/oauth2/token`, metadatos RFC 8414); el grant authorization_code del lado servidor en la hoja de ruta |
| Autenticación LDAP / Active Directory | :status-partial: | Módulo `ldap` opcional: `LdapAuthenticationProvider` (bind auth + autoridades de grupo) + `ActiveDirectoryLdapAuthenticationProvider`, sobre `ldap3` (`ldapAuthentication()`) |
| ACL / seguridad de objetos de dominio · SAML2 | :status-planned: | Hoja de ruta (opcional) |

## Comportamientos fieles a Spring que conviene conocer

Coinciden con los valores por defecto de Spring Security 6 y pueden diferir de
un port ingenuo — cada uno tiene una vía de escape por configuración:

- **`hasRole('ADMIN')` coincide con la autoridad `ROLE_ADMIN`.** Un principal de
  Spring o JWT con autoridades prefijadas con `ROLE_` autoriza sin que tengas
  que quitar prefijos a mano; los nombres de rol sin prefijo siguen funcionando.
- **La seguridad de método funciona tras cualquier mecanismo de autenticación.**
  Un usuario autenticado por sesión u OAuth2-login satisface `#[pre_authorize]`
  / `current_authentication()`, no solo el portador de un token bearer.
- **HSTS se envía solo en peticiones seguras** (valor por defecto de
  `HstsHeaderWriter`). Configura `hsts_include_insecure` para forzarlo.
- **La cookie CSRF es `Secure` solo cuando la petición es segura**, de modo que
  el par de doble envío también funciona en desarrollo local sobre HTTP.
- **Un origen comodín de CORS combinado con credenciales se rechaza** en la
  construcción (`CorsLayer::try_new` devuelve un error) — usa orígenes
  explícitos.
- **La validación JWT/JWKS tolera 60 s de desfase de reloj** y valida `nbf`; las
  claves JWKS EC y EdDSA se verifican, no solo RSA.
- **Un `id_token` de OIDC nunca se confía sin validación** — si no puede
  verificarse, el login falla en vez de recurrir a userinfo.
- **Las reglas de autorización por prefijo de ruta respetan los segmentos**:
  `permit("/api")` coincide con `/api` y `/api/...` pero no con `/api-internal`.
- **El login con usuario desconocido consume un tiempo de bcrypt comparable** al
  de una contraseña incorrecta, cerrando el oráculo temporal de enumeración.

## Form login, HTTP Basic y remember-me

Los mecanismos clásicos de autenticación web, fieles a los valores por defecto
de Spring:

- **HTTP Basic** — `HttpBasicLayer::new(manager)` lee `Authorization: Basic …` y
  autentica mediante el `AuthenticationManager` del Nivel 1. Una cabecera
  **ausente** pasa de largo (para que una capa de sesión o bearer tome el
  relevo); una **inválida o malformada** se rechaza con `401` y un *challenge*
  `WWW-Authenticate: Basic realm="…"` — el `BasicAuthenticationFilter` de Spring.
- **Form login** — `form_login_routes(state)` monta `POST /login`
  (`username` + `password` codificados como formulario), rota el id de sesión al
  tener éxito (anti-fijación) **antes** de persistir el contexto, y luego
  redirige. Las respuestas de éxito/fallo son intercambiables
  (`FormLoginSuccessHandler` / `FormLoginFailureHandler`) y el camino de éxito es
  consciente de la petición guardada.
- **Remember-me** — `TokenBasedRememberMeServices` acuña un token de cookie
  firmado y con caducidad, ligado al hash de la contraseña del usuario y a una
  clave del servidor (el `TokenBasedRememberMeServices` de Spring): un cambio de
  contraseña, un reloj más allá de la caducidad, un token manipulado o una clave
  incorrecta lo rechazan. Un contexto recordado está *autenticado pero no
  totalmente autenticado* — `is_remembered()` es `true` e
  `is_fully_authenticated()` es `false`, de modo que una ruta sensible puede
  exigir un login fresco (`isFullyAuthenticated()` de Spring).
- **Caché de peticiones** — cuando el *entry point* envía a un usuario no
  autenticado a iniciar sesión, `HttpSessionRequestCache` recuerda la página que
  quería; el form login lo devuelve allí en lugar del destino por defecto (el
  `SavedRequestAwareAuthenticationSuccessHandler` de Spring). Solo se respetan
  destinos del **mismo origen** — una ruta guardada se rechaza si pudiera
  redirigir fuera del sitio.
- **Política de creación de sesión** — `SessionCreationPolicy::{Always,
  IfRequired, Never, Stateless}` elige si la capa de seguridad persiste su
  contexto en la sesión; `Stateless` (APIs de tokens) instala el repositorio de
  contexto nulo.
- **Múltiples cadenas de filtros** — `SecurityFilterChains` enruta cada petición
  a la primera cadena cuyo `RequestMatcher` (p. ej.
  `PathRequestMatcher::new("/api")`) coincide, de modo que un `/api/**` blindado
  y una superficie web permisiva coexisten — el `FilterChainProxy` de Spring.

## Seguridad de método

`#[pre_authorize]` / `#[post_authorize]` protegen un método de servicio frente
al principal ambiente — sin `Request` en la firma. Además de las reglas por
palabra clave (`role = "ADMIN"`, `any_authority = [..]`), aceptan
**expresiones**, el análogo en Rust del SpEL de Spring:

- **Enlace de argumentos + principal** — un `#[pre_authorize(...)]` que no es una
  palabra clave es una expresión booleana de Rust evaluada *antes* del cuerpo con
  los parámetros del método y `auth` (un `&Authentication`) a la vista:
  `#[pre_authorize(auth.has_role("ADMIN") || auth.principal == owner)]`
  (el `@PreAuthorize("#owner == authentication.name")` de Spring).
  `#[post_authorize]` enlaza `result` + `auth` sobre el valor de retorno.
- **`PermissionEvaluator`** — registra uno a nivel de proceso con
  `set_permission_evaluator` y luego llama a
  `has_permission(auth, target, permission)` dentro de cualquier expresión
  pre/post (el `hasPermission(#obj, 'read')` de Spring). Sin evaluador
  registrado, todo permiso se **deniega** (cierre seguro).
- **`#[pre_filter]` / `#[post_filter]`** — filtran una colección por un predicado
  por elemento: `#[post_filter(element.owner == auth.principal)]` descarta del
  `Vec` devuelto las filas que el llamante no posee; `#[pre_filter(items, …)]`
  hace lo mismo con un argumento `mut` antes del cuerpo (el
  `@PreFilter`/`@PostFilter` de Spring, donde `element` es el `filterObject`).

Las cuatro fallan en cerrado: sin contexto ambiente se deniega con
`Unauthenticated`, y una expresión falsa con `Forbidden`.

## Ecosistema OAuth2

Más allá del flujo de login en navegador (auth-code + PKCE + OIDC), Firefly
cubre el ecosistema OAuth2 más amplio:

- **Introspección de tokens opacos (RFC 7662)** — `RemoteTokenIntrospector`
  (el `OpaqueTokenIntrospector` de Spring) valida tokens bearer no-JWT contra el
  endpoint `/introspect` del servidor de autorización y mapea la respuesta
  `active` a un `Authentication`. Implementa `Verifier`, así que se conecta a un
  `BearerLayer` como alternativa a la verificación JWT local. Falla en cerrado.
- **Cliente saliente (`AuthorizedClientManager`)** —
  `OAuth2AuthorizedClientManager` + `OAuth2AuthorizedClientService` obtienen,
  **cachean** y **auto-refrescan** los tokens de acceso que la app necesita para
  llamar a servicios aguas abajo (client-credentials para servicio-a-servicio,
  refresh-token para llamadas delegadas), reusando un token hasta que se acerca
  su caducidad.
- **Logout iniciado por RP (OIDC)** — cuando el proveedor de login anuncia un
  `end_session_endpoint`, `POST /logout` redirige el navegador allí con un
  `id_token_hint` + `post_logout_redirect_uri` para que la sesión termine también
  en el IdP (el `OidcClientInitiatedLogoutSuccessHandler` de Spring).
- **Servidor de autorización** — `AuthorizationServer` (client-credentials +
  refresh-token, HS256) se monta sobre HTTP con `AuthorizationServerRouter`:
  `POST /oauth2/token` (RFC 6749) y `GET /.well-known/oauth-authorization-server`
  (metadatos RFC 8414). El grant authorization_code del lado servidor es un
  seguimiento.

## Login sin contraseña

Firefly incluye los dos mecanismos sin contraseña de Spring Security 6.4:

- **Token de un solo uso (enlace mágico)** — `ott_login_routes` expone
  `POST /ott/generate` (acuña un token de un solo uso con caducidad y lo entrega
  a tu manejador) y `GET /login/ott?token=…` (lo canjea, rota la sesión y
  establece el contexto de seguridad). El manejador por defecto solo registra
  que se emitió un token — conecta un manejador real de email/SMS en producción.
- **WebAuthn / passkeys** — el módulo `webauthn` opcional ofrece las ceremonias
  de registro y autenticación (`/webauthn/register/options`,
  `/webauthn/register`, `/webauthn/authenticate/options`, `/login/webauthn`)
  sobre `webauthn-rs`, almacenando credenciales mediante un repositorio
  conectable.

## LDAP / Active Directory

El módulo `ldap` opcional (`--features ldap`, trae `ldap3`) autentica
credenciales usuario/contraseña contra un directorio — el `ldapAuthentication()`
de Spring. Ambos proveedores son `AuthenticationProvider`, así que se conectan
al `ProviderManager` del Nivel 1:

- **`LdapAuthenticationProvider`** — **autenticación por bind**: busca el DN del
  usuario bajo una base con un filtro (`(uid={0})`, el usuario escapado según
  RFC 4515), hace bind como ese DN con la contraseña (el directorio la verifica),
  y luego mapea la pertenencia a grupos (`(member={0})`) a autoridades
  `ROLE_<GRUPO>` (el `BindAuthenticator` + `DefaultLdapAuthoritiesPopulator` de
  Spring).
- **`ActiveDirectoryLdapAuthenticationProvider`** — hace bind como el
  `userPrincipalName` (`usuario@dominio`) y mapea los grupos `memberOf` del
  usuario a roles.

Las operaciones LDAP están tras un puerto `LdapOperations` (adaptador real:
`Ldap3Operations`), de modo que la lógica se prueba sin un directorio real.
Comportamientos de seguridad, fieles a Spring y verificados por una revisión
adversarial previa al *release*:

- Una **contraseña vacía se rechaza antes del bind** — un bind simple con
  contraseña vacía es un bind anónimo que la mayoría de directorios aceptan (un
  *bypass* de autenticación).
- El usuario/DN se **escapa según RFC 4515** en cada filtro (a salvo de
  inyección LDAP), y usuario-desconocido / contraseña-incorrecta devuelven el
  **mismo valor de error**.
- Una **búsqueda de usuario ambigua** (más de una entrada coincidente) se rechaza
  en vez de hacer bind contra una primera coincidencia arbitraria — la
  `IncorrectResultSizeDataAccessException` de Spring.
- Un **error de directorio al poblar autoridades** falla el inicio de sesión en
  lugar de autenticar en silencio sin roles, y una **entrada de directorio
  malformada** se convierte en un error limpio en vez de abortar la petición.

## Hoja de ruta

La paridad se entrega por niveles, cada uno un incremento:

1. **Endurecimiento (hecho)** — los comportamientos fieles a Spring anteriores.
2. **Columna vertebral de autenticación (hecho)** — `AuthenticationManager` /
   `ProviderManager`, `DaoAuthenticationProvider` + `UserDetails`,
   `SecurityContextRepository`, `DelegatingPasswordEncoder`, eventos de
   autenticación, manejadores conectables de entry-point / access-denied.
3. **Mecanismos web (hecho)** — form login, HTTP Basic, remember-me,
   `RequestCache` / `SavedRequest`, `SessionCreationPolicy`, múltiples cadenas de
   filtros.
4. **Profundidad de seguridad de método (hecho)** — enlace de
   argumentos/principal estilo SpEL, `@PreFilter`/`@PostFilter`,
   `PermissionEvaluator`.
5. **Ecosistema OAuth2 (hecho)** — introspección de tokens opacos (RFC 7662),
   el gestor de clientes autorizados salientes, logout iniciado por RP, y el
   servidor de autorización montado sobre HTTP con metadatos RFC 8414. (El grant
   authorization_code del lado servidor queda como seguimiento.)
6. **Subsistemas grandes** — entregados de uno en uno (opcional). **LDAP /
   Active Directory (hecho)** — el módulo `ldap` opcional. Quedan **SAML2** y
   **ACL / seguridad de objetos de dominio**.
