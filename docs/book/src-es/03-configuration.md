# Configuración

En el [Inicio rápido](./02-quickstart.md), Lumen se nombró a sí mismo con dos
cadenas `pub const`, y `FireflyApplication` tomó sus direcciones de enlace
directamente del entorno (`FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`). Ese
es el punto de partida correcto, pero un servicio de monedero real se ejecuta en
desarrollo, en CI y en producción, y cada entorno quiere puertos, niveles de log
y (con el tiempo) URLs de base de datos distintos. Los literales codificados a
mano no sobreviven a ese recorrido.

En este capítulo es donde esos literales dejan de ser literales y empiezan a
provenir de archivos y del entorno, de una forma tipada, en capas y consciente de
los perfiles: la misma forma que `@ConfigurationProperties` de Spring Boot da a
un servicio Java, portada a structs `serde` corrientes. Todo lo de aquí es
*aditivo*: el `main` de una sola línea del Inicio rápido no cambia, y las
constantes que escribiste siguen funcionando mientras aprendes la maquinaria que
acabará reemplazándolas.

Al terminar este capítulo, serás capaz de:

- Definir una configuración como un struct `serde` corriente y **enlazar** sobre
  él valores planos con clave por puntos mediante el binder dirigido por tipos.
- Cargar configuración desde `application.yaml`, una superposición específica de
  perfil y el entorno, y explicar la **cadena de precedencia** que decide quién
  gana.
- Resolver marcadores de posición `${...}` y razonar sobre el orden en que el
  entorno vence a la configuración.
- Convertir un struct de configuración en un **bean inyectable** con
  `#[derive(ConfigProperties)]`, opcionalmente validado al arranque.
- Levantar la datasource y la capa de seguridad de Lumen **desde
  `application.yaml`** con una única llamada de autoconfiguración esperada para
  cada una: sin contenedor, sin cadenas de builders.
- Enmascarar secretos, recargar en tiempo de ejecución y obtener configuración
  desde un servidor de configuración.

## Conceptos que conocerás

Antes del primer struct, aquí están las ideas en las que se apoya este capítulo.
Cada una se reintroduce en su contexto donde se usa por primera vez; esta es la
versión corta.

> **Note** **Término clave — propiedad de configuración.** Una *propiedad de
> configuración* es un único valor con nombre que tu programa lee al arranque:
> `web.addr`, `cache.ttl`, `datasource.url`. Firefly representa todo el conjunto
> como un **mapa plano de cadenas con clave por puntos**
> (`{"web.addr": "127.0.0.1:8080", ...}`) y luego lo *enlaza* sobre tu struct
> tipado. El análogo en Spring es una propiedad en `application.properties` /
> `application.yaml`.

> **Note** **Término clave — source.** Una *source* es cualquier cosa que produce
> algunas de esas entradas planas: un archivo YAML, el entorno del proceso,
> valores por defecto codificados a mano, flags de CLI, un servidor de
> configuración remoto. El trait `Source` de Firefly tiene una sola tarea:
> devolver un `HashMap<String, String>`. El análogo en Spring es un
> `PropertySource`.

> **Note** **Término clave — perfil.** Un *perfil* nombra un entorno —
> `dev`, `test`, `staging`, `prod` — y selecciona una superposición YAML extra
> (`application-prod.yaml`) que se apila sobre el archivo base. Esto es
> exactamente la noción de perfil activo de Spring, hasta la sintaxis de comas
> `dev,cloud`.

> **Note** **Término clave — binding.** El *binding* (enlace) es el acto de
> decodificar el mapa plano de cadenas sobre un struct tipado: `"9090"` se
> convierte en un `u16`, `"alpha,beta"` se convierte en un `Vec<String>`,
> `"true"` se convierte en un `bool`. El binder está **dirigido por tipos**: el
> tipo del campo destino decide cómo se parsea cada cadena. Spring llama a la
> misma idea relaxed binding sobre una clase `@ConfigurationProperties`.

> **Design note.** Firefly enlaza una jerarquía consciente de perfiles
> `application.yaml` → perfil → entorno sobre structs tipados, y las reglas de
> aplanamiento y enlace están especificadas con precisión, de modo que el mismo
> `application.yaml` produce las mismas claves de forma determinista. Firefly
> trata este determinismo como una garantía, no como un accidente: no hay un
> motor YAML de propósito general decidiendo cosas a tus espaldas.

## Paso 1 — Ve dónde está Lumen hoy: la identidad de la app como configuración

No tienes que escribir ninguna config para seguir este paso: ya tienes config,
solo que la deletreaste como constantes. Recuerda el bootstrap de Lumen. El
`main` del Inicio rápido era la forma desnuda; `src/web.rs` conserva un ayudante
`bootstrap` más completo que además estampa la versión:

```rust,ignore
// src/web.rs — the two values that name the service
pub const APP_NAME: &str = "lumen";
pub const VERSION: &str = firefly::VERSION;

firefly::FireflyApplication::new(APP_NAME)
    .version(VERSION)
    .run()
    .await
```

Lo que acaba de ocurrir: esos dos valores se convierten en `CoreConfig.app_name`
/ `CoreConfig.app_version` dentro del framework, configuración corriente.
`FireflyApplication::new(name)` escribe `app_name`; `.version(v)` escribe
`app_version`. Cada uno de los demás campos de `CoreConfig` es también un mando, y
los dos que Lumen establece son exactamente los valores que `/actuator/info`
reporta y que el banner imprime.

> **Note** **Término clave — `CoreConfig`.** `CoreConfig` es el propio struct de
> configuración del framework (CORS, cabeceras de seguridad, idempotencia, el
> nombre y la versión de la app, …). `FireflyApplication` lleva uno y te permite
> ajustarlo con `.configure(|c| ...)`. Los campos restantes toman valores por
> defecto —una caché en memoria, un broker en proceso, un bus CQRS recién
> creado—, razón por la cual un `cargo run` desnudo no necesita infraestructura.
> El análogo en Spring es el paquete de propiedades `server.*` / `spring.*` que
> Spring Boot enlaza por ti.

