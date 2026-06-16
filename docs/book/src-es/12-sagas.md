# Sagas, workflows y TCC

Al terminar este capítulo, Lumen sabrá **mover dinero entre dos monederos** — y
hacerlo *de forma segura*. Una transferencia no es un único comando: adeuda un
monedero y luego abona otro, y eso son dos escrituras independientes sobre dos
flujos de eventos independientes. Si la rama del abono falla después de que el
adeudo ya se haya confirmado, el titular de origen pierde el dinero sin tener
nada al otro lado. No existe ningún `BEGIN … COMMIT` que abarque dos agregados,
así que Lumen recurre a los patrones que hacen este trabajo a través de una
frontera distribuida: una **saga** que compensa el adeudo cuando el abono falla,
un **workflow** que ejecuta comprobaciones previas en paralelo, y un coordinador
**TCC** que reserva en ambos lados antes de confirmar cualquiera de ellos.

Construyes los tres sobre el `Ledger` con event sourcing que desarrollaste en
[Event Sourcing](./11-event-sourcing.md), de modo que una transferencia genera
eventos *reales* `MoneyWithdrawn` / `MoneyDeposited` en ambos flujos — y un
reembolso genera un `MoneyDeposited` real en el flujo de origen. Aquí nada es un
juguete; cada rama acciona el mismo servicio de aplicación que usan los
handlers de CQRS, y cada resultado es observable en el ledger.

Al terminar este capítulo, serás capaz de:

- Explicar *por qué* una transferencia de dinero entre dos agregados necesita una
  saga, y no una transacción de base de datos, y qué es una *compensación*.
- Declarar una `Saga` con `#[firefly::saga]` y `#[saga_step]` — incluyendo el
  orden con `depends_on`, un método `compensate` con nombre y reintentos por paso
  — luego ejecutarla y leer su `Outcome`.
- Declarar un `Workflow` con `#[firefly::workflow]` que ejecuta comprobaciones
  independientes en una capa paralela y une sus veredictos tipados en un nodo de
  decisión.
- Declarar un coordinador TCC con `#[firefly::tcc]` y `#[participant]` para
  reservar y luego confirmar a través de dos recursos.
- Montar los tres en la superficie web de Lumen, renderizando un rollback limpio
  como un problema RFC 9457 `422` en lugar de un `500`.
- Elegir el motor adecuado para un proceso dado, y reconocer el compromiso de
  consistencia eventual que asume cada uno.

## Conceptos que conocerás

Antes de la primera línea de código, aquí están las ideas en las que se apoya
este capítulo. Cada una se reintroduce en contexto allí donde se usa por primera
vez; esta es la versión breve.

> **Note** **Término clave — saga.** Una *saga* es una secuencia de transacciones
> locales donde cada paso tiene una acción *compensatoria* que lo deshace
> semánticamente. Si un paso posterior falla, el motor ejecuta las compensaciones
> de los pasos completados en orden inverso. Así es como se consigue un "todo o
> nada" entre servicios que no pueden compartir una única transacción de base de
> datos. El equivalente en Java es el patrón `@Saga` / `@SagaStep`; pyfly lo
> expresa con decoradores de saga.

> **Note** **Término clave — compensación.** Una *compensación* no es un rollback
> de base de datos — es un *deshacer semántico*. "Reabonar al origen" es un
> `deposit` completamente nuevo que restaura el saldo y deja tras de sí un evento
> de reembolso auditable; no borra la historia, añade un hecho correctivo.

> **Note** **Término clave — workflow (DAG).** Un *workflow* es un grafo dirigido
> acíclico de pasos. Los pasos sin dependencia entre ellos se ejecutan
> concurrentemente en la misma *capa topológica*; un paso que declara
> `depends_on` espera a sus predecesores. Úsalo cuando un proceso tenga ramas
> independientes que deban ejecutarse en paralelo y luego unirse.

> **Note** **Término clave — TCC (Try-Confirm-Cancel).** El *TCC* es un protocolo
> en dos fases: **Try** en cada participante (reservar recursos) y luego
> **Confirm** en todos si hay éxito; ante cualquier fallo en el Try, **Cancel** en
> los participantes ya intentados. Mientras que una saga aplica cada rama de
> inmediato y la deshace más tarde, el TCC reserva primero y solo confirma una vez
> que cada reserva ha tenido éxito.

> **Note** **Término clave — consistencia eventual.** Operar entre agregados
> independientes sin un bloqueo distribuido implica que hay una ventana en la que
> una rama se ha confirmado y otra no. Estos motores garantizan la consistencia
> *al final* — todas las ramas confirmadas, o todas compensadas — no en cada
> instante.

`firefly-orchestration` incluye los tres motores clásicos de transacciones
distribuidas en los que coincide toda plataforma Firefly. Cada uno compone pasos
async, se ejecuta como un simple future sobre la task del llamador, aplica una
política de reintentos por paso, hila un blackboard de contexto tipado y respeta
la cancelación cooperativa. Y — esta es la propiedad clave — no los construyes a
mano como valores. Lumen declara cada motor con una macro de atributo sobre un
bloque `impl`, exactamente como declara los handlers de CQRS y los controladores.

| Motor      | Topología                  | Compensación                       | Se declara con                     |
|------------|----------------------------|------------------------------------|------------------------------------|
| `Saga`     | Pasos ordenados por dependencias | Orden inverso, política configurable | `#[saga]` + `#[saga_step]`         |
| `Workflow` | DAG con capas paralelas    | Orden inverso, política configurable | `#[workflow]` + `#[workflow_step]` |
| `Tcc`      | Try a todos, luego Confirm a todos | Cancel a los intentados ante un fallo de Try | `#[tcc]` + `#[participant]`        |

> **Design note.** El modelo de orquestación de Firefly es *declarativo*. Escribes
> un bloque `impl` corriente de métodos `async fn(&self, …) -> Result<T, E>` y los
> anotas: `#[saga_step]` para una rama de saga, `#[workflow_step]` para un nodo de
> DAG, `#[participant]` para un actor de TCC. La macro baja esos métodos a los
> mismos motores de `firefly-orchestration` — `depends_on` los ordena,
> `compensate` nombra el deshacer, el `Ok(T)` de un paso se publica para los pasos
> posteriores, y un `Err(E)` dispara la compensación en orden inverso. Si has
> usado `@Saga` de Java o los decoradores de saga de pyfly, esta es la forma de
> escribirlo en Rust: el flujo de control vive en métodos que puedes leer de
> arriba abajo, y el cableado se genera por ti.

## Paso 1 — Entender el problema de las escrituras distribuidas

Concreta los modos de fallo antes de escribir una línea de código. Una
transferencia de Lumen tiene dos ramas:

1. **Adeudar el origen** — `withdraw(amount)`, que impone `balance >= 0`.
2. **Abonar el destino** — `deposit(amount)`.

Cada rama es una llamada independiente al `Ledger` que añade al propio flujo de
eventos de ese monedero. Un monedero de destino inexistente, o un descubierto en
el origen, hace fallar una rama después de que la otra ya pueda haberse
confirmado. Reintentar la operación *completa* es inseguro — podrías adeudar dos
veces. Saltarte en silencio la rama fallida deja los saldos inconsistentes.

