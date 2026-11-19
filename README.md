# UGL — Universal Game Launcher

**One library. All your games.**

UGL (Universal Game Launcher) es un launcher de videojuegos de código abierto diseñado para reunir, organizar y ejecutar juegos de diferentes plataformas desde una sola biblioteca.

El proyecto busca ofrecer una alternativa ligera, modular y personalizable que permita administrar juegos instalados localmente, independientemente de su procedencia.

Está desarrollado con Rust, Slint y SQLite, priorizando el almacenamiento local, la modularidad y la posibilidad de ampliar sus funciones progresivamente.

---

# 1. Características generales

- Biblioteca unificada de videojuegos.
- Importación de juegos instalados localmente.
- Compatibilidad con bibliotecas locales de Steam.
- Importación de ejecutables individuales.
- Ejecución de juegos desde UGL.
- Sistema de logros por juego.
- Seguimiento del progreso de logros.
- Gestión local de información y configuraciones.
- Caché de iconos e imágenes.
- Actualización automática de información compatible.
- Interfaz gráfica personalizable.

**Importante:** UGL está enfocado en los juegos instalados localmente. No pretende limitarse a una tienda específica ni requiere que todos los juegos pertenezcan a una plataforma digital.

---

# 2. Historial de versiones

## v0.0.1-alpha — First Experimental Build

<badge>Primera versión funcional</badge>

La primera versión de UGL estableció los fundamentos del proyecto: una aplicación de escritorio capaz de crear una biblioteca local, registrar juegos y ejecutarlos desde una misma interfaz.

### Funcionalidades

- Estructura inicial del proyecto en Rust.
- Interfaz gráfica desarrollada con Slint.
- Base de datos SQLite para almacenar información.
- Biblioteca de juegos instalados localmente.
- Importación de juegos mediante carpetas.
- Importación de bibliotecas locales de Steam.
- Detección de juegos instalados.
- Registro de información de los juegos.
- Ejecución de juegos desde UGL.
- Persistencia de la biblioteca entre sesiones.

### Limitaciones

- Interfaz todavía básica.
- Sin sistema de logros.
- Sin integración con la biblioteca online de Steam.
- Sin recuperación avanzada de metadatos.
- Sin diseño visual definitivo.
- Funcionalidades todavía experimentales.

**Objetivo de esta versión:** demostrar que era posible reunir juegos instalados de diferentes procedencias y ejecutarlos desde una única aplicación.

---

## v0.0.2-alpha — Achievement System

<badge color="success">Sistema de logros</badge>

La segunda versión amplió las capacidades de UGL mediante la incorporación de un sistema de logros integrado en la biblioteca.

Cada juego compatible puede contar con su propia ventana de logros, permitiendo consultar el progreso y distinguir entre logros desbloqueados y pendientes.

### Funcionalidades

**Sistema de logros**
- Ventana independiente de logros para cada juego.
- Visualización del nombre y descripción de cada logro.
- Distinción entre logros desbloqueados y bloqueados.
- Registro de fecha y hora de desbloqueo.
- Porcentaje de progreso por juego.
- Contador de logros desbloqueados respecto al total.

**Filtros**
- Todos.
- Desbloqueados.
- Bloqueados.

**Interfaz**
- Acceso a los logros desde la biblioteca.
- Indicador del progreso de logros junto al juego.
- Barra de progreso y porcentaje de finalización.
- Ventana de información y gestión de cada juego.
- Botón para actualizar manualmente la información de logros.

**Almacenamiento**
- Persistencia local de la información mediante SQLite.
- Conservación del progreso registrado entre sesiones.

### Limitaciones

- La actualización de los logros todavía era manual.
- Las imágenes de los logros no contaban con su integración definitiva.
- No existía actualización automática completa.
- No había notificaciones de desbloqueo.
- La integración con las fuentes de logros todavía estaba en desarrollo.

**Objetivo de esta versión:** convertir UGL en algo más que un simple lanzador, incorporando una biblioteca que también permita consultar y seguir los logros de los juegos.

---

## v0.0.3-alpha — Automation & Library Improvements

<badge color="success">Automatización y mejoras</badge>

La tercera versión se centró en reducir las tareas manuales y mejorar la presentación de los juegos y sus logros.

Se incorporaron mejoras en la extracción de iconos, la actualización automática de información y la utilización de imágenes de logros de Steam.

### Funcionalidades

**Iconos de juegos**
- Extracción automática de iconos desde los ejecutables.
- Visualización del icono correspondiente antes del nombre del juego.
- Uso de iconos provisionales cuando no se encuentra uno adecuado.
- Mejoras en la gestión y conservación de los iconos.

