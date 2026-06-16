# Por qué Firefly para Rust

Cada servicio de este libro es **Lumen**: el servicio de monedero digital y libro
mayor que harás crecer, capítulo a capítulo, hasta convertirlo en el crate
completo
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen).
Antes de andamiarlo en el siguiente capítulo, este responde a la pregunta que
subyace a todo el proyecto: *¿por qué un servicio en Rust necesita un framework
siquiera, y por qué este?* Todavía no aterriza nada de código en Lumen. Al
terminar entenderás el problema que resuelve Firefly, la única dependencia a
través de la cual llega, las capas que viven detrás de esa dependencia y la
decisión de diseño concreta —el intercambio de adaptadores de en memoria a
producción— en torno a la cual se construye el resto del libro.

Este es un capítulo para leer y orientarse, no para teclear a la par, pero no es
vago: cada término que conocerás durante los próximos diecinueve capítulos se
define aquí desde primeros principios, y cada afirmación es algo que puedes
verificar contra el crate real `samples/lumen` en los ejercicios finales.

Al terminar este capítulo, serás capaz de:

- Explicar el **problema de cohesión** que un framework con criterio existe para
  resolver, y por qué Rust en particular acusa su ausencia.
- Describir lo que Firefly *es* —un framework cohesivo, reactivo y nativo de
  async— y nombrar las bibliotecas probadas en producción a las que delega por
  debajo.
- Leer el `Cargo.toml` real de Lumen y explicar por qué un servicio tan rico
  depende de exactamente **un** crate de Firefly: la fachada `firefly`.
- Mapear las cuatro **capas** que hay tras la fachada (fundacional → plataforma →
  adaptadores → starters) y decir dónde vive cada capacidad.
- Describir el **intercambio de adaptadores**: cómo Lumen pasa de una línea base
  en memoria a un despliegue de producción cambiando el cableado, no la lógica de
  negocio.

## Conceptos que conocerás

Antes de la prosa, aquí tienes las cuatro ideas en las que se apoya este
capítulo. Cada una se reintroduce en su contexto allí donde aparece por primera
vez; esta es la versión breve, para que las secciones posteriores se lean rápido.

> **Note** **Término clave — framework frente a biblioteca.** Una *biblioteca* es
> código que tú llamas: mantienes el control del flujo de ejecución y recurres a
> la biblioteca cuando la necesitas. Un *framework* es código que te llama a ti:
> posee el ciclo de vida —arranque, despacho de peticiones, apagado— e invoca las
> piezas pequeñas que tú aportas. Esta inversión es la razón de ser de Firefly, y
> es exactamente la relación que Spring Boot tiene con un servicio Java.

> **Note** **Término clave — crate fachada.** Una *fachada* es un único crate que
> reexporta toda una familia de crates (y sus macros) de modo que dependes de un
> solo nombre en vez de muchos. Firefly distribuye todo su framework tras la
> fachada `firefly`. El equivalente en Spring es un *starter* de Spring Boot,
> salvo que aquí hay esencialmente una única puerta de entrada que lo cubre todo.

> **Note** **Término clave — puerto y adaptador.** Un *puerto* es una capacidad
> abstracta expresada como un trait —"algo que almacena eventos", "algo que
> publica mensajes"— sin implementación. Un *adaptador* es una implementación
> concreta de ese puerto: un almacén en memoria, un almacén PostgreSQL, un broker
> Kafka. Escribes tu código contra el puerto; eliges el adaptador en el momento
> del cableado. Este es el vocabulario de la arquitectura hexagonal y se
> corresponde con el modismo de interfaz-más-bean de Spring.

> **Note** **Término clave — bean y cableado.** Un *bean* es un objeto que el
> framework construye y gestiona por ti, y luego entrega a quien lo necesite. El
> *cableado* es el acto de conectar beans entre sí, dando a cada uno los
> colaboradores de los que depende. Tú declaras los beans; el framework los
> descubre y los cablea en el arranque. Es exactamente la noción de bean de Spring
> dentro de un contexto de aplicación.

## Paso 1 — Reconocer el problema de cohesión