La respuesta de principios es la **consistencia eventual con compensación
explícita**. Cada rama se confirma en su propio flujo de forma independiente, y
diseñas una ruta de recuperación — una transacción compensatoria — para cada paso
que pueda tener éxito antes de que falle uno posterior. "Reabonar al origen" es un
`deposit` completamente nuevo que restaura el saldo, y deja tras de sí un evento
de reembolso auditable.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 220" role="img"
     aria-label="Saga with compensation: forward steps debit, credit and notify run in dependency order; if credit fails, the engine runs the debit's compensation in reverse order to refund"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="280.0" y="24.0" text-anchor="middle" font-size="12" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">forward: dependency-ordered steps</text>
<rect x="40.0" y="50.5" width="150.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="40.0" y="48.0" width="150.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="115.0" y="71.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">debit</text><text x="115.0" y="85.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">withdraw(amount)</text><line x1="190.0" y1="74.0" x2="216.0" y2="74.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="224.0,74.0 216.0,78.5 216.0,69.5" fill="#b5531f"/><rect x="224.0" y="50.5" width="150.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="224.0" y="48.0" width="150.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="299.0" y="71.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">credit</text><text x="299.0" y="85.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">deposit(amount)</text><line x1="374.0" y1="74.0" x2="400.0" y2="74.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="408.0,74.0 400.0,78.5 400.0,69.5" fill="#b5531f"/><rect x="408.0" y="50.5" width="150.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="408.0" y="48.0" width="150.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="483.0" y="71.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">notify</text><text x="483.0" y="85.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">publish event</text>
<text x="299.0" y="44.0" text-anchor="middle" font-size="10.5" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">may fail</text>
<path d="M299.0,100 V150 H115.0 V108" fill="none" stroke="#b03a2e" stroke-width="2.6" stroke-dasharray="6 5" stroke-linecap="round"/>
<polygon points="115.0,100 110.5,109.0 119.5,109.0" fill="#b03a2e"/>
<text x="207.0" y="143.0" text-anchor="middle" font-size="11" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">compensate — reverse order</text>
<text x="280.0" y="200.0" text-anchor="middle" font-size="11" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">a compensation is a forward undo, not a database rollback</text>
</svg>
<figcaption>Una saga ejecuta sus pasos en orden de dependencias. Si un paso falla, el motor ejecuta las compensaciones de los pasos ya completados en <strong>orden inverso</strong> — aquí un <code>credit</code> fallido reembolsa el <code>debit</code>. Una compensación es una acción hacia delante que deshace, no un rollback de base de datos.</figcaption>
</figure>

Lo que acaba de ocurrir: nombraste las dos escrituras, viste por qué ni el
reintento ni saltarse el fallo son seguros, y te decantaste por la forma de
saga — adeudar y luego abonar, con un reembolso esperando por si el abono llega a
fallar. El resto del capítulo convierte esa forma en código.

> **Tip** **Punto de control.** Sabes enunciar, en una sola frase cada uno, por
> qué una transferencia de dinero no puede ser una única transacción de base de
> datos y qué hace una compensación que un rollback no hace. Si ambos están
> claros, estás listo para declarar la saga.

## Paso 2 — Declarar los tipos de la interfaz

La transferencia de Lumen vive en `src/transfer.rs`. Empieza con los tipos que
cruzan la frontera HTTP: el cuerpo de la petición y el resultado que devuelve
`POST /api/v1/transfers`.

```rust
use serde::{Deserialize, Serialize};

/// `POST /api/v1/transfers` command — move `amount` (cents) from `from` to `to`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
#[serde(default)]
pub struct TransferRequest {
    /// The source wallet id (debited).
    pub from: String,
    /// The destination wallet id (credited).
    pub to: String,
    /// The amount to move, in minor units (cents); must be `> 0`.
    pub amount: i64,
}

/// The result of a completed (or compensated) transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
pub struct TransferResult {
    /// `"completed"` when both legs succeeded — the lowercase `SagaStatus`.
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

Lo que acaba de ocurrir: `TransferRequest` lleva los dos ids de monedero y un
importe en unidades menores (céntimos). `TransferResult` refleja el estado de la
saga como una cadena en minúsculas más las dos listas de pasos, de modo que la API
le dice al llamador *exactamente* qué hizo el motor — qué pasos se ejecutaron y
cuáles se revirtieron.

> **Note** **Término clave — `firefly::Schema`.** El derive `Schema` enseña a la
> documentación OpenAPI autogenerada (servida en el puerto de gestión) qué aspecto
> tiene este DTO. Es el equivalente en Rust de la reflexión de modelos de
> springdoc, calculado en tiempo de compilación. Conociste la documentación del
> puerto de gestión en [Quickstart](./02-quickstart.md); todo DTO que cruza la
> interfaz lo deriva.

Una transferencia también necesita un error tipado que distinga una petición
malformada de un fallo de negocio limpio y compensado:

```rust
/// The typed error a transfer surfaces to its caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// The request was malformed (same wallet, non-positive amount).
    Invalid(String),
    /// The transfer failed and was rolled back; the inner string is the
    /// failing leg's domain error (e.g. `insufficient funds`).
    Compensated(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Invalid(detail) => f.write_str(detail),
            TransferError::Compensated(detail) => write!(f, "transfer rolled back: {detail}"),
        }
    }
}

impl std::error::Error for TransferError {}
```

Lo que acaba de ocurrir: `Invalid` es una petición incorrecta (un `422` que nunca
tocó el ledger); `Compensated` es un fallo de negocio que se ejecutó, revirtió
limpiamente y arrastra la causa de la rama fallida. Mantenerlos como variantes
distintas permite al endpoint mapear cada uno al estado HTTP correcto.

> **Note** Lumen escribe a mano `Display` + `std::error::Error` en lugar de
> incorporar `thiserror`. Esa es la misma disciplina de una sola dependencia que
> mantiene el resto del libro: todo el framework y cada error tipado siguen
> llegando a través de la única fachada `firefly`, sin ninguna crate adicional que
> alinear.

## Paso 3 — Declarar la saga

La saga es un bloque `impl` sobre una pequeña struct que contiene el `Ledger`.
Cada rama es un método anotado que llama directamente al ledger y devuelve un
`Result<(), DomainError>` tipado. No hay captura de closures, ni `Mutex` para
sacar a escondidas la causa de un canal de error borrado, ni llamada a un
builder — la macro lee los atributos y genera todo eso.

```rust
use std::sync::Arc;

use firefly::orchestration::SagaError;

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;

/// The money-transfer saga, declared with `#[firefly::saga]`: each leg is an
/// annotated method driving the `Ledger`. The macro generates
/// `TransferSaga::run` (used by `run_transfer`) and `TransferSaga::saga`.
struct TransferSaga {
    ledger: Ledger,
}

