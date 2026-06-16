# Caché

El endpoint `GET /api/v1/wallets/:id` de Lumen ya sirve una vista de monedero desde
una caché de 30 segundos: lo activaste en su día en [CQRS](./09-cqrs.md) con una
anotación y un bean, y lo has usado desde entonces sin pensar en ello. Este
capítulo abre esa maquinaria. Seguiremos una lectura desde el bus de consultas
hasta el cache port a nivel de bytes que hay debajo, demostraremos *por qué* cada
depósito, retirada y transferencia debe *invalidar* esa caché para que una lectura
después de una escritura nunca mienta, y luego envolveremos la llamada lenta a la
que recurre un fallo de caché (cache miss) con los decoradores de resiliencia que
evitan que tire abajo el servicio entero.

Dos crates sostienen esta historia, y ambos llegan a Lumen a través de la única
fachada `firefly`: `firefly-cache` (como `firefly::cache`) expone un único cache
port más un puñado de backends y un envoltorio tipado, y `firefly-resilience`
(como `firefly::resilience`) aporta los decoradores de circuit breaker, rate
limiter, bulkhead y timeout. La caché de lecturas de CQRS que ya tienes
—`firefly::cqrs::QueryCache`— se asienta encima del cache port.

Al terminar este capítulo, serás capaz de:

- Explicar cómo `#[firefly(cache_ttl = "30s")]` sobre una consulta se convierte en
  una caché real y respetada de 30 segundos, y qué bean la respeta.
- Mantener *honesta* una lectura-tras-escritura invalidando una familia de
  consultas en cada frontera de escritura, y demostrar que el ciclo se cierra con
  el propio test HTTP de Lumen.
- Leer y programar contra el cache port `Adapter` —el único trait que implementa
  cada backend (en memoria, Redis, Postgres)— y cambiar el backend en un único
  punto de cableado.
- Memoizar un valor arbitrario fuera del bus de consultas con `Typed<T>::get_or_set`.
- Envolver un cargador lento (o cualquier llamada saliente) en un `Chain` de
  resiliencia para que un timeout, un circuito abierto o un bulkhead lleno fallen
  rápido en lugar de quedarse colgados.

## Conceptos que conocerás

Cada uno de estos se reintroduce en su contexto cuando se usa por primera vez;
esta es la versión breve para que las palabras no te resulten nuevas cuando las
encuentres.

> **Note** **Término clave — caché.** Una *caché* es un almacén rápido, normalmente
> en memoria, que guarda el resultado de un cómputo costoso para que la siguiente
> petición pueda saltarse el trabajo. La parte difícil nunca es el almacenamiento:
> es saber cuándo un valor guardado ha quedado obsoleto. En Spring esto es la
> familia `@Cacheable` / `@CacheEvict` respaldada por un `CacheManager`.

> **Note** **Término clave — cache port.** Un *port* es una interfaz abstracta de la
> que dependen los consumidores en lugar de un backend concreto, de modo que el
> backend pueda intercambiarse sin tocar a los consumidores. El cache port de
> Firefly es el trait `Adapter`; el análogo en Spring es el SPI `Cache` /
> `CacheManager` que hay detrás de `@Cacheable`.

> **Note** **Término clave — TTL.** El *time to live* es cuánto tiempo permanece
> válida una entrada cacheada antes de expirar y tratarse como ausente. Un TTL de
> 30 segundos significa que una lectura dentro de los 30 segundos posteriores al
> último relleno se sirve desde la caché; pasado ese tiempo, vuelve a ejecutar el
> trabajo. El TTL por sí solo es un techo de obsolescencia, no una garantía de
> corrección: para eso está la invalidación.

> **Note** **Término clave — read-through / cache-aside.** Una lectura *read-through*
> (o *cache-aside*) comprueba primero la caché; en un fallo ejecuta el trabajo real
> (el *cargador*), almacena el resultado y lo devuelve. La siguiente lectura dentro
> del TTL se salta el cargador. `Typed<T>::get_or_set` es la primitiva read-through
> de Firefly.

> **Note** **Término clave — decorador de resiliencia.** Un *decorador de
> resiliencia* envuelve una llamada asíncrona para acotar su fallo: un *circuit
> breaker* deja de llamar a una dependencia enferma, un *rate limiter* limita la
> tasa de salida, un *bulkhead* limita la concurrencia y un *timeout* acota la
> duración. Esto refleja a Resilience4j en el mundo de Spring.

## Paso 1 — Ve la caché que ya tienes

No tuviste que escribir nada de código de caché para obtener una lectura cacheada:
la *declaraste*. La consulta `GetWallet` de Lumen lleva su política de caché como
un atributo situado justo al lado del tipo, en `src/commands.rs`:

