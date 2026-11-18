//! Application/Core: la única puerta de entrada para la interfaz.
//! Coordina base de datos, registro, integraciones y lanzamiento de juegos.

use std::path::{Path, PathBuf};

use crate::achievement_engine::{self, AchievementRecord, AchievementView, Provenance};
use crate::database::Database;
use crate::diagnostics;
use crate::domain::{
    path_key, DiscoveredGame, Game, GameId, GameStatus, LaunchKind, RegisterSummary, SortOrder,
};
use crate::error::{Result, UglError};
use crate::integrations::manual::{self, PROVIDER_MANUAL};
use crate::integrations::steam::SteamScan;
use crate::icons::IconJob;
use crate::integrations::steam::PROVIDER_STEAM;
use crate::integrations::steam_achievements::{
    self, SteamAchievementSet, SOURCE_STEAM_LOCAL,
};
use crate::{process_manager, registry, settings};

/// Lo necesario para leer los logros de un juego de Steam fuera del hilo de la interfaz.
#[derive(Debug, Clone)]
pub struct SteamAchievementJob {
    pub steam_root: PathBuf,
    pub app_id: u64,
}

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
        self.remember_steam_root(&scan.steam_root, &scan.libraries);
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

    /// Guarda la carpeta principal de Steam, necesaria para leer su caché de logros.
    /// Se ignora si la carpeta elegida es solo una biblioteca adicional (no tiene `appcache`).
    fn remember_steam_root(&mut self, root: &Path, libraries: &[PathBuf]) {
        if !root.join("appcache").is_dir() {
            return;
        }
        let joined = libraries
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        let saved = settings::set(self.db.conn(), settings::KEY_STEAM_ROOT, &root.to_string_lossy())
            .and_then(|_| settings::set(self.db.conn(), settings::KEY_STEAM_LIBRARIES, &joined));
        if let Err(err) = saved {
            diagnostics::warn(&format!("No se pudo guardar la carpeta de Steam: {err}"));
        }
    }

    /// Carpeta principal de Steam recordada (la que contiene `appcache`).
    pub fn steam_root(&self) -> Option<PathBuf> {
        settings::get(self.db.conn(), settings::KEY_STEAM_ROOT)
            .ok()
            .flatten()
            .map(PathBuf::from)
            .filter(|path| path.join("appcache").is_dir())
    }

    fn steam_libraries(&self) -> Vec<PathBuf> {
        settings::get(self.db.conn(), settings::KEY_STEAM_LIBRARIES)
            .ok()
            .flatten()
            .map(|text| {
                text.lines()
                    .filter(|line| !line.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Carpetas que conviene vigilar: la caché de logros y la carpeta `steamapps` de cada biblioteca.
    pub fn steam_watch_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(root) = self.steam_root() {
            dirs.push(root.join("appcache").join("stats"));
        }
        for library in self.steam_libraries() {
            dirs.push(library.join("steamapps"));
        }
        dirs.retain(|dir| dir.is_dir());
        dirs.sort();
        dirs.dedup();
        dirs
    }

    pub fn find_steam_game(&self, app_id: u64) -> Option<GameId> {
        registry::find_game_by_source(self.db.conn(), PROVIDER_STEAM, &app_id.to_string())
            .ok()
            .flatten()
    }

    /// Juegos de Steam con logros cuyo progreso cambió en Steam desde la última sincronización
    /// (por ejemplo, logros ganados mientras UGL estaba cerrado).
    pub fn stale_achievement_jobs(&self) -> Vec<(GameId, SteamAchievementJob)> {
        let Some(root) = self.steam_root() else {
            return Vec::new();
        };
        let games =
            registry::list_games(self.db.conn(), "", SortOrder::Title).unwrap_or_default();
        games
            .into_iter()
            .filter(|g| g.launch_kind == LaunchKind::SteamUri && g.achievements_total > 0)
            .filter_map(|game| {
                let app_id: u64 = game.launch_target.trim().parse().ok()?;
                let modified = steam_achievements::user_stats_modified(&root, app_id)?;
                let synced = achievement_engine::synced_at(self.db.conn(), game.id)
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                (modified > synced).then(|| {
                    (
                        game.id,
                        SteamAchievementJob {
                            steam_root: root.clone(),
                            app_id,
                        },
                    )
                })
            })
            .collect()
    }

    // ---- Iconos de juegos ----

    /// Juegos a los que todavía no se les ha buscado el icono.
    pub fn icon_jobs(&self, limit: usize) -> Vec<IconJob> {
        registry::games_without_icon(self.db.conn(), limit)
            .unwrap_or_default()
            .into_iter()
            .map(|game| IconJob {
                game_id: game.id,
                install_path: game.install_path,
                launch_kind: game.launch_kind,
                launch_target: game.launch_target,
            })
            .collect()
    }

    /// Anota el resultado: `Some(ruta)` si hay icono, `None` si se intentó y no lo hay.
    pub fn record_icon_result(&mut self, id: GameId, icon: Option<&Path>) {
        let value = icon
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Err(err) = registry::set_icon_path(self.db.conn(), id, &value) {
            diagnostics::warn(&format!("No se pudo guardar el icono del juego {id}: {err}"));
        }
    }

    // ---- Preferencias de automatización ----

    fn flag(&self, key: &str, default: bool) -> bool {
        settings::get(self.db.conn(), key)
            .ok()
            .flatten()
            .map(|value| value == "1")
            .unwrap_or(default)
    }

    /// Vigilar Steam y actualizar juegos y logros automáticamente (activado por defecto).
    pub fn auto_update_enabled(&self) -> bool {
        self.flag(settings::KEY_AUTO_UPDATE, true)
    }

    pub fn set_auto_update_enabled(&mut self, enabled: bool) -> Result<()> {
        settings::set(
            self.db.conn(),
            settings::KEY_AUTO_UPDATE,
            if enabled { "1" } else { "0" },
        )
    }

    /// Descargar de los servidores de Steam las imágenes de logros que no estén en disco.
    pub fn download_icons_enabled(&self) -> bool {
        self.flag(settings::KEY_DOWNLOAD_ICONS, true)
    }

    pub fn set_download_icons_enabled(&mut self, enabled: bool) -> Result<()> {
        settings::set(
            self.db.conn(),
            settings::KEY_DOWNLOAD_ICONS,
            if enabled { "1" } else { "0" },
        )
    }

    // ---- Logros ----

    pub fn achievements(&self, id: GameId) -> Result<Vec<AchievementView>> {
        achievement_engine::list(self.db.conn(), id)
    }

    /// Prepara la lectura de logros de un juego de Steam desde la caché local del cliente.
    pub fn steam_achievement_job(&self, id: GameId) -> Result<SteamAchievementJob> {
        let game = registry::get_game(self.db.conn(), id)?;
        if game.launch_kind != LaunchKind::SteamUri {
            return Err(UglError::Invalid(
                "Por ahora UGL solo puede leer logros de juegos de Steam.".into(),
            ));
        }
        let app_id: u64 = game.launch_target.trim().parse().map_err(|_| {
            UglError::Invalid(format!(
                "Identificador de Steam no válido: {}",
                game.launch_target
            ))
        })?;
        let steam_root = settings::get(self.db.conn(), settings::KEY_STEAM_ROOT)?
            .map(PathBuf::from)
            .filter(|path| path.join("appcache").is_dir())
            .ok_or_else(|| {
                UglError::Invalid(
                    "UGL no conoce la carpeta principal de Steam. Vuelve a importar tu carpeta de \
                     Steam eligiendo la carpeta raíz (no una biblioteca adicional)."
                        .into(),
                )
            })?;
        Ok(SteamAchievementJob { steam_root, app_id })
    }

    /// Guarda los logros leídos de la caché de Steam. Quedan marcados como «importados»:
    /// UGL no los puede verificar oficialmente.
    pub fn save_steam_achievements(
        &mut self,
        id: GameId,
        set: &SteamAchievementSet,
    ) -> Result<usize> {
        let records: Vec<AchievementRecord> = set
            .achievements
            .iter()
            .map(|a| AchievementRecord {
                external_id: a.api_name.clone(),
                name: a.name.clone(),
                description: a.description.clone(),
                hidden: a.hidden,
                icon_path: a.icon.as_ref().map(|p| p.to_string_lossy().into_owned()),
                icon_gray_path: a
                    .icon_gray
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                unlocked: a.unlocked,
                unlocked_at: a.unlocked_at,
            })
            .collect();
        let count = achievement_engine::store(
            self.db.conn_mut(),
            id,
            SOURCE_STEAM_LOCAL,
            Provenance::Imported,
            &records,
        )?;
        diagnostics::info(&format!(
            "Logros de Steam guardados para el juego {id}: {count}"
        ));
        Ok(count)
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
        registry::set_executable(self.db.conn(), id, &relative.to_string_lossy())?;
        // El icono sale del ejecutable: se vuelve a extraer.
        registry::clear_icon(self.db.conn(), id)?;
        Ok(())
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
    fn steam_achievements_flow_from_fake_cache_to_database() {
        use crate::integrations::binary_kv::test_util::*;
        use crate::integrations::steam::scan_steam_folder;
        use crate::integrations::steam_achievements::read_local_achievements;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let apps = root.join("steamapps");
        fs::create_dir_all(apps.join("common").join("Half-Life 2")).unwrap();
        fs::write(
            apps.join("appmanifest_220.acf"),
            "\"AppState\" { \"appid\" \"220\" \"name\" \"Half-Life 2\" \"installdir\" \"Half-Life 2\" }",
        )
        .unwrap();
        let stats_dir = root.join("appcache").join("stats");
        fs::create_dir_all(&stats_dir).unwrap();
        let bit = obj(
            "0",
            cat(&[
                st("name", "ACH_ONE"),
                obj("display", obj("name", st("english", "First"))),
            ]),
        );
        let schema = document(obj(
            "220",
            obj("stats", obj("1", obj("bits", bit))),
        ));
        fs::write(stats_dir.join("UserGameStatsSchema_220.bin"), schema).unwrap();
        let progress = document(obj(
            "cache",
            obj("1", cat(&[int("data", 1), obj("AchievementTimes", int("0", 1_700_000_000))])),
        ));
        fs::write(stats_dir.join("UserGameStats_1_220.bin"), progress).unwrap();

        let mut core = Core::open_in_memory().unwrap();
        let scan = scan_steam_folder(&root).unwrap();
        core.import_steam(&scan).unwrap();
        let id = core.list_games("").unwrap()[0].id;

        assert_eq!(core.find_steam_game(220), Some(id));
        assert_eq!(core.find_steam_game(999), None);

        let job = core.steam_achievement_job(id).unwrap();
        assert_eq!(job.app_id, 220);
        let set = read_local_achievements(&job.steam_root, job.app_id).unwrap();
        core.save_steam_achievements(id, &set).unwrap();

        let list = core.achievements(id).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].unlocked);
        assert_eq!(list[0].provenance, Provenance::Imported);
        let game = core.game(id).unwrap();
        assert_eq!((game.achievements_unlocked, game.achievements_total), (1, 1));
    }

    #[test]
    fn manual_games_have_no_steam_achievement_job() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();
        let id = core.list_games("").unwrap()[0].id;
        assert!(core.steam_achievement_job(id).is_err());
    }

    #[test]
    fn new_games_wait_for_an_icon_until_a_result_is_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();

        let jobs = core.icon_jobs(10);
        assert_eq!(jobs.len(), 1);
        core.record_icon_result(jobs[0].game_id, None);
        assert!(core.icon_jobs(10).is_empty());

        // Cambiar el ejecutable obliga a extraer el icono otra vez.
        core.set_game_executable(jobs[0].game_id, &dir.join("bin").join("otro.exe"))
            .unwrap();
        assert_eq!(core.icon_jobs(10).len(), 1);
    }

    #[test]
    fn automation_defaults_to_on_and_can_be_switched_off() {
        let mut core = Core::open_in_memory().unwrap();
        assert!(core.auto_update_enabled());
        assert!(core.download_icons_enabled());
        core.set_auto_update_enabled(false).unwrap();
        core.set_download_icons_enabled(false).unwrap();
        assert!(!core.auto_update_enabled());
        assert!(!core.download_icons_enabled());
    }

    #[test]
    fn steam_import_remembers_which_folders_to_watch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let apps = root.join("steamapps");
        fs::create_dir_all(apps.join("common")).unwrap();
        fs::create_dir_all(root.join("appcache").join("stats")).unwrap();

        let mut core = Core::open_in_memory().unwrap();
        assert!(core.steam_watch_dirs().is_empty());

        let scan = crate::integrations::steam::scan_steam_folder(&root).unwrap();
        core.import_steam(&scan).unwrap();

        let dirs = core.steam_watch_dirs();
        assert!(dirs.contains(&apps));
        assert!(dirs.contains(&root.join("appcache").join("stats")));
        assert_eq!(core.steam_root(), Some(root));
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
