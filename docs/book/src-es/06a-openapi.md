# OpenAPI, Swagger UI y ReDoc

En [Tu primera API HTTP](./06-first-http-api.md) le diste a Lumen sus primeros
endpoints reales: un `#[rest_controller]` cuyos métodos `#[post]` / `#[get]` se
montan a sí mismos en el arranque. Este capítulo muestra lo que esas mismas
declaraciones *también* te dieron, gratis: un documento **OpenAPI 3.1** completo y
en vivo, una página de **Swagger UI** y una página de **ReDoc**, todo servido sin
una sola línea extra de código de aplicación. La especificación se genera a partir
del inventario en vivo que el framework ya descubrió —cada ruta de controlador más
cada DTO con `#[derive(Schema)]`— y `FireflyApplication` monta los endpoints de
documentación durante el arranque.

Nada en este capítulo cambia una sola línea de `samples/lumen`. El controlador que
escribiste ya lleva los resúmenes, las etiquetas y los DTOs con `#[derive(Schema)]`
que lee el generador. Lo que se busca aquí es *ver* que esas declaraciones de
enrutamiento **son** la documentación de la API, y aprender cómo enriquecer,
sobrescribir y exportar la especificación cuando lo necesites.

Al terminar este capítulo, serás capaz de:

- Llegar a las tres superficies de documentación de Lumen —la especificación
  OpenAPI, Swagger UI y ReDoc— y explicar por qué residen en el puerto de gestión
  y no en el público.
- Derivar un esquema de componente reutilizable a partir de un DTO con
  `#[derive(Schema)]`, y entender cómo respeta el renombrado de serde, los campos
  opcionales, los enums y los tipos anidados.
- Seguir cómo el cuerpo de la petición, el cuerpo de la respuesta y los parámetros
  de ruta/consulta/cabecera de una operación se *infieren* a partir de la propia
  firma de un handler.
- Adjuntar metadatos por operación (resumen, descripción, etiquetas, estado,
  `deprecated`) y sobrescribir la inferencia con `request = ` / `response = ` cuando
  una firma no puede expresar el DTO.
- Exportar la especificación con la CLI de `firefly` y generar a partir de ella un
  cliente Rust tipado.

## Conceptos que conocerás

Antes del primer endpoint, aquí están las ideas en las que se apoya este capítulo.
Cada una se reintroduce en contexto allí donde se usa por primera vez; esta es la
versión breve.

> **Note** **Término clave — OpenAPI.** *OpenAPI* (antes Swagger) es una
> descripción de una API REST neutral respecto al lenguaje y legible por máquina:
> cada path, operación, parámetro, cuerpo de petición, respuesta y esquema
> reutilizable, como un único documento JSON (o YAML). Las herramientas lo leen
> para renderizar documentación, generar clientes y ejecutar pruebas de contrato.
> Firefly emite **OpenAPI 3.1**.

> **Note** **Término clave — esquema de componente.** Un *esquema de componente* es
> un JSON Schema con nombre y reutilizable para un tipo de dato, registrado bajo
> `#/components/schemas/{Type}` y referenciado desde las operaciones mediante un
> `$ref`. El análogo de Java/Spring es un modelo anotado con `@Schema`; en Firefly
> incluyes un tipo con `#[derive(Schema)]`.

> **Note** **Término clave — Swagger UI / ReDoc.** Ambas son aplicaciones de
> navegador que renderizan un documento OpenAPI como documentación interactiva y
> legible para humanos. *Swagger UI* tiene un panel "Try it out" que lanza
> peticiones en vivo; *ReDoc* es una referencia limpia de tres paneles. Firefly
> sirve ambas, cada una apuntando a la misma especificación.

> **Note** **Término clave — el inventario.** Las macros de Firefly emiten
> descriptores en tiempo de compilación a un registro `inventory`: un
> `RouteDescriptor` por cada método `#[rest_controller]` y un `SchemaDescriptor`
> por cada tipo con `#[derive(Schema)]`. El generador de OpenAPI lee ese registro
> en lugar de volver a analizar tu código fuente. Así es como un framework de Rust
> obtiene un comportamiento de "escanear la aplicación" al estilo de springdoc sin
> reflexión en tiempo de ejecución.

## Paso 1 — Llega a las tres superficies de documentación

No escribes ni registras nada para obtener documentación de la API. Arranca Lumen
exactamente como en el [Quickstart](./02-quickstart.md):

```bash
cargo run
```

Entre las líneas de arranque, el framework imprime las URLs de documentación:

```text
:: api docs (management) :: swagger-ui http://0.0.0.0:8081/swagger-ui | redoc http://0.0.0.0:8081/redoc | spec http://0.0.0.0:8081/v3/api-docs
```

Abre cada una en un navegador (o haz `curl` a la especificación). Los endpoints, en
el puerto de **gestión**, por defecto son:

| Path | Sirve |
|------|--------|
| `/v3/api-docs` | la especificación JSON OpenAPI 3.1 (el path de springdoc en Spring Boot) |
| `/openapi.json` | la misma especificación (un alias de retrocompatibilidad) |
| `/swagger-ui` y `/swagger-ui.html` | Swagger UI, apuntando a la especificación |
| `/redoc` | ReDoc, apuntando a la especificación |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 250" role="img"
     aria-label="OpenAPI generation: rest_controller routes and derive Schema DTOs are harvested into one openapi.json spec served at /v3/api-docs, which Swagger UI and ReDoc render"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="24.0" y="42.5" width="200.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="40.0" width="200.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="124.0" y="62.0" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[rest_controller]</text><text x="124.0" y="76.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">routes + status codes</text>
<rect x="24.0" y="152.5" width="200.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="150.0" width="200.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="124.0" y="172.0" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[derive(Schema)]</text><text x="124.0" y="186.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">DTO component schemas</text>
<line x1="224.0" y1="65.0" x2="310.8" y2="106.5" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="318.0,110.0 308.8,110.6 312.7,102.5" fill="#b5531f"/>
<line x1="224.0" y1="175.0" x2="310.8" y2="133.5" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="318.0,130.0 312.7,137.5 308.8,129.4" fill="#b5531f"/>
<rect x="320" y="80" width="120" height="92" rx="9" fill="#d9c4a3" opacity="0.22"/>
<rect x="320" y="78" width="120" height="92" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/>
<rect x="338" y="94" width="84" height="9" rx="4.5" fill="#d4793a"/>
<rect x="338" y="114" width="84" height="6" rx="3" fill="#7a6450" opacity="0.6"/>
<rect x="338" y="127" width="72" height="6" rx="3" fill="#7a6450" opacity="0.6"/>
<rect x="338" y="140" width="60" height="6" rx="3" fill="#7a6450" opacity="0.6"/>
<rect x="338" y="153" width="48" height="6" rx="3" fill="#7a6450" opacity="0.6"/>
<text x="380.0" y="190.0" text-anchor="middle" font-size="11" font-weight="700" fill="#b5531f" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">openapi.json</text>
<text x="380.0" y="206.0" text-anchor="middle" font-size="10" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">(/v3/api-docs)</text>
<line x1="440.0" y1="110.0" x2="476.6" y2="95.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="484.0,92.0 478.3,99.2 474.9,90.9" fill="#b5531f"/>
<line x1="440.0" y1="140.0" x2="476.6" y2="155.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="484.0,158.0 474.9,159.1 478.3,150.8" fill="#b5531f"/>
<g transform="translate(486.0,80.0)"><rect x="0" y="0" width="88.0" height="26.0" rx="13.0" fill="#f6a821" opacity="0.95"/><text x="44.0" y="17.2" text-anchor="middle" font-size="12" font-weight="700" fill="#16110c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Swagger UI</text></g>
<g transform="translate(486.0,146.0)"><rect x="0" y="0" width="53.0" height="26.0" rx="13.0" fill="#f6a821" opacity="0.95"/><text x="26.5" y="17.2" text-anchor="middle" font-size="12" font-weight="700" fill="#16110c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ReDoc</text></g>
</svg>
<figcaption>Sin paso de generación de código, sin framework de anotaciones. En el arranque, <code>FireflyApplication</code> recolecta los atributos de enrutamiento y cada tipo con <code>#[derive(Schema)]</code> en un único documento OpenAPI&nbsp;3.1 (servido en <code>/v3/api-docs</code> en el puerto de gestión) y apunta a él Swagger&nbsp;UI y ReDoc.</figcaption>
</figure>

Lo que acaba de ocurrir: durante el pipeline de arranque (la etapa de montaje de
documentación que conociste en [Bootstrap](./04b-bootstrap.md)), `FireflyApplication`
construyó un documento OpenAPI a partir del inventario en vivo y fusionó un pequeño
router que sirve estos paths sobre la superficie de gestión. No hay ningún framework
de anotaciones que aprender más allá de los atributos de enrutamiento del capítulo 6,
ni paso de generación de código. Esta es la contraparte en Rust de springdoc-openapi.

> **Tip** **Punto de control.** Con `cargo run` en ejecución, `curl
> localhost:8081/v3/api-docs` devuelve un cuerpo JSON que empieza por
> `{"openapi":"3.1.0",...}`, y `http://localhost:8081/swagger-ui` renderiza la API
> del wallet en un navegador. Si `curl` conecta pero da 404, confirma que estás
> apuntando a `8081` (gestión), no a `8080` (público).

## Paso 2 — Entiende por qué la documentación reside en el puerto de gestión

Fíjate en que las URLs de arriba están todas en `:8081`, el puerto de gestión
—junto al actuator y al panel de administración— y **no** en la API pública de
`:8080`.

