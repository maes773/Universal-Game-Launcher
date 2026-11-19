# UGL — Technical Documentation

**Architecture, modules, dependencies, implementation notes and version history**

## 1. Project overview

UGL (Universal Game Launcher) is a local-first desktop game library and launcher written in Rust.

The application is structured as a Cargo workspace containing two main crates:

- `ugl-core`: application logic, data management, integrations and process handling.
- `ugl-app`: graphical interface and desktop application.

The core is designed to remain independent from the graphical interface. This allows integrations, database operations and game management to evolve without putting business logic inside UI components.

### Main data flow

```text
User
 │
 ▼
ugl-app
 │
 ▼
Core (application.rs)
 │
 ├── registry
 │    └── Game library operations
 │
 ├── integrations
 │    ├── manual
 │    ├── Steam
 │    └── Steam achievements
 │
 ├── achievement_engine
 │
 ├── icons
 │
 ├── process_manager
 │
 ├── watcher
 │
 ├── settings
 │
 └── database
      └── SQLite
```

The UI communicates with `Core`. It should not access SQLite or platform integrations directly.

---

# 2. Workspace structure

The current public source is organized around these modules:

```text
crates/
│
├── ugl-core/
│   └── src/
│       ├── achievement_engine.rs
│       ├── application.rs
│       ├── diagnostics.rs
│       ├── domain.rs
│       ├── error.rs
│       ├── icons.rs
│       ├── net.rs
│       ├── paths.rs
│       ├── png.rs
│       ├── process_manager.rs
│       ├── registry.rs
│       ├── settings.rs
│       ├── watcher.rs
│       │
│       ├── database/
│       │   ├── mod.rs
│       │   └── migrations.rs
│       │
│       └── integrations/
│           ├── mod.rs
│           ├── manual.rs
│           ├── steam.rs
│           ├── steam_achievements.rs
│           ├── vdf.rs
│           └── binary_kv.rs
│
└── ugl-app/
    ├── src/
    │   └── main.rs
    │
    ├── ui/
    │   └── main.slint
    │
    ├── build.rs
    └── Cargo.toml
```

This tree describes the modules visible in the public source snapshot. Additional files may exist in the local development branch.

---

# 3. Core modules

## 3.1. `application.rs` — Application Core

The central entry point for the graphical application.

It exposes the `Core` structure, which coordinates the main application operations.

Responsibilities:

- Open the database.
- Retrieve and list games.
- Search and sort the library.
- Import games.
- Remove game entries.
- Manage Steam library configuration.
- Retrieve achievement information.
- Coordinate game launching.
- Coordinate icon extraction and related background jobs.

The UI should call methods exposed by `Core` rather than directly accessing lower-level modules.

This is the main boundary between the interface and the application logic.

## 3.2. `domain.rs` — Domain models

Defines the main data structures and types used throughout the application.

Examples include:

- `Game`
- `GameId`
- `DiscoveredGame`
- `GameStatus`
- `LaunchKind`
- `RegisterSummary`
- `SortOrder`

`LaunchKind` identifies the launch mechanism used by a game.

The current implementation includes:

- `Executable`: launch a local executable.
- `SteamUri`: launch a game through Steam using its AppID.

These types allow integrations to return standardized game information without depending on the UI.

## 3.3. `registry.rs` — Game registry

Manages game registration and retrieval from SQLite.

Responsibilities:

- Register discovered games.
- Retrieve a game by ID.
- List games.
- Search by title.
- Sort by name, recently played, or recently added.
- Prevent duplicate entries.
- Update installation information where applicable.
- Remove entries from the library.

### Duplicate prevention

Games are identified using their source provider and external identifier.

The registry uses:

```text
(provider, external_id)
```

Examples:

```text
("steam", "730")
("manual", "normalized-local-path")
```

This avoids treating two games with identical names as the same game.

Removing an entry from UGL removes its database records, not the original game installation.

## 3.4. `process_manager.rs` — Process management

Responsible for launching games and checking whether their installation paths are valid.

The current public implementation supports:

### Direct executable