Imagina tu primer día en un nuevo microservicio en Rust. Antes de escribir una
sola línea de lógica de negocio, te enfrentas a una cascada de decisiones. ¿Qué
capa HTTP: axum, actix, warp, poem? ¿Qué planteamiento de base de datos: sqlx,
SeaORM, diesel, `tokio-postgres` en crudo? ¿Cómo cableas las dependencias: un
`AppState` hecho a mano, un crate de DI, statics perezosos? ¿Cómo gestionas la
configuración, los errores, los identificadores de correlación, las métricas, el
apagado ordenado? Cada equipo inventa su propia respuesta.

Ensamblas una pila a medida, la pegas con buenas intenciones y la despliegas.
Seis meses después un segundo equipo arranca un segundo servicio y toma
decisiones completamente distintas. Ahora tienes dos bases de código con
convenciones incompatibles, formas de error distintas, enfoques de observabilidad
distintos y ningún entendimiento compartido de cómo funciona nada.

**Rust te da elección infinita. Lo que no te da es cohesión.**

Lo que acaba de ocurrir: has nombrado el problema. El impuesto de ensamblar la
pila no es un fallo de competencias, es una carencia de herramientas. Los
ecosistemas maduros la cerraron con un único framework con criterio y con pilas
incluidas que toma decisiones sensatas, te deja anular lo que importa e impone un
modismo coherente en todos los servicios.

> **Design note.** Esta es la inversión framework-frente-a-biblioteca en la
> práctica. Un montón de bibliotecas te deja a *ti* sosteniendo el ciclo de vida:
> tú decides cuándo se enlaza el servidor HTTP, cómo se carga la configuración,
> dónde se convierten los errores en respuestas. Un framework toma esas decisiones
> transversales una sola vez, de modo que cada servicio que lo usa comparte un
> mismo modismo, y un operador que aprende un servicio Firefly puede leerlos
> todos.

Firefly es ese framework para Rust. Toma las decisiones transversales una sola
vez, de modo que cada servicio comparte un mismo modismo, y el coste de arrancar
el servicio número dos deja de ser una nueva ronda de debates de arquitectura.

> **Tip** **Punto de control.** Puedes enunciar el problema en una frase: *Rust
> ofrece elección infinita pero ninguna cohesión integrada, y un framework con
> criterio aporta la cohesión que falta.* Si esa frase te parece obvia, el resto
> del libro se leerá como "así es como Firefly la aporta".

## Paso 2 — Entender qué es Firefly (y a qué delega)

Firefly es un **framework cohesivo, reactivo y nativo de async** para construir
servicios en Rust de nivel de producción. Toma las decisiones transversales por
ti —middleware HTTP, configuración, caché, segregación de responsabilidades entre
comandos y consultas (CQRS), mensajería, seguridad, observabilidad—, todo
integrado, todo coherente, con valores por defecto listos para producción desde
el primer `cargo run`.

> **Note** **Término clave — reactivo (`Mono` / `Flux`).** *Reactivo* significa
> aquí un modelo de streaming perezoso, componible y consciente de la
> contrapresión. Un `Mono<T>` es un cómputo asíncrono que produce *como mucho un*
> valor; un `Flux<T>` produce *cero o más* a lo largo del tiempo. Están
> construidos de forma nativa sobre Tokio y funcionan de extremo a extremo: desde
> endpoints reactivos, pasando por repositorios reactivos, el cliente HTTP
> reactivo y la mensajería reactiva. Si has usado Project Reactor en el mundo de
> Spring, estos son los mismos dos tipos con los mismos nombres. Los dominarás en
> [el modelo reactivo](./05-reactive-model.md).

Firefly no reinventa la rueda por debajo. **Delega en bibliotecas probadas en
producción**: `tokio` para el runtime, `axum`/`tower` para HTTP, `serde` para la
serialización, `tracing` para el logging estructurado, RustCrypto para la
criptografía. El giro está en la dirección en que dependes de ellas:

- Dependes de los **puertos de Firefly** —traits `async_trait` seguros para
  objetos (object-safe)— para capacidades transversales como el almacenamiento de
  eventos y la mensajería.
- Seleccionas **adaptadores concretos** en el momento del cableado, como un
  `Arc<dyn Port>`.