**Actualización automática**
- Actualización periódica de las bibliotecas locales de Steam.
- Detección de nuevos juegos instalados en las bibliotecas supervisadas.
- Actualización automática de la información de logros.
- Reducción de la necesidad de utilizar manualmente el botón de actualización.
- Opciones de configuración para desactivar las actualizaciones automáticas.

**Imágenes de logros**
- Incorporación de imágenes de logros procedentes de Steam.
- Visualización de la variante a color para los logros desbloqueados.
- Visualización de la variante en escala de grises para los logros bloqueados.
- Descarga opcional de las imágenes para conservarlas localmente.

**Funcionamiento offline**
- Conservación local de la información disponible.
- Reutilización de imágenes almacenadas en caché.
- Las funciones principales de biblioteca y lanzamiento no dependen de una conexión permanente a Internet.
- La descarga de imágenes de logros de Steam requiere conexión a Internet.

**Objetivo de esta versión:** automatizar tareas que anteriormente requerían intervención manual y mejorar la experiencia visual de la biblioteca y el sistema de logros.

---

# 3. Tecnologías utilizadas

| Tecnología | Función |
|---|---|
| Rust | Lenguaje principal |
| Slint | Interfaz gráfica |
| SQLite | Base de datos local |
| Cargo | Gestión de dependencias y compilación |
| Windows API | Funciones específicas del sistema operativo |

### Arquitectura

UGL utiliza una arquitectura modular organizada en dos componentes principales:

- **ugl-core:** lógica principal, biblioteca, base de datos, integraciones y gestión de procesos.
- **ugl-app:** aplicación de escritorio e interfaz gráfica desarrollada con Slint.

La interfaz se comunica con el núcleo de la aplicación, evitando concentrar toda la lógica del programa en los componentes visuales.

---

# 4. Almacenamiento local

UGL utiliza SQLite para conservar los datos de la aplicación.

En Windows, los datos se almacenan normalmente en:

`%LOCALAPPDATA%\UGL`

Dentro de esta ubicación se encuentran:

- `ugl.db`: base de datos.
- `logs\`: registros de ejecución.
- `cache\`: archivos almacenados en caché.

También se contempla la posibilidad de establecer una ubicación personalizada mediante la variable de entorno `UGL_DATA_DIR`.

Los datos de la biblioteca se gestionan independientemente de los archivos originales de los videojuegos.

**Quitar un juego de la biblioteca no elimina su instalación ni sus archivos originales.**

---

# 5. Alcance actual del proyecto

UGL se encuentra en una etapa experimental Alpha.

El desarrollo se centra inicialmente en Windows y en la administración de juegos instalados localmente.

### Lo que busca ofrecer

- Una biblioteca única para juegos de diferentes procedencias.
- Administración local de videojuegos.
- Lanzamiento desde una misma aplicación.
- Seguimiento de logros.
- Almacenamiento local y funcionamiento offline cuando sea posible.
- Una arquitectura preparada para futuras integraciones.

### Lo que todavía no representa

- Una tienda digital.
- Un servicio de descarga de videojuegos.
- Un sustituto de Steam, Epic Games u otras plataformas.
- Una sincronización universal de cuentas online.
- Una garantía de compatibilidad con todos los juegos y emuladores.

---

# 6. Hoja de ruta futura

UGL continuará desarrollándose por etapas.

Entre los objetivos futuros se encuentran:

- Mejorar la compatibilidad con diferentes launchers.
- Ampliar la detección y gestión de instalaciones.
- Mejorar la importación de múltiples ejecutables.
- Perfeccionar la gestión de metadatos e imágenes.
- Ampliar las fuentes compatibles con el sistema de logros.
- Mejorar el seguimiento de tiempo de juego.
- Incorporar nuevas opciones de personalización.
- Desarrollar una interfaz inspirada en las bibliotecas físicas de videojuegos.

Estas funcionalidades forman parte de los objetivos de desarrollo y no deben considerarse implementadas hasta que aparezcan en una versión publicada.

---

# 7. Compilación del proyecto

Requisitos:

- Rust estable.
- Cargo.
- Herramientas de compilación de C++ de Visual Studio en Windows.

Comandos principales:

```powershell
# Comprobar el proyecto
cargo check --workspace

# Ejecutar las pruebas
cargo test --workspace

# Ejecutar UGL en desarrollo
cargo run -p ugl-app

# Generar la versión optimizada
cargo build --release -p ugl-app
```

El ejecutable generado se encuentra normalmente en:

`target/release/ugl.exe`

---

# 8. Licencia y contribuciones

UGL es un proyecto de código abierto publicado bajo la licencia MIT.

El proyecto busca crecer progresivamente mediante mejoras, correcciones y nuevas integraciones.

Repositorio oficial:

https://github.com/maes773/Universal-Game-Launcher

---

**UGL — One library. All your games.**