```rust
/// `GET /api/v1/wallets/:id` query. `#[firefly(cache_ttl = "30s")]` is reflected
/// on the generated `Message::cache_ttl`, so a `QueryCache` memoises reads for
/// 30 seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    /// The wallet id to fetch.
    pub id: String,
}
```

Qué acaba de pasar: la macro `#[derive(Query)]` lee el atributo
`#[firefly(cache_ttl = "30s")]` y emite un método `cache_ttl()` sobre la
implementación generada de `Message`. El atributo es *declarativo*: enuncia la
política donde se define el tipo, y el framework cablea el comportamiento. Nada en
el handler de la consulta menciona la caché en absoluto.

> **Note** **Término clave — caché declarativa.** La caché *declarativa* significa
> que la política vive como una anotación sobre el tipo o el método, no como código
> imperativo en el cuerpo. `@Cacheable(ttl = ...)` de Spring es el análogo; aquí es
> `#[firefly(cache_ttl = "30s")]`.

Como el TTL es ahora un hecho en el código generado, Lumen lo fija con un test
unitario para que nunca pueda desaparecer en silencio:

```rust
#[test]
fn get_wallet_carries_cache_ttl() {
    assert!(GetWallet::default().cache_ttl().is_some());
}
```

Qué acaba de pasar: el test construye un `GetWallet` por defecto, llama al
`cache_ttl()` generado y comprueba que devuelve `Some(_)`. Si alguien borra el
atributo, este test falla: el contrato de caché está protegido, no asumido.

> **Tip** **Punto de control.** Abre `samples/lumen/src/commands.rs` y encuentra la
> línea `#[firefly(cache_ttl = "30s")]` sobre `GetWallet`, más el test
> `get_wallet_carries_cache_ttl`. El atributo y la aserción son los dos extremos de
> la misma declaración.

## Paso 2 — Encuentra el bean que respeta el TTL

Un `cache_ttl()` sobre un mensaje es inerte hasta que algo lo *lee* en la ruta de
despacho. Ese algo es el bean `QueryCache` y el middleware de bus que instala.

> **Note** **Término clave — middleware de bus.** El *middleware* envuelve cada
> mensaje que fluye a través del bus de CQRS, ejecutándose antes y después del
> handler. El middleware de caché de lecturas comprueba la caché antes de que se
> ejecute el handler y la rellena después, de modo que una consulta cacheada nunca
> llega al handler. Esto es la intercepción `@Cacheable` de Spring, materializada
> como un interceptor de bus.

En Lumen el `QueryCache` se declara como un único `#[bean]` dentro de `LumenBeans`
(el contenedor `#[derive(Configuration)]` en `src/web.rs`):

```rust
use firefly::cqrs::QueryCache;

// samples/lumen/src/web.rs — inside `#[bean] impl LumenBeans { ... }`.

/// The read-side query cache honouring `GetWallet`'s 30s TTL (`@Bean`).
#[bean]
fn query_cache(&self) -> QueryCache {
    QueryCache::new()
}
```

Qué acaba de pasar: `QueryCache::new()` construye una caché de consultas vacía y en
memoria, indexada por el tipo de mensaje más un hash del valor del mensaje.
Declararla como un `#[bean]` es todo el cableado que haces: cuando
`FireflyApplication::run()` escanea componentes en el contenedor y encuentra un
bean `QueryCache`, llama a `query_cache.middleware()` por ti y registra ese
middleware en el bus. (El middleware de validación lo instala el núcleo; ninguno de
los dos lo registras a mano.)

Así, cuando un `GetWallet` fluye por el bus, el middleware de caché de lecturas:

- **en un acierto (hit)** devuelve el `WalletView` memoizado *sin llegar nunca al
  handler*;
- **en un fallo (miss)** ejecuta el handler, almacena el resultado bajo la clave de
  la consulta durante los 30 segundos declarados y lo devuelve.

> **Design note.** Este es el mismo patrón de autoconfiguración que viste en
> [Quickstart](./02-quickstart.md): «autoconfigura el bus de CQRS… el middleware de
> caché de lecturas siempre que haya presente un bean `QueryCache`». Añades un
> *bean*, no una llamada de registro. El análogo en Spring es autoconfigurar el
> comportamiento de `@EnableCaching` una vez que existe un bean `CacheManager`.

El mismo bean también se `#[autowired]` en el controlador, de modo que el lado de
escritura pueda alcanzar la misma caché exacta que lee el middleware:

```rust
// samples/lumen/src/web.rs — the WalletApi controller.
#[derive(Clone, Controller)]
pub struct WalletApi {
    #[autowired]
    pub bus: Arc<Bus>,
    #[autowired]
    pub ledger: Arc<Ledger>,
    /// The query cache, invalidated after a mutation so a read-after-write
    /// never serves a stale balance within the 30s `GetWallet` TTL (autowired).
    #[autowired]
    pub query_cache: Arc<QueryCache>,
}
```

