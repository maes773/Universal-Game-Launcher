# UGL — Universal Game Launcher

**One library. All your games.**

UGL is an open-source desktop application designed to bring video games from different platforms, stores, emulators, and local installations into one unified library.

The goal is to create a universal game launcher where users can organize, manage, launch, and track their games without being restricted to a single storefront or ecosystem.

Built with **Rust, Slint, and SQLite**, UGL prioritizes performance, modularity, local data ownership, and extensibility.

> UGL is a launcher and library manager, not a digital game store. It does not distribute games, replace storefronts, or bypass platform DRM.

---

## Project vision

Most game launchers are tied to a particular store or ecosystem. UGL aims to provide a single place to manage games regardless of where they come from.

The long-term vision is to support:

- Steam and other digital storefronts.
- Independently installed PC games.
- DRM-free games.
- Games installed from local folders or external drives.
- Emulators and retro game libraries.
- Achievement systems from different platforms and emulators.
- Custom games and user-defined entries.

UGL should be useful both to players with several digital storefronts and to players who manage their games manually.

The launcher should not require every game to belong to an online account. A local executable should be enough to add and launch a compatible game.

### Core principles

**1. Universal library**

Games from different sources should coexist in the same library, with their origin and platform identified.

**2. Local-first architecture**

Game information, library entries, settings, achievement records, and cached images should be stored locally whenever possible.

**3. Platform independence**

UGL should not depend entirely on one storefront. Platform-specific integrations should be modular and optional.

**4. Lightweight performance**

Background tasks should avoid blocking the interface or consuming unnecessary CPU and memory.

**5. User control**

Users should be able to manage game entries, executable paths, launch methods, metadata, and library organization.

**6. Extensibility**

New storefronts, emulators, achievement providers, and library sources should be addable without rewriting the entire application.

---

## Current development status

<badge color="success">Experimental Alpha</badge>

UGL is under active development. Its first development target is Windows.

The current project focuses on locally installed games and the foundations required for a unified library.

### Current capabilities

- Local game library.
- Manual game import through folders.
- Steam library discovery through local Steam files.
- Game registration and persistent storage using SQLite.
- Game launching through executables or Steam URI.
- Duplicate prevention using source identifiers.
- Game search and sorting.
- Per-game achievement records.
- Local Steam achievement data reader.
- Achievement progress and unlock timestamps.
- Achievement icon and image caching.
- Executable icon extraction on Windows.
- Automatic monitoring of selected Steam library files.
- Local settings and diagnostic logs.

Some integrations depend on the game, platform, and availability of local data. See the technical documentation for implementation details and limitations.

### Development scope

The current priority is to improve the management of games already installed on the computer.

The following are future goals, not guaranteed current features:

- Importing owned but uninstalled games from online accounts.
- Full synchronization with multiple storefront accounts.
- A universal achievement synchronization service.
- Editing official platform achievements.
- Cross-platform builds beyond the initial Windows target.
- Advanced playtime tracking and session history.
- The complete physical-game-shelf interface.

---

## Interface and design direction

UGL is intended to use a customizable skeuomorphic interface inspired by physical video game collections.

The visual direction includes:

- Modern games represented by physical-style DVD or Blu-ray cases.
- Retro games represented by cartridges.
- Game artwork, icons, and platform identification.
- Game details presented as a physical game package.
- Achievement screens inspired by console achievement interfaces, particularly Xbox 360.
- A consistent visual identity across the library and game details.

The current interface is an evolving implementation. The final visual design is still part of the roadmap.

---

## Architecture

UGL is organized as a Rust workspace with two main crates:

```text
Universal-Game-Launcher/
│
├── crates/
│   ├── ugl-core/
│   │   └── Core application logic
│   │
│   └── ugl-app/
│       └── Desktop application and UI
│
├── .github/
│   └── workflows/
│
├── Cargo.toml
├── Cargo.lock
├── README.md
├── LICENSE
└── .gitignore
```

### `ugl-core`

Contains the application logic independently from the graphical interface.

Responsibilities include:

- Game library management.
- Database access and migrations.
- Game discovery and registration.
- Platform integrations.
- Achievement management.
- Icon extraction and caching.
- Process launching.
- Settings and diagnostics.
- Filesystem monitoring.

The core exposes a `Core` application interface. The graphical application should communicate with this interface instead of accessing SQLite or platform integrations directly.

### `ugl-app`

Contains the desktop application.

Responsibilities include:

- Main window.
- Slint user interface.
- User interactions.
- File and folder selection.
- Displaying the game library.
- Displaying game information and achievements.
- Communicating with `ugl-core`.