#[firefly::saga(name = "money-transfer")]
impl TransferSaga {
    /// Debit the source wallet (a real `MoneyWithdrawn` event). Rolled back by
    /// `refund_debit` when a later leg fails.
    #[saga_step(id = "debit", compensate = "refund_debit")]
    async fn debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.withdraw(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    /// Compensation for `debit`: a refund is a normal deposit, so it raises a
    /// real `MoneyDeposited` event on the source stream.
    async fn refund_debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    /// Credit the destination (a real `MoneyDeposited` event). The last leg, so
    /// a failure here rolls back only the debit.
    #[saga_step(id = "credit", depends_on = ["debit"])]
    async fn credit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.to, Money::cents(req.amount)).await?;
        Ok(())
    }
}
```

Cómo se lee, bloque a bloque:

- `debit` es el primer paso (`id = "debit"`), y nombra su deshacer con
  `compensate = "refund_debit"`.
- `refund_debit` *no* lleva el marcador `#[saga_step]` — es un método corriente
  referenciado por nombre, y la macro lo incluye en la saga generada únicamente
  porque `debit` apunta a él.
- `credit` declara `depends_on = ["debit"]`, así que el motor lo ejecuta
  estrictamente después del adeudo. No tiene compensación porque es la última
  rama: lo único que deshacer ante su fallo es el adeudo, lo cual el motor maneja
  automáticamente.

Cada rama toma `#[input] req: TransferRequest`. Ese marcador es el corazón del
modelo.

> **Note** **Término clave — inyección de parámetros.** Los parámetros de cada
> paso se *inyectan* desde el contexto de la saga mediante marcadores que la macro
> lee y elimina: `#[input]` es la entrada completa (o `#[input("field")]` para un
> único campo); `#[from_step("id")]` es el valor `Ok` que publicó un paso
> anterior; `#[variable("key")]` es una variable de contexto con alcance de saga;
> y `#[ctx]` es el propio blackboard `StepContext`. Como aquí cada paso necesita
> la petición completa, cada parámetro es `#[input] req: TransferRequest`.

El `Ok(T)` de un paso se serializa y se pone a disposición de los pasos
posteriores vía `#[from_step]`; un `Err(E)` (donde `E: std::error::Error + Send +
Sync`) dispara la compensación en orden inverso. Como los métodos devuelven
`DomainError` directamente, la causa tipada del fallo se preserva durante todo el
recorrido por el motor — sin necesidad de ningún `Mutex` compartido.

El atributo `#[saga_step]` acepta `id` (obligatorio), `depends_on = ["…"]`,
`compensate = "method"`, y las palancas de recuperación por paso `retry`,
`backoff_ms`, `timeout_ms` y `jitter`. El atributo `#[saga(...)]` acepta un
`name`, una anulación de fachada `crate` y una `policy` de compensación:

- **`best_effort`** (el valor por defecto del motor) — registra y continúa
  compensando los pasos restantes aunque una compensación falle.
- **`stop_on_error`** — aborta el rollback en el primer fallo de compensación y
  expone un `SagaError::Compensation` que envuelve el original.
- además de `retry_with_backoff`, `circuit_breaker`, `best_effort_parallel` y
  `grouped_parallel` para fan-outs mayores.

> **Note** Esta es la misma forma que `@Saga` / `@SagaStep` de Java y los
> decoradores de saga de pyfly — un método de paso, una compensación nombrada por
> cadena, un orden `depends_on` — pero bajada al sistema de tipos de Rust. Un paso
> que devuelve el tipo equivocado o que nombra una compensación inexistente es un
> *error de compilación*, no una sorpresa en tiempo de ejecución.

> **Design note.** Aquí no hay reflexión ni escaneo en tiempo de ejecución.
> `#[saga]` se expande en tiempo de compilación a las llamadas exactas
> `Saga::new(...).step(...)` que de otro modo escribirías a mano, hiladas a través
> del contrato `__rt` de la fachada `firefly` de modo que un servicio de una sola
> dependencia lo compila sin nombrar nunca `firefly-orchestration`. Si alguna vez
> necesitas construir una saga dinámicamente (pasos conocidos solo en tiempo de
> ejecución), el mismo motor expone la costura programática a la que la macro baja:
> `Saga::new(name).step(Step::with_context(id, action).with_context_compensation(undo))`.

> **Tip** **Punto de control.** Tienes una struct `TransferSaga`, un `impl`
> `#[firefly::saga]` con dos ramas `#[saga_step]` y una compensación nombrada.
> `cargo build` debería compilarlo — y si renombras `refund_debit` sin actualizar
> la cadena `compensate = "…"`, la compilación debería fallar con un mensaje que
> apunta a la línea infractora. Pruébalo y luego déjalo como estaba.

## Paso 4 — Ejecutar la saga

La macro genera dos métodos sobre el tipo:

- `TransferSaga::saga(self: Arc<Self>) -> Saga` — construye el motor a partir de
  tus pasos, su orden `depends_on`, las compensaciones y las políticas de
  reintento.
- `TransferSaga::run(self: Arc<Self>, input) -> Result<Outcome, SagaFailure>` —
  serializa `input` en un contexto de paso nuevo y ejecuta todo el DAG,
  compensando ante un fallo.

`run_transfer` valida la petición, construye el valor de saga detrás de un `Arc` y
llama al `run` generado. Si tiene éxito lee el `Outcome`; si falla, extrae el
`DomainError` tipado de la rama fallida del `SagaError::Step` del motor para que la
API pueda responder `insufficient funds` literalmente:

```rust
/// Validates and runs a money transfer as a declarative saga, returning the
/// terminal `TransferResult`.
pub async fn run_transfer(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<TransferResult, TransferError> {
    if req.amount <= 0 {
        return Err(TransferError::Invalid("amount must be > 0".into()));
    }
    if req.from == req.to {
        return Err(TransferError::Invalid("from and to must differ".into()));
    }

    let saga = Arc::new(TransferSaga {
        ledger: ledger.clone(),
    });
    match saga.run(req.clone()).await {
        Ok(outcome) => Ok(TransferResult {
            status: outcome.status.to_string(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
            steps_executed: outcome.steps_executed,
            steps_rolled_back: outcome.steps_rolled,
        }),
        Err(failure) => {
            // Surface the failing leg's typed domain error (e.g. "insufficient
            // funds"), unwrapped from the saga's generic step error.
            let detail = match failure.error() {
                SagaError::Step { source, .. } => source.to_string(),
                other => other.to_string(),
            };
            Err(TransferError::Compensated(detail))
        }
    }
}
```

Lo que el `run` generado hace por ti, línea a línea:

- `saga.run(req.clone())` serializa `req` en un `StepContext` nuevo, construye la
  saga (`debit` → `credit`, con la compensación del adeudo adjunta) y ejecuta el
  DAG.
- En el camino feliz devuelve un `Outcome` cuyo `status` es `Completed`,
  `steps_executed` lista las ramas que se ejecutaron, y `steps_rolled` está vacío.
  Fíjate en que el campo es `outcome.steps_rolled` del lado del motor;
  `run_transfer` lo copia en el campo de la interfaz `steps_rolled_back`.
- Ante un fallo devuelve un `SagaFailure`: su `outcome()` está totalmente poblado
  (estado `Compensated`, con `steps_rolled` nombrando las compensaciones que se
  ejecutaron), y su `error()` es un `SagaError`. Hacemos match contra
  `SagaError::Step { source, .. }` para recuperar el mensaje del `DomainError` de
  la rama — así es como `POST /api/v1/transfers` responde `insufficient funds` en
  lugar de un opaco `step "credit" failed`.