UGL builds the executable path from the installation directory and the stored relative target.

It checks that the file exists and starts the process using Rust's `Command`.

The working directory is set to the executable's parent directory.

### Steam URI

UGL validates the numeric AppID and opens:

```text
steam://rungameid/<APP_ID>
```

On Windows, the URI is opened through the system shell.

This method requires Steam to be installed and registered as the handler for the Steam protocol.

UGL does not bypass Steam DRM or replace Steam's authentication.

### Installation status

The process manager can identify:

- Missing installation folder.
- Missing executable.
- Ready installation.

The launcher should report failures rather than silently attempting invalid paths.

## 3.5. `database/` — SQLite database

The database module manages:

- Connection creation.
- Database configuration.
- Schema migrations.
- Foreign key enforcement.
- Write-ahead logging.

The current configuration includes:

```sql
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
```

It also configures a five-second busy timeout.

### Why WAL?

Write-Ahead Logging improves concurrency between readers and writers and reduces unnecessary blocking for normal library operations.

### Migrations

Schema changes should be introduced through migrations.

Never delete or recreate the user's database to implement a new feature.

The migration system should preserve previous data and provide a backup before applying changes when configured to do so.

## 3.6. `settings.rs` — Application settings

Stores application preferences as key/value pairs in SQLite.

Current settings include:

- Library sort order.
- Last import directory.
- Steam root directory.
- Steam library directories.
- Automatic update preference.
- Achievement icon download preference.

Settings should be read through the core rather than directly from the UI.

## 3.7. `paths.rs` — User data directories

Resolves the location used to store UGL's persistent data.

Default Windows location:

```text
%LOCALAPPDATA%\UGL
```

The following files and folders are used:

```text
UGL/
├── ugl.db
├── logs/
│   └── ugl.log
└── cache/
```

A custom location can be provided through:

```text
UGL_DATA_DIR
```

This makes testing and portable configurations easier.

## 3.8. `diagnostics.rs` — Diagnostics and logging

Provides centralized diagnostic messages.

It is used by modules such as:

- Process manager.
- Game registry.
- Steam integration.
- Filesystem watcher.
- Achievement engine.

The goal is to make errors and background operations traceable without showing technical details unnecessarily in the interface.

## 3.9. `error.rs` — Error handling

Centralizes application error types and conversions.

The project uses `thiserror` to define structured errors.

Modules should return meaningful errors instead of hiding failures or silently ignoring important operations.

## 3.10. `icons.rs` — Game icon extraction

Extracts the icon embedded in a Windows executable.

The implementation uses Windows API functions through native FFI.

The process is:

1. Identify the executable representing the game.
2. Extract its icon.
3. Convert the image to RGBA pixel data.
4. Encode it as PNG.
5. Save it in the UGL cache.
6. Store or retrieve its path through the library.

The current icon size is 64 × 64 pixels.

Cache location:

```text
cache/icons/game-<GAME_ID>.png
```

If an executable cannot provide an icon, the UI can use a fallback image.

Icon extraction is Windows-specific in the current implementation.

## 3.11. `png.rs` — PNG encoding

Contains the PNG encoding functionality used by the icon system.

This avoids requiring the application to depend on a full external image-processing framework just to save extracted icon data.

## 3.12. `net.rs` — Network utilities

Provides network-related functionality used by optional image downloads.

Network access should remain optional.

The application should continue to work with cached data when an image server is unavailable.

## 3.13. `watcher.rs` — Filesystem monitoring

Uses the `notify` crate to watch Steam-related directories.

It identifies changes to files such as:

```text
appmanifest_<APP_ID>.acf
UserGameStatsSchema_<APP_ID>.bin
UserGameStats_<ACCOUNT>_<APP_ID>.bin
```

These changes can indicate:

- A new game installation.
- A game update or removal.
- A change in an achievement schema.
- New achievement progress.

The watcher uses filesystem events and a debounce period to avoid processing repeated events immediately.

The implementation uses a background thread and channels so that filesystem monitoring does not block the graphical interface.

The current code defines a quiet period of 1.5 seconds and a maximum wait of 15 seconds while grouping events.