The UI should not implement platform-specific business logic directly.

---

## Technology stack and dependencies

| Technology / crate | Purpose |
|---|---|
| Rust | Main programming language |
| Slint | Desktop graphical interface |
| SQLite | Persistent local database |
| `rusqlite` | Rust interface to SQLite |
| `thiserror` | Structured error definitions |
| `dirs` | Resolving user data directories |
| `notify` | Filesystem event monitoring |
| `rfd` | Native file and folder dialogs |
| `slint-build` | Compiling Slint UI resources |
| `winresource` | Windows executable resources and icon |
| `tempfile` | Temporary directories in tests |

The project also uses Rust standard library functionality for filesystem operations, process management, threading, synchronization, and Windows-specific API calls.

See `Cargo.toml` and `Cargo.lock` for the exact dependency declarations and resolved versions.

### Why these technologies?

**Rust**

Provides memory safety, predictable performance, and a suitable foundation for a desktop application that interacts with the filesystem and external processes.

**Slint**

Provides a declarative UI system designed for lightweight native applications.

**SQLite**

Keeps the library and application data in a local database without requiring a separate database server.

**notify**

Allows UGL to react to filesystem changes instead of continuously scanning monitored directories.

---

## Local data storage

UGL stores user data independently from the directory where the executable is launched.

Default Windows location:

```text
%LOCALAPPDATA%\UGL\
│
├── ugl.db
│
├── logs/
│   └── ugl.log
│
└── cache/
    ├── icons/
    └── achievements/
```

The database stores structured information such as:

- Games.
- Platforms.
- Installations.
- Game sources.
- Settings.
- Achievements.
- Unlock states.

The cache stores images and other generated or downloaded resources.

A custom data directory can be configured with:

```powershell
$env:UGL_DATA_DIR = "D:\UGL-Data"
```

This is useful for development, testing, or portable setups.

Removing a game from the UGL library does not delete its original installation files.

---

## Building from source

### Requirements

- Windows 10 or later.
- Rust stable.
- Cargo.
- Visual Studio C++ Build Tools.
- Git.

Install Rust through:

https://rustup.rs/

### Clone the repository

```powershell
git clone https://github.com/maes773/Universal-Game-Launcher.git
cd Universal-Game-Launcher
```

### Check the workspace

```powershell
cargo check --workspace
```

### Run tests

```powershell
cargo test --workspace
```

### Run the application

```powershell
cargo run -p ugl-app
```

### Build the release executable

```powershell
cargo build --release -p ugl-app
```

The executable is generated at:

```text
target/release/ugl.exe
```

The exact output can depend on the binary target defined in `crates/ugl-app/Cargo.toml`.

---

## Development guidelines

Contributions should follow the existing architecture.

- Keep platform integrations inside `ugl-core`.
- Keep UI code inside `ugl-app`.
- Avoid direct database access from Slint components.
- Use database migrations for schema changes.
- Preserve existing user data.
- Avoid blocking the UI thread with filesystem or network operations.
- Handle missing files and inaccessible directories gracefully.
- Keep optional integrations independent.
- Add tests for new core functionality.
- Update the technical documentation when adding a new module or dependency.

### Recommended validation

```powershell
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo build --release
```

---

## Roadmap

UGL is being developed incrementally.

### Library foundation
- [x] Rust workspace.
- [x] Slint desktop interface.
- [x] SQLite persistence.
- [x] Manual game import.
- [x] Local Steam library discovery.
- [x] Basic game launching.

### Achievement system
- [x] Achievement database.
- [x] Per-game achievement display.
- [x] Unlock status and timestamps.
- [x] Local achievement data reader.
- [x] Achievement image caching.

### Library improvements
- [x] Executable icon extraction.
- [x] Steam library monitoring.
- [ ] More flexible launch configuration.
- [ ] Advanced multi-executable import.
- [ ] Better detection of moved installations.
- [ ] More storefront and emulator integrations.

### Long-term goals
- [ ] Unified achievement providers.
- [ ] Playtime tracking.
- [ ] More extensive metadata integrations.
- [ ] Background tray operation.
- [ ] Skeuomorphic physical-library interface.
- [ ] Additional operating system support.

Checkboxes describe the implementation status of the public development snapshot and should be updated as the project evolves.

---

## License

UGL is distributed under the MIT License.

See [LICENSE](LICENSE) for details.

## Repository

https://github.com/maes773/Universal-Game-Launcher

**UGL — One library. All your games.**