Promover cualquiera de esos valores por defecto a infraestructura real es un
cambio de un solo campo que harás en
[Producción y despliegue](./20-production.md). La historia de configuración de
*este* capítulo es la maquinaria general que hay debajo: cómo un valor como una
dirección deja de ser un literal en Rust y empieza a llegar desde un archivo o el
entorno.

> **Tip** **Punto de control.** Ya puedes demostrar que la identidad es
> configuración: `curl localhost:8081/actuator/info` (puerto de management) y lee
> de vuelta `"app":{"name":"lumen","version":"..."}`. Cambia la cadena que pasas
> a `new(...)`, vuelve a ejecutar, y tanto el banner como ese endpoint lo siguen.

## Paso 2 — Define un struct de configuración

Un struct de configuración es `serde` corriente. No hay un tipo base especial del
que heredar ni un atributo que recordar: los structs anidados simplemente se
convierten en secciones anidadas con clave por puntos (`web.addr`,
`web.admin_addr`). Aquí está la forma que Lumen adoptaría a medida que crece más
allá de las dos constantes.

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Web {
    /// Public API bind address — the typed home of FIREFLY_SERVER_ADDR.
    addr: String,
    /// Admin/management bind address — the typed home of FIREFLY_MANAGEMENT_ADDR.
    admin_addr: String,
}

#[derive(Debug, Deserialize)]
struct LumenConfig {
    name: String,
    web: Web,
    tags: Vec<String>,
}
```

Lo que acaba de ocurrir: declaraste tres claves de nivel superior —`name`, la
sección `web` y una lista `tags`— puramente con `serde`. El binder alcanza
`web.addr` recorriendo `LumenConfig.web` → `Web.addr`, y alcanza cada elemento de
`tags` dividiendo una cadena unida por comas.

Por qué importa: el binder está **dirigido por tipos**, así que rara vez
necesitas `#[serde(default)]`. Una clave ausente produce el valor cero del tipo
—`0` para un entero, `""` para un `String`, `false` para un `bool`, un `Vec`
vacío para una lista—, exactamente como un struct con valores a cero. Esa es una
decisión deliberada de paridad con los ports de Go y pyfly.

> **Note** **Término clave — clave relajada.** Las claves se normalizan en la
> puerta: pasadas a minúsculas, con los guiones kebab-case plegados a guiones
> bajos snake_case. De modo que `admin-addr:` escrito en YAML enlaza el campo
> serde `admin_addr`, y `WEB.ADDR` del entorno aterriza en la misma clave
> `web.addr` que un `web.addr` de YAML. Spring lo llama relaxed binding.

El catálogo completo de hojas que el binder soporta: `String`, `bool` (acepta las
formas `1`/`0`, `t`/`f` y `true`/`false`), todos los anchos de entero,
`f32`/`f64`, `char`, enums unitarios (emparejados por el nombre de la variante),
`Option<T>` (`None` cuando la clave y todo su subárbol están ausentes), secuencias
de escalares (separadas por comas, recortadas) y subárboles
`HashMap<String, _>` (cada segmento hijo inmediato se convierte en una clave del
mapa). Para una duración, enlaza un `i64`/`u64` de milisegundos y convierte:
`Duration::from_millis(cfg.cache.ttl_ms)`.

## Paso 3 — Enlaza valores sobre el struct

Un struct por sí solo no hace nada; enlazas un mapa plano sobre él. El punto de
entrada de más bajo nivel es `bind`, que toma un `HashMap<String, String>` y lo
decodifica sobre un `T` recién creado.

```rust,ignore
use std::collections::HashMap;
use firefly::config::{bind, ConfigError};

let flat = HashMap::from([
    ("name".to_string(), "lumen".to_string()),
    ("web.addr".to_string(), "127.0.0.1:8080".to_string()),
    ("web.admin-addr".to_string(), "127.0.0.1:8081".to_string()),
    ("tags".to_string(), "wallet, ledger, demo".to_string()),
]);

let cfg: LumenConfig = bind(&flat)?;
assert_eq!(cfg.web.addr, "127.0.0.1:8080");
assert_eq!(cfg.tags, vec!["wallet", "ledger", "demo"]);
# Ok::<(), ConfigError>(())
```

Lo que acaba de ocurrir: `bind` recorrió el tipo de tu struct, buscó cada clave
con puntos y parseó la cadena sobre el campo destino. Fíjate en tres cosas que el
tipo dirigió por sí solo: `web.admin-addr` (kebab) enlazó el campo `admin_addr`
(snake), `"wallet, ledger, demo"` se dividió y recortó sobre un `Vec<String>`, y
nada requirió `#[serde(default)]`.

> **Note** **Término clave — import de fachada.** `firefly::config` es el crate
> `firefly-config` reexportado a través de la fachada de dependencia única, así
> que sigues dependiendo solo de `firefly`. A lo largo de este capítulo
> `firefly::config::X` y `firefly_config::X` nombran el mismo elemento; el libro
> prefiere la ruta de la fachada para mantener honesta la historia de la
> dependencia única.

En código real casi nunca construyes ese mapa a mano: las sources lo construyen
por ti. El loader canónico, `load`, toma una lista de sources, las fusiona,
resuelve los marcadores de posición y enlaza en una sola llamada:

```rust,ignore
use firefly::config::{load, Source};

let cfg: LumenConfig = load(&sources)?;
```

El siguiente paso es de dónde sale `sources`.

> **Tip** **Punto de control.** Mete el ejemplo de `bind` en un test unitario y
> ejecútalo. Un test en verde significa que la forma de tu struct y las claves
> con puntos encajan: esta es la forma más rápida de depurar un enlace antes de
> que el YAML y el entorno entren en la mezcla.