## 3.14. `integrations/manual.rs` — Manual game discovery

Analyzes a selected game directory and identifies possible executable files.

The scanner:

- Searches for `.exe` files.
- Scores candidates according to their names, size and depth.
- Penalizes installers, uninstallers and auxiliary tools.
- Skips common redistributable directories.
- Suggests the most likely main executable.

### Safety limits

The current implementation uses:

- Maximum depth: 4 levels.
- Maximum scanned entries: 20,000.
- Maximum executable candidates: 200.

These limits prevent excessively large directories from blocking the application or consuming unnecessary resources.

The scanner does not modify, move or delete files from the game installation.

## 3.15. `integrations/steam.rs` — Steam library discovery

Reads Steam installation information directly from local files.

It uses:

```text
libraryfolders.vdf
appmanifest_<APP_ID>.acf
```

The integration supports old and new `libraryfolders.vdf` formats.

For each valid installed game it attempts to retrieve:

- AppID.
- Game name.
- Installation directory.
- Steam library location.

It ignores known non-game entries such as Steamworks Common Redistributables.

It also validates installation directory names to prevent unsafe paths.

### Important limitation

This integration reads local Steam files. It does not require a Steam Web API key or account login to discover locally installed games.

It does not currently represent every game owned by the user online.

## 3.16. `integrations/vdf.rs` — Valve VDF parser

A lightweight parser for Valve's text-based KeyValues/VDF format.

Used to read Steam configuration and application manifests.

It handles:

- Quoted strings.
- Escaped characters.
- Nested objects.
- Comments.
- Maximum nesting depth.
- Invalid or incomplete files.

This parser is read-only and is designed for the formats used by the Steam integration.

## 3.17. `integrations/steam_achievements.rs` — Steam achievement reader

Reads achievement data from Steam's local cache.

The current implementation uses files under:

```text
<STEAM_ROOT>/appcache/stats/
```

Relevant files:

```text
UserGameStatsSchema_<APP_ID>.bin
UserGameStats_<ACCOUNT>_<APP_ID>.bin
```

The schema file contains information such as:

- Achievement identifiers.
- Names.
- Descriptions.
- Hidden status.
- Icon identifiers.
- Locked and unlocked image identifiers.

The user statistics file contains achievement progress and unlock timestamps.

### Reading process

1. Locate the game's achievement schema.
2. Parse the binary KeyValues structure.
3. Locate the relevant user statistics file.
4. Read achievement bit masks.
5. Determine which achievements are unlocked.
6. Retrieve unlock timestamps when available.
7. Resolve cached achievement images.
8. Return standardized achievement records to the core.

When multiple Steam user statistics files exist, the implementation selects the most recently modified candidate.

### Images

Steam provides separate image identifiers for unlocked and locked achievement states.

UGL can retrieve those images from Steam's image servers and cache them locally.

Expected appearance:

- Unlocked achievement: colored image.
- Locked achievement: grayscale image.

### Important limitations

- The reader depends on Steam's local cache.
- The binary format is not an officially stable public interface.
- Some games may not expose readable achievement data.
- The Steam client must have generated the relevant local files.
- Image downloads require an Internet connection unless the images are already cached.

The parser must report unsupported or invalid data instead of inventing achievement information.

## 3.18. `achievement_engine.rs` — Achievement storage

Provides a unified representation for achievements from different sources.

It separates achievement data from its origin.

Current provenance types:

- `Official`
- `Imported`
- `Local`
- `Unverified`

This distinction is important because UGL should never present an imported or locally created achievement as an official platform achievement without verification.

The engine stores:

- External achievement ID.
- Name.
- Description.
- Hidden status.
- Image paths.
- Unlock status.
- Unlock timestamp.
- Source.
- Provenance.

The database uses a unique combination of game, source and external achievement ID to avoid duplicate achievement records.

### Unlock preservation

When new achievement data is imported, an already-unlocked achievement should not become locked because of an outdated cache.

The current storage logic preserves an existing unlocked state when a later record reports it as locked.

---

# 4. UI architecture

