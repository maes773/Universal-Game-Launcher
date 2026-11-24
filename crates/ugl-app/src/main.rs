// En Windows y en modo release no se abre una consola detrás de la ventana.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use ugl_core::diagnostics;
use ugl_core::integrations::manual::{self, FolderScan};
use ugl_core::achievement_engine::AchievementView;
use ugl_core::application::{LaunchOutcome, SteamAchievementJob};
use ugl_core::domain::{format_date, unix_now, ItchGame, LaunchSettings, MetadataRecord};
use ugl_core::icons;
use ugl_core::integrations::executables::{self, CandidateStatus, ImportCandidate};
use ugl_core::integrations::shortcut;
use ugl_core::integrations::steam;
use ugl_core::integrations::itch;
use ugl_core::integrations::steam_achievements;
use ugl_core::integrations::steam_store::{self, SearchHit};
use ugl_core::process_manager;
use ugl_core::session::{self, SessionEnd, SessionSpec};
use ugl_core::watcher::{SteamWatcher, WatchEvent};
use ugl_core::{AppPaths, Core, Game, GameId, LaunchKind, SortOrder};

mod tray;

slint::include_modules!();

/// Estado compartido entre el hilo de la interfaz y los hilos de análisis de disco.
/// Un archivo de la vista previa de importación, con el nombre que el usuario va editando.
struct PreviewEntry {
    candidate: ImportCandidate,
    name: String,
    include: bool,
}

