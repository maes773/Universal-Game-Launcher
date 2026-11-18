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
use ugl_core::application::SteamAchievementJob;
use ugl_core::domain::format_date;
use ugl_core::icons;
use ugl_core::integrations::steam;
use ugl_core::integrations::steam_achievements;
use ugl_core::watcher::{SteamWatcher, WatchEvent};
use ugl_core::{AppPaths, Core, Game, GameId, LaunchKind, SortOrder};

slint::include_modules!();

/// Estado compartido entre el hilo de la interfaz y los hilos de análisis de disco.
struct AppState {
    core: Mutex<Core>,
    search: Mutex<String>,
    /// Resultado del último análisis de carpeta, a la espera de que el usuario confirme.
    pending_scan: Mutex<Option<FolderScan>>,
    /// Caché de imágenes de UGL (iconos de juegos y de logros).
    cache_dir: PathBuf,
    /// Evita lanzar dos extractores de iconos a la vez.
    icon_worker_running: AtomicBool,
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
        cache_dir: paths.cache_dir.clone(),
        icon_worker_running: AtomicBool::new(false),
    });

    let ui = MainWindow::new()?;
    ui.set_version_text(env!("CARGO_PKG_VERSION").into());
    ui.set_data_dir(paths.data_dir.display().to_string().into());
    ui.set_db_path(paths.db_path.display().to_string().into());
    ui.set_log_path(paths.log_path.display().to_string().into());
    ui.set_sort_index(lock(&state.core).sort_order().index());
    {
        let core = lock(&state.core);
        ui.set_auto_update(core.auto_update_enabled());
        ui.set_download_icons(core.download_icons_enabled());
    }

    wire_library(&ui, &state);
    wire_import(&ui, &state);
    wire_detail(&ui, &state);
    wire_achievements(&ui, &state);
    wire_settings(&ui, &state);
    refresh_games(&ui, &state);
    restart_automation(&ui, &state);
    startup_sync(ui.as_weak(), state.clone());

    ui.run()?;
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
            let items: Vec<GameItem> = games
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
                    }
                })
                .collect();
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

    let (launch_text, can_change_exe, can_change_path) = match game.launch_kind {
        LaunchKind::Executable => (game.launch_target.clone(), true, true),
        LaunchKind::SteamUri => (format!("Steam (AppID {})", game.launch_target), false, false),
    };
    let image = game_icon(&game);
    ui.set_detail(GameDetail {
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
                    Ok(title) => set_status(&ui, format!("Iniciando «{title}»…")),
                    Err(err) => {
                        diagnostics::warn(&format!("No se pudo iniciar el juego {id}: {err}"));
                        set_status(&ui, format!("No se pudo iniciar el juego: {err}"));
                    }
                }
                refresh_games(&ui, &state);
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