Qué acaba de pasar: el framework instala *un* `QueryCache` como middleware de bus y
entrega el *mismo* `Arc<QueryCache>` al controlador. `QueryCache` está respaldado
por `Arc` y es barato de clonar, así que ambos handles comparten las mismas
entradas: el middleware rellena la caché y el controlador puede eliminar entradas
de ella.

> **Tip** **Punto de control.** En `src/web.rs`, el `#[bean]` `query_cache` y el
> campo `#[autowired] pub query_cache: Arc<QueryCache>` se refieren a la misma caché
> compartida. Uno la lee y la rellena (middleware); el otro la invalida
> (controlador).

## Paso 3 — Mantén honesta la lectura-tras-escritura

Un TTL de 30 segundos es un regalo para una vista intensiva en lecturas y un
desastre para la corrección si nunca invalidas. Deposita `$2.50`, luego lee el
saldo dentro de los 30 segundos, y una caché que solo conociera el TTL serviría
alegremente el número *antiguo*.

> **Note** **Término clave — invalidación.** La *invalidación* (o *expulsión*) es la
> eliminación deliberada de una entrada cacheada que ahora es incorrecta, forzando
> a que la siguiente lectura vuelva a ejecutar el trabajo. La corrección de la
> lectura-tras-escritura proviene de invalidar en la frontera de escritura —el
> momento en que un saldo cambia— y no de esperar a un TTL.

Lumen evita la obsolescencia invalidando toda la familia `GetWallet` después de
cada mutación. Aquí está el handler de depósito en `src/web.rs`:

```rust
#[post("/wallets/:id/deposit", summary = "Deposit funds", status = 200)]
async fn deposit(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    Json(body): Json<AmountBody>,
) -> WebResult<Json<WalletView>> {
    let cmd = Deposit { wallet_id: id, amount: body.amount };
    let view: WalletView = api.bus.send(cmd).await.map_err(cqrs_to_web)?;
    api.query_cache.invalidate_type::<GetWallet>();
    Ok(Json(view))
}
```

Qué acaba de pasar, línea a línea:

- `api.bus.send(cmd)` despacha el comando `Deposit` a través del bus y espera el
  `WalletView` resultante. `map_err(cqrs_to_web)?` convierte un error de CQRS en un
  error web `application/problem+json` según RFC 9457.
- `api.query_cache.invalidate_type::<GetWallet>()` elimina *todas* las entradas
  `GetWallet` cacheadas. Internamente, `invalidate_type::<Q>()` borra cada clave de
  caché prefijada con el nombre de tipo de `Q` más el separador `:`, de modo que se
  limpia toda la familia `GetWallet`: la siguiente lectura vuelve a ejecutar el
  handler y refleja la escritura.

Por qué importa: el TTL acota cuán obsoleto *puede* llegar a estar un valor; la
invalidación explícita garantiza que una lectura *después de una escritura que tú
hiciste* nunca esté obsoleta en absoluto.

El handler de retirada hace exactamente lo mismo, y también lo hace el endpoint de
transferencia de [Sagas](./12-sagas.md): una transferencia cambia *dos* saldos, así
que también debe invalidar la familia:

```rust
// In the transfer handler — a transfer touches both wallets' views.
api.query_cache.invalidate_type::<GetWallet>();
```

Qué acaba de pasar: como la clave de caché incluye el *valor* del mensaje, una
transferencia entre el monedero A y el monedero B tendría que expulsar dos claves
específicas. Invalidar todo el tipo `GetWallet` es más sencillo y siempre correcto
—no puede dejarse nunca una clave por el camino— a costa de descartar entradas de
caché de monederos no relacionados, que simplemente se rellenan de nuevo en su
siguiente lectura.

> **Design note.** Lumen combina *caché read-through sobre el mensaje*
> (`#[firefly(cache_ttl)]`) con *expulsión explícita en la frontera de escritura*
> (`invalidate_type`). El lector memoiza; el escritor descarta la familia en el
> momento en que cambia un saldo. El almacén subyacente es el mismo cache port
> intercambiable `Adapter` que usa cualquier otro consumidor de caché (Paso 4), así
> que esta política es independiente de dónde vivan realmente los bytes.

El test HTTP de extremo a extremo demuestra que el ciclo se cierra. Abre un
monedero con un saldo de `100`, deposita `+250`, retira `-50` y luego vuelve a
leerlo a través del `GET` cacheado:

```rust
// after a deposit(+250) and a withdraw(-50) on an opening balance of 100:
let view: WalletView = get_wallet(&app, &opened.id).await;
assert_eq!(view.balance, 300);   // read-after-write is honest
assert_eq!(view.version, 3);
```

