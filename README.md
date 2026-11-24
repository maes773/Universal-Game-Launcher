# UGL — Universal Game Launcher

Launcher universal de videojuegos para Windows, escrito en Rust con Slint y SQLite.
Local, ligero y modular. **Estado: 0.0.4 alpha (en desarrollo por etapas) — esta versión aún no se ha compilado ni probado** (ver «Verificación»).

## Qué incluye 0.1 Foundation

| Hoja de ruta | Dónde está |
|---|---|
| Estructura del proyecto | workspace con `ugl-core` (sin interfaz) y `ugl-app` (Slint) |
| Ventana principal | `crates/ugl-app/ui/main.slint` |
| SQLite + persistencia | `ugl-core/src/database/` (WAL, claves foráneas, migraciones con copia de seguridad previa) |
| Configuración | `ugl-core/src/settings.rs` (orden de la biblioteca, última carpeta de importación) |
| Biblioteca vacía | primer arranque sin buscar nada; mensaje guía hacia «Importar» |
| Importación individual por carpeta | `integrations/manual.rs` + diálogo de confirmación |
| Importación de Steam por carpeta | `integrations/steam.rs` + `integrations/vdf.rs` |
| Detección de juegos locales | solo lectura de `appmanifest_*.acf` y `libraryfolders.vdf` |
| Ejecución de juegos | `process_manager.rs` (ejecutable propio o `steam://rungameid/<AppID>`) |

Capas: `Slint UI → Core (application.rs) → registry / process_manager → database / integraciones`.
La interfaz solo habla con `Core`.

## Compilar (desarrollo)