> **Note** **Término clave — `Outcome` / `SagaFailure`.** `Outcome` es el registro
> terminal de la saga: `status` (un `SagaStatus` que se muestra en minúsculas —
> `completed` / `compensated` / `failed`), `steps_executed` y `steps_rolled`.
> `SagaFailure` es el par de fallo — `outcome()` da el mismo registro, y `error()`
> da el `SagaError` tipado que finalizó la ejecución. No hay un flag separado de
> "¿revirtió?" que consultar; el outcome te lo dice todo.

> **Tip** **Punto de control.** `run_transfer` compila y sus tres ramas están
> claras: un fallo de validación `Invalid`, un camino feliz `Ok(Outcome)`, y un
> fallo `SagaError::Step` extraído a `TransferError::Compensated`. Estás listo para
> montarlo.

## Paso 5 — Montar el endpoint de la saga

El método del controlador en `src/web.rs` es delgado: acciona la saga y luego
traduce el resultado tipado al contrato HTTP. Un rollback limpio es un fallo de
*negocio*, así que se expone como un problema `422` que lleva la causa — no un
`500`:

```rust
/// `POST /api/v1/transfers` — run a money transfer as a saga.
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
) -> WebResult<Json<TransferResult>> {
    let result = run_transfer(&api.ledger, &body)
        .await
        .map_err(|e| match e {
            TransferError::Invalid(detail) => WebError::from(FireflyError::validation(detail)),
            TransferError::Compensated(detail) => {
                WebError::from(FireflyError::validation(detail))
            }
        })?;
    // A transfer touches both wallets' views; invalidate the family.
    api.query_cache.invalidate_type::<GetWallet>();
    Ok(Json(result))
}
```

Lo que acaba de ocurrir:

- `run_transfer` devuelve el `TransferError` tipado; el `map_err` traduce ambas
  variantes a un problema de validación. `FireflyError::validation(...)` se
  renderiza como un documento RFC 9457 `422 application/problem+json` que lleva la
  cadena de detalle, de modo que el llamador ve `insufficient funds`, no una traza
  de pila.
- `invalidate_type::<GetWallet>()` descarta las vistas `GetWallet` cacheadas,
  porque una transferencia cambió dos saldos y una lectura tras la escritura debe
  ser honesta. Esa caché y su invalidación son el tema de
  [Caching](./17-caching.md); la transferencia es simplemente una mutación más que
  juega con sus reglas.

> **Note** Este handler vive dentro del `impl WalletApi` con
> `#[rest_controller(path = "...")]` de Lumen, montado automáticamente al arrancar
> — nunca editas `main` para añadir una ruta. `WebResult<T>` es `Result<T,
> WebError>`, y cualquier `WebError` se renderiza como un problema RFC 9457.
> Conociste ambos en [Tu primera API HTTP](./06-first-http-api.md).

## Paso 6 — Leer los tres caminos de la saga

Los tests en `src/transfer.rs` ejercitan los tres caminos, y son la mejor
documentación del comportamiento. El **camino feliz** mueve los fondos y no
revierte nada:

```rust
let result = run_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: dst.id.clone(), amount: 300 },
)
.await
.unwrap();

assert_eq!(result.status, "completed");
assert_eq!(result.steps_executed, ["debit", "credit"]);
assert!(result.steps_rolled_back.is_empty());
assert_eq!(balance(&ledger, &src.id).await, 700);
assert_eq!(balance(&ledger, &dst.id).await, 300);
```

El camino de **descubierto** cortocircuita en el adeudo — el origen nunca tiene
los fondos, así que el withdraw falla *antes* de que se aplique nada. No hay nada
que compensar, y ambos saldos quedan intactos:

```rust
let err = run_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: dst.id.clone(), amount: 500 },
)
.await
.unwrap_err();

assert_eq!(err, TransferError::Compensated("insufficient funds".into()));
assert_eq!(balance(&ledger, &src.id).await, 100); // untouched
assert_eq!(balance(&ledger, &dst.id).await, 0);   // untouched
```

El camino de **fallo en el abono** es donde la compensación se gana su sueldo. El
adeudo se aplicó, luego el abono falló (el destino no existe), así que el motor
ejecuta la compensación del adeudo — un depósito de reembolso. El saldo neto del
origen queda restaurado, y el flujo registra *tanto* el adeudo como su reembolso,
un rastro de auditoría de exactamente lo que ocurrió:

```rust
let err = run_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: "wlt_missing".into(), amount: 400 },
)
.await
.unwrap_err();
assert!(matches!(err, TransferError::Compensated(_)));

// open(1000) − withdraw(400) + refund(400) = 1000, with 3 events on the stream.
let src_events = ledger.load_events(&src.id).await.unwrap();
assert_eq!(Wallet::rehydrate(&src.id, &src_events).view().balance, 1_000);
assert_eq!(src_events.len(), 3); // open + withdraw + refund-deposit
```

Lo que acaba de ocurrir: la tercera aserción es el quid de la compensación como
*deshacer semántico*. El saldo se restaura a `1_000`, pero el flujo **no** mide
dos eventos como si nada hubiera pasado — mide *tres* eventos: el open, el
withdraw y el depósito de reembolso. La historia de lo que realmente ocurrió se
preserva y es auditable.

> **Note** Una saga no te da serializabilidad. Entre el momento en que se adeuda
> el origen y el momento en que el abono se confirma (o se ejecuta el reembolso),
> otra petición podría leer el origen y ver un saldo más bajo del que tendrá en
> última instancia. Ese es el compromiso de operar entre agregados independientes
> sin un bloqueo distribuido: consistencia *al final* — todas las ramas
> confirmadas, o todas compensadas — no en cada instante.

> **Tip** **Punto de control.** Ejecuta `cargo test -p lumen transfer`. Los tests
> del camino feliz, del descubierto y del fallo en el abono pasan, y el test del
> fallo en el abono confirma tres eventos en el flujo de origen. Ese rastro de tres
> eventos es tu prueba de que la compensación añadió en lugar de borrar.

## Paso 7 — Añadir un workflow de cumplimiento en paralelo

Una transferencia grande debería pasar por un filtro de comprobaciones de
cumplimiento *antes* de que el dinero se mueva. Esas comprobaciones son
independientes entre sí — una comprobación de saldo y un tope por transferencia no
tienen nada que ver entre ellas — así que deberían ejecutarse en paralelo. Eso es
un `Workflow`: un DAG de nodos con declaraciones `depends_on`, donde los nodos
independientes se ejecutan concurrentemente dentro de una capa topológica y un
nodo que declara dependencias se ejecuta solo después de que estas se completen.

Lo declaras con `#[firefly::workflow]` y marcas cada nodo con `#[workflow_step]` —
la misma inyección de parámetros que una saga. `#[workflow_step]` acepta `id`
(obligatorio), `depends_on = ["…"]`, `compensate = "method"`, `when = "expr"` (una
condición de salto — el nodo se omite cuando el predicado es falso) y
`fire_and_forget` (planifica el nodo sin bloquear la capa). La macro genera
`Workflow::workflow(self: Arc<Self>)` y `run(self, input) -> Result<(),
WorkflowError>`.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 220" role="img"
     aria-label="Workflow DAG: balance-check and limit-check run in parallel in one layer and both feed the approve gate, which depends on both"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="170.0" y="26.0" text-anchor="middle" font-size="11" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">parallel layer</text>