Qué acaba de pasar: cada llamada mutadora invalidó la familia `GetWallet`, así que
el `GET` final volvió a ejecutar la consulta contra el modelo de lectura en lugar
de reproducir una vista cacheada obsoleta. El saldo refleja ambas escrituras
(`100 + 250 - 50 = 300`) y la versión es `3` (un evento por mutación encima de la
apertura).

> **Tip** **Punto de control.** Ejecuta los tests HTTP del monedero:
> `cargo test -p lumen deposit_and_withdraw_update_the_balance`. Un test en verde
> significa que el ciclo de lectura-tras-escritura se cierra: la caché se respeta
> *y* se invalida.

## Paso 4 — Sigue la lectura hasta el cache port

Todo lo de los Pasos 1-3 se ejecuta sobre una caché en proceso por defecto, pero el
`QueryCache` —como cualquier otro consumidor de caché— depende en última instancia
del cache port abstracto `Adapter`, nunca de un cliente concreto. Esa única costura
es lo que te permite mover la caché de Lumen a Redis sin tocar un solo handler.

Aquí está el port (de `firefly-cache`, accesible como `firefly::cache::Adapter`):

```rust,ignore
use std::time::Duration;
use async_trait::async_trait;

#[async_trait]
pub trait Adapter: Send + Sync {
    /// Returns the cached bytes for `key`, or `CacheError::NotFound` when absent.
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError>;

    /// Stores `value` under `key` for `ttl` (None or zero = no expiry).
    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError>;

    /// Removes the entry. A missing key is a no-op.
    async fn delete(&self, key: &str) -> Result<(), CacheError>;

    /// Removes every entry.
    async fn clear(&self) -> Result<(), CacheError>;

    /// Human-readable adapter identifier (`memory`|`redis`|`noop`|...).
    fn name(&self) -> String;

    /// Returns Ok when the backend is reachable.
    async fn health_check(&self) -> Result<(), CacheError>;

    // The methods below ship default impls so older adapters keep compiling;
    // backends with a cheaper native path (Redis SET NX, SCAN MATCH) override them.

    /// Writes only when `key` is absent; true when the write happened.
    async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool, CacheError>;

    /// Whether a live entry exists for `key`.
    async fn exists(&self, key: &str) -> Result<bool, CacheError>;

    /// Removes every entry whose key starts with `prefix`; returns the count.
    async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError>;

    /// A point-in-time counter snapshot, or None when the adapter has none.
    async fn stats(&self) -> Option<CacheStats>;
}
```

Qué acaba de pasar: los valores cruzan el port como `Vec<u8>` en bruto; el propio
port no sabe nada de tus tipos. Un fallo de caché se señaliza con la variante
`CacheError::NotFound` (no con un `Option`), y un `ttl` de `None` (o cero) significa
«sin expiración». Los cuatro métodos del trait con implementaciones por defecto
(`set_if_absent`, `exists`, `delete_prefix`, `stats`) permiten que un adaptador se
publique sin ellos y permiten que un backend más rico los sobrescriba con una ruta
nativa más barata.

> **Note** **Término clave — adaptador.** Un *adaptador* es una implementación
> concreta de un port. Firefly aporta varios, y eliges uno en tiempo de cableado:

| Implementación        | Respaldo               | Uso                                       |
|-----------------------|------------------------|-------------------------------------------|
| `MemoryAdapter`       | `HashMap` + `RwLock`   | en proceso, consciente del TTL — **el predeterminado** |
| `NoOpAdapter`         | ninguno                | tests / una caché deshabilitada a propósito |
| `FallbackAdapter`     | compuesto (dos ports)  | primario-y-luego-secundario, escribe en ambos |
| `RedisAdapter`        | Redis (RESP)           | caché distribuida (`firefly-cache-redis`) |

Un `NoOpAdapter` reporta todo `get` como `NotFound` y tiene éxito en silencio en
toda escritura: es la caché que no hace nada, perfecta para un test que quiere que
el handler se ejecute siempre. `MemoryAdapter` es el mapa en proceso, vivo y
consciente del TTL, que Lumen usa de fábrica.

> **Tip** **Punto de control.** Sabes nombrar los cuatro adaptadores y decir cuál es
> el predeterminado (`MemoryAdapter`) y cuál deshabilita la caché (`NoOpAdapter`).
> Todos ellos son `firefly::cache::*` e implementan el único trait `Adapter`.

## Paso 5 — Memoiza un valor fuera del bus de consultas

`QueryCache` indexa y serializa los resultados de las consultas por ti. Pero cuando
quieres cachear algo que *no* fluye por el bus de consultas —digamos, la puntuación
de riesgo de un monedero obtenida de un servicio externo— el envoltorio `Typed<T>`
es la primitiva.