Gracias a esa indirección puedes cambiar un almacén de eventos en memoria por
PostgreSQL, o el broker en proceso por Kafka, sin tocar una sola línea de lógica
de negocio: exactamente el intercambio que Lumen está estructurado para hacer y
al que vuelve el Paso 5.

Los principios definitorios de Firefly, cada uno de los cuales un capítulo
posterior concreta:

- **Compuesto, no construido.** Una línea arranca todo el servicio.
  `FireflyApplication::new("lumen").run()` escanea por componentes tus beans,
  autocablea y automonta los controladores, handlers, listeners y tareas
  programadas, autoaloja un panel de administración y sirve los puertos público y
  de gestión con apagado ordenado: el framework ensambla el grafo de objetos en
  lugar de que tú lo deletrees a mano. Tú escribes comandos, consultas, handlers y
  rutas; nada más. [Inicio rápido](./02-quickstart.md) recorre esta línea etapa
  por etapa.
- **Primero el contrato e interoperable.** El contrato de cable —la forma de error
  `application/problem+json` (RFC 9457), la semántica de `Idempotency-Key`, las
  definiciones de los pasos de la saga, los sobres de eventos— es una
  especificación estable, versionada y neutral respecto al lenguaje. Cualquier
  servicio que lo respete interopera con un servicio Firefly byte a byte, de modo
  que Firefly encaja en una flota políglota sin pegamento a medida.
- **Enchufable en la capa de adaptadores.** Cada punto de integración (caché,
  broker, proveedor de identidad, almacén de contenidos, canal de notificación) es
  un puerto con múltiples implementaciones de adaptador, seleccionadas en el
  momento del cableado como un `Arc<dyn Port>`.
- **Observable por defecto.** El logging estructurado con `tracing` y
  enriquecimiento con identificador de correlación, los endpoints de salud y
  métricas del actuator, los sobres de error RFC 9457 y un banner de arranque
  están todos activados desde el primer momento.
- **Reactivo hasta el núcleo.** La superficie `Mono`/`Flux` corre desde los
  endpoints hasta los repositorios, el cliente HTTP y la mensajería: perezosa,
  componible y consciente de la contrapresión.

> **Note** **Término clave — respuestas de problema RFC 9457.** El RFC 9457 (que
> deja obsoleto al RFC 7807) define `application/problem+json`: una forma JSON
> estándar para errores HTTP con un `type`, un `title`, un `status` y un `detail`.
> Firefly renderiza automáticamente todo error de handler con esta forma, de modo
> que tu API habla un único dialecto de error desde el primer endpoint. Lo
> conocerás de verdad en [Tu primera API HTTP](./06-first-http-api.md).

> **Design note.** `FireflyApplication::new(name).run()` es la raíz de composición
> de Firefly, el equivalente en Rust de `SpringApplication.run(App.class, args)`
> de Spring Boot. Levanta el middleware, el bus, el broker, la salud y las
> métricas, y luego escanea por componentes y cablea tus beans, todo desde una
> línea. La configuración se superpone por defectos → perfil → entorno, y
> cualquier handler puede devolver un `Mono<T>` / `Flux<T>`. Si has usado antes un
> framework con pilas incluidas, esto te resultará familiar.

> **Tip** **Punto de control.** Puedes nombrar dos cosas a la vez: *qué* te da
> Firefly (una pila cohesiva, reactiva y observable) y *sobre qué se apoya*
> (tokio, axum, serde, tracing, RustCrypto). Firefly es la capa de cohesión, no
> una reimplementación desde cero.

## Paso 3 — Leer la fachada de una sola dependencia

Aquí está la parte que sorprende a la gente. Lumen —un servicio con segregación
de responsabilidades entre comandos y consultas (CQRS), event sourcing, una saga,
seguridad JWT, programación de tareas y una superficie de actuator— declara
exactamente una dependencia de Firefly. Esta es la forma de su `Cargo.toml` real:

```toml
[dependencies]
# The whole framework AND every `#[derive(...)]` / `#[...]` macro. The `admin`
# feature pulls in the self-hosted admin dashboard the management port mounts.
firefly = { version = "26.6.28", features = ["admin"] }