struct AppState {
    core: Mutex<Core>,
    search: Mutex<String>,
    /// Resultado del último análisis de carpeta, a la espera de que el usuario confirme.
    pending_scan: Mutex<Option<FolderScan>>,
    /// Vista previa de la importación de ejecutables, a la espera de que el usuario confirme.
    pending_preview: Mutex<Vec<PreviewEntry>>,
    /// Caché de imágenes de UGL (iconos de juegos y de logros).
    cache_dir: PathBuf,
    /// Evita lanzar dos extractores de iconos a la vez.
    icon_worker_running: AtomicBool,
    /// Ídem para la descarga de fichas de Steam Store.
    metadata_worker_running: AtomicBool,
    /// Resultados de la última búsqueda en Steam Store (asociación manual).
    link_results: Mutex<Vec<SearchHit>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

thread_local! {
    // Imágenes ya decodificadas (solo en el hilo de la interfaz). Evita releer el disco en cada
    // búsqueda o cambio de filtro. Solo se recuerdan las que existen: una que aparezca después
    // (por ejemplo, tras descargarla) se cargará en el siguiente repintado.
    static IMAGE_CACHE: RefCell<HashMap<PathBuf, slint::Image>> = RefCell::new(HashMap::new());
}

fn cached_image(path: &Path) -> Option<slint::Image> {
    IMAGE_CACHE.with(|cache| {
        if let Some(hit) = cache.borrow().get(path) {
            return Some(hit.clone());
        }
        let loaded = slint::Image::load_from_path(path).ok()?;
        cache
            .borrow_mut()
            .insert(path.to_path_buf(), loaded.clone());
        Some(loaded)
    })
}

fn forget_image(path: &Path) {
    IMAGE_CACHE.with(|cache| {
        cache.borrow_mut().remove(path);
    });
}

fn game_icon(game: &Game) -> Option<slint::Image> {
    game.icon_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .and_then(|path| cached_image(Path::new(path)))
}

fn initial_of(title: &str) -> SharedString {
    title
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
        .into()
}

fn main() {
    if let Err(err) = run() {
        diagnostics::error(&format!("Error fatal al iniciar: {err}"));
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("UGL")
            .set_description(format!("UGL no pudo iniciarse:\n\n{err}"))
            .show();
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::resolve()?;
    paths.ensure_dirs()?;
    if let Err(err) = diagnostics::init(&paths.log_path) {
        eprintln!("No se pudo abrir el registro: {err}");
    }
    std::panic::set_hook(Box::new(|info| {
        diagnostics::error(&format!("panic: {info}"));
    }));
    diagnostics::info(&format!("UGL {} iniciando", env!("CARGO_PKG_VERSION")));

    let core = Core::open(&paths.db_path)?;
    let state = Arc::new(AppState {
        core: Mutex::new(core),
        search: Mutex::new(String::new()),
        pending_scan: Mutex::new(None),
        pending_preview: Mutex::new(Vec::new()),
        cache_dir: paths.cache_dir.clone(),
        icon_worker_running: AtomicBool::new(false),
        metadata_worker_running: AtomicBool::new(false),
        link_results: Mutex::new(Vec::new()),
    });

    let ui = MainWindow::new()?;
    ui.set_version_text(format!("{} alpha", env!("CARGO_PKG_VERSION")).into());
    ui.set_data_dir(paths.data_dir.display().to_string().into());
    ui.set_db_path(paths.db_path.display().to_string().into());
    ui.set_log_path(paths.log_path.display().to_string().into());
    ui.set_sort_index(lock(&state.core).sort_order().index());
    {
        let core = lock(&state.core);
        ui.set_auto_update(core.auto_update_enabled());
        ui.set_download_icons(core.download_icons_enabled());
        ui.set_fetch_metadata(core.fetch_metadata_enabled());
        ui.set_minimize_on_launch(core.minimize_on_launch_enabled());
    }

    wire_library(&ui, &state);
    wire_import(&ui, &state);
    wire_detail(&ui, &state);
    wire_achievements(&ui, &state);
    wire_settings(&ui, &state);
    wire_preview(&ui, &state);
    wire_launch(&ui, &state);
    wire_steam_link(&ui, &state);
    wire_background(&ui, &state);
    wire_account(&ui, &state);
    refresh_games(&ui, &state);
    refresh_account_ui(&ui, &state);
    restart_automation(&ui, &state);
    startup_sync(ui.as_weak(), state.clone());

    // Cerrar la ventana (X) sale de UGL; para dejarlo en segundo plano está el botón de la bandeja.
    ui.window().on_close_requested(|| {
        TRAY.with(|slot| {
            *slot.borrow_mut() = None;
        });
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    MAIN_WINDOW.with(|slot| {
        *slot.borrow_mut() = Some(ui.clone_strong());
    });

    ui.show()?;
    // No `ui.run()`: así el programa sigue activo aunque la ventana esté oculta en la bandeja.
    slint::run_event_loop_until_quit()?;

    TRAY.with(|slot| {
        *slot.borrow_mut() = None;
    });
    MAIN_WINDOW.with(|slot| {
        *slot.borrow_mut() = None;
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Utilidades de interfaz
// ---------------------------------------------------------------------------

fn set_status(ui: &MainWindow, message: impl Into<SharedString>) {
    ui.set_status_text(message.into());
}

fn source_label(source: &str) -> &'static str {
    match source {
        "steam" => "Steam",
        "manual" => "Importado manualmente",
        _ => "Otra fuente",
    }
}

/// «Valve · 16 NOV 2004 · Acción» a partir de la ficha de la tienda.
fn metadata_info_line(record: &MetadataRecord) -> String {
    let developers = record.developers.join(", ");
    let genres = record.genres.join(", ");
    [developers, record.release_date.clone(), genres]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("  ·  ")
}

fn launch_description(game: &Game) -> String {
    match game.launch_kind {
        LaunchKind::Executable => {
            let mut text = format!("Ejecutable directo: {}", game.launch_target);
            if !game.launch_args.trim().is_empty() {
                text.push_str(&format!("  ·  Argumentos: {}", game.launch_args.trim()));
            }
            if let Some(dir) = game.working_dir.as_deref().filter(|d| !d.is_empty()) {
                text.push_str(&format!("  ·  Carpeta de trabajo: {dir}"));
            }
            text
        }
        LaunchKind::SteamUri => format!("Steam (AppID {})", game.launch_target),
        LaunchKind::Uri => format!("Plataforma: {}", game.launch_target),
    }
}

fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_073_741_824.0 {
        format!("{:.1} GB", b / 1_073_741_824.0)
    } else if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn refresh_games(ui: &MainWindow, state: &AppState) {
    let query = lock(&state.search).clone();
    let result = lock(&state.core).list_games(&query);
    match result {
        Ok(games) => {
            let mut items: Vec<GameItem> = games
                .iter()
                .map(|game| {
                    let image = game_icon(game);
                    GameItem {
                        id: game.id as i32,
                        title: game.title.clone().into(),
                        source: source_label(&game.source).into(),
                        ach_total: game.achievements_total as i32,
                        ach_unlocked: game.achievements_unlocked as i32,
                        has_icon: image.is_some(),
                        icon: image.unwrap_or_default(),
                        initial: initial_of(&game.title),
                        owned_only: false,
                    }
                })
                .collect();

            // Con la cuenta de itch.io vinculada: también los juegos de tu biblioteca que no están
            // instalados. En la lista llevan la inicial (sin imagen) para no gastar memoria; su portada se
            // carga al abrirlos.
            let owned = {
                let core = lock(&state.core);
                if core.itch_account().is_some() && core.show_itch_enabled() {
                    core.list_itch_games(&query)
                } else {
                    Vec::new()
                }
            };
            for game in owned {
                items.push(GameItem {
                    id: game.game_id as i32,
                    title: game.title.clone().into(),
                    source: owned_source_line(&game).into(),
                    ach_total: 0,
                    ach_unlocked: 0,
                    icon: Default::default(),
                    has_icon: false,
                    initial: initial_of(&game.title),
                    owned_only: true,
                });
            }
            ui.set_games(ModelRc::new(VecModel::from(items)));
        }
        Err(err) => {
            diagnostics::error(&format!("No se pudo cargar la biblioteca: {err}"));
            set_status(ui, format!("No se pudo cargar la biblioteca: {err}"));
        }
    }
}

fn show_candidates(ui: &MainWindow, scan: &FolderScan, selected: i32) {
    let items: Vec<ExeItem> = scan
        .candidates
        .iter()
        .map(|candidate| ExeItem {
            label: candidate.relative.display().to_string().into(),
            detail: format_size(candidate.size).into(),
            suggested: candidate.suggested,
        })
        .collect();
    ui.set_exe_candidates(ModelRc::new(VecModel::from(items)));
    ui.set_selected_exe(selected);
    ui.set_scan_note(scan.note().unwrap_or_default().into());
}

fn show_detail(ui: &MainWindow, state: &AppState, id: GameId) {
    let loaded = {
        let core = lock(&state.core);
        core.game(id).map(|game| {
            let status = core.game_status(&game);
            (game, status)
        })
    };
    let (game, status) = match loaded {
        Ok(pair) => pair,
        Err(err) => {
            set_status(ui, format!("No se pudo abrir el juego: {err}"));
            return;
        }
    };

    let launch_text = launch_description(&game);
    let (can_change_exe, can_change_path) = match game.launch_kind {
        LaunchKind::Executable => (true, true),
        LaunchKind::SteamUri | LaunchKind::Uri => (false, false),
    };
    let metadata = lock(&state.core).metadata(id).filter(|m| m.available);
    let cover = metadata
        .as_ref()
        .and_then(|m| m.cover_path.as_deref())
        .and_then(|path| cached_image(Path::new(path)));
    let store_name = metadata.as_ref().map(|m| m.name.clone()).unwrap_or_default();
    let can_use_store_name = !store_name.is_empty() && store_name != game.title;
    let image = game_icon(&game);
    ui.set_detail(GameDetail {
        has_cover: cover.is_some(),
        cover: cover.unwrap_or_default(),
        info: metadata.as_ref().map(metadata_info_line).unwrap_or_default().into(),
        description: metadata
            .as_ref()
            .map(|m| m.short_description.clone())
            .unwrap_or_default()
            .into(),
        store_name: store_name.into(),
        can_use_store_name,
        id: game.id as i32,
        has_icon: image.is_some(),
        icon: image.unwrap_or_default(),
        initial: initial_of(&game.title),
        title: game.title.clone().into(),
        source: source_label(&game.source).into(),
        install_path: game.install_path.display().to_string().into(),
        launch_target: launch_text.into(),
        status: status.message().into(),
        can_change_exe,
        can_change_path,
        ach_total: game.achievements_total as i32,
        ach_unlocked: game.achievements_unlocked as i32,
        can_show_achievements: game.launch_kind == LaunchKind::SteamUri
            || game.steam_app_id.is_some()
            || game.achievements_total > 0,
    });
    ui.set_confirm_remove(false);
    ui.set_detail_visible(true);
}

// ---------------------------------------------------------------------------
// Biblioteca: búsqueda, orden, jugar, abrir ficha
// ---------------------------------------------------------------------------

fn wire_library(ui: &MainWindow, state: &Arc<AppState>) {
    ui.on_search_changed({
        let weak = ui.as_weak();
        let state = state.clone();
        move |text| {
            *lock(&state.search) = text.to_string();
            if let Some(ui) = weak.upgrade() {
                refresh_games(&ui, &state);
            }
        }
    });

    ui.on_sort_changed({
        let weak = ui.as_weak();
        let state = state.clone();
        move |index| {
            let order = SortOrder::from_index(index);
            let saved = lock(&state.core).set_sort_order(order);
            if let Err(err) = saved {
                diagnostics::warn(&format!("No se pudo guardar el orden: {err}"));
            }
            if let Some(ui) = weak.upgrade() {
                refresh_games(&ui, &state);
            }
        }
    });

    ui.on_game_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                show_detail(&ui, &state, id as GameId);
            }
        }
    });

    ui.on_play_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let result = lock(&state.core).launch_game(id as GameId);
            if let Some(ui) = weak.upgrade() {
                match result {
                    Ok(outcome) => {
                        set_status(&ui, format!("Iniciando «{}»…", outcome.title));
                        refresh_games(&ui, &state);
                        let minimize = lock(&state.core).minimize_on_launch_enabled();
                        if minimize {
                            begin_background_session(&ui, &state, outcome);
                        }
                    }
                    Err(err) => {
                        diagnostics::warn(&format!("No se pudo iniciar el juego {id}: {err}"));
                        set_status(&ui, format!("No se pudo iniciar el juego: {err}"));
                        refresh_games(&ui, &state);
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Importación (el análisis de disco ocurre en hilos aparte para no bloquear la ventana)
// ---------------------------------------------------------------------------

fn wire_import(ui: &MainWindow, state: &Arc<AppState>) {
    // --- Juego individual: elegir carpeta ---
    ui.on_import_folder_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_busy(true);
                set_status(&ui, "Selecciona la carpeta del juego…");
            }
            let start_dir = lock(&state.core).last_import_dir();
            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Selecciona la carpeta donde está instalado el juego");
                if let Some(dir) = start_dir {
                    dialog = dialog.set_directory(dir);
                }
                let Some(folder) = dialog.pick_folder() else {
                    let _ = weak.upgrade_in_event_loop(|ui| {
                        ui.set_busy(false);
                        ui.set_status_text("".into());
                    });
                    return;
                };
                let _ = weak.upgrade_in_event_loop(|ui| {
                    set_status(&ui, "Analizando la carpeta del juego…");
                });
                let scan = manual::scan_game_folder(&folder);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    on_folder_scanned(&ui, &state, &folder, scan);
                });
            });
        }
    });

    // --- Juego individual: buscar un ejecutable que el análisis no encontró ---
    ui.on_browse_exe_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let start_dir = lock(&state.pending_scan)
                .as_ref()
                .map(|scan| scan.install_dir.clone());
            let Some(start_dir) = start_dir else {
                return;
            };
            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Selecciona el ejecutable del juego")
                    .set_directory(&start_dir)
                    .add_filter("Ejecutables", &["exe"])
                    .pick_file();
                let Some(file) = picked else {
                    return;
                };
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let mut pending = lock(&state.pending_scan);
                    let Some(scan) = pending.as_mut() else {
                        return;
                    };
                    match scan.add_candidate(&file) {
                        Ok(index) => show_candidates(&ui, scan, index as i32),
                        Err(err) => set_status(&ui, format!("No se pudo usar ese archivo: {err}")),
                    }
                });
            });
        }
    });

    // --- Juego individual: confirmar ---
    ui.on_import_confirm({
        let weak = ui.as_weak();
        let state = state.clone();
        move |name, index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let outcome = {
                let pending = lock(&state.pending_scan);
                let Some(scan) = pending.as_ref() else {
                    return;
                };
                let Some(candidate) = usize::try_from(index)
                    .ok()
                    .and_then(|i| scan.candidates.get(i))
                else {
                    return;
                };
                let result = lock(&state.core).import_folder_game(
                    &scan.install_dir,
                    name.as_str(),
                    &candidate.relative,
                );
                result
            };
            match outcome {
                Ok(summary) => {
                    *lock(&state.pending_scan) = None;
                    ui.set_import_dialog_visible(false);
                    ui.set_current_view(0);
                    let message = if summary.added > 0 {
                        "Juego añadido a la biblioteca."
                    } else {
                        "Ese juego ya estaba en la biblioteca."
                    };
                    set_status(&ui, message);
                    refresh_games(&ui, &state);
                    kick_icon_worker(ui.as_weak(), state.clone());
                }
                Err(err) => {
                    // El diálogo sigue abierto para que el usuario pueda corregir.
                    diagnostics::warn(&format!("No se pudo importar el juego: {err}"));
                    set_status(&ui, format!("No se pudo importar: {err}"));
                }
            }
        }
    });

    ui.on_import_cancel({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            *lock(&state.pending_scan) = None;
            if let Some(ui) = weak.upgrade() {
                ui.set_import_dialog_visible(false);
                set_status(&ui, "");
            }
        }
    });

    // --- Carpeta de Steam ---
    ui.on_import_steam_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_busy(true);
                set_status(&ui, "Selecciona tu carpeta de Steam…");
            }
            let start_dir = lock(&state.core).last_import_dir();
            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let mut dialog = rfd::FileDialog::new().set_title("Selecciona tu carpeta de Steam");
                if let Some(dir) = start_dir {
                    dialog = dialog.set_directory(dir);
                }
                let Some(folder) = dialog.pick_folder() else {
                    let _ = weak.upgrade_in_event_loop(|ui| {
                        ui.set_busy(false);
                        ui.set_status_text("".into());
                    });
                    return;
                };
                let _ = weak.upgrade_in_event_loop(|ui| {
                    set_status(&ui, "Analizando la carpeta de Steam…");
                });
                let scan = steam::scan_steam_folder(&folder);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    on_steam_scanned(&ui, &state, &folder, scan);
                });
            });
        }
    });
}