> **Note** **Término clave — superficie de gestión.** La *superficie de gestión* es
> el conjunto de endpoints HTTP operativos —health, info, métricas, admin y ahora
> la documentación de la API— servidos en un puerto separado de tu API de negocio,
> para operadores y herramientas en lugar de para usuarios finales. Esto refleja el
> puerto de gestión dedicado de Spring Boot Actuator.

Por qué separarlos: Swagger UI, ReDoc y la especificación en bruto exponen tu
superficie de API **completa** y cada esquema, una cuestión del plano de control.
Pertenecen allí donde los operadores ya acceden a `/actuator/*` y `/admin/`,
manteniendo el puerto público del plano de datos libre de endpoints de
introspección de la API.

Esa separación crea una arruga que el framework resuelve por ti. Como la
documentación se *carga* desde el origen de gestión (`:8081`) pero la API
*responde* en el puerto público (`:8080`), el documento declara la **URL base de la
API pública** como su `server` de OpenAPI. Así, el "Try it out" de Swagger UI y las
muestras de ReDoc apuntan a la API (`:8080`), no al origen de gestión desde el que
se cargaron. `FireflyApplication` deriva esa URL de la dirección de enlace de la API
—un host comodín como `0.0.0.0` no es utilizable por un cliente, así que recurre a
`localhost`:

```text
http://localhost:8080
```

Detrás de un proxy inverso querrás en su lugar una URL pública real. Define
`FIREFLY_OPENAPI_SERVER_URL` y sobrescribirá el valor derivado:

```bash
FIREFLY_OPENAPI_SERVER_URL=https://api.lumen.example cargo run
```

Lo que acaba de ocurrir: el `servers[0].url` de la especificación pasa a ser el
valor que proporcionaste, de modo que cada llamada de "Try it out" va a tu hostname
público. (Un path desconocido en **cualquiera** de los dos listeners sigue
respondiendo con el mismo 404 `application/problem+json` RFC 9457 que conociste en
el [capítulo 6](./06-first-http-api.md), así que la superficie de documentación
también degrada de forma limpia).

> **Tip** **Punto de control.** `curl -s localhost:8081/v3/api-docs | jq '.servers'`
> muestra una entrada cuya `url` es `http://localhost:8080` por defecto: la API
> pública, no el origen `:8081` desde el que la obtuviste.

## Paso 3 — Convierte un DTO en un esquema de componente con `#[derive(Schema)]`

Un tipo de dato se convierte en un `#/components/schemas/{Type}` reutilizable al
derivar `Schema`. Como Rust no tiene reflexión en tiempo de ejecución, el JSON
Schema se calcula **en tiempo de expansión de macro** recorriendo los campos del
struct, de modo que lo que acaba en la especificación se decide cuando compilas, no
en el arranque.

> **Note** **Término clave — `#[derive(Schema)]`.** Este derive es el análogo en
> Rust de un modelo Spring con `@Schema`. Lee el struct (o el enum sin campos) en
> tiempo de compilación, emite un fragmento de JSON Schema y lo envía al inventario
> para que el generador pueda registrarlo como un componente con nombre y hacerle
> `$ref` desde las operaciones.

Aquí está la vista del modelo de lectura de Lumen, exactamente como la escribiste en
`src/domain.rs`:

```rust,ignore
// src/domain.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WalletView {
    /// The wallet id.
    pub id: String,
    /// The owner's display name.
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied).
    pub version: i64,
}
```

El derive recorre los cuatro campos y registra un esquema equivalente a:

```json
{ "type": "object",
  "properties": {
    "id":      {"type": "string"},
    "owner":   {"type": "string"},
    "balance": {"type": "integer"},
    "version": {"type": "integer"}
  },
  "required": ["id", "owner", "balance", "version"] }
```

Lo que acaba de ocurrir: cada campo `String` se convirtió en `{"type":"string"}`,
cada `i64` en `{"type":"integer"}` y —como ninguno está envuelto en `Option`— los
cuatro aterrizaron en `required`. El mapeo refleja lo que produce un modelo
Java/Spring con `@Schema`:

- `String` / `str` / `char` → `string`; `bool` → `boolean`; todos los tipos enteros
  (`i8`…`u128`, `usize`, …) → `integer`; `f32` / `f64` → `number`.
- `Uuid` → `string` con `format: uuid`; los date-times de chrono / time → `string`
  con `format: date-time`; las fechas → `format: date`; las horas → `format: time`.
- `Option<T>` es un envoltorio transparente: describe `T` pero hace la propiedad
  **no requerida** (de modo que los opcionales se caen de la lista `required`).
- `Box<T>` / `Arc<T>` / `Rc<T>` también son transparentes; `Vec` / `HashSet` /
  `BTreeSet` / … → un `array` del esquema del elemento; `HashMap` / `BTreeMap` → un
  `object` abierto con `additionalProperties`.