# The two ecosystem crates a Firefly service still writes against directly:
# axum (you author the controller handlers) and serde (your messages and
# event payloads are Serialize/Deserialize); serde_json encodes the event
# payloads.
axum  = { version = "0.7" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Async runtime, plus the id/clock crates the wallet domain uses.
tokio  = { version = "1" }
uuid   = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }

# Backs the `async fn` trait methods the domain ports implement.
async-trait = { version = "0.1" }
```

Lo que acaba de ocurrir, bloque a bloque:

- La primera línea —`firefly = { version = "26.6.28", features = ["admin"] }`— es
  el *framework entero*. Cada capacidad y cada macro llega a través de ella.
- El bloque `axum` / `serde` / `serde_json` es la pequeña superficie contra la que
  todavía escribes *directamente*: tú redactas las funciones handler de los
  controladores sobre `axum`, y tus mensajes y payloads de eventos derivan
  `Serialize`/`Deserialize` de `serde`.
- El bloque `tokio` / `uuid` / `chrono` es el runtime y los crates de
  id/reloj a los que recurre el dominio del monedero: ids de monedero y marcas de
  tiempo de eventos.
- `async-trait` respalda los métodos `async fn` de los traits de puerto del
  dominio.

Fíjate en lo que *no* está ahí: ningún `firefly-web`, ningún `firefly-cqrs`,
ningún `firefly-security`. Nunca listas a mano un subcrate `firefly-*`.

> **Note** **Término clave — glob del prelude.** Un *prelude* es un módulo con los
> elementos más usados que un crate te invita a importar de una sola vez con un
> glob (`use … ::*`). La superficie de alta frecuencia de Firefly —más todas sus
> macros— entra a través de una única línea:
>
> ```rust,ignore
> use firefly::prelude::*;
> ```
>
> Esa única importación da a Lumen el `Bus` de CQRS, el `Container` de inyección
> de dependencias, el `Scheduler`, los tipos de orquestación `Saga`/`Step`, la
> `Application` del ciclo de vida, los `Mono`/`Flux` reactivos, los tipos web
> `WebResult`/`WebError`, el error de kernel `FireflyError` y cada macro
> `#[derive(...)]` / `#[...]` que el servicio usa. Los desarrolladores de Spring
> reconocerán el movimiento: una sola importación en lugar de una página entera de
> ellas.

Lumen lleva la disciplina un paso más allá. Incluso sus enums de error tipados
—`MoneyError`, `DomainError` y el mapeo `CqrsError`— escriben a mano `Display` y
`std::error::Error` en lugar de recurrir a `thiserror`. La promesa de una sola
dependencia se mantiene de extremo a extremo, y los capítulos lo señalan allí
donde importa.

> **Design note.** La fachada `firefly` es un único crate de puerta de entrada:
> una sola coordenada en tu lista de dependencias arrastra una pila curada y
> alineada por versión de calendario, y `use firefly::prelude::*;` trae toda la
> superficie de alta frecuencia y cada macro al ámbito de una vez. Muchos
> frameworks te obligan a ensamblar una constelación de artefactos de starter o
> plugin y a mantener sus versiones alineadas a mano. Firefly colapsa todo eso en
> una línea: no hay starter que olvidar ni desfase de versiones entre subsistemas
> como `firefly-web` y `firefly-cqrs`, porque cada crate `firefly-*` se distribuye
> como una única release versionada por calendario —aquí `26.6.28`— y tú dependes
> de la fachada.

> **Tip** **Punto de control.** Puedes señalar la única línea de Firefly en un
> `Cargo.toml` real y explicar las demás entradas como el puñado de crates del
> ecosistema contra los que un servicio Firefly escribe directamente. El Ejercicio
> 1 te hace confirmar esto tú mismo contra `samples/lumen/Cargo.toml`.

## Paso 4 — Mapear las capas que hay tras la fachada

Detrás de ese único crate, el framework está organizado en capas estrictamente
estratificadas, con una dirección de dependencia de izquierda a derecha. Cada capa
puede depender de las capas a su izquierda, nunca a su derecha; el grafo de crates
de Cargo impone la estratificación. Rara vez nombras estos crates directamente
—la fachada los reexporta— pero conocer la forma te dice dónde vive cada capacidad
y *qué* capítulo del libro la desbloquea.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 360" role="img"
     aria-label="Four-tier architecture: the firefly facade is the front door; below it Foundational, Platform, Adapters and Starters tiers build left to right, each depending on the tiers to its left, all resting on the firefly-reactive Mono/Flux core"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="120.0" y="18.5" width="320.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="120.0" y="16.0" width="320.0" height="46.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="280.0" y="36.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">firefly + firefly-macros</text><text x="280.0" y="50.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">one dependency · use firefly::prelude::*;</text>