## Paso 4 — Carga con perfiles

El bootstrap más común es una llamada a un ayudante. `load_from_profile` lee
`application.yaml`, luego el `application-{profile}.yaml` específico del perfil,
luego las variables de entorno `FIREFLY_*`, las fusiona en ese orden y enlaza el
resultado:

```rust,ignore
use firefly::config::{load_from_profile, ConfigError};

fn main() -> Result<(), ConfigError> {
    // dir, app basename, fallback profile (FIREFLY_PROFILE overrides at runtime).
    let cfg: LumenConfig = load_from_profile("/etc/lumen", "application", "dev")?;
    println!("public API on {}", cfg.web.addr);
    Ok(())
}
```

Lo que acaba de ocurrir, argumento a argumento:

- `"/etc/lumen"` es el directorio donde viven los archivos YAML.
- `"application"` es el *basename* del archivo, así que lee `application.yaml` y
  `application-{profile}.yaml`. (Pasa `"lumen"` para leer `lumen.yaml` en su
  lugar.)
- `"dev"` es el perfil de **reserva** (fallback), usado solo cuando
  `FIREFLY_PROFILE` no está establecido.

Se tolera la ausencia de ambos archivos YAML: un servicio que codifica todo a
mano en Rust puede no enviar ningún YAML y esta llamada sigue teniendo éxito solo
contra el entorno.

> **Note** **Término clave — `FIREFLY_PROFILE`.** Esta variable de entorno
> selecciona el o los perfiles activos en tiempo de ejecución.
> `FIREFLY_PROFILE=prod` lee `application-prod.yaml`; un valor separado por comas
> (`FIREFLY_PROFILE=dev,cloud`) superpone un archivo por perfil, en orden
> (`application-dev.yaml` y luego `application-cloud.yaml`, gana el posterior).
> Así es como Lumen llevaría un almacén de eventos en memoria en `dev` y uno de
> Postgres en `prod` sin un solo `if` en el código de cableado.

> **Warning** `load_from_profile` siempre añade `from_env("FIREFLY")` como su capa
> superior, así que sus overrides de entorno se deletrean `FIREFLY_*`
> (`FIREFLY_WEB_ADDR`), *no* `LUMEN_*`. Si quieres una capa de entorno con prefijo
> `LUMEN_`, construye la cadena tú mismo (Paso 6) con `from_env("LUMEN")`.

## Paso 5 — Entiende la precedencia de sources

Todo el sistema descansa sobre una regla: **`Layered::new(vec![s1, s2, ...])`
fusiona sus sources de izquierda a derecha, y gana la última escritura.** Las
filas más altas de la tabla siguiente se sitúan más tarde en la lista y, por
tanto, sobrescriben a las más bajas.

| Orden | Source                                              | Vence a      |
|-------|-----------------------------------------------------|--------------|
| 1     | Defaults — `StaticSource::new(name, entries)`       | nada         |
| 2     | YAML base — `from_optional_yaml("application.yaml")` | defaults    |
| 3     | YAML de perfil — `from_optional_yaml("application-prod.yaml")` | base |
| 4     | Entorno — `from_env("FIREFLY")`                     | archivos YAML |
| 5     | Flags de CLI — `FlagSource::new().set("web.addr", "0.0.0.0:80")` | todo |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 250" role="img"
     aria-label="Configuration precedence: defaults, base YAML, profile YAML, environment and CLI flags are merged left to right with the last write winning, so a CLI flag beats environment, which beats YAML, which beats defaults"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="24.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="24.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="70.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">defaults</text><text x="70.0" y="109.0" text-anchor="middle" font-size="9" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">StaticSource</text>
<line x1="116.0" y1="98.0" x2="124.0" y2="98.0" stroke="#d4793a" stroke-width="2.6" stroke-linecap="round"/><polygon points="132.0,98.0 124.0,102.5 124.0,93.5" fill="#b5531f"/>
<rect x="132.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="132.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="178.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">base YAML</text><text x="178.0" y="109.0" text-anchor="middle" font-size="9" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">application.yaml</text>
<line x1="224.0" y1="98.0" x2="232.0" y2="98.0" stroke="#d4793a" stroke-width="2.6" stroke-linecap="round"/><polygon points="240.0,98.0 232.0,102.5 232.0,93.5" fill="#b5531f"/>
<rect x="240.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="240.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="286.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">profile YAML</text><text x="286.0" y="109.0" text-anchor="middle" font-size="9" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">application-prod.yaml</text>
<line x1="332.0" y1="98.0" x2="340.0" y2="98.0" stroke="#d4793a" stroke-width="2.6" stroke-linecap="round"/><polygon points="348.0,98.0 340.0,102.5 340.0,93.5" fill="#b5531f"/>
<rect x="348.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="348.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="394.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">environment</text><text x="394.0" y="109.0" text-anchor="middle" font-size="9" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">FIREFLY_*</text>
<line x1="440.0" y1="98.0" x2="448.0" y2="98.0" stroke="#d4793a" stroke-width="2.6" stroke-linecap="round"/><polygon points="456.0,98.0 448.0,102.5 448.0,93.5" fill="#b5531f"/>
<rect x="456.0" y="72.5" width="92.0" height="56.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="456.0" y="70.0" width="92.0" height="56.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="502.0" y="95.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">CLI flags</text><text x="502.0" y="109.0" text-anchor="middle" font-size="9" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">FlagSource</text>
<text x="280.0" y="40.0" text-anchor="middle" font-size="13" font-weight="800" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">merged left → right  ·  last write wins</text>
<text x="70.0" y="160.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif" font-style="italic">beats nothing</text>
<text x="490.0" y="160.0" text-anchor="middle" font-size="10.5" font-weight="700" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">beats everything</text>
<text x="280.0" y="200.0" text-anchor="middle" font-size="11" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">an env override beats a YAML file; a CLI flag beats both</text>
</svg>
<figcaption><code>Layered::new(...)</code> fusiona sus sources de izquierda a derecha y gana la <strong>última escritura</strong>. Los defaults se sitúan los primeros y no vencen a nada; un YAML base vence a los defaults; una superposición de perfil vence a la base; el entorno vence a los archivos YAML; y un flag de CLI vence a todo: un único artefacto, desplegable en cualquier lugar.</figcaption>
</figure>