fn on_folder_scanned(
    ui: &MainWindow,
    state: &Arc<AppState>,
    folder: &Path,
    scan: ugl_core::Result<FolderScan>,
) {
    ui.set_busy(false);
    match scan {
        Ok(scan) => {
            lock(&state.core).remember_import_dir(folder);
            ui.set_import_folder(scan.install_dir.display().to_string().into());
            ui.set_import_name(scan.suggested_name.clone().into());
            // Se preselecciona solo si hay una propuesta razonable; si no, el usuario decide.
            let selected = if scan.candidates.first().map_or(false, |c| c.suggested) {
                0
            } else {
                -1
            };
            show_candidates(ui, &scan, selected);
            *lock(&state.pending_scan) = Some(scan);
            ui.set_import_dialog_visible(true);
            set_status(ui, "");
        }
        Err(err) => {
            diagnostics::warn(&format!("No se pudo analizar la carpeta: {err}"));
            set_status(ui, format!("No se pudo analizar la carpeta: {err}"));
        }
    }
}

fn on_steam_scanned(
    ui: &MainWindow,
    state: &Arc<AppState>,
    folder: &Path,
    scan: ugl_core::Result<steam::SteamScan>,
) {
    ui.set_busy(false);
    let scan = match scan {
        Ok(scan) => scan,
        Err(err) => {
            diagnostics::warn(&format!("No se pudo leer la carpeta de Steam: {err}"));
            set_status(ui, format!("No se pudo importar desde Steam: {err}"));
            return;
        }
    };
    for warning in &scan.warnings {
        diagnostics::warn(&format!("Steam: {warning}"));
    }

    let result = lock(&state.core).import_steam(&scan);
    match result {
        Ok(summary) => {
            lock(&state.core).remember_import_dir(folder);
            let mut message = if scan.games.is_empty() {
                "No se encontraron juegos instalados en esa carpeta de Steam.".to_string()
            } else {
                format!(
                    "Steam: {} juego(s) nuevo(s), {} ya estaban en la biblioteca, {} actualizado(s).",
                    summary.added, summary.already_present, summary.updated
                )
            };
            if scan.skipped_not_installed > 0 {
                message.push_str(&format!(
                    " {} sin instalar, omitido(s).",
                    scan.skipped_not_installed
                ));
            }
            if !scan.warnings.is_empty() {
                message.push_str(&format!(
                    " Hubo {} aviso(s); consulta el registro.",
                    scan.warnings.len()
                ));
            }
            set_status(ui, message);
            ui.set_current_view(0);
            refresh_games(ui, state);
            // Ya se conocen las carpetas de Steam: se vigilan y se buscan los iconos.
            restart_automation(ui, state);
            kick_icon_worker(ui.as_weak(), state.clone());
        }
        Err(err) => {
            diagnostics::error(&format!("No se pudo guardar la importación de Steam: {err}"));
            set_status(ui, format!("No se pudo guardar la importación: {err}"));
        }
    }
}

// ---------------------------------------------------------------------------
// Ficha del juego: cambiar ejecutable / carpeta, quitar
// ---------------------------------------------------------------------------

fn wire_detail(ui: &MainWindow, state: &Arc<AppState>) {
    ui.on_detail_close({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_detail_visible(false);
                ui.set_confirm_remove(false);
            }
        }
    });

    ui.on_change_exe_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let id = id as GameId;
            let start_dir = lock(&state.core).game(id).ok().map(|g| g.install_path);
            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Selecciona el ejecutable del juego")
                    .add_filter("Ejecutables", &["exe"]);
                if let Some(dir) = start_dir {
                    dialog = dialog.set_directory(dir);
                }
                let Some(file) = dialog.pick_file() else {
                    return;
                };
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let result = lock(&state.core).set_game_executable(id, &file);
                    match result {
                        Ok(()) => {
                            set_status(&ui, "Ejecutable actualizado.");
                            show_detail(&ui, &state, id);
                            kick_icon_worker(ui.as_weak(), state.clone());
                        }
                        Err(err) => {
                            set_status(&ui, format!("No se pudo cambiar el ejecutable: {err}"))
                        }
                    }
                });
            });
        }
    });

    ui.on_change_path_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let id = id as GameId;
            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let Some(folder) = rfd::FileDialog::new()
                    .set_title("Selecciona la nueva carpeta del juego")
                    .pick_folder()
                else {
                    return;
                };
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let result = lock(&state.core).set_game_install_path(id, &folder);
                    match result {
                        Ok(()) => {
                            set_status(&ui, "Carpeta actualizada.");
                            show_detail(&ui, &state, id);
                        }
                        Err(err) => {
                            set_status(&ui, format!("No se pudo cambiar la carpeta: {err}"))
                        }
                    }
                });
            });
        }
    });

    ui.on_remove_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let result = lock(&state.core).remove_game(id as GameId);
            if let Some(ui) = weak.upgrade() {
                match result {
                    Ok(()) => {
                        ui.set_detail_visible(false);
                        ui.set_confirm_remove(false);
                        set_status(&ui, "Juego quitado de la biblioteca (no se borró ningún archivo).");
                        refresh_games(&ui, &state);
                    }
                    Err(err) => set_status(&ui, format!("No se pudo quitar el juego: {err}")),
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Logros: ventana aparte, estructura tipo Xbox 360 con la paleta de UGL
// ---------------------------------------------------------------------------

thread_local! {
    // La ventana de logros solo existe en el hilo de la interfaz y se reutiliza.
    static ACH_WINDOW: RefCell<Option<AchievementsWindow>> = RefCell::new(None);
}

fn wire_achievements(ui: &MainWindow, state: &Arc<AppState>) {
    ui.on_achievements_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                open_achievements(&ui, &state, id as GameId);
            }
        }
    });
}

fn achievements_window(
    main: &slint::Weak<MainWindow>,
    state: &Arc<AppState>,
) -> Result<AchievementsWindow, slint::PlatformError> {
    ACH_WINDOW.with(|slot| {
        if let Some(existing) = slot.borrow().as_ref() {
            return Ok(existing.clone_strong());
        }
        let win = AchievementsWindow::new()?;
        wire_achievements_window(&win, main, state);
        *slot.borrow_mut() = Some(win.clone_strong());
        Ok(win)
    })
}

fn wire_achievements_window(
    win: &AchievementsWindow,
    main: &slint::Weak<MainWindow>,
    state: &Arc<AppState>,
) {
    win.on_filter_changed({
        let weak = win.as_weak();
        let state = state.clone();
        move |filter| {
            if let Some(win) = weak.upgrade() {
                win.set_filter(filter);
                render_achievements(&win, &state);
            }
        }
    });

    win.on_refresh_clicked({
        let weak = win.as_weak();
        let main = main.clone();
        let state = state.clone();
        move |id| {
            if let Some(win) = weak.upgrade() {
                start_achievement_refresh(&win, &main, &state, id as GameId);
            }
        }
    });

    win.on_close_clicked({
        let weak = win.as_weak();
        move || {
            if let Some(win) = weak.upgrade() {
                let _ = win.hide();
            }
        }
    });
}

fn open_achievements(main_ui: &MainWindow, state: &Arc<AppState>, id: GameId) {
    let main = main_ui.as_weak();
    let win = match achievements_window(&main, state) {
        Ok(win) => win,
        Err(err) => {
            diagnostics::error(&format!("No se pudo crear la ventana de logros: {err}"));
            set_status(main_ui, format!("No se pudo abrir la ventana de logros: {err}"));
            return;
        }
    };
    win.set_game_id(id as i32);
    win.set_filter(0);
    win.set_status_text("".into());
    render_achievements(&win, state);
    if let Err(err) = win.show() {
        set_status(main_ui, format!("No se pudo mostrar la ventana de logros: {err}"));
        return;
    }

    // Primera vez: se intenta leer la caché local de Steam automáticamente.
    let has_any = lock(&state.core)
        .achievements(id)
        .map_or(false, |list| !list.is_empty());
    if !has_any {
        start_achievement_refresh(&win, &main, state, id);
    }
}

fn render_achievements(win: &AchievementsWindow, state: &AppState) {
    let id = win.get_game_id() as GameId;
    let filter = win.get_filter();
    let loaded = {
        let core = lock(&state.core);
        core.game(id)
            .and_then(|game| core.achievements(id).map(|list| (game, list)))
    };
    let (game, list) = match loaded {
        Ok(pair) => pair,
        Err(err) => {
            win.set_status_text(format!("No se pudieron cargar los logros: {err}").into());
            return;
        }
    };

    let total = list.len();
    let unlocked = list.iter().filter(|a| a.unlocked).count();
    win.set_game_title(game.title.into());
    if total == 0 {
        win.set_summary_text("Sin logros cargados".into());
        win.set_percent_text("".into());
        win.set_progress(0.0);
    } else {
        win.set_summary_text(format!("{unlocked} de {total} logros desbloqueados").into());
        win.set_percent_text(format!("{} %", unlocked * 100 / total).into());
        win.set_progress(unlocked as f32 / total as f32);
    }

    let items: Vec<AchievementItem> = list
        .iter()
        .filter(|a| match filter {
            1 => a.unlocked,
            2 => !a.unlocked,
            _ => true,
        })
        .map(achievement_item)
        .collect();
    win.set_items(ModelRc::new(VecModel::from(items)));
}