> **Note** **Término clave — `Typed<T>`.** `Typed<T>` envuelve un `Adapter` con
> ayudantes de lectura/escritura codificados en JSON para un tipo concreto `T`.
> Serializa los valores como bytes `serde_json` (compatibles a nivel de cable con
> los demás ports) y te da `get_or_set`: consulta la caché, llama al cargador en un
> fallo, persiste el resultado y lo devuelve. Un error de caché nunca enmascara un
> resultado exitoso del cargador.

```rust
use std::sync::Arc;
use std::time::Duration;
use firefly::cache::{MemoryAdapter, Typed};

#[derive(serde::Serialize, serde::Deserialize)]
struct WalletView {
    id: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), firefly::cache::CacheError> {
    let cache = Arc::new(MemoryAdapter::new());
    let typed: Typed<WalletView> = Typed::new(cache);

    let view = typed
        .get_or_set("wallet:wlt_alice", Some(Duration::from_secs(60)), || async {
            // loaded from the ledger / read model on a miss
            Ok(WalletView { id: "wlt_alice".into() })
        })
        .await?;
    assert_eq!(view.id, "wlt_alice");
    Ok(())
}
```

Qué acaba de pasar, bloque a bloque:

- `Arc::new(MemoryAdapter::new())` construye la caché a nivel de bytes y la envuelve
  en un `Arc` para que `Typed` pueda compartirla. `Typed::new(cache)` superpone
  codificación JSON para el tipo `WalletView`.
- `get_or_set(key, ttl, loader)` es la llamada read-through. En la primera ejecución
  la clave está ausente, así que el cierre del cargador se ejecuta, su `WalletView`
  se codifica en JSON y se almacena bajo la clave durante 60 segundos, y el valor se
  devuelve. Una segunda llamada dentro de los 60 segundos se salta el cargador y
  decodifica los bytes almacenados.
- El cargador devuelve `Result<WalletView, CacheError>`: cualquier error en su
  interior aflora; pero un fallo al *escribir* el valor cargado de vuelta en la
  caché **no** enmascara una carga exitosa (el valor se devuelve igualmente).

`Typed<T>` también ofrece `put` (escribe y devuelve siempre —la ruta de
almacenamiento incondicional—), `delete` (elimina una clave) y `delete_prefix`
(expulsa una familia de claves), pero `get_or_set` es el caballo de batalla.

> **Tip** **Punto de control.** Sabes describir los tres resultados de `get_or_set`:
> un acierto (decodifica y devuelve, sin cargador), un fallo (ejecuta el cargador,
> almacena, devuelve) y un fallo-de-almacenamiento-tras-carga (devuelve el valor de
> todos modos, descarta el error de escritura).

## Paso 6 — Intercambia y compón backends en un único punto de cableado

La caché predeterminada es `MemoryAdapter`. ¿Dónde vive ese valor por defecto?
`Core::new` (y por tanto `WebStack`, sobre el que se construye Lumen) lee
`CoreConfig.cache: Option<Arc<dyn cache::Adapter>>` y sustituye un `MemoryAdapter`
cuando es `None`. Para usar un backend distinto, le pasas un `Arc<dyn Adapter>`
diferente ahí: un constructor, nada más.

> **Note** **Término clave — `FallbackAdapter`.** Un `FallbackAdapter` es él mismo un
> `Adapter` que envuelve un *primario* y un *secundario*: intenta primero el
> primario y, ante un fallo de transporte (cualquier cosa que no sea un simple
> fallo de caché), degrada la petición al secundario y escribe en ambos. Los
> consumidores nunca ven la conmutación por error (failover): solo ven un `Adapter`.

Para alta disponibilidad, compón Redis con un fallback en proceso de modo que un
parpadeo de Redis degrade a caché local en lugar de hacer fallar la petición:

```rust,ignore
use std::sync::Arc;
use firefly::cache::{FallbackAdapter, MemoryAdapter, RedisAdapter};

// Connect the distributed primary (RESP over the network)...
let redis = Arc::new(RedisAdapter::connect("redis://127.0.0.1:6379/0").await?);

// ...and fall through to a local in-process cache on a transport error or miss,
// writing to both so the local layer warms up.
let cache: Arc<dyn firefly::cache::Adapter> =
    Arc::new(FallbackAdapter::new(redis, Arc::new(MemoryAdapter::new())));
```

Qué acaba de pasar: `RedisAdapter::connect(url)` marca a Redis y devuelve un
adaptador listo; `FallbackAdapter::new(primary, secondary)` lo compone con un
`MemoryAdapter`. El compuesto es *también* un `Adapter`, así que se lo entregas a
`CoreConfig.cache` exactamente igual que harías con cualquier backend individual.