Así, un override de entorno (`FIREFLY_WEB_ADDR=0.0.0.0:80`) siempre vence a un
archivo YAML, y un flag de CLI vence a ambos. Esa misma precedencia es
exactamente la razón por la que `FireflyApplication` deja que
`FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` ganen sobre cualquier dirección
de enlace por defecto codificada: la capa de entorno tiene más rango que el
default.

Por qué importa: la precedencia es lo que hace que un único artefacto sea
desplegable en cualquier lugar. Confirmas valores por defecto sensatos y un
`application.yaml` base, envías una fina superposición `application-prod.yaml` y
dejas que la plataforma inyecte secretos y overrides de última milla a través del
entorno: cada capa solo declara lo que necesita cambiar.

> **Tip** **Punto de control.** Puedes razonar sobre cualquier valor leyendo la
> tabla de arriba abajo y tomando la primera source que lo define. Si `web.addr`
> aparece tanto en `application.yaml` como en `FIREFLY_WEB_ADDR`, gana el entorno
> porque la fila 4 es posterior a la fila 2.

## Paso 6 — Construye la cadena de sources de forma explícita

`load_from_profile` es el valor por defecto cómodo. Cuando necesitas control
total —un prefijo de entorno distinto, valores por defecto codificados a mano, una
source remota intercalada— ensamblas tú mismo el `Vec<Box<dyn Source>>` y se lo
pasas a `load`:

```rust,ignore
use std::collections::HashMap;
use firefly::config::{from_env, from_optional_yaml, load, Source, StaticSource};

let sources: Vec<Box<dyn Source>> = vec![
    // 1. Defaults at the bottom — overridden by anything below.
    Box::new(StaticSource::new(
        "defaults",
        HashMap::from([("web.addr".to_string(), "127.0.0.1:8080".to_string())]),
    )),
    // 2. Base YAML beats defaults.
    Box::new(from_optional_yaml("application.yaml")),
    // 3. A LUMEN_*-prefixed environment layer beats the YAML.
    Box::new(from_env("LUMEN")),
];
let cfg: LumenConfig = load(&sources)?;
```

Lo que acaba de ocurrir: deletreaste la cadena de precedencia en el orden de la
lista. `StaticSource::new` toma un nombre y un `HashMap` de entradas codificadas a
mano: se sitúa en el fondo. `from_optional_yaml` lee un archivo si está presente
(y queda silenciosamente vacío si no). `from_env("LUMEN")` mapea
`LUMEN_WEB_ADDR` → `web.addr`. Como la source de entorno es la *última*,
`LUMEN_WEB_ADDR=0.0.0.0:80` sobrescribe tanto el YAML como el default.

> **Note** **Término clave — `StaticSource` / `from_env` / `from_optional_yaml` /
> `FlagSource`.** Estas son las cuatro sources integradas. `StaticSource` envuelve
> un mapa en memoria (defaults). `from_env(prefix)` lee `PREFIX_FOO_BAR` →
> `foo.bar` del entorno del proceso. `from_optional_yaml(path)` lee un archivo
> YAML, tolerando la ausencia. `FlagSource` recopila overrides de CLI
> establecidos con `.set("web.addr", "...")`. Las cuatro implementan el mismo
> trait `Source`, así que el orden en el `vec!` *es* la precedencia.

## Paso 7 — Escribe el YAML y conoce las reglas de los valores

Los archivos YAML los parsea un pequeño escáner de un subconjunto de YAML, línea
a línea —no un motor YAML de propósito general—, de modo que la salida aplanada es
determinista y estable para cualquier archivo dado:

```yaml
# application.yaml
name: lumen
web:
  addr: 127.0.0.1:8080
  admin-addr: 127.0.0.1:8081
tags: wallet, ledger, demo   # a comma-joined scalar binds a Vec<String>
```

Las reglas que el escáner garantiza:

- los mappings anidados se convierten en claves **unidas por puntos y en
  minúsculas** (`web.admin-addr` → la clave plana `web.admin_addr` tras la
  normalización relajada);
- los lexemas escalares se **preservan literalmente** (`1.10` sigue siendo
  `"1.10"`) hasta que el binder los parsea contra el tipo del campo destino;
- las claves duplicadas siguen la regla de **gana la última escritura**;
- los aliases, anclas, archivos multidocumento, tags y secuencias en flujo
  **deliberadamente no se interpretan**: trae tu propio parser si los necesitas.

Lo que acaba de ocurrir: este archivo base declara la identidad de Lumen y sus dos
direcciones de enlace en el hogar tipado que esas direcciones siempre quisieron.
La línea `tags` muestra la única sutileza: una secuencia se escribe como un
escalar unido por comas, y el binder la vuelve a dividir en el campo
`Vec<String>`.

> **Tip** **Punto de control.** Coloca este archivo junto a un test que llame a
> `load_from_profile(".", "application", "dev")` y afirme
> `cfg.web.admin_addr == "127.0.0.1:8081"`. Que pase demuestra que tanto la
> normalización kebab→snake como la división por comas de la lista funcionan de
> extremo a extremo.

## Paso 8 — Resuelve los marcadores de posición `${...}`

