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

| Área | Estado | Notas |
|------|--------|-------|
| Autorización de peticiones HTTP (`FilterChain`, RBAC, jerarquía de roles) | ✅ | Coincidencia por segmentos de ruta, denegar por defecto, gana la primera regla |
| Servidor de recursos Bearer / OAuth2 (JWT) | ✅ | JWKS con RSA + **EC (ES256/384)** + **EdDSA**; validación de `iss`/`aud`/`exp`/`nbf`; tolerancia de reloj de 60 s; *challenge* `WWW-Authenticate` (RFC 6750) |
| JWT simétrico (`JwtService`) | ✅ | HS256/384/512, `exp` obligatorio, tolerancia de reloj |
| Seguridad de método (`#[pre_authorize]` / `#[post_authorize]`) | ✅ | Funciona igual con autenticación **bearer *y* de sesión/OAuth2-login** |
| Comprobación de roles (`hasRole`) | ✅ | Acepta el prefijo `ROLE_` de Spring *y* nombres de rol sin prefijo |
| CORS | ✅ | Rechaza la combinación insegura de origen comodín + credenciales |
| Cabeceras de respuesta de seguridad | ✅ | HSTS, CSP, X-Frame-Options, X-Content-Type-Options, Referrer-Policy, Permissions-Policy; **HSTS solo en peticiones seguras** por defecto |
| CSRF (cookie de doble envío) | ✅ | El atributo `Secure` sigue el esquema de la petición; *bypass* para Bearer |
| Gestión de sesiones | ✅ | Rotación anti-fijación, control de concurrencia, registros distribuidos (Redis / **Postgres, con purga por TTL** / Mongo) |
| Codificación de contraseñas | ✅ | BCrypt + Argon2id; login en tiempo constante (sin oráculo temporal de enumeración de usuarios) |
| Login OAuth2 / OIDC | ✅ | Código de autorización + PKCE + state/nonce; **el `id_token` siempre se valida** (nunca se omite en silencio) |
| Login con token de un solo uso (enlace mágico) | ✅ | `oneTimeTokenLogin()` de Spring 6.4 — `OneTimeTokenService` + manejador de entrega + `/ott/generate` + `/login/ott` |
| WebAuthn / passkeys | 🧩 | `webAuthn()` de Spring 6.4 — módulo `webauthn` opcional (ceremonias de registro y autenticación) |
| Adaptadores de IdP | ✅ | Internal-DB, Keycloak, Azure AD / Entra, AWS Cognito |
| Arquitectura de autenticación | ✅ | `AuthenticationManager`/`ProviderManager`/`AuthenticationProvider`, `UserDetails`+`DaoAuthenticationProvider`, `SecurityContextRepository`, `AuthenticationEventPublisher`, `AuthenticationEntryPoint`/`AccessDeniedHandler` conectables |
| Codificador de contraseñas delegado (migración `{id}`) | ✅ | `DelegatingPasswordEncoder` (`{bcrypt}`/`{argon2}`/`{noop}`) con re-hash en login (`upgrade_encoding`) |
| Form login / HTTP Basic / remember-me | 🚧 | Hoja de ruta |
| Cliente OAuth2 (`AuthorizedClientManager`) / Servidor de autorización | 🚧 | Lado de login presente; cliente saliente y servidor de autorización montado en la hoja de ruta |
| ACL / seguridad de objetos de dominio · SAML2 · LDAP/AD | 🚧 | Hoja de ruta (crates opcionales) |

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

## Hoja de ruta

La paridad se entrega por niveles, cada uno un incremento:

1. **Endurecimiento (hecho)** — los comportamientos fieles a Spring anteriores.
2. **Columna vertebral de autenticación (hecho)** — `AuthenticationManager` /
   `ProviderManager`, `DaoAuthenticationProvider` + `UserDetails`,
   `SecurityContextRepository`, `DelegatingPasswordEncoder`, eventos de
   autenticación, manejadores conectables de entry-point / access-denied.
3. **Mecanismos web** — form login, HTTP Basic, remember-me, `RequestCache`,
   `SessionCreationPolicy`, múltiples cadenas de filtros.
4. **Profundidad de seguridad de método** — enlace de argumentos/principal estilo
   SpEL, `@PreFilter`/`@PostFilter`, `PermissionEvaluator`.
5. **Ecosistema OAuth2** — introspección de tokens opacos, gestor de clientes
   autorizados salientes, logout iniciado por RP, servidor de autorización
   montado.
6. **Subsistemas grandes** — ACL / seguridad de objetos de dominio, LDAP /
   Active Directory, SAML2.