The `ugl-app` crate contains the desktop application.

The main Slint interface is located at:

```text
crates/ugl-app/ui/main.slint
```

The application entry point is:

```text
crates/ugl-app/src/main.rs
```

The build script is:

```text
crates/ugl-app/build.rs
```

The build script compiles Slint resources and configures the UI style.

The current UI should remain a presentation layer.

When adding a new feature:

1. Add the required core operation.
2. Expose it through `Core`.
3. Connect the UI to that operation.
4. Display the result or error.
5. Add tests for the core behavior.

Avoid placing Steam parsing, SQL statements, or process-launching logic directly inside Slint components.

---

# 5. Dependencies

The following table describes the direct dependencies declared in the public workspace manifests.

| Dependency | Crate | Role |
|---|---|---|
| `rusqlite` | ugl-core | SQLite database access |
| `thiserror` | ugl-core | Structured error types |
| `dirs` | ugl-core | User data directory discovery |
| `notify` | ugl-core | Filesystem monitoring |
| `tempfile` | ugl-core, dev only | Temporary directories for tests |
| `slint` | ugl-app | UI runtime |
| `rfd` | ugl-app | Native file and folder dialogs |
| `slint-build` | ugl-app, build | Compile Slint resources |
| `winresource` | ugl-app, Windows build | Windows executable resources |

### Dependency configuration

The workspace declares:

```toml
[workspace.dependencies]
rusqlite = { version = "0.32", features = ["bundled"] }
thiserror = "1"
dirs = "5"
```

The core crate declares:

```toml
notify = "6"
tempfile = "3"
```

The application crate declares:

```toml
slint = "1.8"
rfd = "0.15"
slint-build = "1.8"
winresource = "0.1"
```

### Version resolution

The manifest version is a compatibility requirement, not necessarily the exact version compiled.

`Cargo.lock` records the exact resolved versions.

For example, the public lockfile currently resolves Slint and Slint Build to 1.19.0, even though the manifest declares the compatible `1.8` version range.

Do not edit `Cargo.lock` manually. Use Cargo to update dependencies.

### Why `rusqlite` uses `bundled`

The `bundled` feature includes SQLite in the build.

This avoids requiring the end user to install a separate SQLite library.

### Why `tempfile` is a development dependency

Temporary directories are useful for tests that create disposable game folders, database files and fake Steam installations.

They should not be included as a runtime dependency unless a production feature genuinely needs them.

---

# 6. Database design

The current database uses the following main entities:

| Table | Purpose |
|---|---|
| `platforms` | Available platforms |
| `games` | Main game records |
| `installations` | Installation paths and launch information |
| `game_sources` | Source provider and external identifier |
| `settings` | Application preferences |
| `achievements` | Achievement definitions |
| `unlocks` | Achievement unlock states and timestamps |

### Relationships

```text
platforms
   │
   └── games
         │
         ├── installations
         │
         ├── game_sources
         │
         └── achievements
                │
                └── unlocks

settings
```

A game can have one or more source records and installation records.

The primary installation is used for normal game launching.

### Data integrity

- Foreign keys maintain relationships.
- Migrations update the schema.
- Unique constraints prevent duplicate source records.
- Transactions are used for multi-record registration and achievement storage.
- Removing a game from the library should not delete its original files.

---

# 7. Performance and optimization

UGL is designed to avoid unnecessary work.

The current implementation includes several optimizations.

### 7.1. Local-first storage

Game information and achievement records are stored in SQLite.

This reduces repeated file parsing and allows the library to open without requiring an online service.

### 7.2. Database WAL mode

SQLite uses Write-Ahead Logging.

This improves normal read/write concurrency and reduces unnecessary database blocking.

### 7.3. Cached SQL statements

The registry uses prepared and cached SQL statements for repeated library queries.

### 7.4. Filesystem monitoring

The `notify` watcher reacts to filesystem changes.

It avoids relying exclusively on continuous directory polling.

### 7.5. Event debouncing

Steam may write the same files multiple times during an installation or update.

The watcher groups repeated events before notifying the application.