Como todo lo que hay aguas abajo depende del port, cambiar el backend modifica *un*
constructor: los handlers de Lumen, el `QueryCache` y el almacén de sesiones quedan
intactos. Un Lumen de un solo proceso conserva la caché en memoria predeterminada;
un despliegue multinodo intercambia `RedisAdapter` de modo que un `GetWallet`
cacheado en un nodo sea visible en el siguiente, y de modo que un `invalidate_type`
en cualquier nodo limpie la entrada compartida.

> **Design note.** Esto refleja el intercambio del event store y del broker que ya
> has visto: desarrolla y prueba contra el adaptador en memoria, cablea el backend
> distribuido en producción vía `CoreConfig`. La base de enseñanza sigue siendo un
> `cargo run` sin infraestructura; la ruta de producción es una línea de cableado.
> [Producción y despliegue](./20-production.md) hace exactamente este intercambio de
> verdad.

> **Tip** **Punto de control.** Sabes nombrar la única costura —`CoreConfig.cache`—
> que cambia el backend de caché para todo el servicio, y explicar por qué ningún
> handler, `QueryCache` o controlador tiene que cambiar cuando la intercambias.

## Paso 7 — Protege el cargador con decoradores de resiliencia

Un fallo de caché recurre a una llamada lenta: el modelo de lectura, el event store
o un servicio externo. Si esa llamada se cuelga o empieza a fallar, un cargador sin
protección puede arrastrar consigo todo el servicio. `firefly-resilience` protege
exactamente eso, y cualquier llamada saliente, como la liquidación de Payments de
[Clientes HTTP](./13-http-clients.md).

> **Note** **Término clave — circuit breaker.** Un *circuit breaker* vigila una
> llamada protegida. Mientras está *cerrado*, las llamadas pasan y se cuentan los
> fallos; tras suficientes fallos se *abre* y cortocircuita las llamadas posteriores
> con un error inmediato, evitando molestar a la dependencia enferma; tras un
> enfriamiento deja pasar una llamada de prueba (*half-open*) para decidir si
> cerrarse de nuevo. Esto es el `CircuitBreaker` de Resilience4j.

Hay cuatro decoradores, cada uno protegiendo de un modo de fallo:

| Decorador        | Protege contra                               | Error al saltar                 |
|------------------|----------------------------------------------|---------------------------------|
| `CircuitBreaker` | fallo en cascada de una dependencia lenta / fallida | `ResilienceError::CircuitOpen`  |
| `RateLimiter`    | exceso de la tasa de salida (token bucket)   | `ResilienceError::RateLimited`  |
| `Bulkhead`       | agotamiento de recursos por concurrencia desbocada | `ResilienceError::BulkheadFull` |
| `Timeout`        | llamadas atascadas                           | `ResilienceError::Timeout`      |

> **Note** **Término clave — `Chain`.** Un `Chain` compone decoradores en una única
> llamada protegida. Los decoradores se ejecutan de izquierda a derecha con el de
> más a la izquierda como el más externo, así que
> `Chain::new().with(timeout).with(breaker).with(bulkhead)` se evalúa como
> `timeout(breaker(bulkhead(call)))`: una fecha límite acota toda la llamada
> mientras el breaker y el bulkhead protegen la operación interna.

```rust,no_run
use std::{sync::Arc, time::Duration};
use firefly::resilience::{Bulkhead, Chain, CircuitBreaker, CircuitConfig, Timeout};

# async fn ex() -> Result<(), firefly::resilience::ResilienceError> {
let breaker = Arc::new(CircuitBreaker::new(CircuitConfig::default()));

let guarded = Chain::new()
    .with(Timeout::new(Duration::from_secs(2)))   // per-call deadline (outermost)
    .with_shared(breaker.clone())                 // open the circuit on repeated failures
    .with(Bulkhead::new(20));                      // cap concurrent in-flight calls

guarded.execute(|| async {
    // the protected operation — a cache loader, an upstream call, ...
    Ok(())
}).await?;
# Ok(())
# }
```

Qué acaba de pasar, línea a línea:

- `CircuitBreaker::new(CircuitConfig::default())` construye un breaker con la
  política por defecto (salta tras 5 fallos, permanece abierto 30 segundos). Va
  envuelto en `Arc` para que puedas a la vez entregarlo a la cadena *y* conservar un
  handle para inspeccionar su estado.
- `Chain::new()` arranca una cadena vacía. `.with(decorator)` añade un decorador del
  que la cadena es *propietaria*; `.with_shared(arc_decorator)` añade uno del que
  conservas un handle; por eso el breaker usa `.with_shared(breaker.clone())`
  mientras que el `Timeout` y el `Bulkhead` recién construidos usan `.with(...)`.