<rect x="24" y="82" width="124" height="206" rx="11" fill="#f7ecd8" stroke="#e6d4b0" stroke-width="1.2"/>
<rect x="24" y="82" width="124" height="34" rx="11" fill="#d4793a" opacity="0.30"/>
<text x="86.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 1</text>
<text x="86.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Foundational</text>
<text x="86.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">kernel</text>
<text x="86.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">reactive</text>
<text x="86.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">web</text>
<text x="86.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">config</text>
<text x="86.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">container</text>
<text x="86.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">i18n</text>
<line x1="86.0" y1="62.0" x2="86.0" y2="72.0" stroke="#d4793a" stroke-width="2.5" stroke-linecap="round"/><polygon points="86.0,80.0 81.5,72.0 90.5,72.0" fill="#b5531f"/>
<line x1="148.0" y1="200.0" x2="152.0" y2="200.0" stroke="#d4793a" stroke-width="2.5" stroke-linecap="round"/><polygon points="160.0,200.0 152.0,204.5 152.0,195.5" fill="#b5531f"/>
<rect x="160" y="82" width="124" height="206" rx="11" fill="#f7ecd8" stroke="#e6d4b0" stroke-width="1.2"/>
<rect x="160" y="82" width="124" height="34" rx="11" fill="#ffc24a" opacity="0.30"/>
<text x="222.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 2</text>
<text x="222.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Platform</text>
<text x="222.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cqrs</text>
<text x="222.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">eda</text>
<text x="222.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">event-sourcing</text>
<text x="222.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">orchestration</text>
<text x="222.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cache</text>
<text x="222.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">security</text>
<line x1="222.0" y1="62.0" x2="222.0" y2="72.0" stroke="#d4793a" stroke-width="2.5" stroke-linecap="round"/><polygon points="222.0,80.0 217.5,72.0 226.5,72.0" fill="#b5531f"/>
<line x1="284.0" y1="200.0" x2="288.0" y2="200.0" stroke="#d4793a" stroke-width="2.5" stroke-linecap="round"/><polygon points="296.0,200.0 288.0,204.5 288.0,195.5" fill="#b5531f"/>
<rect x="296" y="82" width="124" height="206" rx="11" fill="#f7ecd8" stroke="#e6d4b0" stroke-width="1.2"/>
<rect x="296" y="82" width="124" height="34" rx="11" fill="#d4793a" opacity="0.30"/>
<text x="358.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 3</text>
<text x="358.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Adapters</text>
<text x="358.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">data-sqlx</text>
<text x="358.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">data-mongodb</text>
<text x="358.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">eda-kafka</text>
<text x="358.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cache-redis</text>
<text x="358.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">idp-*</text>
<text x="358.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">notif-*</text>
<line x1="358.0" y1="62.0" x2="358.0" y2="72.0" stroke="#d4793a" stroke-width="2.5" stroke-linecap="round"/><polygon points="358.0,80.0 353.5,72.0 362.5,72.0" fill="#b5531f"/>
<line x1="420.0" y1="200.0" x2="424.0" y2="200.0" stroke="#d4793a" stroke-width="2.5" stroke-linecap="round"/><polygon points="432.0,200.0 424.0,204.5 424.0,195.5" fill="#b5531f"/>
<rect x="432" y="82" width="124" height="206" rx="11" fill="#f7ecd8" stroke="#e6d4b0" stroke-width="1.2"/>
<rect x="432" y="82" width="124" height="34" rx="11" fill="#ffc24a" opacity="0.30"/>
<text x="494.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 4</text>
<text x="494.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Starters</text>
<text x="494.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-core</text>
<text x="494.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-web</text>
<text x="494.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-domain</text>
<text x="494.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-data</text>
<text x="494.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">admin</text>
<text x="494.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cli</text>
<line x1="494.0" y1="62.0" x2="494.0" y2="72.0" stroke="#d4793a" stroke-width="2.5" stroke-linecap="round"/><polygon points="494.0,80.0 489.5,72.0 498.5,72.0" fill="#b5531f"/>
<rect x="80.0" y="306.5" width="400.0" height="44.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="80.0" y="304.0" width="400.0" height="44.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="323.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">firefly-reactive</text><text x="280.0" y="337.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">the Mono / Flux core every tier rests on (tokio · axum)</text>
</svg>
<figcaption>Las cuatro capas. Un servicio depende únicamente de la fachada <code>firefly</code> (la puerta de entrada). Las capas se construyen de izquierda a derecha: el vocabulario <strong>Fundacional</strong>, los motores de <strong>Plataforma</strong> que definen los puertos, los <strong>Adaptadores</strong> que los implementan y los <strong>Starters</strong> que componen y distribuyen, cada uno dependiendo solo de las capas a su izquierda, todos apoyados sobre el núcleo <code>firefly-reactive</code>.</figcaption>
</figure>