fn achievement_item(view: &AchievementView) -> AchievementItem {
    let hidden_locked = view.hidden && !view.unlocked;

    // Bloqueado: icono gris si existe; si no, el de color (la interfaz lo atenúa).
    let path = if view.unlocked {
        view.icon.as_ref()
    } else {
        view.icon_gray.as_ref().or(view.icon.as_ref())
    };
    let image = if hidden_locked {
        None
    } else {
        path.and_then(|p| cached_image(p))
    };

    let date_text = if view.unlocked {
        match view.unlocked_at {
            Some(time) => format!("Desbloqueado el {}", format_date(time)),
            None => "Desbloqueado".to_string(),
        }
    } else {
        "Bloqueado".to_string()
    };

    AchievementItem {
        name: if hidden_locked {
            "Logro oculto".into()
        } else {
            view.name.clone().into()
        },
        description: if hidden_locked {
            "Sigue jugando para descubrirlo.".into()
        } else {
            view.description.clone().into()
        },
        date_text: date_text.into(),
        provenance: view.provenance.label().into(),
        has_icon: image.is_some(),
        icon: image.unwrap_or_default(),
        unlocked: view.unlocked,
        hidden: view.hidden,
    }
}

fn start_achievement_refresh(
    win: &AchievementsWindow,
    main: &slint::Weak<MainWindow>,
    state: &Arc<AppState>,
    id: GameId,
) {
    let prepared = lock(&state.core).steam_achievement_job(id);
    let job = match prepared {
        Ok(job) => job,
        Err(err) => {
            win.set_status_text(err.to_string().into());
            return;
        }
    };
    win.set_busy(true);
    win.set_status_text("Leyendo la caché local de Steam…".into());

    let main = main.clone();
    let state = state.clone();
    std::thread::spawn(move || {
        refresh_achievements_blocking(&main, &state, id, &job);
    });
}

/// Avisa a la interfaz de que los logros de un juego cambiaron: repinta la biblioteca (insignias)
/// y, si la ventana de logros muestra ese juego, también la lista.
fn notify_achievements(
    main: &slint::Weak<MainWindow>,
    state: &Arc<AppState>,
    id: GameId,
    message: Option<String>,
    busy: bool,
) {
    let state = state.clone();
    let _ = main.upgrade_in_event_loop(move |ui| {
        refresh_games(&ui, &state);
        ACH_WINDOW.with(|slot| {
            if let Some(win) = slot.borrow().as_ref() {
                if win.get_game_id() as GameId == id {
                    win.set_busy(busy);
                    if let Some(text) = message {
                        win.set_status_text(text.into());
                    }
                    render_achievements(win, &state);
                }
            }
        });
    });
}

/// Lee, guarda y completa con imágenes los logros de un juego de Steam. Bloquea: ejecutar siempre
/// en un hilo aparte (lo hacen el botón «Actualizar» y el vigilante de carpetas).
fn refresh_achievements_blocking(
    main: &slint::Weak<MainWindow>,
    state: &Arc<AppState>,
    id: GameId,
    job: &SteamAchievementJob,
) {
    let allow_download = lock(&state.core).download_icons_enabled();

    let mut set = match steam_achievements::read_local_achievements(&job.steam_root, job.app_id) {
        Ok(set) => set,
        Err(err) => {
            diagnostics::warn(&format!("No se pudieron leer los logros del juego {id}: {err}"));
            notify_achievements(
                main,
                state,
                id,
                Some(format!("No se pudieron leer los logros: {err}")),
                false,
            );
            return;
        }
    };
    for warning in &set.warnings {
        diagnostics::warn(&format!("Logros de Steam: {warning}"));
    }

    // 1) Guardar enseguida, con las imágenes que ya estén en disco.
    let local = steam_achievements::resolve_missing_icons(&mut set, &state.cache_dir, false);
    let saved = lock(&state.core).save_steam_achievements(id, &set);
    if let Err(err) = saved {
        diagnostics::error(&format!("No se pudieron guardar los logros: {err}"));
        notify_achievements(
            main,
            state,
            id,
            Some(format!("No se pudieron guardar los logros: {err}")),
            false,
        );
        return;
    }
    let downloading = allow_download && local.still_missing > 0;
    let mut message = format!(
        "{} logros leídos de la caché local de Steam.",
        set.achievements.len()
    );
    if !set.progress_found {
        message.push_str(
            " No se encontró tu progreso guardado: los logros pueden aparecer como bloqueados.",
        );
    }
    if downloading {
        message.push_str(" Descargando imágenes…");
    }
    notify_achievements(main, state, id, Some(message), downloading);

    // 2) Descargar las imágenes que falten, sin bloquear la interfaz.
    if downloading {
        let report = steam_achievements::resolve_missing_icons(&mut set, &state.cache_dir, true);
        if report.downloaded > 0 {
            let saved = lock(&state.core).save_steam_achievements(id, &set);
            if let Err(err) = saved {
                diagnostics::error(&format!("No se pudieron guardar las imágenes: {err}"));
            }
        }
        let message = if report.failed > 0 {
            format!(
                "{} imágenes descargadas; {} no se pudieron descargar (¿sin conexión?).",
                report.downloaded, report.failed
            )
        } else {
            format!("{} imágenes descargadas.", report.downloaded)
        };
        notify_achievements(main, state, id, Some(message), false);
    }
}

// ---------------------------------------------------------------------------
// Automatización: vigilar Steam, buscar iconos y ponerse al día al arrancar
// ---------------------------------------------------------------------------

thread_local! {
    // El vigilante vive en el hilo de la interfaz; al soltarlo se detiene todo.
    static STEAM_WATCHER: RefCell<Option<SteamWatcher>> = RefCell::new(None);
}

/// (Re)inicia la vigilancia según las preferencias y las carpetas de Steam conocidas.
fn restart_automation(ui: &MainWindow, state: &Arc<AppState>) {
    let (enabled, dirs) = {
        let core = lock(&state.core);
        (core.auto_update_enabled(), core.steam_watch_dirs())
    };
    // Primero se detiene la anterior (si la hay).
    STEAM_WATCHER.with(|slot| {
        *slot.borrow_mut() = None;
    });
    if !enabled || dirs.is_empty() {
        return;
    }

    let weak = ui.as_weak();
    let handler_state = state.clone();
    let started = SteamWatcher::start(&dirs, move |event| {
        handle_watch_event(&weak, &handler_state, event);
    });
    match started {
        Ok(watcher) => STEAM_WATCHER.with(|slot| {
            *slot.borrow_mut() = Some(watcher);
        }),
        Err(err) => diagnostics::warn(&format!("Vigilancia de Steam desactivada: {err}")),
    }
}

/// Se ejecuta en el hilo del vigilante, ya sin ráfagas repetidas.
fn handle_watch_event(weak: &slint::Weak<MainWindow>, state: &Arc<AppState>, event: WatchEvent) {
    match event {
        WatchEvent::LibraryChanged => sync_steam_library(weak, state),
        WatchEvent::AchievementsChanged(app_id) => {
            let prepared = {
                let core = lock(&state.core);
                core.find_steam_game(app_id)
                    .map(|id| (id, core.steam_achievement_job(id)))
            };
            // Solo juegos que están en la biblioteca.
            if let Some((id, Ok(job))) = prepared {
                refresh_achievements_blocking(weak, state, id, &job);
            }
        }
    }
}

/// Vuelve a leer los manifiestos de Steam e importa lo que sea nuevo. Bloquea (hilo aparte).
/// Solo mira archivos `appmanifest`: es una lectura ligera, no un escaneo de los juegos.
fn sync_steam_library(weak: &slint::Weak<MainWindow>, state: &Arc<AppState>) {
    let root = lock(&state.core).steam_root();
    let Some(root) = root else {
        return;
    };
    let scan = match steam::scan_steam_folder(&root) {
        Ok(scan) => scan,
        Err(err) => {
            diagnostics::warn(&format!("No se pudo releer la carpeta de Steam: {err}"));
            return;
        }
    };
    for warning in &scan.warnings {
        diagnostics::warn(&format!("Steam: {warning}"));
    }

    let (before, result, after) = {
        let mut core = lock(&state.core);
        let before = core.steam_watch_dirs();
        let result = core.import_steam(&scan);
        let after = core.steam_watch_dirs();
        (before, result, after)
    };
    let summary = match result {
        Ok(summary) => summary,
        Err(err) => {
            diagnostics::warn(&format!("No se pudo actualizar la biblioteca de Steam: {err}"));
            return;
        }
    };
    let libraries_changed = before != after;
    if summary.added == 0 && summary.updated == 0 && !libraries_changed {
        return;
    }

    let state = state.clone();
    let _ = weak.upgrade_in_event_loop(move |ui| {
        if summary.added > 0 {
            set_status(
                &ui,
                format!(
                    "Steam: {} juego(s) nuevo(s) detectado(s) automáticamente.",
                    summary.added
                ),
            );
        }
        refresh_games(&ui, &state);
        if libraries_changed {
            restart_automation(&ui, &state);
        }
        kick_icon_worker(ui.as_weak(), state.clone());
    });
}