- Cualquier *otro* tipo con nombre se asume que es un DTO hermano que también deriva
  `Schema`, y se emite como un `$ref`, de modo que un DTO anidado queda **enlazado**,
  no incrustado, y los dos esquemas de componente se componen.

> **Tip** **Punto de control.** `curl -s localhost:8081/v3/api-docs | jq
> '.components.schemas.WalletView'` imprime el esquema de objeto de arriba. Cada DTO
> que deriva `Schema` aparece bajo `.components.schemas`.

### El renombrado de serde se respeta

`#[derive(Schema)]` lee las directivas serde del struct para que los nombres de las
propiedades en el esquema coincidan con la forma del **cable** JSON —`rename`,
`rename_all` y `skip`— no con los identificadores de Rust. El `TransferResult` de
Lumen lleva renombrados de campos, y el esquema los sigue:

```rust,ignore
// src/transfer.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
pub struct TransferResult {
    pub status: String,
    pub from: String,
    pub to: String,
    pub amount: i64,
    #[serde(rename = "stepsExecuted")]
    pub steps_executed: Vec<String>,
    #[serde(rename = "stepsRolledBack")]
    pub steps_rolled_back: Vec<String>,
}
```

El esquema nombra las propiedades de array `stepsExecuted` / `stepsRolledBack` —el
JSON exacto que serializa el handler— no los identificadores de Rust en snake_case.
Un `#[serde(rename_all = "camelCase")]` a nivel de struct se aplica a cada campo de
la misma forma, y un campo con `#[serde(skip)]` se omite por completo del esquema.
La regla práctica: el esquema describe lo que va por el cable, así que siempre
coincide con tu JSON serializado.

> **Design note.** Por esto el esquema es fiel al cable sin que mantengas una
> segunda copia de los nombres de los campos: el único conjunto de atributos serde
> que controla la serialización controla también el esquema. No hay una anotación
> separada que mantener sincronizada, ni forma de que la documentación se desvíe de
> los bytes.

### Los enums sin campos se vuelven enumeraciones de cadenas

Un enum sin campos (de variantes unitarias) que deriva `Schema` emite una
enumeración `string` de JSON Schema, el tratamiento de springdoc para un `enum` de
Java. El renombrado de serde se respeta aquí también, de modo que los valores
permitidos coinciden con la forma del cable. La muestra por capas `lumen-ledger`
modela así el ciclo de vida de un wallet:

```rust,ignore
// lumen-ledger: interfaces/.../wallet_status.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
#[serde(rename_all = "lowercase")]
pub enum WalletStatus {
    #[default]
    Active,
    Frozen,
    Closed,
}
```

registra:

```json
"WalletStatus": { "type": "string", "enum": ["active", "frozen", "closed"] }
```

Lo que acaba de ocurrir: cada variante se convirtió en una cadena permitida,
pasada a minúsculas por el `rename_all` a nivel de struct. Un campo de DTO de este
tipo entonces hace `$ref` al componente enum registrado en lugar de convertirse en
una cadena sin tipo; el `WalletResponse` de `lumen-ledger` usa exactamente esto para
su campo `status: WalletStatus`, de modo que los dos esquemas de componente se
componen. (`#[derive(Schema)]` solo admite enums sin campos; un enum con datos en
una variante se rechaza en tiempo de compilación).

## Paso 4 — Deja que la macro infiera los modelos de petición y respuesta

**No** nombras los modelos de petición y respuesta en el atributo del verbo. La
macro los infiere a partir de la propia firma del handler, en tiempo de compilación:

- el **cuerpo de la petición** es el tipo interno del primer parámetro `Json<T>` *o*
  `Valid<T>` (de modo que el extractor validador también documenta su cuerpo), y
- la **respuesta** es el `Json<T>` que se encuentra dentro del tipo de retorno, tras
  desenvolver `WebResult<…>` / `Result<…>` y mirar a través de una tupla
  `(StatusCode, Json<T>)`.

Toma el handler `open` de Lumen, sin cambios respecto al capítulo 6:

```rust,ignore
// src/web.rs
#[post(
    "/wallets",
    summary = "Open a wallet",
    description = "Opens a new wallet for an owner with an optional opening balance.",
    status = 201
)]
async fn open(
    State(api): State<WalletApi>,
    Json(body): Json<OpenWallet>,
) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
    let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
    Ok((axum::http::StatusCode::CREATED, Json(view)))
}
```

Lo que acaba de ocurrir: a partir de la firma por sí sola, la macro registró
`OpenWallet` como el esquema de la petición (el parámetro `Json<OpenWallet>`) y
`WalletView` como el esquema de la respuesta (el `Json<WalletView>` dentro de la
tupla `(StatusCode, …)` dentro de `WebResult<…>`). Ambos derivan `Schema`, así que la
operación hace `$ref` a `#/components/schemas/OpenWallet` y
`#/components/schemas/WalletView`, sin declaración de petición/respuesta en el
atributo.