Un servicio depende únicamente de la fachada `firefly` (la puerta de entrada).
Las cuatro capas se construyen de izquierda a derecha —cada una dependiendo solo
de las capas a su izquierda— todas apoyadas sobre el núcleo `Mono`/`Flux` de
`firefly-reactive`.

- Los crates **Fundacionales** son el vocabulario: `firefly-kernel` (errores,
  reloj, ámbitos de correlación, el kit de DDD), `firefly-reactive`
  (`Mono`/`Flux`), `firefly-web` (middleware), `firefly-config`,
  `firefly-validators`, `firefly-i18n` y `firefly-container` —un motor completo de
  inyección de dependencias con escaneo de componentes y derives de estereotipo,
  tratado en profundidad en [Cableado de dependencias](./04-dependency-wiring.md).
- Los crates de **Plataforma** son las capacidades: caché, segregación de
  responsabilidades entre comandos y consultas (CQRS), arquitectura orientada a
  eventos, event sourcing, orquestación, programación de tareas, resiliencia,
  seguridad, observabilidad. Lumen recurre a `firefly::cqrs`,
  `firefly::eventsourcing`, `firefly::orchestration`, `firefly::scheduling` y
  `firefly::security`. Y crucial: esta capa *define los puertos* —los traits
  `EventStore`, `Broker`, `cache::Adapter` y `security::Verifier`— que implementa
  la capa siguiente.
- Los **Adaptadores** son las integraciones concretas: el cliente HTTP
  REST/reactivo, los proveedores de identidad de terceros, los almacenes de
  contenidos, las notificaciones, los transportes de eventos (Kafka, RabbitMQ,
  outbox de Postgres, Redis Streams) y los adaptadores de persistencia
  —`firefly-data-sqlx` para almacenes relacionales, `firefly-data-mongodb` para
  documentos. Este es un enfoque enchufable y multibase de datos sobre el que se
  construye [Persistencia](./07-persistence.md). Lumen se distribuye sobre los
  adaptadores en memoria y apunta a los intercambios de producción en sus
  llamadas de atención.
- Los **Starters** empaquetan una pila por defecto sensata para que un servicio
  dependa de un solo crate. La capa web de Lumen es
  `firefly::starter_web::WebStack`, que cablea el núcleo
  (`firefly::starter_core`) más el middleware web: la pila que
  `FireflyApplication` construye por ti en el arranque.

> **Note** **Término clave — superficie de actuator / gestión.** La *superficie de
> gestión* es un conjunto de endpoints HTTP operativos —comprobaciones de salud,
> información de build, métricas, introspección de configuración y de beans— que
> existen para los operadores y las herramientas, no para los usuarios finales.
> Firefly los sirve en un puerto *separado* de tu API de negocio, de modo que los
> endpoints operativos nunca se filtran a la red pública. Esto refleja Spring Boot
> Actuator, y lo alcanzas por primera vez en [Inicio rápido](./02-quickstart.md).

Para el catálogo completo por crate, consulta el
[Índice de módulos](./91-appendix-modules.md).