/// Busca en segundo plano el icono de los juegos que aún no lo tienen. Si ya hay un extractor
/// trabajando, no hace nada (ese mismo repasará los juegos nuevos al terminar su tanda).
fn kick_icon_worker(weak: slint::Weak<MainWindow>, state: Arc<AppState>) {
    // Toda alta de juegos pasa por aquí: se aprovecha para pedir también las fichas de la tienda.
    kick_metadata_worker(weak.clone(), state.clone());
    if state.icon_worker_running.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let mut handled: HashSet<GameId> = HashSet::new();
        loop {
            let jobs: Vec<icons::IconJob> = lock(&state.core)
                .icon_jobs(25)
                .into_iter()
                .filter(|job| !handled.contains(&job.game_id))
                .collect();
            if jobs.is_empty() {
                break;
            }

            let mut results: Vec<(GameId, Option<PathBuf>)> = Vec::new();
            for job in &jobs {
                handled.insert(job.game_id);
                let icon = match icons::extract_and_cache(job, &state.cache_dir) {
                    Ok(path) => path,
                    Err(err) => {
                        diagnostics::info(&format!(
                            "Sin icono para el juego {}: {err}",
                            job.game_id
                        ));
                        None
                    }
                };
                results.push((job.game_id, icon));
            }
            {
                let mut core = lock(&state.core);
                for (id, icon) in &results {
                    core.record_icon_result(*id, icon.as_deref());
                }
            }

            let changed: Vec<PathBuf> = results.iter().filter_map(|(_, p)| p.clone()).collect();
            let state_for_ui = state.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                for path in &changed {
                    forget_image(path);
                }
                refresh_games(&ui, &state_for_ui);
            });
        }
        state.icon_worker_running.store(false, Ordering::SeqCst);
    });
}

/// Al arrancar: se ponen al día los juegos y logros que cambiaron con UGL cerrado y se buscan
/// los iconos pendientes. Todo en segundo plano.
fn startup_sync(weak: slint::Weak<MainWindow>, state: Arc<AppState>) {
    std::thread::spawn(move || {
        let auto = lock(&state.core).auto_update_enabled();
        if auto {
            sync_steam_library(&weak, &state);
            let stale = lock(&state.core).stale_achievement_jobs();
            for (id, job) in stale {
                refresh_achievements_blocking(&weak, &state, id, &job);
            }
        }
        kick_icon_worker(weak.clone(), state.clone());
        if auto {
            let state_for_ui = state.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| maybe_auto_sync_account(&ui, &state_for_ui));
        }
    });
}

fn wire_settings(ui: &MainWindow, state: &Arc<AppState>) {
    ui.on_auto_update_toggled({
        let weak = ui.as_weak();
        let state = state.clone();
        move |enabled| {
            let saved = lock(&state.core).set_auto_update_enabled(enabled);
            if let Err(err) = saved {
                diagnostics::warn(&format!("No se pudo guardar el ajuste: {err}"));
            }
            if let Some(ui) = weak.upgrade() {
                restart_automation(&ui, &state);
                set_status(
                    &ui,
                    if enabled {
                        "Actualización automática activada."
                    } else {
                        "Actualización automática desactivada."
                    },
                );
            }
        }
    });

    ui.on_fetch_metadata_toggled({
        let weak = ui.as_weak();
        let state = state.clone();
        move |enabled| {
            let saved = lock(&state.core).set_fetch_metadata_enabled(enabled);
            if let Err(err) = saved {
                diagnostics::warn(&format!("No se pudo guardar el ajuste: {err}"));
            }
            if let Some(ui) = weak.upgrade() {
                if enabled {
                    set_status(&ui, "Se descargarán las fichas públicas de Steam Store.");
                    kick_metadata_worker(ui.as_weak(), state.clone());
                } else {
                    set_status(&ui, "UGL no consultará Steam Store: usará solo las fichas ya guardadas.");
                }
            }
        }
    });

    ui.on_minimize_on_launch_toggled({
        let weak = ui.as_weak();
        let state = state.clone();
        move |enabled| {
            let saved = lock(&state.core).set_minimize_on_launch_enabled(enabled);
            if let Err(err) = saved {
                diagnostics::warn(&format!("No se pudo guardar el ajuste: {err}"));
            }
            if let Some(ui) = weak.upgrade() {
                set_status(
                    &ui,
                    if enabled {
                        "UGL se minimizará a la bandeja al iniciar un juego."
                    } else {
                        "UGL se quedará visible al iniciar un juego."
                    },
                );
            }
        }
    });

    ui.on_download_icons_toggled({
        let weak = ui.as_weak();
        let state = state.clone();
        move |enabled| {
            let saved = lock(&state.core).set_download_icons_enabled(enabled);
            if let Err(err) = saved {
                diagnostics::warn(&format!("No se pudo guardar el ajuste: {err}"));
            }
            if let Some(ui) = weak.upgrade() {
                set_status(
                    &ui,
                    if enabled {
                        "Las imágenes de logros que falten se descargarán de Steam."
                    } else {
                        "UGL no descargará imágenes: usará solo las que ya existan en tu equipo."
                    },
                );
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Importación múltiple de ejecutables, con vista previa
// ---------------------------------------------------------------------------

fn wire_preview(ui: &MainWindow, state: &Arc<AppState>) {
    // Varios .exe (o .lnk/.url) a la vez.
    ui.on_import_exes_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_busy(true);
                set_status(&ui, "Selecciona los juegos…");
            }
            let start_dir = lock(&state.core).last_import_dir();
            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Selecciona uno o varios juegos")
                    .add_filter("Juegos (.exe, .lnk, .url)", &["exe", "lnk", "url"]);
                if let Some(dir) = start_dir {
                    dialog = dialog.set_directory(dir);
                }
                let picked = dialog.pick_files();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    match picked {
                        Some(paths) if !paths.is_empty() => {
                            lock(&state.core).remember_import_dir(&paths[0]);
                            start_preview(&ui, &state, paths);
                        }
                        _ => set_status(&ui, ""),
                    }
                });
            });
        }
    });

    // Accesos directos de una carpeta (así no se pierden sus argumentos).
    ui.on_import_shortcuts_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_busy(true);
                set_status(&ui, "Selecciona la carpeta con los accesos directos…");
            }
            let start_dir = lock(&state.core).last_import_dir();
            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Selecciona la carpeta con los accesos directos");
                if let Some(dir) = start_dir {
                    dialog = dialog.set_directory(dir);
                }
                let paths = dialog
                    .pick_folder()
                    .map(|folder| executables::list_shortcuts(&folder, 500))
                    .unwrap_or_default();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    if paths.is_empty() {
                        set_status(&ui, "No se encontraron accesos directos (.lnk/.url) en esa carpeta.");
                    } else {
                        start_preview(&ui, &state, paths);
                    }
                });
            });
        }
    });

    ui.on_preview_toggled({
        let weak = ui.as_weak();
        let state = state.clone();
        move |index, include| {
            {
                let mut entries = lock(&state.pending_preview);
                if let Some(entry) = usize::try_from(index)
                    .ok()
                    .and_then(|i| entries.get_mut(i))
                {
                    if entry.candidate.status == CandidateStatus::New {
                        entry.include = include;
                    }
                }
            }
            if let Some(ui) = weak.upgrade() {
                render_preview(&ui, &state);
            }
        }
    });

    // Editar un nombre solo cambia el estado interno (no se repinta: así no se pierde el foco).
    ui.on_preview_name_edited({
        let state = state.clone();
        move |index, text| {
            let mut entries = lock(&state.pending_preview);
            if let Some(entry) = usize::try_from(index)
                .ok()
                .and_then(|i| entries.get_mut(i))
            {
                entry.name = text.to_string();
            }
        }
    });

    ui.on_preview_cancel({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            lock(&state.pending_preview).clear();
            if let Some(ui) = weak.upgrade() {
                ui.set_preview_visible(false);
                set_status(&ui, "");
            }
        }
    });

    ui.on_preview_confirm({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let items: Vec<(ImportCandidate, String)> = lock(&state.pending_preview)
                .iter()
                .filter(|e| e.include && e.candidate.status == CandidateStatus::New)
                .map(|e| (e.candidate.clone(), e.name.clone()))
                .collect();
            if items.is_empty() {
                set_status(&ui, "No hay nada seleccionado para importar.");
                return;
            }
            let result = lock(&state.core).import_candidates(&items);
            match result {
                Ok(summary) => {
                    lock(&state.pending_preview).clear();
                    ui.set_preview_visible(false);
                    ui.set_current_view(0);
                    let mut message = format!("{} juego(s) añadido(s) a la biblioteca.", summary.added);
                    if summary.already_present > 0 {
                        message.push_str(&format!(" {} ya estaba(n).", summary.already_present));
                    }
                    set_status(&ui, message);
                    refresh_games(&ui, &state);
                    kick_icon_worker(ui.as_weak(), state.clone());
                }
                Err(err) => {
                    diagnostics::warn(&format!("No se pudo importar la selección: {err}"));
                    set_status(&ui, format!("No se pudo importar: {err}"));
                }
            }
        }
    });
}