> **Note** Un `$ref` se emite **solo** cuando el tipo inferido es realmente un
> componente `#[derive(Schema)]` registrado. El `transfer_compliance` de Lumen
> devuelve `Json<serde_json::Value>`; `serde_json::Value` no es un esquema
> registrado, así que el generador no emite ningún `$ref` de petición/respuesta para
> él en lugar de referenciar un componente que no existe. El documento permanece
> válido sin importar lo que devuelvan tus handlers: nunca hay `$ref`s colgantes.

> **Tip** **Punto de control.** `curl -s localhost:8081/v3/api-docs | jq
> '.paths."/api/v1/wallets".post.requestBody'` muestra un `$ref` a
> `#/components/schemas/OpenWallet`, y la respuesta `201` hace `$ref` a `WalletView`.

### Los parámetros de ruta, consulta y cabecera también se infieren

La misma inferencia dirigida por la firma cubre los **parámetros** de operación, de
modo que Swagger UI y ReDoc renderizan una entrada para cada uno, sin una lista de
parámetros escrita a mano:

- Los parámetros de **ruta** vienen de la plantilla de la ruta: cada segmento `:id`
  (axum) / `{id}` se convierte en un parámetro `in: path` requerido. El
  `GET /wallets/:id` de Lumen obtiene un parámetro de ruta `id` requerido
  automáticamente.
- Los parámetros de **consulta** vienen de un extractor `Query<T>` /
  `ValidQuery<T>`: el generador expande los campos `#[derive(Schema)]` de `T` en un
  parámetro `in: query` cada uno (requerido si y solo si el campo no es opcional).
  Un argumento `PageRequest` añade los parámetros de consulta estándar de Spring Data
  `page` / `size` / `sort`.
- Los parámetros de **cabecera** se declaran en el atributo del verbo:
  `header("Idempotency-Key", required, description = "…")` emite un parámetro
  `in: header` (y el handler lo lee como cualquier cabecera de axum). Una declaración
  `query("…")` añade un parámetro de consulta extra de la misma forma.

El `WalletApi` de Lumen mantiene sus handlers simples —solo de ruta—, así que su
inferencia de parámetros son únicamente los segmentos `:id`. La historia más rica de
consulta/cabecera es lo que ejercita el `WalletController` de la muestra por capas
`lumen-ledger`. Su endpoint de listado paginado enlaza una consulta de filtro *y* el
resolutor de paginación del framework:

```rust,ignore
// lumen-ledger: web/.../wallet_controller.rs
#[get("/wallets/page", summary = "List wallets by status (paged)")]
async fn list_paged(
    State(api): State<WalletController>,
    Query(query): Query<StatusQuery>,
    PageRequest(pageable): PageRequest,
) -> WebResult<Json<Page<WalletResponse>>> {
    let page = api.service.list_by_status(query.status, pageable).await.map_err(service_to_web)?;
    Ok(Json(page))
}
```

Lo que acaba de ocurrir: `Query<StatusQuery>` expandió el único campo `status` del
esquema `StatusQuery` en un parámetro `in: query`, y `PageRequest` añadió `page`,
`size` y `sort`, de modo que Swagger UI renderiza cuatro entradas de consulta para
este endpoint con cero boilerplate de parámetros. Su handler `open` muestra la forma
de cabecera, declarando una cabecera de petición `Idempotency-Key` directamente en
el atributo del verbo:

```rust,ignore
// lumen-ledger: web/.../wallet_controller.rs
#[post(
    "/wallets",
    summary = "Open a wallet",
    status = 201,
    header("Idempotency-Key", description = "optional client-supplied key to make retries safe")
)]
async fn open(/* … */) -> WebResult<(StatusCode, Json<WalletResponse>)> { /* … */ }
```

— de modo que los llamadores ven y pueden rellenar la cabecera en Swagger UI, y el
handler la lee del `HeaderMap` como cualquier otra cabecera.

## Paso 5 — Adjunta metadatos por operación

Más allá del path, cada atributo de verbo admite metadatos opcionales que aterrizan
en la operación OpenAPI. La forma completa es:

```text
#[get("/x", summary = "…", description = "…", tags = ["A", "B"], status = 200, deprecated, request = T, response = T)]
```

| Argumento | Efecto en la operación |
|----------|-------------------------|
| `summary = "…"` | el resumen de una línea |
| `description = "…"` | la descripción más larga |
| `tags = ["A", "B"]` | etiquetas de agrupación (sobrescriben la etiqueta del controlador, abajo) |
| `status = 201` | el código de estado de éxito (por defecto 201 para `POST`, si no 200) |
| `deprecated` | marca la operación como `deprecated: true` (flag escueta; `deprecated = false` para desactivar) |
| `request = T` | el nombre del esquema del cuerpo de la petición — sobrescribe la inferencia |
| `response = T` | el nombre del esquema de la respuesta de éxito — sobrescribe la inferencia |