> **Tip** **Punto de control.** Dada una capacidad —"¿dónde vive el almacenamiento
> de eventos?"— puedes situarla en una capa (es un puerto de *plataforma*,
> implementado por un *adaptador*) y nombrar el capítulo que la introduce. Las
> capas son un mapa; el resto del libro es un recorrido por él.

## Paso 5 — Entender el intercambio de adaptadores

Esta es la única decisión de diseño sobre la que gira todo el libro, así que
merece su propio paso. Lumen funciona con **cero infraestructura externa**: eso es
lo que lo hace una buena línea base didáctica y un objetivo de pruebas rápido.
Arranca sobre el `MemoryEventStore` en proceso y el broker en proceso, de modo que
`cargo run` y `cargo test` no necesitan nada más que el crate. Ningún Postgres que
arrancar, ningún Kafka que aprovisionar.

Cuando estés listo para producción, cambias el *cableado*, no los handlers. Cada
uno de los intercambios siguientes es una edición en un solo sitio, en la costura
donde se construye el `Arc<dyn Port>`:

- **Almacén de eventos.** Cambia `MemoryEventStore` por un adaptador duradero
  donde se construye el `Arc<dyn EventStore>`; el `Ledger`, la proyección y cada
  handler de comando quedan intactos.
- **Transporte de eventos.** El broker en proceso que transporta los eventos de
  dominio de Lumen implementa el mismo puerto `Broker` que `firefly-eda-kafka`,
  `-rabbitmq`, `-postgres` y `-redis`. Cambia el constructor, conserva tu
  `#[event_listener]`.
- **Caché, identidad, notificaciones.** Programa contra el trait del puerto padre
  (`cache::Adapter`, `security::Verifier`, `notifications::Channel`) e incorpora
  el crate del adaptador concreto en el momento del cableado, de modo que los SDK
  pesados se queden fuera de los servicios que no los usan.

> **Note** **Término clave — factoría `#[bean]`.** Una factoría `#[bean]` es una
> función que el framework llama en el arranque para *construir* un bean, y donde
> tú decides qué adaptador concreto satisface un puerto. Es el único sitio donde
> ocurre el intercambio anterior: el cuerpo de la función devuelve
> `Arc::new(MemoryEventStore::new())` en desarrollo y
> `Arc::new(SqlEventStore::new(pool))` en producción, y nada aguas abajo se entera.
> El equivalente en Spring es un método `@Bean` en una clase `@Configuration`.
> Escribes tu primera en [Cableado de dependencias](./04-dependency-wiring.md).

Lo que acaba de ocurrir: has visto por qué la línea base en memoria no es un
juguete. Como Lumen programa contra puertos, la build en memoria y el despliegue
de producción difieren *únicamente* en una factoría `#[bean]` —el cableado que el
framework escanea, no el código de negocio. Este es el hilo que recorre todo el
libro.

> **Tip** **Punto de control.** Puedes terminar esta frase: *para llevar Lumen a
> producción cambias una factoría `#[bean]`, no un handler.* Si eso cala, las
> llamadas de atención del libro del tipo "Lumen se distribuye en memoria; aquí
> está el intercambio de producción" se leerán como algo rutinario en lugar de
> mágico. El Ejercicio 4 te hace localizar los tres traits de puerto tras estos
> intercambios.

## El camino por delante: Lumen, capítulo a capítulo

El resto del libro es el crecimiento de Lumen, aditivo y en orden. Los primeros
capítulos presentan el framework con pequeños fragmentos autónomos; **Lumen
propiamente dicho comienza en [Tu primera API HTTP](./06-first-http-api.md)**.

- **Fundamentos** — andamiar y arrancar Lumen, vincular su configuración y
  perfiles, entender cómo `FireflyApplication` cablea los beans que escanea,
  dominar `Mono`/`Flux` y exponer los primeros endpoints REST validados.
- **Modelar y persistir** — un modelo de lectura tras un repositorio, el objeto de
  valor `Money` y el agregado `Wallet`, y la división CQRS de comando/consulta
  sobre un bus.
- **Orientado a eventos** — eventos de dominio, una proyección que mantiene
  actualizado el modelo de lectura y el libro mayor con event sourcing que pliega
  su stream.
- **Hacia los microservicios** — un esbozo de cliente HTTP y la saga de
  transferencia compensatoria.