### 7.6. Bounded folder scanning

Manual game discovery has explicit depth, entry and candidate limits.

This prevents large directories from producing unbounded scans.

### 7.7. Icon caching

Executable icons are extracted once and saved as PNG files.

The UI can reuse the cached image instead of extracting the icon every time the library is displayed.

### 7.8. Background work

Filesystem analysis, icon extraction and other potentially slow operations should run outside the UI thread.

The UI should remain responsive while these operations are running.

---

# 8. Version history

This section documents the evolution of UGL based on the development history and the functionality visible in the project.

**Version attribution note:** the public repository snapshot does not currently expose a complete release/tag history for every `0.0.x-alpha` milestone. The following history reconstructs the development stages from the project notes and implementation. Confirm exact commit boundaries before treating every entry as an immutable release changelog.

## v0.0.1-alpha — Foundation

### Objective

Create the first functional game launcher.

### Changes

- Initial Rust workspace.
- `ugl-core` and `ugl-app` separation.
- Slint desktop interface.
- SQLite database.
- Local game library.
- Manual game import through folder selection.
- Local Steam library import.
- Game discovery through Steam manifests.
- Basic game launching.
- Persistent settings and library data.

### How it works

Manual imports are analyzed by the manual integration. The user selects a game folder and chooses the executable that represents the game.

Steam imports read local VDF and ACF files to identify installed games and their AppIDs.

The registry stores the resulting game records in SQLite.

The process manager launches local executables or opens the Steam URI for Steam games.

### Main limitation

This version established the library and launching foundations. It did not yet have the complete achievement system.

## v0.0.2-alpha — Achievement System

### Objective

Add per-game achievement tracking.

### Changes

- Achievement data model.
- Achievement database tables.
- Unlock states.
- Unlock timestamps.
- Achievement progress counters.
- Per-game achievement window.
- Filters for all, unlocked and locked achievements.
- Manual achievement refresh.
- Achievement provenance and source information.

### How it works

Achievement definitions are stored separately from unlock states.

The achievement engine receives normalized records from an integration and saves them to SQLite.

Each record contains an external ID, source, name, description, image paths and unlock information.

The UI reads the stored records and calculates the game's achievement progress.

### Main improvement

Achievements became part of the game library rather than a feature that existed outside the launcher.

## v0.0.3-alpha — Library Automation and Visual Improvements

### Objective

Reduce manual library maintenance and improve game and achievement presentation.

### Changes represented in the current public code

- Executable icon extraction.
- Local PNG icon cache.
- Steam filesystem monitoring.
- Achievement cache monitoring.
- Local Steam achievement schema parsing.
- Achievement unlock state parsing.
- Achievement image identifiers.
- Optional achievement image downloads.
- Separate locked and unlocked achievement images.
- Automatic update preferences.

### How it works

The icon system identifies a suitable executable and extracts its embedded Windows icon.

The icon is converted to RGBA data and saved in the UGL cache.

The Steam watcher monitors local Steam files and identifies changes to application manifests and achievement cache files.

When achievement data changes, the integration can read the updated local files and pass normalized records to the achievement engine.

Steam achievement images are resolved from their identifiers and downloaded when enabled and necessary.

### Performance improvements

- Cached game icons.
- Cached achievement images.
- Filesystem events instead of relying exclusively on repeated scanning.
- Debounced change notifications.
- Bounded directory scans.
- Database transactions for achievement updates.

### Important limitations

The achievement reader depends on local Steam cache files.

It is not a universal online achievement API, and it does not guarantee compatibility with every game.

The public repository's current code contains these features, but their exact assignment to the `0.0.3-alpha` tag should be checked against the relevant commits.

## v0.0.4-alpha — Smart Launch and Library Improvements

### Objective

Improve the reliability of importing and launching local games.

### Development goals

- Better handling of Windows `.lnk` shortcuts.
- Recovery of original executable paths.
- Preservation of launch arguments.
- Preservation of working directories.
- Direct executable launching when appropriate.
- Platform-based launching when required.
- Manual launch method configuration.
- Multiple executable import.
- Better duplicate detection.
- Better installation path management.
- Improved library filtering and sorting.
- Background tray operation.
- Provisional UGL executable icon.