<rect x="40.0" y="42.5" width="188.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="40.0" y="40.0" width="188.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="134.0" y="63.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">balance-check</text><text x="134.0" y="77.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">funds_ok: bool</text>
<rect x="40.0" y="130.5" width="188.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="40.0" y="128.0" width="188.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="134.0" y="151.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">limit-check</text><text x="134.0" y="165.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">within_limit: bool</text>
<rect x="360.0" y="86.5" width="188.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="360.0" y="84.0" width="188.0" height="52.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="454.0" y="107.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">approve</text><text x="454.0" y="121.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends_on both</text>
<path d="M228.0,66.0 Q288.2,105.2 352.0,102.4" fill="none" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="360.0,102.0 352.2,106.9 351.8,97.9" fill="#b5531f"/>
<path d="M228.0,154.0 Q288.2,114.8 352.0,117.6" fill="none" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="360.0,118.0 351.8,122.1 352.2,113.1" fill="#b5531f"/>
</svg>
<figcaption>Un workflow es un DAG de pasos. <code>balance-check</code> y <code>limit-check</code> no tienen dependencia entre sí, así que se ejecutan en la misma capa paralela; <code>approve</code> espera a ambos y consume sus veredictos.</figcaption>
</figure>

El `src/compliance.rs` de Lumen ejecuta dos comprobaciones independientes en
paralelo y luego un filtro de aprobación que consume ambas. Primero, el tipo de
error y la entrada de política:

```rust
use std::sync::Arc;

use firefly::orchestration::WorkflowError;

use crate::domain::Wallet;
use crate::ledger::Ledger;
use crate::transfer::TransferRequest;

/// The per-transfer ceiling, in minor units (cents).
pub const MAX_TRANSFER_CENTS: i64 = 1_000_000; // 10,000.00

/// Why a transfer failed compliance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComplianceError {
    /// The source wallet does not exist, so its balance cannot be checked.
    NotFound(String),
    /// A check failed — the transfer is not allowed (the string says why).
    Rejected(String),
}

impl std::fmt::Display for ComplianceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComplianceError::NotFound(id) => write!(f, "source wallet {id} not found"),
            ComplianceError::Rejected(why) => write!(f, "transfer rejected: {why}"),
        }
    }
}

impl std::error::Error for ComplianceError {}
```

Ahora el workflow en sí. `balance-check` y `limit-check` no tienen dependencia
entre sí, así que el motor los ejecuta en la misma capa topológica; `approve`
declara `depends_on` sobre ambos y lee sus veredictos booleanos a través de
`#[from_step(...)]`:

```rust
/// The compliance workflow: each node drives the `Ledger` or a policy input.
struct ComplianceCheck {
    ledger: Ledger,
    max_cents: i64,
}

#[firefly::workflow(name = "transfer-compliance")]
impl ComplianceCheck {
    /// Does the source wallet hold enough to cover the transfer? Reads the real
    /// source aggregate. Errors if the source does not exist.
    #[workflow_step(id = "balance-check")]
    async fn balance_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> {
        let events = self
            .ledger
            .load_events(&req.from)
            .await
            .map_err(|e| ComplianceError::NotFound(e.to_string()))?;
        if events.is_empty() {
            return Err(ComplianceError::NotFound(req.from.clone()));
        }
        let balance = Wallet::rehydrate(&req.from, &events).view().balance;
        Ok(balance >= req.amount)
    }

    /// Is the amount within the per-transfer ceiling? Independent of the
    /// balance check, so it runs in the same parallel layer.
    #[workflow_step(id = "limit-check")]
    async fn limit_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> {
        Ok(req.amount <= self.max_cents)
    }

    /// The decision node: runs only after both checks (`depends_on`) and
    /// consumes their boolean verdicts via `#[from_step]`.
    #[workflow_step(id = "approve", depends_on = ["balance-check", "limit-check"])]
    async fn approve(
        &self,
        #[from_step("balance-check")] funds_ok: bool,
        #[from_step("limit-check")] within_limit: bool,
    ) -> Result<(), ComplianceError> {
        if !funds_ok {
            return Err(ComplianceError::Rejected("insufficient funds".into()));
        }
        if !within_limit {
            return Err(ComplianceError::Rejected(format!(
                "amount exceeds the {} cent per-transfer ceiling",
                self.max_cents
            )));
        }
        Ok(())
    }
}
```

Lo que acaba de ocurrir — y por qué importa: esta es la recompensa del modelo de
inyección. `balance_check` devuelve `Ok(true)` u `Ok(false)`, y el motor serializa
ese `bool` bajo el id de nodo `balance-check`. `approve` declara
`#[from_step("balance-check")] funds_ok: bool`, y la macro deserializa el valor
almacenado de vuelta a ese parámetro — tipado en ambos extremos, sin fontanería de
contexto manual. `balance-check` lee el agregado de origen *real* desde el
`Ledger`; solo el tope por transferencia es una entrada de política nueva.

> **Tip** **Punto de control.** Fíjate en la diferencia de topología respecto a la
> saga: el `credit` de la saga declara `depends_on = ["debit"]` para que los dos se
> ejecuten *en serie*; el `balance-check` y el `limit-check` del workflow *no*
> declaran dependencia entre sí, así que se ejecutan *en la misma capa*. Solo
> `approve` espera. Esa única diferencia de `depends_on` es la diferencia entre una
> cadena y un DAG.

## Paso 8 — Ejecutar el workflow y recuperar la causa

`run_compliance` construye el workflow detrás de un `Arc` y llama al `run`
generado. `Ok(())` significa aprobado; un `Err` se recupera en un
`ComplianceError` tipado. El motor de workflow expone un fallo de nodo como
`WorkflowError::Node { source, .. }`, donde el `source` empaquetado se puede hacer
downcast de vuelta al error original:

```rust
/// Runs the compliance workflow for `req`. `Ok(())` means the transfer is
/// approved (both checks passed); `Err` carries the typed reason it was rejected.
pub async fn run_compliance(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<(), ComplianceError> {
    let check = Arc::new(ComplianceCheck {
        ledger: ledger.clone(),
        max_cents: MAX_TRANSFER_CENTS,
    });
    match check.run(req.clone()).await {
        Ok(()) => Ok(()),
        Err(failure) => Err(compliance_cause(failure)),
    }
}

/// Recovers a typed `ComplianceError` from the failing node's error.
fn compliance_cause(failure: WorkflowError) -> ComplianceError {
    let detail = match &failure {
        WorkflowError::Node { source, .. } => {
            if let Some(err) = source.downcast_ref::<ComplianceError>() {
                return err.clone();
            }
            source.to_string()
        }
        other => other.to_string(),
    };
    if detail.contains("not found") {
        ComplianceError::NotFound(detail)
    } else {
        ComplianceError::Rejected(detail)
    }
}
```