- **Asegurar, observar, distribuir** — autenticación bearer JWT y control de
  acceso basado en roles, la superficie de actuator, la caché, una tarea
  programada, la suite de pruebas y el punto de entrada de producción con apagado
  ordenado y un endpoint de streaming reactivo.

Para la última página, Lumen es el crate completo `samples/lumen`, y habrás
escrito cada una de sus líneas.

## Resumen — qué cambió en Lumen

Nada en código todavía. Este capítulo encuadró el viaje y abasteció tu
vocabulario:

- El **problema de cohesión** que Firefly existe para resolver —Rust ofrece
  elección infinita pero ninguna cohesión integrada— y la inversión
  framework-frente-a-biblioteca que permite que un framework con criterio la
  aporte.
- Qué **es Firefly** (un framework cohesivo, reactivo y nativo de async) y a qué
  *delega* (tokio, axum/tower, serde, tracing, RustCrypto), dependiendo tú de sus
  **puertos** y seleccionando **adaptadores** en el momento del cableado.
- La **fachada de una sola dependencia** —Lumen depende de un único
  `firefly = { version = "26.6.28", features = ["admin"] }`, y
  `use firefly::prelude::*;` trae toda la superficie de alta frecuencia y cada
  macro. Incluso los errores tipados evitan `thiserror`, de modo que la promesa se
  mantiene de extremo a extremo.
- Las **cuatro capas** tras esa fachada (fundacional → plataforma → adaptadores →
  starters) apoyadas sobre el núcleo `firefly-reactive`, y dónde vive cada
  capacidad.
- El **intercambio de adaptadores** que Lumen está construido para hacer —pasar de
  la línea base en memoria a producción cambiando una única factoría `#[bean]`,
  nunca un handler.

## Ejercicios

1. **Confirma la única dependencia.** Abre `samples/lumen/Cargo.toml` y confirma la
   lista de dependencias: un `firefly` (con la feature `admin`), más
   `axum`/`serde`/`serde_json`/`tokio`/`uuid`/`chrono`/`async-trait`. Fíjate en que
   no se lista directamente ningún subcrate `firefly-*`.
2. **Encuentra el `main` de una sola línea.** Hojea `samples/lumen/src/main.rs` —la
   raíz del crate de binario único. Lista los diez módulos que declara (`commands`,
   `compliance`, `domain`, `housekeeping`, `ledger`, `money`, `security`,
   `tcc_transfer`, `transfer`, `web`) y predice qué parte del libro introduce cada
   uno. Confirma que `main` es genuinamente una línea sobre
   `FireflyApplication::new("lumen")`.
3. **Lee la documentación del crate.** Ejecuta `cargo doc -p firefly-sample-lumen --open`
   y lee la documentación a nivel de crate. Contiene la misma tabla "bloque de
   construcción → módulo → superficie de Firefly" en torno a la cual se organiza el
   libro.
4. **Localiza los traits de puerto.** Para cada uno de estos intercambios de
   producción, encuentra el trait de puerto que implementaría en la fachada: un
   almacén de eventos Postgres, un broker Kafka, una caché Redis. (Pista:
   `firefly::eventsourcing::EventStore`, `firefly::eda::Broker`,
   `firefly::cache::Adapter`.) Estas son las costuras que describió el Paso 5.
5. **Rastrea el prelude.** Abre el módulo `prelude` de la fachada `firefly` (o su
   documentación) y encuentra cinco tipos que usarás repetidamente: el `Bus` de
   CQRS, el `Container`, `Mono`/`Flux`, `WebResult` y `FireflyError`. Confirma que
   todos llegan a través del único glob `use firefly::prelude::*;`.

## Adónde ir después

- Pon Lumen en marcha por primera vez en **[Inicio rápido](./02-quickstart.md)**:
  andamia el crate, escribe el `main` de una línea y alcanza sus dos puertos.
- Añade configuración tipada, estratificada y consciente de perfiles en
  **[Configuración](./03-configuration.md)**.
- Aprende cómo el framework cablea el grafo de objetos que escanea —incluida tu
  primera factoría `#[bean]`— en
  **[Cableado de dependencias](./04-dependency-wiring.md)**.