- `guarded.execute(|| async { ... })` ejecuta tu cierre a través de los tres
  decoradores, el de más a la izquierda como el más externo. El cierre devuelve
  `Result<(), ResilienceError>`; si cualquier decorador salta, `execute` devuelve el
  error de ese decorador y puede que tu operación no llegue a ejecutarse nunca.

> **Warning** `Chain::with(...)` toma la propiedad y exige que su argumento
> implemente el trait de decorador directamente; un `Arc<CircuitBreaker>` pelado
> *no* lo hace. Cuando quieres conservar un handle a un breaker (para leer su
> estado, o para compartirlo entre cadenas), usa `.with_shared(breaker.clone())`,
> que toma el `Arc`. Usar `.with(breaker)` sobre un `Arc` no compilará.

Cada decorador también funciona por sí solo. A diferencia de `Chain::execute` (cuyo
valor descartas), `CircuitBreaker::execute` *devuelve el valor de la operación*, así
que una lectura protegida sigue entregándote el `WalletView`:

```rust,ignore
use std::time::Duration;
use firefly::resilience::{Bulkhead, CircuitBreaker, CircuitConfig, RateLimiter, Timeout};

let cb = CircuitBreaker::new(CircuitConfig::default());
let _ = cb.execute(|| async { settle().await }).await;       // returns settle()'s value

let rl = RateLimiter::new(100.0, 200);                       // 100 rps, burst 200
let _ = rl.execute(|| async { call().await }).await;

let bh = Bulkhead::new(20);
let _ = bh.try_execute(|| async { call().await }).await;     // non-blocking; BulkheadFull if full

let to = Timeout::new(Duration::from_secs(2));
let _ = to.execute(|| async { slow_call().await }).await;
```

Qué acaba de pasar: cada primitiva tiene su propio `execute` que envuelve un cierre
que devuelve `Result<T, ResilienceError>` y propaga el valor de la operación en caso
de éxito. `Bulkhead` ofrece además `try_execute`, la variante no bloqueante que
devuelve `BulkheadFull` de inmediato en lugar de esperar a una plaza libre.

> **Tip** **Punto de control.** Sabes explicar la diferencia entre `Chain::execute`
> (valor descartado, devuelve `Result<(), _>`) y `CircuitBreaker::execute` (devuelve
> el `T` de la operación), y sabes recurrir a `.with_shared(arc.clone())` cuando la
> cadena necesita un breaker que aún conservas.

## Paso 8 — Monta una lectura cache-aside resiliente

Las dos mitades de este capítulo se componen en una única forma: una lectura
cache-aside cuyo cargador está protegido por un circuit breaker. Esto es
exactamente lo que usaría un Lumen multinodo para servir una vista de monedero
desde Redis, reparando desde el modelo de lectura (o el flujo de eventos) en un
fallo mientras el breaker protege esa reparación:

```rust,ignore
use std::sync::Arc;
use std::time::Duration;
use firefly::cache::{MemoryAdapter, Typed};
use firefly::resilience::{CircuitBreaker, CircuitConfig};

let typed: Typed<WalletView> = Typed::new(Arc::new(MemoryAdapter::new()));
let breaker = CircuitBreaker::new(CircuitConfig::default());

let view = typed
    .get_or_set("wallet:wlt_alice", Some(Duration::from_secs(30)), || async {
        // the loader is what the circuit protects: the read model / event store.
        breaker
            .execute(|| async { load_wallet_view("wlt_alice").await })
            .await
            .map_err(|e| firefly::cache::CacheError::Backend(e.to_string()))
    })
    .await?;
```

Qué acaba de pasar: `get_or_set` es la lectura cache-aside externa. En un acierto
devuelve el `WalletView` decodificado y el cargador nunca se ejecuta. En un fallo el
cargador ejecuta la reparación real —pero envuelta en `breaker.execute(...)`—, así
que una racha de fallos abre el circuito y el *siguiente* fallo falla rápido con
`CircuitOpen` en lugar de machacar un modelo de lectura enfermo. El `map_err` adapta
el `ResilienceError` a un `CacheError::Backend` para que encaje con el tipo de error
de `get_or_set`.

Ahora tienes una ruta de lectura rápida y resiliente —construida a mano a partir de
las dos primitivas—, y la misma forma que el `#[firefly(cache_ttl = "30s")]`
declarativo de Lumen te da gratis sobre el bus de consultas.

> **Tip** **Punto de control.** Sabes seguir la estratificación: `get_or_set`
> (cache-aside) envuelve `breaker.execute` (protección frente a fallos) envuelve el
> cargador real (modelo de lectura / event store). La caché absorbe la ruta feliz;
> el breaker absorbe la ruta de fallo.

## Resumen — lo que ahora entiendes sobre la caché de Lumen

