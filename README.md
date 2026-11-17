# UGL — Universal Game Launcher

Launcher universal de videojuegos para Windows, escrito en Rust con Slint y SQLite.
Local, ligero y modular. **Estado: 0.1 Foundation — código sin compilar ni probar todavía** (ver «Verificación»).

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

## Verificación

Nada de este código se ha compilado ni ejecutado todavía. Hay pruebas unitarias escritas (VDF, Steam, escaneo de carpetas, base de datos, importación) pero **no se han corrido**.
El flujo `.github/workflows/ci.yml` ejecuta pruebas, clippy y compilación release en Windows.

## Pendiente (no forma parte de 0.1)

Estanterías esqueumórficas, logros, ZIP portable, GitHub Releases automáticos, icono y manifiesto de Windows, y elección de licencia (el proyecto aún no tiene una).