fn start_preview(ui: &MainWindow, state: &Arc<AppState>, paths: Vec<PathBuf>) {
    let candidates = lock(&state.core).preview_import(&paths);
    let entries: Vec<PreviewEntry> = candidates
        .into_iter()
        .map(|candidate| PreviewEntry {
            name: candidate.name.clone(),
            include: candidate.status == CandidateStatus::New,
            candidate,
        })
        .collect();
    *lock(&state.pending_preview) = entries;
    render_preview(ui, state);
    ui.set_preview_visible(true);
}

fn render_preview(ui: &MainWindow, state: &AppState) {
    let entries = lock(&state.pending_preview);
    let items: Vec<PreviewItem> = entries
        .iter()
        .map(|entry| {
            let candidate = &entry.candidate;
            let (status, kind) = match candidate.status {
                CandidateStatus::New => ("Nuevo", 0),
                CandidateStatus::Duplicate => ("Duplicado", 1),
                CandidateStatus::Invalid => ("No válido", 2),
            };
            // Para los válidos, el detalle muestra el ejecutable real y los argumentos detectados.
            let note = if !candidate.note.is_empty() {
                candidate.note.clone()
            } else if candidate.kind == LaunchKind::SteamUri {
                "Se iniciará con Steam.".to_string()
            } else {
                let exe = candidate
                    .exe
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                if candidate.args.is_empty() {
                    exe
                } else {
                    format!("{exe}  ·  Argumentos: {}", candidate.args)
                }
            };
            PreviewItem {
                include: entry.include,
                name: entry.name.clone().into(),
                path: candidate.source.display().to_string().into(),
                note: note.into(),
                status: status.into(),
                status_kind: kind,
                selectable: candidate.status == CandidateStatus::New,
            }
        })
        .collect();

    let to_import = entries
        .iter()
        .filter(|e| e.include && e.candidate.status == CandidateStatus::New)
        .count();
    let duplicates = entries
        .iter()
        .filter(|e| e.candidate.status == CandidateStatus::Duplicate)
        .count();
    let invalid = entries
        .iter()
        .filter(|e| e.candidate.status == CandidateStatus::Invalid)
        .count();
    drop(entries);

    ui.set_preview_items(ModelRc::new(VecModel::from(items)));
    ui.set_preview_selected(to_import as i32);
    ui.set_preview_summary(
        format!(
            "{to_import} para importar · {duplicates} duplicado(s) omitido(s) · {invalid} no válido(s). \
             Puedes editar los nombres antes de importar."
        )
        .into(),
    );
}

// ---------------------------------------------------------------------------
// Método de inicio de cada juego
// ---------------------------------------------------------------------------

fn wire_launch(ui: &MainWindow, state: &Arc<AppState>) {
    ui.on_launch_settings_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                open_launch_dialog(&ui, &state, id as GameId);
            }
        }
    });

    ui.on_launch_cancel({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_launch_dialog_visible(false);
            }
        }
    });

    ui.on_launch_browse_exe({
        let weak = ui.as_weak();
        move || {
            let current = weak
                .upgrade()
                .map(|ui| ui.get_launch_exe().to_string())
                .unwrap_or_default();
            let start_dir = Path::new(&current)
                .parent()
                .filter(|dir| dir.is_dir())
                .map(Path::to_path_buf);
            let weak = weak.clone();
            std::thread::spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Selecciona el ejecutable del juego")
                    .add_filter("Ejecutables y accesos directos", &["exe", "lnk"]);
                if let Some(dir) = start_dir {
                    dialog = dialog.set_directory(dir);
                }
                let Some(file) = dialog.pick_file() else {
                    return;
                };
                let _ = weak.upgrade_in_event_loop(move |ui| apply_picked_executable(&ui, &file));
            });
        }
    });

    ui.on_launch_browse_workdir({
        let weak = ui.as_weak();
        move || {
            let weak = weak.clone();
            std::thread::spawn(move || {
                let Some(folder) = rfd::FileDialog::new()
                    .set_title("Selecciona el directorio de trabajo")
                    .pick_folder()
                else {
                    return;
                };
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_launch_workdir(folder.display().to_string().into());
                });
            });
        }
    });

    ui.on_launch_save({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let non_empty = |text: String| -> Option<String> {
                let text = text.trim().to_string();
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            };

            let steam_app_id = match non_empty(ui.get_launch_steam_id().to_string()) {
                None => None,
                Some(text) => match text.parse::<u64>() {
                    Ok(app_id) => Some(app_id),
                    Err(_) => {
                        ui.set_launch_error("El AppID de Steam debe ser un número.".into());
                        return;
                    }
                },
            };
            let settings = LaunchSettings {
                kind: LaunchKind::from_index(ui.get_launch_method()),
                exe: non_empty(ui.get_launch_exe().to_string()).map(PathBuf::from),
                args: ui.get_launch_args().to_string(),
                working_dir: non_empty(ui.get_launch_workdir().to_string()).map(PathBuf::from),
                steam_app_id,
                uri: non_empty(ui.get_launch_uri().to_string()),
            };

            let result = lock(&state.core).update_launch(id as GameId, &settings);
            match result {
                Ok(()) => {
                    ui.set_launch_dialog_visible(false);
                    set_status(&ui, "Método de inicio actualizado.");
                    show_detail(&ui, &state, id as GameId);
                    refresh_games(&ui, &state);
                    kick_icon_worker(ui.as_weak(), state.clone());
                }
                Err(err) => ui.set_launch_error(err.to_string().into()),
            }
        }
    });
}

fn open_launch_dialog(ui: &MainWindow, state: &AppState, id: GameId) {
    let loaded = lock(&state.core).game(id);
    let game = match loaded {
        Ok(game) => game,
        Err(err) => {
            set_status(ui, format!("No se pudo abrir el juego: {err}"));
            return;
        }
    };
    let exe = game
        .exe_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(|path| game.install_path.join(path).display().to_string())
        .unwrap_or_default();
    let uri = if game.launch_kind == LaunchKind::Uri {
        game.launch_target.clone()
    } else {
        String::new()
    };
    let steam_id = game
        .steam_app_id
        .map(|app_id| app_id.to_string())
        .unwrap_or_default();

    ui.set_launch_game_id(id as i32);
    ui.set_launch_game_title(game.title.clone().into());
    ui.set_launch_method(game.launch_kind.index());
    ui.set_launch_exe(exe.into());
    ui.set_launch_args(game.launch_args.clone().into());
    ui.set_launch_workdir(game.working_dir.clone().unwrap_or_default().into());
    ui.set_launch_steam_id(steam_id.into());
    ui.set_launch_uri(uri.into());
    ui.set_launch_error("".into());
    ui.set_launch_dialog_visible(true);
}

/// Si se elige un acceso directo, se usa su ejecutable real, sus argumentos y su directorio de trabajo.
fn apply_picked_executable(ui: &MainWindow, file: &Path) {
    if !shortcut::is_shortcut(file) {
        ui.set_launch_exe(file.display().to_string().into());
        ui.set_launch_error("".into());
        return;
    }
    match shortcut::read_shortcut(file) {
        Ok(info) => match info.target {
            Some(target) => {
                ui.set_launch_exe(target.display().to_string().into());
                if !info.arguments.is_empty() {
                    ui.set_launch_args(info.arguments.into());
                }
                if let Some(dir) = info.working_dir {
                    ui.set_launch_workdir(dir.display().to_string().into());
                }
                ui.set_launch_error("".into());
            }
            None => ui.set_launch_error("El acceso directo no indica un ejecutable.".into()),
        },
        Err(err) => ui.set_launch_error(err.to_string().into()),
    }
}

// ---------------------------------------------------------------------------
// Fichas de Steam Store (guardadas en local) y asociación manual
// ---------------------------------------------------------------------------