Lo que acaba de ocurrir: `WorkflowError::Node` empaqueta el error del nodo fallido
como un `source`. `compliance_cause` primero intenta
`downcast_ref::<ComplianceError>()` para recuperar la variante tipada exacta; si
lo consigue, devuelve el error original literalmente. La comparación de cadenas de
respaldo es una ruta de cinturón y tirantes para cuando el tipo empaquetado no se
puede hacer downcast.

El endpoint en `src/web.rs` es una comprobación previa de solo lectura que nunca
mueve fondos — `200 OK` con la decisión cuando se aprueba, `404` cuando el
monedero de origen es desconocido, y `422` que lleva el motivo cuando una
comprobación de cumplimiento rechaza:

```rust
/// `POST /api/v1/transfers/compliance` — gate a transfer through the parallel
/// compliance workflow (balance + limit checks → approve).
#[post(
    "/transfers/compliance",
    summary = "Compliance-gated transfer (workflow)",
    description = "Runs the parallel compliance workflow (balance + limit checks) before approving a transfer.",
    tags = ["Transfers"],
    status = 200
)]
async fn transfer_compliance(
    State(api): State<WalletApi>,
    Json(body): Json<TransferRequest>,
) -> WebResult<Json<serde_json::Value>> {
    run_compliance(&api.ledger, &body).await.map_err(|e| match e {
        // An unknown source wallet is a 404 (like GET /wallets/:id); a
        // failed check is a 422.
        ComplianceError::NotFound(detail) => WebError::from(FireflyError::not_found(detail)),
        ComplianceError::Rejected(detail) => WebError::from(FireflyError::validation(detail)),
    })?;
    Ok(Json(serde_json::json!({
        "decision": "approved",
        "from": body.from,
        "to": body.to,
        "amount": body.amount,
    })))
}
```

Lo que acaba de ocurrir: un origen ausente se mapea a `FireflyError::not_found`
(un problema `404`, coherente con `GET /wallets/:id`), y una comprobación
rechazada se mapea a `FireflyError::validation` (un problema `422`). Como la
comprobación nunca mueve fondos, no hay caché que invalidar.

> **Note** El starter de la capa de experiencia, `firefly-starter-experience`,
> construye sobre exactamente este motor de workflow con pasos *dirigidos por
> señales* que se aparcan hasta que un llamador externo entrega una señal nombrada,
> y luego se reanudan desde donde lo dejaron. Volvemos a esa capa en
> [HTTP Clients](./13-http-clients.md).

> **Tip** **Punto de control.** Ejecuta `cargo test -p lumen compliance`. Una
> transferencia financiada y dentro del límite se aprueba; una con descubierto es
> `Rejected`; una que excede el tope es `Rejected` con un mensaje de "ceiling"; un
> origen desconocido es `NotFound`.

## Paso 9 — Replantear la transferencia como TCC

La misma transferencia se puede modelar de una segunda forma —
reservar-y-luego-capturar — y Lumen incluye ambas para que las puedas comparar.
`Tcc` ejecuta un protocolo en dos fases: **Try** en cada participante (reservar
recursos) y luego **Confirm** en todos si hay éxito; ante cualquier fallo de Try,
**Cancel** en los participantes ya intentados, en orden inverso. Mientras que una
saga aplica cada rama de inmediato y deshace una rama confirmada ante un fallo, el
TCC reserva primero y solo confirma una vez que cada reserva ha tenido éxito — de
modo que una reserva fallida se cancela, nunca se compensa a posteriori.

Lo declaras con `#[firefly::tcc]` y marcas cada método *try* con
`#[participant(name, confirm, cancel)]`. Los métodos confirm y cancel son simples
`async fn` referenciados por nombre. El resultado del try de un participante se
publica bajo su nombre, de modo que confirm y cancel pueden leerlo vía
`#[from_step("<name>")]`. `#[participant]` acepta `name` y `confirm`
(obligatorios), además de `cancel`, `retry`, `backoff_ms` y `timeout_ms`. La macro
genera `Tcc::tcc(self: Arc<Self>)` y `run(self, input) -> Result<(), TccError>`.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 616 250" role="img"
     aria-label="TCC phases for two participants source and dest: a Try column reserves, a Confirm column captures on success, and a Cancel column releases on a try failure"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="176.0" y="28.0" text-anchor="middle" font-size="14" font-weight="800" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Try</text>
<text x="176.0" y="44.0" text-anchor="middle" font-size="10" font-weight="600" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">reserve</text>
<text x="356.0" y="28.0" text-anchor="middle" font-size="14" font-weight="800" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Confirm</text>
<text x="356.0" y="44.0" text-anchor="middle" font-size="10" font-weight="600" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">on all-tried</text>
<text x="536.0" y="28.0" text-anchor="middle" font-size="14" font-weight="800" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Cancel</text>
<text x="536.0" y="44.0" text-anchor="middle" font-size="10" font-weight="600" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">on a try failure</text>
<text x="20.0" y="88.0" text-anchor="start" font-size="11.5" font-weight="700" fill="#8a6d3b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">source</text>
<rect x="97.0" y="62.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="97.0" y="60.0" width="158.0" height="46.0" rx="9" fill="#fdf6ea" stroke="#d4793a" stroke-width="1.5"/><text x="176.0" y="87.5" text-anchor="middle" font-size="11" font-weight="700" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">withdraw (hold)</text>
<rect x="277.0" y="62.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="277.0" y="60.0" width="158.0" height="46.0" rx="9" fill="#ecf9f0" stroke="#1f8a4c" stroke-width="1.5"/><text x="356.0" y="87.5" text-anchor="middle" font-size="11" font-weight="700" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">(none — held)</text>
<rect x="457.0" y="62.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="457.0" y="60.0" width="158.0" height="46.0" rx="9" fill="#fdecea" stroke="#b03a2e" stroke-width="1.5"/><text x="536.0" y="87.5" text-anchor="middle" font-size="11" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">deposit (release)</text>
<text x="20.0" y="162.0" text-anchor="start" font-size="11.5" font-weight="700" fill="#8a6d3b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">dest</text>
<rect x="97.0" y="136.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="97.0" y="134.0" width="158.0" height="46.0" rx="9" fill="#fdf6ea" stroke="#d4793a" stroke-width="1.5"/><text x="176.0" y="161.5" text-anchor="middle" font-size="11" font-weight="700" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">verify exists</text>
<rect x="277.0" y="136.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="277.0" y="134.0" width="158.0" height="46.0" rx="9" fill="#ecf9f0" stroke="#1f8a4c" stroke-width="1.5"/><text x="356.0" y="161.5" text-anchor="middle" font-size="11" font-weight="700" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">deposit (capture)</text>
<rect x="457.0" y="136.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="457.0" y="134.0" width="158.0" height="46.0" rx="9" fill="#fdecea" stroke="#b03a2e" stroke-width="1.5"/><text x="536.0" y="161.5" text-anchor="middle" font-size="11" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">(none — nothing held)</text>
<line x1="252.0" y1="216.0" x2="260.0" y2="216.0" stroke="#1f8a4c" stroke-width="2.5" stroke-linecap="round"/><polygon points="268.0,216.0 260.0,220.5 260.0,211.5" fill="#1f8a4c"/>
<text x="348.0" y="212.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">all tried → confirm</text>
<text x="430.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">any try fails → cancel tried in reverse</text>
</svg>
<figcaption>Try / Confirm / Cancel. El <strong>Try</strong> de cada participante reserva; una vez que todos han intentado, <strong>Confirm</strong> captura; si algún Try falla, el motor <strong>Cancela</strong> los participantes ya intentados en orden inverso. El origen retiene fondos en el Try y los libera en el Cancel; el destino captura en el Confirm.</figcaption>
</figure>