La operación `transfer` de Lumen usa summary, description, un `tags` explícito y un
`status`:

```rust,ignore
// src/web.rs
#[post(
    "/transfers",
    summary = "Transfer funds (saga)",
    description = "Moves funds between two wallets as a compensating saga (debit then credit).",
    tags = ["Transfers"],
    status = 200
)]
async fn transfer(
    State(api): State<WalletApi>,
    Json(body): Json<TransferRequest>,
) -> WebResult<Json<TransferResult>> { /* … */ }
```

Lo que acaba de ocurrir: la macro estampó el resumen, la descripción, la etiqueta
`Transfers` y el estado `200` en la operación, y luego *aun así* infirió
`TransferRequest` y `TransferResult` de la firma. Los metadatos y la inferencia se
componen: solo deletreas lo que la firma no puede expresar.

### Etiquetas a nivel de controlador

`#[rest_controller(tag = "…")]` establece una etiqueta por defecto para **cada**
operación del controlador, el análogo del `@Tag(name = …)` de Spring. Lumen etiqueta
toda su superficie de wallet:

```rust,ignore
// src/web.rs
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletApi { /* … */ }
```

La resolución de etiquetas por operación está en capas:

1. un `tags = [...]` explícito por método gana; si no,
2. se aplica el valor por defecto de `#[rest_controller(tag)]`; si no,
3. el generador deriva una etiqueta del nombre del tipo del controlador quitando un
   sufijo final `Api` / `Controller` (`WalletApi` → `Wallet`, `CatalogController`
   → `Catalog`).

Lumen establece la etiqueta del controlador explícitamente a `"Wallets"`, así que en
su especificación `open`, `get`, `deposit` y `withdraw` llevan la etiqueta
**Wallets** (el valor por defecto del controlador), mientras que `transfer`,
`transfer_compliance` y `transfer_2pc` llevan **Transfers** (su sobrescritura por
método `tags = ["Transfers"]`). Swagger UI agrupa las operaciones bajo esos dos
encabezados.

### Sobrescribir la inferencia con `request = ` / `response = `

Cuando un tipo de cuerpo no puede leerse de la firma —un handler que toma un
`axum::body::Bytes` en bruto, devuelve un `impl IntoResponse` o de otro modo oculta
su DTO— nómbralo explícitamente. `request = T` / `response = T` toman el **nombre**
del esquema (el último segmento de path del tipo, coincidiendo con aquello bajo lo
que `#[derive(Schema)]` lo registra) y **tienen prioridad** sobre la inferencia:

```rust,ignore
#[post("/import", summary = "Bulk import", request = ImportBatch, response = ImportReport)]
async fn import(/* a non-Json body */) -> impl axum::response::IntoResponse { /* … */ }
```

Lo que acaba de ocurrir: aunque la firma no revela ningún `Json<T>`, la operación
ahora hace `$ref` a `ImportBatch` e `ImportReport` (siempre que ambos deriven
`Schema`). Lumen nunca necesita esto —el cuerpo de cada handler es un `Json<T>` de un
tipo `#[derive(Schema)]`, así que la inferencia lo cubre—, pero la vía de escape está
ahí para los casos que una firma no puede expresar.

## Paso 6 — Lee el ejemplo trabajado de principio a fin

Juntándolo todo para el `WalletApi` de Lumen:

- El controlador es `#[rest_controller(path = "/api/v1", tag = "Wallets")]`.
- Sus DTOs con `#[derive(Schema)]` —`OpenWallet`, `WalletView`, `AmountBody`,
  `TransferRequest`, `TransferResult`, `TccTransferResult`— se convierten cada uno en
  una entrada `#/components/schemas/*` y son referenciados con `$ref` por las
  operaciones que los usan.
- La petición/respuesta de cada operación se infiere de su parámetro `Json<T>` y de
  su retorno; su summary / description / tags / status vienen del atributo del verbo;
  y las operaciones `transfers/*` se agrupan bajo **Transfers**.
- `transfer_compliance` toma `Json<TransferRequest>` (un esquema registrado, así que
  su petición hace `$ref` a `TransferRequest`) pero devuelve `Json<serde_json::Value>`,
  así que su respuesta no lleva **ningún** `$ref`, y eso es correcto, no una carencia.

Cada operación obtiene además una respuesta `default` RFC 9457 que referencia
`#/components/schemas/ProblemDetail`, que el generador siempre añade al documento.
Así, la forma de error uniforme del [capítulo 6](./06-first-http-api.md) queda
documentada automáticamente para cada endpoint: Swagger UI muestra una respuesta de
error en cada operación sin que escribas ninguna.

