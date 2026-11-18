//! Application/Core: la única puerta de entrada para la interfaz.
//! Coordina base de datos, registro, integraciones y lanzamiento de juegos.

use std::path::{Path, PathBuf};

use crate::database::Database;
use crate::diagnostics;
use crate::domain::{
    path_key, DiscoveredGame, Game, GameId, GameStatus, LaunchKind, RegisterSummary, SortOrder,
};
use crate::error::{Result, UglError};
use crate::integrations::manual::{self, PROVIDER_MANUAL};
use crate::integrations::steam::SteamScan;
use crate::{process_manager, registry, settings};

pub struct Core {
    db: Database,
    sort_order: SortOrder,
}

impl Core {
    pub fn open(db_path: &Path) -> Result<Self> {
        Self::from_database(Database::open(db_path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_database(Database::open_in_memory()?)
    }

    fn from_database(db: Database) -> Result<Self> {
        let sort_order = settings::get(db.conn(), settings::KEY_SORT_ORDER)?
            .and_then(|value| SortOrder::parse(&value))
            .unwrap_or_default();
        Ok(Self { db, sort_order })
    }

    // ---- Biblioteca ----

    pub fn sort_order(&self) -> SortOrder {
        self.sort_order
    }

    pub fn set_sort_order(&mut self, order: SortOrder) -> Result<()> {
        settings::set(self.db.conn(), settings::KEY_SORT_ORDER, order.as_str())?;
        self.sort_order = order;
        Ok(())
    }

    pub fn list_games(&self, query: &str) -> Result<Vec<Game>> {
        registry::list_games(self.db.conn(), query, self.sort_order)
    }

    pub fn game(&self, id: GameId) -> Result<Game> {
        registry::get_game(self.db.conn(), id)
    }

    pub fn game_status(&self, game: &Game) -> GameStatus {
        process_manager::check_status(game)
    }

    pub fn remove_game(&mut self, id: GameId) -> Result<()> {
        registry::remove_game(self.db.conn(), id)?;
        diagnostics::info(&format!("Juego {id} quitado de la biblioteca"));
        Ok(())
    }

    // ---- Importación ----

    /// Importa un juego individual a partir de la carpeta analizada y el ejecutable elegido
    /// (ruta relativa a la carpeta). No modifica nada en disco.
    pub fn import_folder_game(
        &mut self,
        install_dir: &Path,
        title: &str,
        exe_relative: &Path,
    ) -> Result<RegisterSummary> {
        let title = title.trim();
        if title.is_empty() {
            return Err(UglError::Invalid(
                "El nombre del juego no puede estar vacío.".into(),
            ));
        }
        manual::validate_game_dir(install_dir)?;
        let exe = install_dir.join(exe_relative);
        if !exe.is_file() {
            return Err(UglError::NotFound(format!("el ejecutable {}", exe.display())));
        }

        let game = DiscoveredGame {
            provider: PROVIDER_MANUAL.to_string(),
            external_id: path_key(install_dir),
            title: title.to_string(),
            platform_slug: "pc".to_string(),
            install_path: install_dir.to_path_buf(),
            launch_kind: LaunchKind::Executable,
            launch_target: exe_relative.to_string_lossy().into_owned(),
        };
        let summary = registry::register(self.db.conn_mut(), &[game])?;
        diagnostics::info(&format!(
            "Importación manual de «{title}»: {} nuevo(s), {} ya existente(s)",
            summary.added, summary.already_present
        ));
        Ok(summary)
    }

    pub fn import_steam(&mut self, scan: &SteamScan) -> Result<RegisterSummary> {
        let summary = registry::register(self.db.conn_mut(), &scan.to_discovered())?;
        diagnostics::info(&format!(
            "Importación de Steam: {} nuevo(s), {} ya existente(s), {} actualizado(s)",
            summary.added, summary.already_present, summary.updated
        ));
        Ok(summary)
    }

    pub fn last_import_dir(&self) -> Option<PathBuf> {
        settings::get(self.db.conn(), settings::KEY_LAST_IMPORT_DIR)
            .ok()
            .flatten()
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
    }

    /// Recuerda la carpeta que contiene la elegida, para abrir el selector ahí la próxima vez.
    pub fn remember_import_dir(&mut self, picked: &Path) {
        let dir = picked.parent().unwrap_or(picked);
        if let Err(err) = settings::set(
            self.db.conn(),
            settings::KEY_LAST_IMPORT_DIR,
            &dir.to_string_lossy(),
        ) {
            diagnostics::warn(&format!("No se pudo guardar la última carpeta: {err}"));
        }
    }

    // ---- Ejecución y mantenimiento ----

    /// Inicia el juego. Devuelve su título para mostrarlo en la interfaz.
    pub fn launch_game(&mut self, id: GameId) -> Result<String> {
        let game = registry::get_game(self.db.conn(), id)?;
        process_manager::launch(&game)?;
        if let Err(err) = registry::touch_last_played(self.db.conn(), id) {
            diagnostics::warn(&format!("No se pudo anotar la última partida: {err}"));
        }
        Ok(game.title)
    }

    /// Cambia el ejecutable asociado (debe estar dentro de la carpeta de instalación).
    pub fn set_game_executable(&mut self, id: GameId, exe: &Path) -> Result<()> {
        let game = registry::get_game(self.db.conn(), id)?;
        if game.launch_kind != LaunchKind::Executable {
            return Err(UglError::Invalid(
                "Este juego se inicia a través de Steam y no tiene un ejecutable propio.".into(),
            ));
        }
        if !exe.is_file() {
            return Err(UglError::NotFound(format!("el archivo {}", exe.display())));
        }
        let relative = exe.strip_prefix(&game.install_path).map_err(|_| {
            UglError::Invalid(
                "El ejecutable debe estar dentro de la carpeta de instalación del juego.".into(),
            )
        })?;
        registry::set_executable(self.db.conn(), id, &relative.to_string_lossy())
    }

    /// Actualiza la carpeta de instalación (por ejemplo, si el usuario movió el juego). El ejecutable
    /// se guarda como ruta relativa, así que sigue valiendo si la estructura interna no cambió.
    pub fn set_game_install_path(&mut self, id: GameId, new_dir: &Path) -> Result<()> {
        let game = registry::get_game(self.db.conn(), id)?;
        if game.launch_kind != LaunchKind::Executable {
            return Err(UglError::Invalid(
                "La ubicación de los juegos de Steam la gestiona Steam; vuelve a importar tu carpeta de Steam."
                    .into(),
            ));
        }
        manual::validate_game_dir(new_dir)?;
        registry::set_install_path(self.db.conn(), id, &new_dir.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn make_game_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin").join("juego.exe"), b"MZ").unwrap();
        fs::write(dir.join("bin").join("otro.exe"), b"MZ").unwrap();
        dir
    }

    #[test]
    fn import_dedupes_and_survives_moving_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Mi Juego");
        let mut core = Core::open_in_memory().unwrap();

        let first = core
            .import_folder_game(&dir, "Mi Juego", Path::new("bin/juego.exe"))
            .unwrap();
        assert_eq!(first.added, 1);
        let again = core
            .import_folder_game(&dir, "Mi Juego (otra vez)", Path::new("bin/juego.exe"))
            .unwrap();
        assert_eq!(again.already_present, 1);

        let game = core.list_games("").unwrap().remove(0);
        assert_eq!(game.title, "Mi Juego");
        assert_eq!(core.game_status(&game), GameStatus::Ready);

        // El usuario mueve la carpeta: UGL lo detecta y permite corregir la ruta.
        let moved = tmp.path().join("Movido");
        fs::rename(&dir, &moved).unwrap();
        let game = core.game(game.id).unwrap();
        assert_eq!(core.game_status(&game), GameStatus::MissingFolder);

        core.set_game_install_path(game.id, &moved).unwrap();
        let game = core.game(game.id).unwrap();
        assert_eq!(core.game_status(&game), GameStatus::Ready);
    }

    #[test]
    fn can_change_executable_only_inside_the_install_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let outside = tmp.path().join("fuera.exe");
        fs::write(&outside, b"MZ").unwrap();

        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();
        let id = core.list_games("").unwrap()[0].id;

        core.set_game_executable(id, &dir.join("bin").join("otro.exe"))
            .unwrap();
        let game = core.game(id).unwrap();
        assert_eq!(PathBuf::from(&game.launch_target), Path::new("bin").join("otro.exe"));

        assert!(core.set_game_executable(id, &outside).is_err());
    }

    #[test]
    fn rejects_empty_title_and_missing_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let mut core = Core::open_in_memory().unwrap();
        assert!(core
            .import_folder_game(&dir, "   ", Path::new("bin/juego.exe"))
            .is_err());
        assert!(core
            .import_folder_game(&dir, "Juego", Path::new("bin/no_existe.exe"))
            .is_err());
    }

    #[test]
    fn sort_order_is_persisted_in_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("ugl.db");
        {
            let mut core = Core::open(&db_path).unwrap();
            core.set_sort_order(SortOrder::RecentlyAdded).unwrap();
        }
        let core = Core::open(&db_path).unwrap();
        assert_eq!(core.sort_order(), SortOrder::RecentlyAdded);
    }
}