### Smart Launch behavior

A game may need to be started in different ways.

For example:

```text
Standalone game
    └── game.exe

Steam game
    └── steam://rungameid/<APP_ID>

Shortcut-based game
    └── shortcut properties
         ├── target executable
         ├── arguments
         └── working directory
```

The correct launch method depends on the game.

A Steam game may open Steam even when the user knows the path to its executable. This is expected when the game needs the Steam client or its platform services.

### Multiple executable import

A single directory may contain multiple independent games.

For example:

```text
FNAF/
├── FNAF1.exe
├── FNAF2.exe
├── FNAF3.exe
└── FNAF4.exe
```

The intended behavior is to register each selected executable as a separate game, even if they share the same installation directory.

### Development status

This version is under active development in the local project. Features should be marked as complete only after they have been tested and included in the corresponding source commit.

---

# 9. Testing

The core should have tests for its main behaviors.

Run:

```powershell
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo build --release
```

### Current public test coverage

The repository includes tests for areas such as:

- VDF parsing.
- Steam library discovery.
- Folder scanning.
- Executable candidate ranking.
- Database behavior.
- Game registration.
- Duplicate handling.
- Achievement parsing and storage.

### Recommended integration tests

When modifying the project, test:

1. Importing a valid local game.
2. Importing an invalid or inaccessible directory.
3. Importing a Steam library.
4. Registering the same game twice.
5. Launching a standalone executable.
6. Launching a Steam game.
7. Reading achievement schema files.
8. Reading achievement progress files.
9. Handling missing achievement cache files.
10. Extracting and caching executable icons.
11. Detecting changes to Steam manifests.
12. Preserving the database through migrations.

Do not mark a test as successful unless it was actually executed.

---

# 10. Adding a new integration

New platforms should be implemented as independent providers.

A provider should:

1. Discover its installed games.
2. Extract the platform's external identifiers.
3. Determine installation paths.
4. Determine the correct launch method.
5. Convert discovered games into the shared `DiscoveredGame` representation.
6. Register them through the core registry.

A platform-specific provider should not need to modify the UI's database logic.

### Example future providers

```text
integrations/
├── manual.rs
├── steam.rs
├── epic.rs
├── itch.rs
├── retroarch.rs
└── other_provider.rs
```

These are architectural examples, not a claim that all listed integrations already exist.

---

# 11. Adding a new achievement provider

Achievement providers should return normalized achievement records.

The provider should supply:

- Source name.
- External achievement ID.
- Name.
- Description.
- Hidden status.
- Locked and unlocked image paths.
- Unlock state.
- Unlock timestamp.
- Provenance.

The achievement engine stores the records independently from their source.

This allows future integrations such as:

- Steam.
- RetroAchievements.
- Emulator-specific achievements.
- User-defined local achievements.
- Other supported platforms.

Official and local achievement data must remain distinguishable.

---

# 12. Known limitations and future work

The project is experimental and its architecture is still evolving.

Known limitations and areas requiring future work include:

- Windows is the initial target.
- Some Steam achievement data depends on undocumented local file formats.
- Steam launch requires the Steam client when using its protocol.
- Some games need custom launch arguments or working directories.
- Not every executable contains a usable icon.
- Local achievement files may be unavailable or outdated.
- Storefront account synchronization is not part of the current local-library foundation.
- Universal achievement synchronization remains a future objective.
- The final skeuomorphic interface is still under development.

---

# 13. Contribution guidelines

Before contributing:

1. Read this document.
2. Inspect the relevant core module.
3. Avoid duplicating existing functionality.
4. Preserve database compatibility.
5. Add or update tests.
6. Run the workspace validation commands.
7. Update the README or technical documentation when behavior changes.

Keep the core independent from the UI and keep integrations isolated.

---

# 14. License

UGL is distributed under the MIT License.

See the root `LICENSE` file for details.

Repository:

https://github.com/maes773/Universal-Game-Launcher