> **Tip** **Punto de control.** `curl -s localhost:8081/v3/api-docs | jq
> '.components.schemas | keys'` lista cada esquema registrado, incluyendo
> `ProblemDetail`. `jq '.paths."/api/v1/transfers".post.responses | keys'` muestra
> tanto la respuesta `200` como la `default` (problem).

## Paso 7 — Una tabla de descriptores, tres superficies

La tabla de descriptores de `#[rest_controller]` la leen **tres** superficies, de
modo que nunca pueden desviarse:

- el **documento OpenAPI** en `/v3/api-docs`,
- la tabla de rutas **`/admin/api/mappings`** del panel de administración
  ([Observabilidad y administración](./15-observability.md)), y
- el bloque `:: routes (N) ::` del **informe de arranque**
  ([Bootstrap](./04b-bootstrap.md)).

Añade una ruta, y las tres se actualizan a partir del mismo registro en el siguiente
build. El informe de arranque incluso imprime los recuentos de operaciones y
esquemas de componente para que puedas confirmar que la especificación está en vivo:

```text
:: openapi :: N operations | K component schemas (served at /v3/api-docs) ::
```

Lo que acaba de ocurrir: como el documento, la vista de mappings de admin y el log
de arranque leen todos un único inventario, "lo que hace la API" tiene una única
fuente de verdad. No hay un segundo archivo de especificación mantenido a mano que se
quede atrás respecto a tu código.

## Paso 8 — Exporta la especificación con la CLI

La CLI de `firefly` puede escribir un documento OpenAPI para herramientas y CI:

```bash
firefly openapi                              # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml
```

Hay una salvedad de alcance que vale la pena entender (cubierta en su totalidad en
[La CLI](./19-cli.md)). Un binario *compilado* no puede arrancar una aplicación
arbitraria para enumerar sus rutas en vivo: las rutas viven en el propio crate del
consumidor, y no hay contenedor de inyección de dependencias que introspeccionar
desde una herramienta genérica. Así que `firefly openapi` emite un **esqueleto**
sellado con metadatos: el bloque `info` (leído de `firefly.yaml` / `Cargo.toml`), el
componente `ProblemDetail` siempre presente y `paths` vacío. La forma del cable es
idéntica a la que sirve una app en vivo, solo que la lista de rutas está en blanco.

Para capturar las rutas **reales** de Lumen, ejecuta el servicio y obtén
`/v3/api-docs`. Ese documento, construido por el `from_inventory()` del framework, *es*
la especificación en vivo:

```bash
cargo run --bin lumen &
curl -s http://localhost:8081/v3/api-docs | jq .
```

> **Tip** **Punto de control.** `firefly openapi | jq '.openapi'` imprime `"3.1.0"`
> incluso fuera de una app en ejecución, y `jq '.components.schemas.ProblemDetail'`
> está presente. El `paths` del esqueleto es `{}`; la especificación en vivo en
> `:8081/v3/api-docs` tiene tus rutas de wallet rellenadas.

## Paso 9 — Genera un cliente tipado a partir de la especificación

La dirección inversa: dado un documento OpenAPI, generar un cliente Rust tipado
sobre el `RestClient` del framework, el análogo en Rust del SDK WebClient generado a
partir de OpenAPI de springdoc.

```bash
# capture the live spec, then generate a client from it
curl -s http://localhost:8081/v3/api-docs -o wallet-openapi.json
firefly openapi-client --spec wallet-openapi.json -o src/generated.rs --client-name WalletClient
```

Lo que acaba de ocurrir: el generador recorrió la especificación y emitió un `struct`
de modelo por cada esquema de objeto (y un `enum` por cada enumeración de cadena),
con los renombrados de serde y los campos opcionales preservados, más una `async fn`
por operación —parámetros de ruta/consulta tipados, un cuerpo de petición JSON y el
tipo de la respuesta de éxito—, cada una llamando a `RestClient` por debajo. El
cliente generado tiene la misma forma que el que escribirías a mano; la muestra por
capas `lumen-ledger` incluye exactamente un SDK así, que conocerás en
[Microservicios por capas](./22-layered-microservices.md).

> **Tip** **Punto de control.** Tras el segundo comando, `src/generated.rs` existe y
> contiene un `pub struct WalletClient` más los modelos `WalletResponse` /
> `WalletStatus` que reflejan los esquemas de componente de la especificación. Las
> formas no mapeadas degradan a `serde_json::Value` en lugar de hacer fracasar la
> generación.

## Resumen

En este capítulo viste que el controlador que ya escribiste *es* la documentación de
la API:

- Firefly sirve una especificación **OpenAPI 3.1** en vivo (`/v3/api-docs`,
  con alias `/openapi.json`), **Swagger UI** (`/swagger-ui`) y **ReDoc** (`/redoc`)
  en el puerto de **gestión**, construida a partir del inventario en el arranque con
  cero código de aplicación.