El `src/tcc_transfer.rs` de Lumen modela la transferencia como
reservar-y-luego-capturar. El try del origen *retiene* los fondos adeudando ahora;
su confirm es un no-op (el adeudo ya capturó), y su cancel libera la retención con
un reembolso. El try del destino *verifica* que existe (todavía no hay nada
confirmado, así que no hay cancel); su confirm captura abonando:

```rust
use std::sync::Arc;

use firefly::orchestration::TccError;
use serde::{Deserialize, Serialize};

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;
use crate::transfer::{TransferError, TransferRequest};

/// The wire result of a confirmed two-phase transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
pub struct TccTransferResult {
    /// `"confirmed"` when both participants captured.
    pub status: String,
    pub from: String,
    pub to: String,
    pub amount: i64,
}

/// The two-phase transfer coordinator: each participant drives the `Ledger`.
struct TwoPhaseTransfer {
    ledger: Ledger,
}

#[firefly::tcc(name = "transfer-2pc")]
impl TwoPhaseTransfer {
    /// Source **try**: hold the funds by debiting now (a real `MoneyWithdrawn`).
    #[participant(name = "source", confirm = "capture_source", cancel = "release_source")]
    async fn hold_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .withdraw(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }
    /// Source **confirm**: the debit on try already captured the funds.
    async fn capture_source(&self) -> Result<(), DomainError> {
        Ok(())
    }
    /// Source **cancel**: release the hold by refunding it (a real `MoneyDeposited`).
    async fn release_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .deposit(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }

    /// Destination **try**: pre-authorize by verifying the destination exists;
    /// nothing is committed yet, so there is no cancel.
    #[participant(name = "dest", confirm = "capture_dest")]
    async fn hold_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        let events = self.ledger.load_events(&req.to).await?;
        if events.is_empty() {
            return Err(DomainError::NotFound(req.to.clone()));
        }
        Ok(())
    }
    /// Destination **confirm**: capture by crediting the destination.
    async fn capture_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.to, Money::cents(req.amount)).await?;
        Ok(())
    }
}
```

Cómo se lee: el participante `source` nombra las tres fases —
`confirm = "capture_source"`, `cancel = "release_source"` — mientras que `dest`
omite `cancel` porque su try no retiene nada. El confirm `capture_source` toma solo
`&self`: un método de participante sin parámetros inyectados es válido, y un
confirm no-op es exactamente la forma correcta cuando el try ya capturó.

> **Tip** **Punto de control.** Compara los participantes de origen y destino. El
> try del origen *confirma un efecto secundario* (el withdraw), así que necesita un
> cancel real que reembolse. El try del destino solo *lee* (verifica la
> existencia), así que no retiene nada y no necesita cancel. La asimetría es
> intencionada y es exactamente por lo que el TCC te permite omitir un cancel
> cuando no hay nada que liberar.

## Paso 10 — Ejecutar el TCC y montarlo

`run_tcc_transfer` construye el coordinador detrás de un `Arc` y lo ejecuta. Si
tiene éxito, ambos lados capturaron (`status: "confirmed"`); ante cualquier fallo
de reserva los participantes intentados se cancelan y la causa de la fase fallida
se renderiza a partir de `TccError`:

```rust
/// Validates and runs a two-phase transfer. On success both sides captured
/// (`status: "confirmed"`); on any reservation failure the tried participants
/// are cancelled (the source hold released) and this returns
/// `TransferError::Compensated` with the cause.
pub async fn run_tcc_transfer(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<TccTransferResult, TransferError> {
    if req.amount <= 0 {
        return Err(TransferError::Invalid("amount must be > 0".into()));
    }
    if req.from == req.to {
        return Err(TransferError::Invalid("from and to must differ".into()));
    }
    let tcc = Arc::new(TwoPhaseTransfer {
        ledger: ledger.clone(),
    });
    match tcc.run(req.clone()).await {
        Ok(()) => Ok(TccTransferResult {
            status: "confirmed".into(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
        }),
        Err(err) => Err(TransferError::Compensated(tcc_cause(err))),
    }
}

/// Renders the failing phase's cause for the caller.
fn tcc_cause(err: TccError) -> String {
    match err {
        TccError::Try { source, .. } => source.to_string(),
        TccError::Confirm(errors) => errors
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; "),
    }
}
```

Lo que acaba de ocurrir: `TccError::Try { source, .. }` lleva la reserva que falló
(p. ej. un origen con descubierto o un destino inexistente); `tcc_cause` renderiza
su mensaje. `TccError::Confirm(errors)` recopila los fallos de la fase de confirm
si una captura falla de algún modo después de que todas las reservas tuvieran
éxito — sus mensajes se unen con `; `. El endpoint refleja el de la saga: `200 OK`
con el resultado confirmado, o `422` cuando una reserva falló y la retención del
origen se liberó.

```rust
/// `POST /api/v1/transfers/2pc` — run a two-phase (Try/Confirm/Cancel) transfer
/// via the TCC coordinator.
#[post(
    "/transfers/2pc",
    summary = "Two-phase transfer (TCC)",
    description = "Runs a Try/Confirm/Cancel two-phase transfer via the TCC coordinator.",
    tags = ["Transfers"],
    status = 200
)]
async fn transfer_2pc(
    State(api): State<WalletApi>,
    Json(body): Json<TransferRequest>,
) -> WebResult<Json<TccTransferResult>> {
    let result = run_tcc_transfer(&api.ledger, &body)
        .await
        .map_err(|e| match e {
            TransferError::Invalid(detail) => WebError::from(FireflyError::validation(detail)),
            TransferError::Compensated(detail) => {
                WebError::from(FireflyError::validation(detail))
            }
        })?;
    api.query_cache.invalidate_type::<GetWallet>();
    Ok(Json(result))
}
```

Los tests fijan la semántica de dos fases. Una transferencia a un destino
inexistente *retiene y luego libera* el origen, dejándolo intacto:

```rust
let err = run_tcc_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: "wlt_missing".into(), amount: 400 },
)
.await
.unwrap_err();
assert!(matches!(err, TransferError::Compensated(_)));
// Source try held the funds, then the dest try failed → source cancel
// released them: the hold + its release net to the original balance.
assert_eq!(balance(&ledger, &src.id).await, 1_000);
```

> **Note** Lumen incluye *ambos* planteamientos de la misma transferencia para que
> los puedas comparar. La saga aplica cada rama localmente y reembolsa el adeudo si
> el abono falla — lo más sencillo cuando un deshacer es en sí mismo una acción
> local limpia. El TCC reserva en ambos lados, y luego confirma o libera de forma
> conjunta — mejor cuando un participante puede *retener* una reserva de forma
> barata y quieres semántica de todo o nada sin ninguna ventana en la que un lado
> se haya confirmado y el otro no.