/// Descarga en segundo plano la ficha pública de los juegos con AppID que aún no la tienen.
/// Un fallo de red no se guarda: se reintentará la próxima vez.
fn kick_metadata_worker(weak: slint::Weak<MainWindow>, state: Arc<AppState>) {
    if !lock(&state.core).fetch_metadata_enabled() {
        return;
    }
    if state.metadata_worker_running.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let mut handled: HashSet<GameId> = HashSet::new();
        loop {
            let jobs: Vec<_> = lock(&state.core)
                .metadata_jobs(10)
                .into_iter()
                .filter(|job| !handled.contains(&job.game_id))
                .collect();
            if jobs.is_empty() {
                break;
            }

            let mut updated: Vec<GameId> = Vec::new();
            for job in &jobs {
                handled.insert(job.game_id);
                match steam_store::fetch_record(job.app_id, &state.cache_dir) {
                    Ok(record) => {
                        let saved = lock(&state.core).save_metadata(job.game_id, &record);
                        match saved {
                            Ok(()) => updated.push(job.game_id),
                            Err(err) => diagnostics::warn(&format!(
                                "No se pudo guardar la ficha del juego {}: {err}",
                                job.game_id
                            )),
                        }
                    }
                    Err(err) => diagnostics::info(&format!(
                        "Sin ficha de Steam Store para el AppID {} (se reintentará): {err}",
                        job.app_id
                    )),
                }
                // Cortesía con el servidor: una petición cada medio segundo como máximo.
                std::thread::sleep(std::time::Duration::from_millis(500));
            }

            if !updated.is_empty() {
                let state_for_ui = state.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    refresh_games(&ui, &state_for_ui);
                    if ui.get_detail_visible() {
                        let open = ui.get_detail().id as GameId;
                        if updated.contains(&open) {
                            show_detail(&ui, &state_for_ui, open);
                        }
                    }
                });
            }
        }
        state.metadata_worker_running.store(false, Ordering::SeqCst);
    });
}

fn wire_steam_link(ui: &MainWindow, state: &Arc<AppState>) {
    ui.on_steam_link_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = id as GameId;
            let loaded = {
                let core = lock(&state.core);
                core.game(id).map(|game| (game, core.metadata(id)))
            };
            let (game, metadata) = match loaded {
                Ok(pair) => pair,
                Err(err) => {
                    set_status(&ui, format!("No se pudo abrir el juego: {err}"));
                    return;
                }
            };
            lock(&state.link_results).clear();
            ui.set_link_game_id(id as i32);
            ui.set_link_game_title(game.title.clone().into());
            let query = metadata
                .filter(|m| m.available && !m.name.is_empty())
                .map(|m| m.name)
                .unwrap_or_else(|| game.title.clone());
            ui.set_link_query(query.into());
            ui.set_link_results(ModelRc::new(VecModel::from(Vec::<LinkResult>::new())));
            ui.set_link_selected(-1);
            ui.set_link_busy(false);
            ui.set_link_status(
                "Escribe el nombre del juego o su AppID y pulsa Buscar. Consulta la tienda pública de Steam (no necesita Steam instalado)."
                    .into(),
            );
            ui.set_link_dialog_visible(true);
        }
    });

    ui.on_steam_link_search({
        let weak = ui.as_weak();
        let state = state.clone();
        move |query| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let query = query.trim().to_string();
            if query.is_empty() {
                ui.set_link_status("Escribe un nombre o un AppID.".into());
                return;
            }
            ui.set_link_busy(true);
            ui.set_link_status("Buscando en Steam Store…".into());

            let weak = weak.clone();
            let state = state.clone();
            std::thread::spawn(move || {
                let work_dir = state.cache_dir.join("tmp");
                // Un número se trata como AppID; cualquier otra cosa, como nombre a buscar.
                let outcome: Result<Vec<SearchHit>, String> = match query.parse::<u64>() {
                    Ok(app_id) => match steam_store::fetch_info(app_id, &work_dir) {
                        Ok(Some(info)) => Ok(vec![SearchHit {
                            app_id,
                            name: info.name,
                        }]),
                        Ok(None) => Ok(Vec::new()),
                        Err(err) => Err(err.to_string()),
                    },
                    Err(_) => steam_store::search(&query, &work_dir).map_err(|err| err.to_string()),
                };
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_link_busy(false);
                    match outcome {
                        Ok(hits) => {
                            let items: Vec<LinkResult> = hits
                                .iter()
                                .map(|hit| LinkResult {
                                    name: hit.name.clone().into(),
                                    app_id: hit.app_id.to_string().into(),
                                })
                                .collect();
                            ui.set_link_status(
                                if hits.is_empty() {
                                    "No se encontró nada con ese nombre o AppID.".to_string()
                                } else {
                                    format!("{} resultado(s). Elige el correcto y pulsa Asociar.", hits.len())
                                }
                                .into(),
                            );
                            ui.set_link_selected(if hits.len() == 1 { 0 } else { -1 });
                            *lock(&state.link_results) = hits;
                            ui.set_link_results(ModelRc::new(VecModel::from(items)));
                        }
                        Err(message) => ui.set_link_status(
                            format!("No se pudo consultar Steam Store (¿sin conexión?): {message}").into(),
                        ),
                    }
                });
            });
        }
    });

    ui.on_steam_link_confirm({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let hit = usize::try_from(ui.get_link_selected())
                .ok()
                .and_then(|index| lock(&state.link_results).get(index).cloned());
            let Some(hit) = hit else {
                ui.set_link_status("Elige primero un resultado de la lista.".into());
                return;
            };
            let result = lock(&state.core).associate_steam(id as GameId, hit.app_id);
            match result {
                Ok(()) => {
                    ui.set_link_dialog_visible(false);
                    set_status(
                        &ui,
                        format!(
                            "Asociado con «{}» (AppID {}). El método de inicio no ha cambiado.",
                            hit.name, hit.app_id
                        ),
                    );
                    show_detail(&ui, &state, id as GameId);
                    refresh_games(&ui, &state);
                    kick_metadata_worker(ui.as_weak(), state.clone());
                }
                Err(err) => ui.set_link_status(format!("No se pudo asociar: {err}").into()),
            }
        }
    });

    ui.on_steam_link_cancel({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_link_dialog_visible(false);
            }
        }
    });

    ui.on_use_store_name_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let name = lock(&state.core)
                .metadata(id as GameId)
                .map(|m| m.name)
                .filter(|name| !name.is_empty());
            let Some(name) = name else {
                return;
            };
            let result = lock(&state.core).rename_game(id as GameId, &name);
            if let Some(ui) = weak.upgrade() {
                match result {
                    Ok(()) => {
                        set_status(&ui, format!("Nombre actualizado: «{name}»."));
                        show_detail(&ui, &state, id as GameId);
                        refresh_games(&ui, &state);
                    }
                    Err(err) => set_status(&ui, format!("No se pudo cambiar el nombre: {err}")),
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Segundo plano: bandeja del sistema y seguimiento de la sesión de juego
// ---------------------------------------------------------------------------

thread_local! {
    // La ventana principal y la bandeja solo existen en el hilo de la interfaz. Los manejadores de
    // la bandeja se limitan a pedir, con `invoke_from_event_loop`, que se actúe sobre ellas.
    static MAIN_WINDOW: RefCell<Option<MainWindow>> = RefCell::new(None);
    static TRAY: RefCell<Option<tray::Tray>> = RefCell::new(None);
}

fn on_tray_open() {
    let _ = slint::invoke_from_event_loop(|| {
        MAIN_WINDOW.with(|slot| {
            if let Some(ui) = slot.borrow().as_ref() {
                restore_from_tray(ui);
            }
        });
    });
}

fn on_tray_quit() {
    let _ = slint::invoke_from_event_loop(|| {
        TRAY.with(|slot| {
            *slot.borrow_mut() = None;
        });
        let _ = slint::quit_event_loop();
    });
}

/// Oculta UGL y deja su icono en la bandeja. Sin bandeja disponible, lo minimiza normalmente.
fn minimize_to_tray(ui: &MainWindow) -> bool {
    let already = TRAY.with(|slot| slot.borrow().is_some());
    let ready = already || {
        match ui.get_app_icon().to_rgba8() {
            Some(buffer) => match tray::create(
                buffer.as_bytes().to_vec(),
                buffer.width(),
                buffer.height(),
                on_tray_open,
                on_tray_quit,
            ) {
                Ok(handle) => {
                    TRAY.with(|slot| {
                        *slot.borrow_mut() = Some(handle);
                    });
                    true
                }
                Err(err) => {
                    diagnostics::warn(&format!("No se pudo crear el icono de la bandeja: {err}"));
                    false
                }
            },
            None => {
                diagnostics::warn("No se pudo leer el icono de UGL para la bandeja");
                false
            }
        }
    };

    if ready {
        ACH_WINDOW.with(|slot| {
            if let Some(win) = slot.borrow().as_ref() {
                let _ = win.hide();
            }
        });
        let _ = ui.hide();
    } else {
        ui.window().set_minimized(true);
    }
    ready
}

/// Vuelve a mostrar UGL y quita su icono de la bandeja.
fn restore_from_tray(ui: &MainWindow) {
    TRAY.with(|slot| {
        *slot.borrow_mut() = None;
    });
    let _ = ui.show();
    ui.window().set_minimized(false);
}

fn wire_background(ui: &MainWindow, _state: &Arc<AppState>) {
    ui.on_minimize_to_tray_clicked({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                minimize_to_tray(&ui);
            }
        }
    });
}

/// Al iniciar un juego: UGL pasa a segundo plano y, cuando el juego se cierra, vuelve solo.
fn begin_background_session(ui: &MainWindow, state: &Arc<AppState>, outcome: LaunchOutcome) {
    minimize_to_tray(ui);

    let spec = SessionSpec {
        root_pid: outcome.pid,
        watch_dir: outcome.watch_dir,
    };
    let title = outcome.title;
    let weak = ui.as_weak();
    let state = state.clone();
    std::thread::spawn(move || {
        let end = session::wait_for_game(&spec);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            restore_from_tray(&ui);
            refresh_games(&ui, &state);
            let message = match end {
                SessionEnd::Ended => format!("«{title}» se ha cerrado. UGL vuelve a estar activo."),
                SessionEnd::NeverStarted => {
                    format!("No se detectó el proceso de «{title}»; UGL vuelve a mostrarse.")
                }
                SessionEnd::Unsupported => {
                    "No se puede seguir el juego en este sistema; UGL vuelve a mostrarse.".to_string()
                }
            };
            set_status(&ui, message);
        });
    });
}