- La especificación anuncia la **URL base de la API pública** como su `server`
  (recurriendo a `localhost`, sobrescribible con `FIREFLY_OPENAPI_SERVER_URL`), de
  modo que "Try it out" apunta a la API, no al origen de la documentación.
- `#[derive(Schema)]` convierte un DTO en un `#/components/schemas/{Type}` en tiempo
  de expansión de macro, respetando serde `rename` / `rename_all` / `skip`, tratando
  `Option` / `Box` / `Arc` / `Rc` como transparentes, mapeando colecciones a arrays y
  mapas, haciendo `$ref` a DTOs anidados y renderizando los enums sin campos como
  enumeraciones de cadenas.
- Los cuerpos de petición, los cuerpos de respuesta y los parámetros de
  ruta/consulta/cabecera se **infieren** de la firma del handler (cuerpos `Json<T>` /
  `Valid<T>`, parámetros de consulta `Query<T>` y `PageRequest`, segmentos de ruta
  `:id`, parámetros `header(...)` declarados), con un `$ref` emitido solo para
  esquemas realmente registrados, de modo que el documento nunca queda colgando.
- Los metadatos por operación (`summary`, `description`, `tags`, `status`,
  `deprecated`) y el `tag` a nivel de controlador dan forma a cada operación;
  `request = ` / `response = ` sobrescriben la inferencia cuando una firma no puede
  expresar el DTO.
- Cada operación lleva una respuesta `default` RFC 9457 `ProblemDetail`, de modo que
  el contrato de error uniforme queda documentado automáticamente.
- Una única tabla de descriptores alimenta la especificación, la vista
  `/admin/api/mappings` y el informe de arranque —una única fuente de verdad—, y los
  comandos `firefly openapi` / `openapi-client` exportan la especificación y generan
  un cliente tipado a partir de ella.

Nada en `samples/lumen` cambió: las declaraciones de enrutamiento que ya escribiste
produjeron Swagger UI, ReDoc y una especificación OpenAPI 3.1 válida, gratis.

## Ejercicios

1. **Lee la especificación en vivo.** Con `cargo run` en ejecución, `curl -s
   localhost:8081/v3/api-docs | jq '.paths | keys'`. Confirma que cada ruta de wallet
   y de transferencia del capítulo 6 está presente, luego `jq '.components.schemas |
   keys'` para ver cada DTO con `#[derive(Schema)]` más `ProblemDetail`.
2. **Observa cómo fluye un renombrado.** En `jq`, inspecciona
   `.components.schemas.TransferResult.properties` y confirma que las claves de las
   propiedades son `stepsExecuted` / `stepsRolledBack` (los nombres serde del cable),
   no los identificadores en snake_case. Luego elimina temporalmente un
   `#[serde(rename = "…")]` en `src/transfer.rs`, recompila y observa cómo cambia el
   nombre de la propiedad del esquema.
3. **Mueve la URL del servidor.** Arranca Lumen con
   `FIREFLY_OPENAPI_SERVER_URL=https://api.lumen.example cargo run`, luego
   `curl -s localhost:8081/v3/api-docs | jq '.servers'`. Confirma que la URL cambió:
   este es el valor que llamará el "Try it out" de Swagger UI.
4. **Deprecia una operación.** Añade la flag escueta `deprecated` a un atributo de
   verbo en `src/web.rs` (p. ej. `#[post("/wallets/:id/withdraw", summary = "Withdraw funds",
   status = 200, deprecated)]`), recompila y confirma que
   `jq '.paths."/api/v1/wallets/{id}/withdraw".post.deprecated'` es `true` y que
   Swagger UI tacha la operación.
5. **Exporta y compara.** Ejecuta `firefly openapi --format yaml -o skeleton.yaml`,
   luego `curl -s localhost:8081/v3/api-docs | jq . > live.json`. Observa que el
   esqueleto de la CLI tiene `paths` vacío mientras que el documento en vivo lleva tus
   rutas, y que ambos comparten el mismo bloque `info` y el componente `ProblemDetail`.

## Adónde ir después

- Construye el modelo de lectura tras el `WalletView` que describen estas
  documentaciones en
  **[Persistencia y repositorios reactivos](./07-persistence.md)**.
- Mira dónde se construye y monta el documento OpenAPI en el pipeline de arranque en
  **[Bootstrap](./04b-bootstrap.md)**, y la vista `/admin/api/mappings` con la que
  comparte fuente en **[Observabilidad y administración](./15-observability.md)**.
- Consume un cliente generado a partir de OpenAPI contra un servicio upstream real en
  **[Microservicios por capas](./22-layered-microservices.md)**, apoyándote en
  **[Clientes HTTP](./13-http-clients.md)**.