Requiere Rust estable reciente (https://rustup.rs) y, en Windows, las herramientas de C++ de Visual Studio.

```
cargo test --workspace          # pruebas del núcleo
cargo run -p ugl-app            # ejecutar en modo desarrollo
cargo build --release -p ugl-app  # genera target/release/ugl.exe
```

Datos del usuario: `%LOCALAPPDATA%\UGL` (base de datos `ugl.db`, `logs\ugl.log`, `cache\`).
Con la variable de entorno `UGL_DATA_DIR` se puede usar otra carpeta.

## Decisiones de diseño de 0.1

- **Sin Tokio por ahora.** El análisis de carpetas corre en hilos estándar y devuelve el resultado al hilo de la interfaz. Tokio se añadirá cuando exista trabajo de red (sincronización, metadatos).
- **Esquema mínimo.** Solo las tablas que 0.1 usa (`platforms`, `games`, `installations`, `game_sources`, `settings`). `achievements`, `unlocks`, `achievement_sources`, `accounts`, `sync_history` y `metadata_cache` llegarán con migraciones nuevas cuando las use una función real.
- **Ejecutable guardado como ruta relativa** a la carpeta del juego: si el usuario mueve la carpeta entera, basta con actualizar la ruta.
- **Duplicados:** clave única (proveedor, id externo). Steam usa el AppID; los juegos manuales, la ruta normalizada.
- **«Quitar de la biblioteca»** solo borra los registros de UGL; nunca toca archivos del juego.
- **Steam:** los juegos se inician con el protocolo `steam://`, así que requieren Steam instalado. UGL no omite ni altera ningún DRM.
- **Límites del análisis de carpetas:** 4 niveles, 20 000 entradas, sin seguir enlaces simbólicos y omitiendo carpetas de redistribuibles. Si el ejecutable está más hondo, se puede añadir con «Buscar otro ejecutable…».

## Logros (solo juegos de Steam, leídos en local)

Junto al botón «Jugar» aparece una insignia «Logros 12/50» para los juegos que ya tienen logros cargados;
en la ficha de cualquier juego de Steam hay un botón «Logros». Abre una ventana aparte (estructura de la
«blade» de Xbox 360 —cabecera con progreso, filtros y lista— con la paleta de UGL) con icono, nombre,
descripción, marca de verificación y fecha de desbloqueo.

- **De dónde salen los datos:** de la caché que el propio cliente de Steam deja en
  `<Steam>\appcache\stats\` (`UserGameStatsSchema_<AppID>.bin` y `UserGameStats_<cuenta>_<AppID>.bin`)
  y de los iconos de `appcache\librarycache\<AppID>\`. Sin red y sin iniciar sesión.
- **Límites:** Steam solo tiene esos archivos para juegos que ha sincronizado en ese equipo. Si no existen,
  UGL lo dice y no inventa nada. Los juegos que no son de Steam no tienen logros por ahora.
- **Procedencia:** se guardan como «Importado», nunca como «Oficial»: UGL no puede verificarlos con Steam.
- **Un logro desbloqueado no se vuelve a bloquear** aunque una lectura posterior de la caché esté atrasada.
- **Carpeta de Steam:** para encontrar la caché, importa tu carpeta de Steam eligiendo la carpeta raíz
  (no una biblioteca adicional); UGL recuerda esa ruta.
- **Sin comprobar:** el formato binario de esos archivos solo está documentado por herramientas de terceros.
  El lector (`integrations/binary_kv.rs` y `steam_achievements.rs`) no se ha probado con archivos reales.
  Las pruebas usan archivos fabricados a mano según esa descripción.
- Las secciones/DLC de logros de Steam (Achievement Sets) aún no se separan: se muestran en una sola lista.

## 0.0.4 alpha — Library Improvements (por etapas)

| Etapa | Contenido | Estado |
|---|---|---|
| 1 | Sistema inteligente de lanzamiento | Escrita, sin compilar |
| 2 | Importación múltiple con vista previa | Escrita, sin compilar |
| 3 | Información de Steam Store (AppID, ficha local, asociación manual) | Escrita, sin compilar |
| 4 | Segundo plano: bandeja del sistema | Escrita, sin compilar |

**Etapa 1 — Lanzamiento.** Cada juego tiene un método de inicio, editable desde su ficha («Método de inicio…»):
- *Ejecutable directo*: ruta del `.exe`, argumentos y directorio de trabajo (por defecto, la carpeta del
  ejecutable). Los argumentos se pasan tal cual a Windows.
- *Steam*: `steam://rungameid/<AppID>`; el ejecutable que ya se conoce se conserva por si quieres volver al inicio directo.
- *Otra plataforma*: una dirección de un esquema permitido (steam, com.epicgames.launcher, goggalaxy, itch, uplay,
  origin2, battlenet). Se entrega a Windows con `ShellExecuteW` (nunca por `cmd` ni `explorer.exe`).
- UGL **nunca inicia un juego a través de un `.lnk`/`.url`**: al importar o elegir un acceso directo se lee su destino
  (`integrations/shortcut.rs`, sin COM) y se guarda el `.exe` real, con sus argumentos y su directorio de trabajo. Un
  acceso `.lnk` de Steam (`steam.exe -applaunch <id>`) o un `.url` (`steam://rungameid/<id>`) se reconoce como juego de Steam.
- Cambiar de método no borra los datos de los otros.

**Etapa 2 — Importación múltiple.** «Importar → Importar ejecutables» acepta varios `.exe` (o `.lnk`/`.url`) y muestra una vista
previa con nombre editable, estado (Nuevo / Duplicado / No válido) y casillas. Cada `.exe` es un juego independiente aunque
compartan carpeta. Se mantienen la importación de carpeta y la de Steam.
- Un juego manual se identifica por su **ejecutable** (antes, por su carpeta); la migración v4 actualiza los registros viejos.
- Duplicados: mismo ejecutable ya importado, repetido en la selección, o ejecutable dentro de la carpeta de un juego de Steam ya importado.
- Un `.url`/`.lnk` de Steam solo se acepta si ese juego ya está en la biblioteca: los juegos de Steam entran por la importación
  de la carpeta de Steam, que exige que estén instalados.
- El selector de archivos de Windows puede sustituir un `.lnk` por su destino (y perder los argumentos). Para conservarlos
  existe «Desde una carpeta de accesos directos…», que lee la carpeta directamente.

**Base de datos v4** (con copia de seguridad automática antes de migrar): `installations` gana `exe_path`, `launch_args`,
`working_dir`, `steam_app_id` y el método `uri`; la tabla se reconstruye conservando los datos.

**Etapa 3 — Información de Steam Store.**
- El AppID sale de los manifiestos de Steam. Un `.exe` agregado a mano que esté en `steamapps\\common\\<carpeta>` se
  enlaza solo con su AppID leyendo el manifiesto de esa biblioteca (`steam::app_id_for_path`). Si más tarde se importa la
  carpeta de Steam, ese juego no se duplica.
- Con el AppID, UGL consulta la tienda **pública** (`store.steampowered.com/api/appdetails`, en español): nombre,
  desarrolladores, descripción corta, géneros, fecha, web y portada (`library_600x900.jpg`, o `header.jpg` si falta). No
  hace falta Steam instalado ni iniciar sesión. Todo se guarda en la tabla `game_metadata` y en la caché de portadas, así
  que UGL funciona sin conexión. Un error de red se reintenta más tarde; una ficha que Steam no ofrece se recuerda y no
  se vuelve a pedir. Una petición cada medio segundo como máximo.
- **Asociación manual:** ficha del juego → «Asociar con Steam…». Se busca por nombre (`storesearch`) o por AppID (si lo
  escrito es solo un número, se trata como AppID). Asociar no cambia el método de inicio; sí habilita la ficha de la
  tienda y los logros de ese juego.
- La ficha muestra portada, desarrollador, fecha, géneros y descripción; si el nombre de la tienda difiere, ofrece
  «Usar el nombre de Steam».
- **No se importan juegos de la cuenta que no estén instalados:** UGL nunca consulta tu cuenta; los juegos de Steam solo
  entran por los manifiestos locales, y solo con instalación completa.
- Se puede desactivar en Ajustes («Descargar la ficha pública de Steam Store»). Todas las funciones que usan internet
  son opcionales (ver «Cuenta de Steam» para la lista).

**Etapa 4 — Segundo plano.**
- Botón «Minimizar a la bandeja» (barra lateral) y, por defecto, minimizado automático al iniciar un juego (desactivable
  en Ajustes).
- Icono en la bandeja del sistema solo mientras UGL está oculto (`tray-icon`): clic izquierdo = abrir; clic derecho = menú
  «Abrir UGL» / «Salir de UGL».
- Al iniciar un juego, un hilo sigue su sesión con la lista de procesos de Windows (`session.rs`): el proceso lanzado y sus
  descendientes (inicio directo) o cualquier proceso dentro de la carpeta del juego (Steam). Cuando desaparecen (con 6 s de
  margen por si el juego pasa de un lanzador a otro proceso), UGL vuelve a mostrarse. Si no se detecta el juego en 2
  minutos, UGL también vuelve. Solo se consulta mientras hay una sesión: en reposo no hay sondeo.
- Cerrar la ventana con la X sale de UGL (no lo deja oculto sin icono).
- Limitación: los procesos protegidos (juegos con permisos elevados) no dejan leer su ruta; el seguimiento por carpeta de
  Steam no los verá, y el de inicio directo (por proceso hijo) sí.

## Cuenta de itch.io (opcional)

Ninguna función de UGL exige una cuenta. Si vinculas la tuya (barra lateral → «Cuenta de itch.io»), los juegos de tu
biblioteca de itch.io (comprados o reclamados) aparecen en UGL **aunque no estén instalados** en este equipo. UGL ya no tiene
ninguna integración con cuentas de Steam: la importación de la carpeta de Steam, las fichas públicas de la tienda y los logros
locales siguen igual.

- **Cómo se vincula:** con una clave de API personal que generas tú en itch.io (Ajustes → API keys → «Generate new API key»; el
  botón «Conseguir clave» abre esa página). UGL nunca ve tu usuario ni tu contraseña.
- **Cómo se guarda la clave:** cifrada con tu usuario de Windows (DPAPI), nunca en claro. Al consultar, se envía a `api.itch.io`
  en la cabecera `Authorization: Bearer` mediante `curl.exe` (incluido en Windows 10/11), entregada por la entrada estándar: no
  queda en la línea de comandos ni en la caché de internet de Windows.
- **Qué se descarga:** la lista de tu biblioteca (`profile/owned-keys`, página a página) y tu nombre de usuario. Se guarda una
  copia local en la tabla `itch_games`. La portada de un juego se descarga solo al abrirlo, y solo desde `img.itch.zone`.
- En la biblioteca, esos juegos llevan la etiqueta «No instalado» (casilla «Mostrar no instalados»). Su ficha ofrece «Abrir en
  itch.io»; UGL no instala nada. Si un juego con el mismo título ya está en tu biblioteca local, deja de figurar como no
  instalado (la comparación es por título).
- Se sincroniza al vincular, al pulsar «Sincronizar biblioteca» y al arrancar si hace más de 12 horas. Si itch.io devuelve una
  lista vacía, UGL no borra lo que ya tenía.
- **Desvincular** borra la clave y la copia local de la biblioteca.
- **Migración v7:** elimina la tabla de la biblioteca de Steam y toda la configuración guardada de la antigua cuenta de Steam
  (identidad, nombre y clave cifrada de la API de Steam).

**Funciones que usan internet (todas opcionales y desactivables):** imágenes de logros, fichas de Steam Store y, si vinculas la
cuenta, la API de itch.io.

## Iconos y actualización automática (0.2)

**Icono del juego.** Antes del nombre aparece el icono del ejecutable, el mismo que muestra Windows. Se extrae
una sola vez en segundo plano (API de Windows `PrivateExtractIconsW`) y se guarda como PNG en
`%LOCALAPPDATA%\UGL\cache\icons`. Para juegos de Steam se usa el mejor candidato de la carpeta del juego
(la misma heurística que la importación manual); si no hay una propuesta clara, se muestra la inicial del título.
Al cambiar el ejecutable de un juego, el icono se vuelve a extraer.

**Imágenes de logros.** Desbloqueado = icono a color; bloqueado = icono gris. Orden de búsqueda: caché de
Steam, caché de UGL y, si está activado en Ajustes, descarga desde los servidores de imágenes de Steam
(`/steamcommunity/public/images/apps/<AppID>/<código>.jpg`). Usa internet (igual que la ficha de Steam Store); se puede
desactivar y entonces solo se usan imágenes ya presentes en el equipo. Si no existe el icono gris, se muestra el
de color atenuado. Los logros ocultos no muestran imagen ni nombre hasta desbloquearse.

**Actualización automática** (se puede desactivar en Ajustes):
- Se vigilan con eventos del sistema (`notify`; sin sondeo, sin consumo en reposo) `appcache\stats` y la carpeta
  `steamapps` de cada biblioteca conocida.
- Cuando Steam guarda el progreso de un juego de tu biblioteca, UGL relee los logros y actualiza la insignia y
  la ventana de logros si está abierta. Las ráfagas de escrituras se agrupan (1,5 s de silencio, máximo 15 s).
- Cuando cambia un `appmanifest`, UGL relee los manifiestos e importa los juegos nuevos **solo cuando Steam los
  marca como instalados por completo** (`StateFlags` con el bit 4). No se importan descargas a medias.
- Al arrancar se hace una puesta al día ligera: manifiestos de Steam y logros cuyo archivo de progreso cambió
  con UGL cerrado. No se escanean los juegos.

**Icono de UGL.** Icono provisional (una «U» ámbar sobre fondo madera) en `crates/ugl-app/assets/`. Se incrusta
en `ugl.exe` con `winresource` (necesita el compilador de recursos del Windows SDK; si falta, la compilación sigue
y solo avisa) y se usa como icono de las ventanas.

## Verificación

Nada de este código se ha compilado ni ejecutado todavía (ni el de 0.1 ni el de 0.2). Hay pruebas unitarias escritas (VDF, Steam, escaneo de carpetas, base de datos, importación) pero **no se han corrido**.
Partes escritas sin poder comprobarlas: la API de itch.io y el uso de `curl` con la clave por la entrada estándar (`itch.rs`, `net.rs`), el cifrado DPAPI (`secrets.rs`), el formato de las respuestas de Steam Store (`steam_store.rs`), la lista de procesos de Windows (`session.rs`), la API `tray-icon` (`tray.rs`), el lector de `.lnk` (`shortcut.rs`, escrito según la especificación [MS-SHLLINK] y probado solo con archivos fabricados a mano), la migración v4, la extracción de iconos con la API de Windows (`icons.rs`), la descarga con `urlmon` (`net.rs`), el formato binario de la caché de Steam y la ubicación local de los iconos de logros.
El flujo `.github/workflows/ci.yml` ejecuta pruebas, clippy y compilación release en Windows.

## Pendiente (no forma parte de 0.1)

Estanterías esqueumórficas, logros de juegos que no sean de Steam, ZIP portable, GitHub Releases automáticos, icono y manifiesto de Windows, y elección de licencia (el proyecto aún no tiene una).