`load` (y `bind`) ejecutan una pasada posterior a la fusión que resuelve los
marcadores de posición `${...}` dentro de los valores: la misma sintaxis `${...}`
que usa Spring. También se expone de forma independiente como
`resolve_placeholders(&flat)`.

```yaml
name: lumen
datasource:
  url: ${DATABASE_URL:postgres://localhost/lumen}   # env var, else default
  pool: ${name}-pool                                 # config reference
```

El orden de resolución, de mayor prioridad primero:

- `${ENV_VAR}` — una variable de entorno literal, leída tal cual;
- la **forma relajada `FIREFLY_*`** de una clave de config: `${name}` también
  honra `FIREFLY_NAME` antes de consultar el mapa fusionado, así que **el entorno
  vence a la configuración**;
- `${name}` — una referencia de config al propio mapa fusionado, resuelta de forma
  recursiva con una protección de profundidad 10 contra ciclos;
- `${key:default}` — el texto tras el primer `:` es una reserva cuando ni el
  entorno ni la config resuelven `key`.

Lo que acaba de ocurrir: `datasource.url` lee `DATABASE_URL` del entorno cuando
está presente y, en caso contrario, recurre al default local: una sola línea que
es correcta tanto en dev como en prod. `datasource.pool` interpola otro valor de
config (`name` → `lumen`) para producir `lumen-pool`.

> **Warning** Un marcador de posición irresoluble *sin* un default lanza
> `ConfigError::Placeholder`, y lo mismo hace una referencia circular (`a: ${b}` /
> `b: ${a}`) en cuanto activa la protección de profundidad 10. Un
> `${DATBASE_URL}` con una errata y sin `:default` falla la carga ruidosamente en
> lugar de enlazar una cadena vacía.

## Paso 9 — Enlaza la config directamente en un bean con `#[derive(ConfigProperties)]`

Cargar un struct a mano en `main` está bien, pero los servicios de Lumen quieren
su configuración *inyectada*, no enhebrada a través de cada constructor.
`#[derive(ConfigProperties)]` convierte un struct `serde` en un bean gestionado
por el contenedor y enlazado por prefijo: el patrón exacto sobre el que construye
el siguiente capítulo.

```rust,ignore
use firefly::prelude::*;
use serde::Deserialize;

/// Binds the `lumen.web.*` config subtree into an injectable bean.
#[derive(Deserialize, ConfigProperties, Default)]
#[firefly(prefix = "lumen.web")]
pub struct WebProperties {
    pub addr: String,
    #[serde(default)]
    pub admin_addr: String,
}
```

Lo que acaba de ocurrir: el derive registra `WebProperties` como un singleton cuya
factoría enlaza la porción `lumen.web.*` del mapa de config fusionado, resuelto
por perfil y con los marcadores de posición expandidos. El contenedor lo calienta
con avidez al arranque, de modo que cualquier bean puede recibirlo después por
tipo.

> **Note** **Término clave — bean / autowiring.** Un *bean* es un objeto que el
> framework construye y gestiona por ti; el *autowiring* es que el framework
> entrega un bean a quienquiera que declare un campo para él. Un bean
> `#[derive(Service)]` escribe `#[autowired] props: Arc<WebProperties>` y recibe
> los valores enlazados: sin `load` manual, sin global. Cablearás uno en
> [Cableado de dependencias](./04-dependency-wiring.md). Esto es el bean
> `@ConfigurationProperties` de Spring inyectado con `@Autowired`.

Para escalares aislados hay un toque más ligero: inyecta un único valor resuelto
sobre un campo con un default:

```rust,ignore
#[firefly(value = "${lumen.web.addr:127.0.0.1:8080}")]
addr: String,
```

Para *validar* un bean de propiedades tras el enlace —el `@Validated` de Spring
sobre una clase `@ConfigurationProperties`— añade `#[firefly(validate)]` y
`#[derive(Validate)]`. La macro ejecuta las restricciones declarativas del struct
una vez que la config está enlazada, y una violación **hace fallar la creación del
bean** en el refresco del contexto con los errores estructurados por campo, en
lugar de dejar arrancar una configuración malformada:

```rust,ignore
use firefly::prelude::*;
use serde::Deserialize;

#[derive(Deserialize, ConfigProperties, Validate, Default)]
#[firefly(prefix = "lumen.web", validate)]   // @ConfigurationProperties @Validated
pub struct WebProperties {
    #[validate(not_empty)]
    pub addr: String,
    #[serde(default)]
    pub admin_addr: String,
}
```

Lo que acaba de ocurrir: un `lumen.web.addr` vacío ahora aborta el arranque con
una violación clara por campo (`addr: must not be empty (not_empty)`) en lugar de
enlazar `""` y fallar más tarde cuando algo intente enlazar un socket.

> **Design note.** Firefly ofrece dos estilos de enlace contra el *mismo* mapa
> fusionado, resuelto por perfil y con los marcadores de posición expandidos. Un
> bean enlazado por prefijo (`#[derive(ConfigProperties)]` +
> `#[firefly(prefix = "...")]`) tira de todo un subárbol de config a un único
> struct inyectable; la inyección de un solo valor (`#[firefly(value = "${...}")]`)
> cablea un único escalar resuelto sobre un campo. Usa el primero para un grupo de
> ajustes cohesivo, el segundo para un mando suelto.

## Paso 10 — Autoconfigura la datasource y la seguridad desde `application.yaml`

La maquinaria de propiedades hasta ahora te entrega un struct tipado. Los crates
de infraestructura de Firefly dan el siguiente paso: un puñado de subsistemas son
**dirigidos por config y libres de DI**: enlazas un struct `serde` corriente desde
`application.yaml`/entorno, luego haces `await` de una única llamada de
autoconfiguración al arranque, y el subsistema se levanta a sí mismo. Sin
contenedor, sin cadenas de builders manuales, sin ramificación
`if scheme == "postgres"` en tu cableado. Dos subsistemas en los que Lumen se
apoya de esta manera son su datasource y su capa de seguridad.

