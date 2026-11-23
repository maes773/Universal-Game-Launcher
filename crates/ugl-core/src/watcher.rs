//! Vigilancia de las carpetas de Steam mediante eventos del sistema operativo.
//! El hilo duerme bloqueado en un canal: sin sondeo, sin consumo mientras no pasa nada.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::diagnostics;
use crate::error::{Result, UglError};

/// Silencio necesario antes de actuar: Steam escribe sus archivos varias veces seguidas.
const QUIET_PERIOD: Duration = Duration::from_millis(1500);
/// Aunque no haya silencio (descargas largas), se actúa al menos cada tanto.
const MAX_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WatchEvent {
    /// Cambió algún `appmanifest_*.acf` (juego instalado, actualizado o desinstalado).
    LibraryChanged,
    /// Cambió la caché de logros (esquema o progreso) de este AppID.
    AchievementsChanged(u64),
}

/// Traduce la ruta de un archivo modificado al evento que nos interesa (si alguno).
pub fn classify(path: &Path) -> Option<WatchEvent> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.starts_with("appmanifest_") && name.ends_with(".acf") {
        return Some(WatchEvent::LibraryChanged);
    }
    if let Some(rest) = name.strip_prefix("usergamestatsschema_") {
        let app_id = rest.strip_suffix(".bin")?.parse().ok()?;
        return Some(WatchEvent::AchievementsChanged(app_id));
    }
    if let Some(rest) = name.strip_prefix("usergamestats_") {
        // usergamestats_<cuenta>_<appid>.bin
        let app_id = rest
            .strip_suffix(".bin")?
            .rsplit('_')
            .next()?
            .parse()
            .ok()?;
        return Some(WatchEvent::AchievementsChanged(app_id));
    }
    None
}

/// Mientras exista este valor, las carpetas se vigilan. Al soltarlo, se detiene todo.
pub struct SteamWatcher {
    _watcher: RecommendedWatcher,
}

impl SteamWatcher {
    /// `on_event` se ejecuta en un hilo propio, ya sin ráfagas repetidas.
    pub fn start<F>(dirs: &[PathBuf], on_event: F) -> Result<Self>
    where
        F: Fn(WatchEvent) + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    for path in event.paths {
                        let _ = tx.send(path);
                    }
                }
            })
            .map_err(|err| {
                UglError::Invalid(format!("No se pudo iniciar la vigilancia de Steam: {err}"))
            })?;

        let mut watched = 0usize;
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            match watcher.watch(dir, RecursiveMode::NonRecursive) {
                Ok(()) => watched += 1,
                Err(err) => {
                    diagnostics::warn(&format!("No se pudo vigilar {}: {err}", dir.display()))
                }
            }
        }
        if watched == 0 {
            return Err(UglError::Invalid(
                "No hay carpetas de Steam que vigilar.".into(),
            ));
        }
        diagnostics::info(&format!("Vigilando {watched} carpeta(s) de Steam"));

        std::thread::spawn(move || debounce_loop(rx, on_event));
        Ok(Self { _watcher: watcher })
    }
}

fn debounce_loop<F: Fn(WatchEvent)>(rx: Receiver<PathBuf>, on_event: F) {
    // `recv` bloquea sin gastar CPU; termina cuando se suelta el vigilante (canal cerrado).
    while let Ok(first) = rx.recv() {
        let mut pending: HashSet<WatchEvent> = HashSet::new();
        if let Some(event) = classify(&first) {
            pending.insert(event);
        }
        let started = Instant::now();
        loop {
            match rx.recv_timeout(QUIET_PERIOD) {
                Ok(path) => {
                    if let Some(event) = classify(&path) {
                        pending.insert(event);
                    }
                    if started.elapsed() > MAX_WAIT {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        for event in pending {
            on_event(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_steam_files() {
        assert_eq!(
            classify(Path::new("C:\\Steam\\steamapps\\appmanifest_220.acf")),
            Some(WatchEvent::LibraryChanged)
        );
        assert_eq!(
            classify(Path::new("stats/UserGameStats_12345_220.bin")),
            Some(WatchEvent::AchievementsChanged(220))
        );
        assert_eq!(
            classify(Path::new("stats/UserGameStatsSchema_220.bin")),
            Some(WatchEvent::AchievementsChanged(220))
        );
    }

    #[test]
    fn ignores_temporary_and_unrelated_files() {
        assert_eq!(
            classify(Path::new("stats/UserGameStats_12345_220.bin.tmp")),
            None
        );
        assert_eq!(classify(Path::new("steamapps/libraryfolders.vdf")), None);
        assert_eq!(
            classify(Path::new("stats/UserGameStats_12345_abc.bin")),
            None
        );
    }
}
