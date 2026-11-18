// En Windows y en modo release no se abre una consola detrás de la ventana.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use ugl_core::diagnostics;
use ugl_core::integrations::manual::{self, FolderScan};
use ugl_core::integrations::steam;
use ugl_core::{AppPaths, Core, GameId, LaunchKind, SortOrder};

slint::include_modules!();

/// Estado compartido entre el hilo de la interfaz y los hilos de análisis de disco.
struct AppState {
    core: Mutex<Core>,
    search: Mutex<String>,
    /// Resultado del último análisis de carpeta, a la espera de que el usuario confirme.
    pending_scan: Mutex<Option<FolderScan>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
    });

    let ui = MainWindow::new()?;
    ui.set_version_text(env!("CARGO_PKG_VERSION").into());
    ui.set_data_dir(paths.data_dir.display().to_string().into());
    ui.set_db_path(paths.db_path.display().to_string().into());
    ui.set_log_path(paths.log_path.display().to_string().into());
    ui.set_sort_index(lock(&state.core).sort_order().index());

    wire_library(&ui, &state);
    wire_import(&ui, &state);
    wire_detail(&ui, &state);
    refresh_games(&ui, &state);

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
                .map(|game| GameItem {
                    id: game.id as i32,
                    title: game.title.clone().into(),
                    source: source_label(&game.source).into(),
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
    ui.set_detail(GameDetail {
        id: game.id as i32,
        title: game.title.clone().into(),
        source: source_label(&game.source).into(),
        install_path: game.install_path.display().to_string().into(),
        launch_target: launch_text.into(),
        status: status.message().into(),
        can_change_exe,
        can_change_path,
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
