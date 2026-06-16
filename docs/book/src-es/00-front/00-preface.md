## Prefacio

Rust te ofrece concurrencia sin miedo, abstracciones de coste cero y un compilador que se niega a publicar una condición de carrera de datos. Lo que no te da es *cohesión*. Cada nuevo servicio de back-office obliga a la misma cascada de decisiones antes de escribir una sola línea de lógica de negocio: qué capa HTTP, qué planteamiento de base de datos, cómo cablear las dependencias, cómo gestionar la configuración, los errores, los identificadores de correlación, las métricas y el apagado ordenado. **Firefly** cambia eso. Es un framework dogmático, de convención sobre configuración, que toma esas decisiones transversales una sola vez, de modo que todos los servicios comparten un mismo idioma — construido desde cero para Rust 1.88+ sobre `tokio` y `axum`.

Este libro enseña Firefly **mediante ejemplos**. Construyes una aplicación real desde un crate vacío hasta un servicio asegurado, observable y basado en event sourcing — haciendo concreto cada concepto antes de pasar al siguiente. El código de estas páginas no es pseudocódigo ilustrativo: cada listado es una porción de un **proyecto real que compila, arranca y supera sus pruebas** contra la versión 26.6.x del framework. Cada fragmento se ha extraído del ejemplo en ejecución y se ha verificado contra las API de los crates, de modo que lo que lees es lo que realmente funciona. Cuando un listado se desvía de la fuente, la compilación del ejemplo se rompe y una prueba falla — esa es la garantía que respalda cada listado de este libro.

### Para quién es este libro

Este libro está dirigido a desarrolladores de Rust de nivel intermedio, cómodos con `async`/`await`, los traits y los fundamentos de los servicios HTTP. No necesitas experiencia previa con ningún framework — si has construido algo con `axum`, `actix` o `sqlx`, estás bien preparado.

Si has usado antes un framework dogmático y con baterías incluidas, o una biblioteca de reactive streams, los conceptos de Firefly — beans y estereotipos, mensajería declarativa, eventos de aplicación, `Mono`/`Flux` — te resultarán rápidos de asimilar. Aparece un recuadro **Design note** allí donde una idea te resulte familiar, para que puedas apoyarte en lo que ya sabes; cada uno se plantea como una decisión de diseño propia de Firefly, no como una traducción de otro framework.

### Lo que vas a construir: Lumen

Cada capítulo hace avanzar **Lumen**, un servicio de monedero digital y libro mayor (ledger) — el ejemplo trabajado en torno al cual se ha construido este libro. Lumen permite a un cliente abrir un monedero, ingresar y retirar dinero, transferir fondos entre monederos y leer un saldo en vivo. Tras esa pequeña superficie se esconde todo el abanico de patrones que necesita un servicio de back-office real: un value object que hace aritmética monetaria exacta, un aggregate que impone invariantes, CQRS con una caché del lado de lectura, eventos de dominio, un libro mayor basado en event sourcing, una saga de transferencia compensatoria, endpoints asegurados con JWT, una superficie de actuator, una tarea programada y una suite de pruebas de extremo a extremo.

La propiedad más importante de todas en Lumen es su lista de dependencias:

```toml
[dependencies]
firefly = { version = "26.6.24" }   # the whole framework — and every macro
axum   = { version = "0.7" }       # you author the handler functions
serde  = { version = "1", features = ["derive"] }
```

**Una única dependencia de Firefly.** El framework completo — CQRS, inyección de dependencias, la pila web reactiva, mensajería basada en eventos, event sourcing, orquestación de sagas, planificación, resiliencia, seguridad, observabilidad — y cada macro `#[derive(...)]` / `#[...]` llegan a través de `use firefly::prelude::*;`. Los capítulos hacen hincapié deliberado en esto: incluso los enums de error tipados de Lumen escriben a mano `Display` + `std::error::Error` en lugar de incorporar `thiserror`, de modo que la promesa de una sola dependencia se mantiene de principio a fin.

El recorrido sigue un arco deliberado, una porción de Lumen a la vez:

- **Parte I — Fundamentos.** Montas el primer servicio de Lumen, vinculas configuración tipada y perfiles, aprendes cómo el composition root cablea los colaboradores, dominas la superficie reactiva `Mono`/`Flux` y expones tus primeros endpoints REST validados.
- **Parte II — Modelar y persistir.** Levantas un modelo de lectura tras un repositorio, modelas el dominio con un value object `Money` y un aggregate `Wallet`, y separas las lecturas de las escrituras con manejadores de comandos y consultas CQRS despachados a través de un bus.
- **Parte III — Orientado a eventos.** El aggregate emite eventos de dominio; una proyección `#[event_listener]` mantiene el modelo de lectura al día; y un **libro mayor basado en event sourcing** reconstruye cada saldo plegando (folding) su flujo de eventos — con esos mismos eventos listos para salir hacia Kafka o RabbitMQ.
- **Parte IV — Hacia los microservicios.** Lumen va más allá de su propio proceso: un esbozo de cliente HTTP tipado muestra cómo un monedero llamaría a un proveedor de pagos externo, y una **saga de transferencia** orquestada mueve dinero entre monederos y *compensa* cuando el tramo de abono falla.
- **Parte V — Asegurar · Observar · Publicar.** Aseguras los endpoints con autenticación bearer JWT y RBAC basado en rutas, haces observable el servicio con métricas, trazado y una superficie de administración de actuator, añades una caché del lado de lectura y una tarea programada de mantenimiento, pruebas la pila completa en proceso y, por último, lo publicas tras la CLI de `firefly` con apagado ordenado y un endpoint de streaming reactivo.

Al llegar a la última página tendrás un servicio funcional, probado, observable, asegurado y basado en event sourcing — y el modelo mental para ampliarlo.

### Cómo usar este libro

**Lee en orden.** Cada capítulo se apoya en el anterior, y el código de Lumen crece de forma incremental; saltar adelante deja huecos. El capítulo del **Modelo reactivo** es la piedra angular — toda la superficie reactiva se construye sobre `Mono` y `Flux`, así que léelo antes de los capítulos de construcción del servicio. Los primeros capítulos (1–5) presentan el framework con pequeños fragmentos independientes; **Lumen propiamente dicho empieza en el capítulo 6** y crece a partir de ahí. Cada capítulo es *aditivo* — nunca reescribe lo que entregó un capítulo anterior, solo lo amplía — de modo que el estado final es exactamente el crate de acompañamiento.

**Escribe tú mismo cada listado.** Leer y escribir código al mismo tiempo es la forma de fijar los patrones. Resiste la tentación de copiar y pegar hasta que hayas escrito cada listado al menos una vez.

**Ejecútalo.** Lumen se ejecuta de verdad. Desde la raíz del workspace:

```sh
cargo run   -p firefly-sample-lumen          # boot the service (API + admin)
cargo test  -p firefly-sample-lumen          # run the unit + HTTP test suite
```

Siempre que un capítulo añada una funcionalidad, arranca la aplicación o las pruebas y obsérvala funcionar. Ver volver JSON real de un endpoint real — y ver cómo una saga *compensa* una transferencia fallida — vale más que cien diagramas.

Cada capítulo se cierra con un **Resumen** de lo que cambió en el código de Lumen y un conjunto de **Ejercicios** que dan un paso más allá. Los ejercicios son opcionales, pero recomendables para cualquier cosa que pretendas aplicar de inmediato.

### Convenciones en breve

Las convenciones tipográficas y estructurales — los pies de los listados de código, los tipos de recuadro y las notas de diseño — se demuestran, con ejemplos en vivo, en la sección de **Convenciones** que sigue a continuación.

### El código de acompañamiento

El proyecto Lumen completo y ejecutable reside en el directorio `samples/lumen` del framework. Es un único crate de Firefly limpio — un módulo por cada incumbencia (`money`, `domain`, `ledger`, `commands`, `transfer`, `security`, `web`, `housekeeping`) — que haces crecer capítulo a capítulo; el código terminado que hay allí es el destino al que este libro te lleva. Compílalo una vez con `cargo build -p firefly-sample-lumen` y úsalo para comparar tu trabajo, ponerte al día si te quedas atrás, o simplemente ejecutar las partes sobre las que estás leyendo. El mapa capítulo a capítulo de *qué código aterriza dónde* vive junto a él en `docs/book/LUMEN-ARC.md`.