// ---------------------------------------------------------------------------
// Cuenta de itch.io (opcional): clave de la API y biblioteca
// ---------------------------------------------------------------------------

fn owned_source_line(game: &ItchGame) -> String {
    if game.author.is_empty() {
        "itch.io  ·  No instalado".to_string()
    } else {
        format!("itch.io  ·  No instalado  ·  de {}", game.author)
    }
}

/// Pinta en la vista «Cuenta de itch.io» lo que UGL sabe: cuenta y última sincronización.
fn refresh_account_ui(ui: &MainWindow, state: &AppState) {
    let (account, counts, last_sync, show) = {
        let core = lock(&state.core);
        (
            core.itch_account(),
            core.itch_counts(),
            core.last_itch_sync(),
            core.show_itch_enabled(),
        )
    };
    ui.set_show_owned(show);
    ui.set_account_linked(account.is_some());

    match &account {
        None => {
            ui.set_account_status(
                "No hay ninguna cuenta vinculada. Es opcional: UGL funciona igual sin ella.".into(),
            );
            ui.set_account_sync_status("".into());
        }
        Some(account) => {
            let who = account
                .username
                .clone()
                .unwrap_or_else(|| "tu cuenta de itch.io".to_string());
            ui.set_account_status(
                format!("Cuenta vinculada: {who}. La clave está guardada y cifrada en este equipo.").into(),
            );
            ui.set_account_sync_status(
                match last_sync {
                    Some(time) => format!(
                        "Última sincronización: {}  ·  {} juegos en tu biblioteca, {} sin instalar.",
                        format_date(time),
                        counts.0,
                        counts.1
                    ),
                    None => "Aún no se ha sincronizado la biblioteca.".to_string(),
                }
                .into(),
            );
        }
    }
}

fn wire_account(ui: &MainWindow, state: &Arc<AppState>) {
    // --- Vincular con la clave de la API ---
    ui.on_account_save_key({
        let weak = ui.as_weak();
        let state = state.clone();
        move |text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let result = lock(&state.core).link_itch_account(text.as_str());
            match result {
                Ok(()) => {
                    ui.set_account_key_input("".into());
                    set_status(&ui, "Cuenta vinculada. Sincronizando tu biblioteca…");
                    refresh_account_ui(&ui, &state);
                    refresh_games(&ui, &state);
                    start_account_sync(&ui, &state);
                }
                Err(err) => set_status(&ui, format!("No se pudo vincular la cuenta: {err}")),
            }
        }
    });

    ui.on_account_open_key_page({
        let weak = ui.as_weak();
        move || {
            if let Err(err) = process_manager::open_web_url(itch::API_KEY_PAGE) {
                if let Some(ui) = weak.upgrade() {
                    set_status(&ui, format!("No se pudo abrir el navegador: {err}"));
                }
            }
        }
    });

    ui.on_account_unlink_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            let result = lock(&state.core).unlink_itch_account();
            if let Some(ui) = weak.upgrade() {
                match result {
                    Ok(()) => set_status(
                        &ui,
                        "Cuenta desvinculada. Se borraron la clave y la copia local de tu biblioteca.",
                    ),
                    Err(err) => set_status(&ui, format!("No se pudo desvincular: {err}")),
                }
                refresh_account_ui(&ui, &state);
                refresh_games(&ui, &state);
            }
        }
    });

    ui.on_account_sync_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                start_account_sync(&ui, &state);
            }
        }
    });

    ui.on_show_owned_toggled({
        let weak = ui.as_weak();
        let state = state.clone();
        move |enabled| {
            let saved = lock(&state.core).set_show_itch_enabled(enabled);
            if let Err(err) = saved {
                diagnostics::warn(&format!("No se pudo guardar el ajuste: {err}"));
            }
            if let Some(ui) = weak.upgrade() {
                refresh_games(&ui, &state);
            }
        }
    });

    // --- Juegos de la biblioteca de itch.io que no están instalados ---
    ui.on_owned_clicked({
        let weak = ui.as_weak();
        let state = state.clone();
        move |game_id| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let game_id = game_id as u64;
            let found = lock(&state.core)
                .list_itch_games("")
                .into_iter()
                .find(|game| game.game_id == game_id);
            let Some(game) = found else {
                return;
            };
            ui.set_owned_app_id(game_id as i32);
            ui.set_owned_title(game.title.clone().into());
            ui.set_owned_info(
                if game.author.is_empty() {
                    String::new()
                } else {
                    format!("Autor: {}", game.author)
                }
                .into(),
            );
            ui.set_owned_description(game.description.clone().into());

            let cover_file = itch::cover_file(&state.cache_dir, &game);
            let cover = cover_file
                .as_ref()
                .and_then(|path| slint::Image::load_from_path(path).ok());
            ui.set_owned_has_cover(cover.is_some());
            ui.set_owned_cover(cover.unwrap_or_default());
            ui.set_owned_dialog_visible(true);

            // La portada se descarga solo al abrir el juego (bajo demanda).
            let missing = cover_file.as_ref().map_or(false, |path| !path.is_file());
            if missing && game.cover_url.is_some() {
                let weak = weak.clone();
                let state = state.clone();
                std::thread::spawn(move || {
                    if let Ok(path) = itch::ensure_cover(&game, &state.cache_dir) {
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            if ui.get_owned_dialog_visible() && ui.get_owned_app_id() as u64 == game_id {
                                if let Ok(image) = slint::Image::load_from_path(&path) {
                                    ui.set_owned_cover(image);
                                    ui.set_owned_has_cover(true);
                                }
                            }
                        });
                    }
                });
            }
        }
    });

    ui.on_owned_store({
        let weak = ui.as_weak();
        let state = state.clone();
        move |game_id| {
            let url = lock(&state.core)
                .list_itch_games("")
                .into_iter()
                .find(|game| game.game_id == game_id as u64)
                .map(|game| game.url);
            let Some(url) = url else {
                return;
            };
            if let Err(err) = process_manager::open_web_url(&url) {
                if let Some(ui) = weak.upgrade() {
                    set_status(&ui, format!("No se pudo abrir la página del juego: {err}"));
                }
            }
        }
    });

    ui.on_owned_close({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_owned_dialog_visible(false);
            }
        }
    });
}

/// Trae de itch.io el nombre de usuario y la lista de juegos de la biblioteca, en un hilo aparte.
fn start_account_sync(ui: &MainWindow, state: &Arc<AppState>) {
    let (linked, key) = {
        let core = lock(&state.core);
        (core.itch_account().is_some(), core.itch_api_key())
    };
    let Some(key) = key else {
        set_status(
            ui,
            if linked {
                "No se pudo leer la clave guardada (¿otro usuario de Windows?). Vuelve a vincular la cuenta."
            } else {
                "Primero vincula tu cuenta de itch.io."
            },
        );
        return;
    };
    ui.set_account_busy(true);
    ui.set_account_sync_status("Sincronizando tu biblioteca con itch.io…".into());

    let weak = ui.as_weak();
    let state = state.clone();
    std::thread::spawn(move || {
        let name = itch::fetch_username(&key).ok().flatten();
        let games = itch::fetch_library(&key);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_account_busy(false);
            if let Some(name) = name {
                let _ = lock(&state.core).set_itch_username(&name);
            }
            match games {
                Ok(list) if list.is_empty() => {
                    // No se pisa lo que ya había.
                    refresh_account_ui(&ui, &state);
                    ui.set_account_sync_status(
                        "itch.io no devolvió ningún juego: tu biblioteca puede estar vacía o la clave no tiene acceso."
                            .into(),
                    );
                }
                Ok(list) => {
                    let saved = lock(&state.core).save_itch_games(&list);
                    match saved {
                        Ok(()) => set_status(&ui, format!("Biblioteca de itch.io sincronizada: {} juegos.", list.len())),
                        Err(err) => set_status(&ui, format!("No se pudo guardar la biblioteca: {err}")),
                    }
                    refresh_account_ui(&ui, &state);
                    refresh_games(&ui, &state);
                }
                Err(err) => {
                    refresh_account_ui(&ui, &state);
                    ui.set_account_sync_status(err.to_string().into());
                }
            }
        });
    });
}

/// Al arrancar: si hay cuenta vinculada y la última sincronización tiene más de 12 horas, se repite.
fn maybe_auto_sync_account(ui: &MainWindow, state: &Arc<AppState>) {
    let due = {
        let core = lock(&state.core);
        if core.itch_account().is_some() {
            match core.last_itch_sync() {
                Some(last) => unix_now() - last > 12 * 3600,
                None => true,
            }
        } else {
            false
        }
    };
    if due {
        start_account_sync(ui, state);
    }
}
