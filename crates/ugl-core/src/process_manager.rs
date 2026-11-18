//! process_manager: inicia juegos. En 0.1 solo lanza; el seguimiento de procesos llegará después.

use std::io;
use std::path::Path;
use std::process::Command;

use crate::diagnostics;
use crate::domain::{Game, GameStatus, LaunchKind};
use crate::error::{Result, UglError};

/// Comprobación barata (solo consulta si existen carpeta y ejecutable). No se hace en bloque al
/// arrancar: se usa al abrir la ficha de un juego y al iniciarlo.
pub fn check_status(game: &Game) -> GameStatus {
    if !game.install_path.is_dir() {
        return GameStatus::MissingFolder;
    }
    if game.launch_kind == LaunchKind::Executable
        && !game.install_path.join(&game.launch_target).is_file()
    {
        return GameStatus::MissingExecutable;
    }
    GameStatus::Ready
}

pub fn launch(game: &Game) -> Result<()> {
    match game.launch_kind {
        LaunchKind::Executable => launch_executable(&game.install_path, &game.launch_target),
        LaunchKind::SteamUri => {
            // Solo dígitos: evita que un valor corrupto llegue al intérprete de comandos.
            let app_id: u64 = game.launch_target.trim().parse().map_err(|_| {
                UglError::Invalid(format!(
                    "Identificador de Steam no válido: {}",
                    game.launch_target
                ))
            })?;
            diagnostics::info(&format!("Iniciando juego de Steam (AppID {app_id})"));
            open_uri(&format!("steam://rungameid/{app_id}"))
        }
    }
}

fn launch_executable(install_path: &Path, relative: &str) -> Result<()> {
    let exe = install_path.join(relative);
    if !exe.is_file() {
        return Err(UglError::NotFound(format!(
            "No se encontró el ejecutable en {}. Puedes elegir otro desde la ficha del juego.",
            exe.display()
        )));
    }
    let working_dir = exe.parent().unwrap_or(install_path);
    diagnostics::info(&format!("Iniciando {}", exe.display()));
    Command::new(&exe)
        .current_dir(working_dir)
        .spawn()
        .map(|_child| ())
        .map_err(|err| describe_spawn_error(&exe, err))
}

fn describe_spawn_error(exe: &Path, err: io::Error) -> UglError {
    // 740 = ERROR_ELEVATION_REQUIRED en Windows.
    if err.raw_os_error() == Some(740) {
        UglError::Invalid(format!(
            "{} requiere permisos de administrador. Ejecuta UGL como administrador o inicia el juego manualmente.",
            exe.display()
        ))
    } else {
        UglError::Invalid(format!("No se pudo iniciar {}: {err}", exe.display()))
    }
}

#[cfg(windows)]
fn open_uri(uri: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("cmd")
        .args(["/C", "start", "", uri])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_child| ())
        .map_err(|err| UglError::Invalid(format!("No se pudo abrir {uri}: {err}")))
}

#[cfg(not(windows))]
fn open_uri(uri: &str) -> Result<()> {
    Command::new("xdg-open")
        .arg(uri)
        .spawn()
        .map(|_child| ())
        .map_err(|err| UglError::Invalid(format!("No se pudo abrir {uri}: {err}")))
}