Ambos se alimentan de un único árbol YAML. `firefly.datasource.*` se enlaza sobre
`DataSourceProperties` y `firefly.security.*` sobre `SecurityProperties`:

```yaml
firefly:
  datasource:
    url: ${DATABASE_URL:postgres://localhost/lumen}  # scheme picks the backend
    max-connections: 16
    min-connections: 2
    acquire-timeout-ms: 5000
    idle-timeout-ms: 600000
    max-lifetime-ms: 1800000
  security:
    jwt:
      jwk-set-uri: https://idp.example.com/.well-known/jwks.json
      issuer-uri: https://idp.example.com/
      audience: lumen-api
    bearer:
      header-name: Authorization
      allow-anonymous: false
```

### La datasource — `DataSourceProperties` → pool → gestor de transacciones

`DataSourceProperties` es un struct `serde` corriente con los campos `{ url,
max_connections, min_connections, acquire_timeout_ms, idle_timeout_ms,
max_lifetime_ms }`. El **esquema de la URL selecciona el backend**, cada uno tras
su propia feature de cargo: `postgres://` / `postgresql://` → PostgreSQL,
`mysql://` → MySQL, `sqlite:` → SQLite. Un `0` en cualquier ajuste del pool deja en
su lugar el valor por defecto de `sqlx`.

`firefly::data_sqlx::auto_configure(&props)` hace lo único que quieres al
arranque: construye el pool de conexiones **y** registra un
`SqlxTransactionManager` sobre él, de modo que `#[transactional]` se resuelve más
tarde sin cableado manual. El `Db` devuelto es el mismo pool, listo para construir
repositorios tipados. (Para un control más fino, `Db::connect(url)` y
`Db::connect_with(&props)` construyen solo el pool.)

```rust,ignore
use firefly::data_sqlx::{auto_configure, DataSourceProperties};

// `Db` carries the pool; auto_configure also registers the tx manager.
let db = auto_configure(&props).await?;     // Result<Db, FireflyError>
```

> **Note** **Término clave — gestor de transacciones.** Un *gestor de
> transacciones* abre, confirma y revierte transacciones de base de datos en
> nombre del atributo `#[transactional]`. Al registrar uno, `auto_configure` hace
> que `#[transactional]` funcione en todo el proceso sin que tú construyas ni
> enhebres el gestor en ningún sitio: el análogo en Rust de que Spring Boot
> autoconfigure un `DataSourceTransactionManager`. Lo usarás en
> [Persistencia](./07-persistence.md).

### La capa de seguridad — `SecurityProperties` → verifier → capa bearer

`SecurityProperties` anida `{ jwt: JwtProperties, bearer: BearerProperties }`.
`JwtProperties` contiene `{ jwk_set_uri, issuer_uri, audience, secret, algorithm,
expiration_seconds }`; `BearerProperties` contiene `{ header_name, allow_anonymous }`.
Dos funciones lo convierten en middleware en ejecución:

- `verifier_from_config(&props.jwt)` devuelve
  `Result<Option<Arc<dyn Verifier>>, SecurityError>`. Un `jwk_set_uri` no vacío
  construye un verifier de servidor de recursos JWKS (RS256); en caso contrario,
  un `secret` no vacío construye un verifier HMAC (`HS256`/`HS384`/`HS512`); en
  caso contrario, `None`.
- `bearer_layer_from_config(&props)` devuelve
  `Result<Option<BearerLayer>, SecurityError>`: la capa lista para montar con el
  nombre de cabecera configurado y la política anónima ya aplicados, o `None`
  cuando no hay ningún verifier configurado.

> **Note** **Término clave — verifier / capa bearer.** Un *verifier* comprueba la
> firma y los claims de un JWT entrante; una *capa bearer* es el middleware HTTP
> que extrae el token de la cabecera de la petición y ejecuta el verifier. Juntos
> son el análogo en Rust de una cadena de filtros de servidor de recursos de
> Spring Security. La historia completa de seguridad está en
> [Seguridad](./14-security.md); aquí solo estás aprendiendo que ambos pueden
> *configurarse*, no construirse a mano.

### El cableado de arranque de una sola llamada

Enlaza un único struct de config y luego dirige ambos subsistemas desde él. Todo
el cableado es una carga más dos llamadas esperadas:

```rust,ignore
use firefly::config::{load_from_profile, ConfigError};
use firefly::data_sqlx::{auto_configure, DataSourceProperties};
use firefly::security::{bearer_layer_from_config, SecurityProperties};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Firefly {
    datasource: DataSourceProperties,
    security: SecurityProperties,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LumenConfig {
    firefly: Firefly,   // binds the `firefly.datasource.*` / `firefly.security.*` subtree
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load + merge + profile-resolve + placeholder-expand, then bind.
    let cfg: LumenConfig = load_from_profile("/etc/lumen", "application", "dev")?;

    // 2. Build the pool AND register the transaction manager in one await.
    let db = auto_configure(&cfg.firefly.datasource).await?;

    // 3. Build the ready-to-mount bearer layer (None if no JWT settings).
    let bearer = bearer_layer_from_config(&cfg.firefly.security)?;

    // `db` builds typed repositories; mount `bearer` on the web stack.
    // ...
    let _ = (db, bearer);
    Ok(())
}
```

Lo que acaba de ocurrir: esta es la cadena de precedencia del Paso 5 haciendo
trabajo real. `DATABASE_URL` en el entorno sobrescribe el default del YAML para el
pool, y el endpoint JWKS puede reapuntarse por perfil sin tocar el código. Tanto
la maquinaria de `#[transactional]` como el middleware bearer recogen lo que
`auto_configure` y `bearer_layer_from_config` registraron: sin globales
enhebrados a través de tus constructores.