> **Tip** **Punto de control.** Ejecuta `cargo test -p lumen tcc_transfer`. El test
> de éxito mueve los fondos y reporta `confirmed`; el test del destino inexistente
> retiene y luego libera el origen, de modo que su saldo vuelve a `1_000`; el test
> del origen insuficiente aborta antes de retener nada.

## Paso 11 — Cancelación

Los tres motores respetan un `CancellationToken` para la cancelación cooperativa.
Los motores dependen únicamente de `futures`, así que cualquier executor (Tokio
incluido) los acciona. El `run` declarativo siempre honra un token hilado a través
del contexto; cuando necesitas accionarlo de forma explícita, la API del builder de
más bajo nivel expone `run_cancellable(&token)`. Cancela el token desde un timeout
o una señal de apagado para drenar la ejecución.

```rust,ignore
// Sketch — the builder seam `#[saga]` lowers onto, for a run you cancel yourself.
let token = firefly::orchestration::CancellationToken::new();
let outcome = saga.run_cancellable(&token).await?;
// elsewhere: token.cancel();  // drains the run cooperatively
```

Lo que acaba de ocurrir: la cancelación es *cooperativa* — el motor comprueba el
token antes de ejecutar el siguiente paso, así que un paso en curso termina pero
ningún paso posterior arranca. Una ejecución cancelada se expone como
`SagaError::Cancelled` (y los equivalentes en `WorkflowError` / `TccError`), no
como un fallo de paso.

## Resumen — qué cambió en Lumen

- Lumen ahora declara sus orquestaciones con **macros**, no con valores
  construidos a mano. La transferencia es un `impl`
  `#[firefly::saga(name = "money-transfer")]` cuyo paso `debit` nombra
  `compensate = "refund_debit"` y cuyo paso `credit` declara
  `depends_on = ["debit"]`. La macro genera `TransferSaga::saga` y
  `TransferSaga::run`, y `run_transfer` simplemente llama a `saga.run(req.clone())`.
- Un nuevo **workflow de cumplimiento** en `src/compliance.rs`:
  `#[firefly::workflow(name = "transfer-compliance")]` ejecuta `balance-check` y
  `limit-check` en una capa paralela, y luego `approve` (que hace `depends_on` de
  ambos) consume sus veredictos `bool` a través de `#[from_step(...)]`.
- Una nueva **transferencia TCC en dos fases** en `src/tcc_transfer.rs`:
  `#[firefly::tcc(name = "transfer-2pc")]` con un participante `source`
  (`confirm = "capture_source"`, `cancel = "release_source"`) y un participante
  `dest` (`confirm = "capture_dest"`, sin cancel) — reservar todos, y luego
  confirmar todos o cancelar los intentados.
- Los tres están montados en la superficie web en `src/web.rs`:
  `POST /api/v1/transfers` (saga), `POST /api/v1/transfers/compliance` (workflow) y
  `POST /api/v1/transfers/2pc` (TCC) — cada uno renderizando un rollback limpio
  como un problema RFC 9457 `422` e invalidando la caché `GetWallet` cuando mueve
  fondos.
- Como cada rama devuelve su `DomainError` / `ComplianceError` tipado, la causa del
  fallo se preserva a través del motor y se recupera de `SagaError::Step`,
  `WorkflowError::Node` y `TccError::Try` — sin contrabando de `Mutex`, sin cadenas
  opacas empaquetadas.
- Los comportamientos — camino feliz, cortocircuito por descubierto, reembolso por
  fallo en el abono (tres eventos en el flujo de origen), rechazo en paralelo y una
  retención de TCC liberada — están todos fijados por tests, de modo que la prosa
  nunca puede desviarse del código.

También ahora sabes cómo **elegir un motor**:

| Necesidad                                       | Motor      |
|-------------------------------------------------|------------|
| Proceso ordenado por dependencias, deshacer ante fallo | `Saga`     |
| Ramas paralelas que se unen                     | `Workflow` |
| Reservar-y-luego-confirmar entre recursos       | `Tcc`      |

La transferencia de dinero de Lumen es una `Saga` (debit → credit, con un
reembolso del adeudo). Su filtro de cumplimiento previo es un `Workflow`
(comprobaciones de saldo y de límite en paralelo, luego approve). Y la misma
transferencia replanteada como reservar-y-luego-capturar es un `Tcc`. Los tres se
declaran de la misma manera — un bloque `impl` anotado — y los tres están montados
en la superficie web.

## Ejercicios

1. **Añade un paso `notify` a la saga.** Añade un tercer método
   `#[saga_step(id = "notify", depends_on = ["credit"])]` a `TransferSaga` que
   "envíe un recibo" (devuelve `Ok(())` por ahora). Afirma que en el camino feliz
   `steps_executed == ["debit", "credit", "notify"]`, y que cuando el abono falla
   el paso notify nunca se ejecuta y solo se revierte el adeudo.
2. **Haz que el adeudo reintente.** Dale al paso `debit`
   `#[saga_step(id = "debit", compensate = "refund_debit", retry = 2, backoff_ms = 50)]`.
   Acciona un `Ledger` inestable que falle el primer withdraw y tenga éxito en el
   segundo, y afirma que la transferencia aún se completa — demostrando que el
   reintento por paso recupera un fallo transitorio antes de que la compensación se
   llegue siquiera a considerar.
3. **Añade un nodo de KYC al workflow.** Añade un tercer
   `#[workflow_step(id = "kyc-check")]` independiente a `ComplianceCheck` que
   devuelva un `bool`, y haz que `approve` haga `depends_on` de los tres, leyendo el
   nuevo veredicto vía `#[from_step("kyc-check")]`. Afirma que `kyc-check` se
   ejecuta en la misma capa paralela que las comprobaciones existentes y que un KYC
   fallido rechaza la transferencia.
4. **Confirma que el TCC es de todo o nada.** Escribe un test que ejecute
   `run_tcc_transfer` con un origen con descubierto y afirme que ningún saldo se
   movió — el try del origen aborta antes de retener nada, así que no hay nada que
   cancelar. Contrástalo con el test del destino inexistente, donde el origen *sí*
   se retiene y luego se libera.
5. **Cambia la política de compensación de la saga.** Cambia el atributo de la saga
   a `#[firefly::saga(name = "money-transfer", policy = "stop_on_error")]` y lee la
   documentación de `SagaError::Compensation`. Razona (o haz un test) sobre qué
   expondría la rama de error de `run_transfer` si una *compensación* en sí misma
   fallara — y por qué `best_effort` es el valor por defecto del motor.

## Adónde ir después

- Para llamar a los servicios externos que estos motores coordinan — un procesador
  de pagos, un proveedor de FX — necesitas un cliente HTTP. Continúa a
  **[HTTP Clients](./13-http-clients.md)**.
- Los endpoints de transferencia invalidan la caché `GetWallet` en cada movimiento;
  aprende cómo funcionan esa caché del lado de lectura y su invalidación en
  **[Caching](./17-caching.md)**.
- Revisita el `Ledger` con event sourcing que cada rama acciona en
  **[Event Sourcing](./11-event-sourcing.md)** para ver de dónde provienen los
  eventos `MoneyWithdrawn` / `MoneyDeposited` que estas sagas generan.