- La caché del lado de lectura que Lumen usa desde [CQRS](./09-cqrs.md) es
  *declarativa*: `#[firefly(cache_ttl = "30s")]` sobre `GetWallet` lo respeta el
  middleware de bus de caché de lecturas que `FireflyApplication` autoinstala
  siempre que hay presente un `#[bean]` `QueryCache`.
- El `QueryCache` es un único bean —instalado como middleware de bus por el
  framework y `#[autowired]` en el controlador—, así que cada handler mutador
  (depósito, retirada **y** transferencia) llama a
  `invalidate_type::<GetWallet>()` para mantener honesta la lectura-tras-escritura
  dentro del TTL de 30 segundos.
- Bajo el `QueryCache` está el cache port intercambiable `Adapter`: `MemoryAdapter`
  por defecto, `NoOpAdapter` para deshabilitar la caché, `FallbackAdapter` para
  Redis-con-fallback-local y `RedisAdapter` para un despliegue multinodo, elegido en
  un único punto de cableado, `CoreConfig.cache`.
- `Typed<T>::get_or_set` es la primitiva de memoización read-through para valores
  fuera del bus de consultas; un fallo de escritura tras una carga exitosa nunca
  enmascara el valor.
- `firefly-resilience` aporta `CircuitBreaker`, `RateLimiter`, `Bulkhead` y
  `Timeout`, componibles a través de un `Chain`, para proteger tanto un cargador de
  caché como cualquier llamada saliente (como la liquidación de Payments de
  [Clientes HTTP](./13-http-clients.md)).

## Ejercicios

1. **Demuestra que el TTL es real.** Escribe un test que abra un monedero, lo lea
   (cebando la caché), luego deposite *directamente a través del ledger* (saltándose
   el controlador, de modo que no se ejecute ningún `invalidate_type`) y vuelva a
   leerlo dentro de los 30 segundos. Comprueba que sigues viendo el saldo *antiguo*
   —demostrando que el TTL está sirviendo genuinamente un valor memoizado—, luego
   llama a `query_cache.invalidate_type::<GetWallet>()` y comprueba que la siguiente
   lectura refleja el depósito.

2. **Deshabilita la caché con `NoOpAdapter`.** El propio `QueryCache` está en
   memoria, pero la caché a nivel de bytes que usa el resto del servicio es
   `CoreConfig.cache`. Construye un `CoreConfig` con
   `cache: Some(Arc::new(NoOpAdapter::default()))`, arranca el servicio y confirma
   que la caché a nivel de bytes reporta siempre un fallo mientras el flujo del
   monedero sigue pasando: útil cuando quieres medir la latencia de la ruta fría.

3. **Intercambia un adaptador de fallback.** Construye un `FallbackAdapter` cuyo
   primario siempre dé error en `get`/`set` (un `Adapter` hecho a mano que devuelva
   `CacheError::Backend(...)`) y cuyo secundario sea un `MemoryAdapter`. Cablealo en
   `CoreConfig.cache`, ejecuta el flujo de depósito/retirada/lectura y comprueba que
   la corrección no se ve afectada: la caché degrada a la capa en proceso en lugar
   de fallar.

4. **Protege un cargador con un `Chain`.** Envuelve un cargador deliberadamente lento
   en un `Chain::new().with(Timeout::new(Duration::from_millis(50)))` y comprueba que
   un cargador que exceda la fecha límite aflora `ResilienceError::Timeout` (comprueba
   `err.is_timeout()`) en lugar de quedarse colgado. Luego añade
   `.with_shared(breaker.clone())` para un `CircuitBreaker`, hazlo saltar con fallos
   repetidos y comprueba que la siguiente llamada falla rápido con
   `ResilienceError::CircuitOpen` (comprueba `err.is_circuit_open()`).

5. **Memoiza fuera del bus.** Usa `Typed<T>::get_or_set` para cachear un valor
   calculado (p. ej. la puntuación de riesgo de un monedero) bajo un TTL de 10
   segundos. Llámalo dos veces con un cargador que incremente un contador y comprueba
   que el contador avanzó solo una vez, demostrando que la segunda llamada acertó en
   la caché en lugar de reejecutar el cargador.

## Adónde ir después

- Mira cómo *cada* declaración de este capítulo —`#[firefly(cache_ttl)]`,
  `#[bean]`, `#[autowired]`, `#[rest_controller]`— la produce la capa de macros de
  Firefly en **[Servicios declarativos con macros](./21-declarative-macros.md)**.
- Dirige el ciclo de lectura-tras-escritura cacheado de extremo a extremo, en
  proceso y sin enlazar a ningún socket, en **[Testing](./18-testing.md)**.
- Realiza el intercambio de caché en memoria → Redis para un despliegue real en
  **[Producción y despliegue](./20-production.md)**.