> **Design note.** Firefly mantiene deliberadamente esta ruta libre de DI: los
> structs de config son tipos `serde` corrientes y las llamadas de
> autoconfiguración son `async fn`s corrientes de las que haces `await` al
> arranque. Puedes adoptar más tarde el estilo completo de contenedor
> `#[derive(ConfigProperties)]` sin reescribir nada de esto: los mismos valores
> enlazados fluyen de cualquiera de las dos maneras.

> **Tip** **Punto de control.** Incluso sin una base de datos real, este `main`
> compila: `auto_configure` contra `sqlite::memory:` (establece
> `firefly.datasource.url` a `sqlite::memory:`) devuelve un `Db` vivo que puedes
> conservar, y un `firefly.security.*` vacío hace que `bearer_layer_from_config`
> devuelva `Ok(None)`.

## Paso 11 — Condiciona beans por expresión de perfil

A veces un valor no basta: quieres que todo un bean exista solo en algunos
entornos. `accepts_profiles(&active, &exprs)` evalúa una gramática de expresiones
de perfil contra una lista de perfiles activos: AND (`&`), OR (`|`), negación
(`!`) y agrupación con paréntesis.

```rust,ignore
use firefly::config::{accepts_profiles, active_profiles};

let active = active_profiles("dev");                  // e.g. ["prod", "cloud"]
accepts_profiles(&active, &["prod & cloud"]);         // AND
accepts_profiles(&active, &["prod | qa"]);            // OR
accepts_profiles(&active, &["!test"]);                // negation
accepts_profiles(&active, &["(prod & cloud) | qa"]);  // grouping
```

Lo que acaba de ocurrir: `active_profiles("dev")` lee el `FIREFLY_PROFILE`
separado por comas (recurriendo a `"dev"`), y `accepts_profiles` responde si
*alguna* de las expresiones dadas coincide con ese conjunto activo. Devuelve
`true` ante una coincidencia; una expresión malformada evalúa a `false` y nunca
provoca panic.

Por qué importa: el siguiente capítulo muestra un bean que declara
`#[firefly(profile = "prod")]`, y el contenedor aplica exactamente esta regla en
tiempo de escaneo, de modo que un bean exclusivo de Postgres simplemente no existe
en el perfil `dev`.

## Paso 12 — Recarga en tiempo de ejecución y enmascara secretos

Dos preocupaciones operativas redondean el cuadro.

**Recarga en tiempo de ejecución.** `ReloadableConfig<T>` mantiene viva la cadena
de sources tras el primer enlace. `reload()` reproduce el pipeline completo de
fusión → resolución de marcadores de posición → enlace y cambia atómicamente el
snapshot; una recarga fallida conserva el anterior. Este es el gancho que cablea
un endpoint `POST /actuator/refresh`, de modo que un operador podría reapuntar la
datasource de Lumen sin reiniciar.

```rust,ignore
use firefly::config::{ReloadableConfig, Source};

let cfg: ReloadableConfig<LumenConfig> = ReloadableConfig::load(sources)?;
let snapshot = cfg.get();                  // Arc<LumenConfig> — read per use
let mut rx = cfg.subscribe();              // tokio watch receiver
let changed: Vec<String> = cfg.reload()?;  // sorted, changed top-level keys
```

`Arc<ReloadableConfig<T>>` se convierte en `Arc<dyn Refresher>`: el trait
object-safe del que depende el endpoint de refresco del actuator.

> **Note** **Término clave — refresh scope.** Un lector *con ámbito de refresco*
> llama a `cfg.get()` en cada uso en lugar de cachear el valor interno, de modo
> que siempre ve el snapshot más reciente tras una recarga. Este es el análogo en
> Rust del `@RefreshScope` de Spring Cloud más su contrato
> `POST /actuator/refresh`.

**Enmascarado de secretos.** `Layered::property_sources()` devuelve
`PropertySourceView`s ordenados y atribuidos a su origen (mayor precedencia
primero): los datos que renderiza la vista `/actuator/env` de Firefly, con los
secretos enmascarados. Las claves que nombran secretos (`password`, `secret`,
`token`, `credential`, `*key`, …) se enmascaran como `******`, y una contraseña
incrustada en el userinfo de una URI se redacta
(`postgresql://user:******@host`). El módulo `mask` expone directamente
`mask_value`, `is_sensitive_key` y `sanitize_uri`.

Por qué importa para Lumen: en el momento en que sostenga una clave de firma JWT
(capítulo 14) y una URL de datasource, ninguna debería aparecer jamás en texto
plano en `/actuator/env`, y con el enmascarado activado por defecto, ninguna lo
hace.

> **Tip** **Punto de control.** Añade una clave `datasource.password` a un
> `StaticSource`, llama a `Layered::new(sources).property_sources()` y confirma
> que el valor renderizado es `******`, no el secreto.

## Paso 13 — Obtén configuración desde un servidor de configuración (opcional)

Para una flota de servicios, puedes centralizar la configuración. `ConfigClient`
obtiene un documento remoto (compatible con el formato de cable del servidor
Spring Cloud Config) y lo aplana en un `StaticSource` que insertas en la cadena por
encima de los defaults:

```rust,ignore
use firefly::config::ConfigClient;

let remote = ConfigClient::new("http://config:8888", "lumen")
    .with_profile("prod")
    .with_label("main")
    .with_basic_auth("user", "pass")
    .fetch_source()           // fail-fast; .fetch_source_or_empty() = soft fallback
    .await?;
sources.insert(1, Box::new(remote)); // above defaults, below env/flags
```

Lo que acaba de ocurrir: `ConfigClient::new(url, app)` construye un cliente (el
perfil por defecto es `default`, la label es `main`); los métodos del builder
establecen el resto; `fetch_source().await` consulta
`{url}/{app}/{profile}/{label}` y devuelve un `StaticSource`. Una respuesta no 2xx
registra un warning y produce un mapa vacío (un fallo suave); los fallos de
transporte o de decodificación lanzan `ConfigError::Remote`. El servidor
independiente vive en [`firefly-config-server`](./91-appendix-modules.md).

## Eventos de aplicación en proceso

Vale la pena nombrar una pieza más del crate de config, porque te la encontrarás
en los límites del ciclo de vida. `ApplicationEventBus` es un pub/sub
**en proceso, despachado por `TypeId`, ordenado y síncrono** para eventos de ciclo
de vida y de notificación local, distinto del broker asíncrono `firefly-eda` que
Lumen usa para los eventos de dominio (sin transporte, sin topics; los listeners
se ejecutan en el hilo que publica):

```rust,ignore
use firefly::config::{ApplicationEventBus, ApplicationReadyEvent};

let bus = ApplicationEventBus::new();
bus.subscribe::<ApplicationReadyEvent, _>(|_e| { /* on ready */ });
bus.publish(&ApplicationReadyEvent);
```

Eventos de ciclo de vida que vienen incluidos: `ContextRefreshedEvent`,
`ApplicationReadyEvent`, `ContextClosedEvent` y `RefreshScopeRefreshedEvent`
(disparado tras una recarga exitosa). Cualquier tipo `'static` puede publicarse
como un evento de dominio local.

> **Note** No confundas esto con
> [Arquitectura dirigida por eventos](./10-eda-messaging.md): el
> `ApplicationEventBus` es un canal *local* de ciclo de vida/notificación; los
> eventos de dominio del monedero de Lumen viajan sobre el `Broker` de
> `firefly-eda` por un topic, con un adaptador real de Kafka/RabbitMQ esperando
> tras el default en memoria.

## Resumen — qué cambió en Lumen

| Antes | Después de este capítulo |
|-------|--------------------------|
| identidad codificada a mano en dos cadenas `pub const` | los mismos valores entendidos como mandos de `CoreConfig` que alimentan el banner y `/actuator/info` |
| direcciones de enlace leídas por `FireflyApplication` desde `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` | el hogar tipado para esas direcciones, situado en lo alto de una cadena de precedencia documentada |
| sin ruta hacia ajustes por entorno | perfiles, marcadores de posición y `#[derive(ConfigProperties)]` listos para la inyección en el siguiente capítulo |
| la datasource y la seguridad se construirían a mano | ambas se levantan desde `application.yaml` con una única llamada de autoconfiguración esperada para cada una |
| secretos sin considerar | enmascarado + redacción de `/actuator/env` en su sitio antes de que Lumen sostenga siquiera una clave de firma |

Ahora también sabes:

- Que la configuración es un **mapa plano de cadenas con clave por puntos**
  enlazado sobre un struct `serde` tipado, con el *tipo destino* dirigiendo cada
  parseo.
- La **cadena de precedencia** —defaults → YAML base → YAML de perfil → entorno →
  flags de CLI— y que gana la última source.
- Que `load_from_profile` es el valor por defecto cómodo (con una capa de entorno
  `FIREFLY_*`), mientras que un `Vec<Box<dyn Source>>` explícito + `load` da
  control total.
- Cómo se resuelven los marcadores de posición `${...}` (el entorno vence a la
  config, con reservas `:default` y una protección contra ciclos), cómo
  `#[derive(ConfigProperties)]` inyecta un subárbol enlazado, y cómo
  `auto_configure` / `bearer_layer_from_config` levantan subsistemas enteros desde
  YAML.

## Ejercicios

1. **Promueve los puertos a YAML.** Escribe un `application.yaml` con `web.addr` /
   `web.admin-addr`, cárgalo con `load_from_profile(".", "application", "dev")` y
   confirma que una variable de entorno `FIREFLY_WEB_ADDR` sigue ganando (la fila
   4 de precedencia vence a la fila 2). Luego reconstruye la cadena a mano con
   `from_env("LUMEN")` y muestra que `LUMEN_WEB_ADDR` gana en su lugar.
2. **Añade un perfil.** Crea `application-prod.yaml` que sobrescriba `web.addr` a
   `0.0.0.0:80`, ejecuta con `FIREFLY_PROFILE=prod` y verifica que el valor de
   prod surte efecto mientras un `dev` simple conserva el enlace a localhost.
3. **Resuelve un marcador de posición.** Establece `datasource.url:
   ${DATABASE_URL:postgres://localhost/lumen}` en el YAML, carga una vez con
   `DATABASE_URL` sin establecer (afirma el default) y otra con él establecido
   (afirma el override). Luego elimina el `:default` y confirma que el caso sin
   establecer ahora lanza `ConfigError::Placeholder`.
4. **Enlaza un bean `ConfigProperties`.** Define el struct `WebProperties` del
   Paso 9, establece `lumen.web.addr` mediante un
   `ConditionContext::new().with_property(...)` y resuelve `WebProperties` desde un
   `Container`: reconocerás este patrón en los tests de DI del siguiente capítulo.
5. **Enmascara un secreto.** Añade una clave `datasource.password` a un
   `StaticSource`, llama a `Layered::new(sources).property_sources()` y confirma
   que el valor se renderiza como `******` en lugar de en texto plano.

## Adónde ir después

- Mira cómo la raíz de composición de Lumen resuelve sus colaboradores —y cómo el
  contenedor de primera clase escanea y cablea los beans (incluidos los de
  `#[derive(ConfigProperties)]` que acabas de conocer)— en
  **[Cableado de dependencias](./04-dependency-wiring.md)**.
- Convierte la datasource configurada en repositorios tipados en
  **[Persistencia](./07-persistence.md)**.
- Promueve los defaults en proceso a Postgres y Kafka reales en
  **[Producción y despliegue](./20-production.md)**.
