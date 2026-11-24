//! Application/Core: la única puerta de entrada para la interfaz.
//! Coordina base de datos, registro, integraciones y lanzamiento de juegos.

use std::path::{Path, PathBuf};

use crate::achievement_engine::{self, AchievementRecord, AchievementView, Provenance};
use crate::database::Database;
use crate::diagnostics;
use crate::domain::{
    path_key, DiscoveredGame, Game, GameId, GameStatus, Installation, ItchGame, LaunchKind,
    LaunchSettings, MetadataRecord, RegisterSummary, SortOrder,
};
use crate::error::{Result, UglError};
use crate::icons::IconJob;
use crate::integrations::executables::{self, CandidateStatus, ImportCandidate};
use crate::integrations::itch;
use crate::integrations::manual::{self, PROVIDER_MANUAL};
use crate::integrations::steam::SteamScan;
use crate::integrations::steam::PROVIDER_STEAM;
use crate::integrations::steam_achievements::{self, SteamAchievementSet, SOURCE_STEAM_LOCAL};
use crate::{process_manager, registry, secrets, settings};

/// Lo necesario para leer los logros de un juego de Steam fuera del hilo de la interfaz.
#[derive(Debug, Clone)]
pub struct SteamAchievementJob {
    pub steam_root: PathBuf,
    pub app_id: u64,
}

/// Resultado de iniciar un juego: lo que hace falta para seguir su sesión.
#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    pub title: String,
    /// PID del proceso lanzado (inicio directo).
    pub pid: Option<u32>,
    /// Carpeta del juego, para reconocer sus procesos (Steam).
    pub watch_dir: Option<PathBuf>,
}

/// Juego con AppID conocido cuya ficha de la tienda falta por descargar.
#[derive(Debug, Clone)]
pub struct MetadataJob {
    pub game_id: GameId,
    pub app_id: u64,
}

/// Estado de la consulta de metadatos de un candidato descubierto mediante Steam Lua.
#[derive(Debug, Clone)]
pub enum SteamLuaCandidateStatus {
    /// Steam confirmó que el AppID corresponde a un juego.
    Ready(crate::integrations::steam_store::StoreInfo),

    /// Steam no ofrece una ficha pública para este AppID.
    Unavailable,

    /// El AppID existe, pero corresponde a otro tipo de producto.
    NotGame {
        product_type: String,
    },

    /// La consulta falló y puede volver a intentarse.
    Failed {
        message: String,
    },
}

/// Candidato Lua junto con el resultado de consultar su ficha pública.
#[derive(Debug, Clone)]
pub struct SteamLuaCandidate {
    pub app_id: u64,
    pub status: SteamLuaCandidateStatus,
}

/// Cuenta de itch.io vinculada (opcional).
#[derive(Debug, Clone)]
pub struct ItchAccount {
    pub username: Option<String>,
}

/// Obtiene la instalación principal de un juego o devuelve un error
/// si todavía no está instalado.
fn require_installation(game: &Game) -> Result<&Installation> {
    game.installation.as_ref().ok_or_else(|| {
        UglError::Invalid(format!(
            "«{}» no tiene una instalación local.",
            game.title
        ))
    })
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
            return Err(UglError::NotFound(format!(
                "el ejecutable {}",
                exe.display()
            )));
        }

        // El juego se identifica por su ejecutable: otro .exe de la misma carpeta es otro juego.
        let relative = exe_relative.to_string_lossy().into_owned();
        let game = DiscoveredGame {
            provider: PROVIDER_MANUAL.to_string(),
            external_id: path_key(&exe),
            title: title.to_string(),
            platform_slug: "pc".to_string(),
            install_path: install_dir.to_path_buf(),
            launch_kind: LaunchKind::Executable,
            launch_target: relative.clone(),
            exe_path: Some(relative),
            ..Default::default()
        };
        let summary = registry::register(self.db.conn_mut(), &[game])?;
        diagnostics::info(&format!(
            "Importación manual de «{title}»: {} nuevo(s), {} ya existente(s)",
            summary.added, summary.already_present
        ));
        Ok(summary)
    }

    /// Vista previa de una selección de ejecutables (.exe, .lnk, .url): resuelve los accesos
    /// directos y marca lo que ya está en la biblioteca. No cambia nada.
    pub fn preview_import(&self, paths: &[PathBuf]) -> Vec<ImportCandidate> {
        let mut candidates = executables::inspect_all(paths);
        let games = registry::list_games(self.db.conn(), "", SortOrder::Title).unwrap_or_default();

        for candidate in candidates
            .iter_mut()
            .filter(|c| c.status == CandidateStatus::New)
        {
            if candidate.kind == LaunchKind::SteamUri {
                let known = candidate
                    .steam_app_id
                    .and_then(|app_id| self.find_steam_game(app_id));
                if known.is_some() {
                    candidate.status = CandidateStatus::Duplicate;
                    candidate.note = "Ya está en la biblioteca.".to_string();
                } else {
                    // Los juegos de Steam solo entran por la importación de la carpeta de Steam, que
                    // exige que estén instalados.
                    candidate.status = CandidateStatus::Invalid;
                    candidate.note = "Es un juego de Steam que UGL aún no conoce. Importa tu carpeta de Steam (solo se importan los instalados).".to_string();
                }
                continue;
            }
            let Some(exe) = candidate.exe.clone() else {
                continue;
            };
            let already =
                registry::find_game_by_source(self.db.conn(), PROVIDER_MANUAL, &path_key(&exe))
                    .ok()
                    .flatten()
                    .is_some();
            if already {
                candidate.status = CandidateStatus::Duplicate;
                candidate.note = "Ya está en la biblioteca.".to_string();
            } else if games.iter().any(|g| {
                g.installation.as_ref().is_some_and(|installation| {
                    installation.launch_kind == LaunchKind::SteamUri
                        && executables::is_inside(&exe, &installation.install_path)
                })
            }) {
                candidate.status = CandidateStatus::Duplicate;
                candidate.note = "Ya está en la biblioteca como juego de Steam.".to_string();
            }
        }
        candidates
    }

    /// Importa los candidatos aceptados, cada uno como juego independiente. `items` lleva el nombre
    /// final elegido por el usuario; los que no estén «nuevos» se ignoran.
    pub fn import_candidates(
        &mut self,
        items: &[(ImportCandidate, String)],
    ) -> Result<RegisterSummary> {
        let discovered: Vec<DiscoveredGame> = items
            .iter()
            .filter(|(candidate, _)| candidate.status == CandidateStatus::New)
            .filter_map(|(candidate, name)| executables::to_discovered(candidate, name))
            .collect();
        if discovered.is_empty() {
            return Ok(RegisterSummary::default());
        }
        let summary = registry::register(self.db.conn_mut(), &discovered)?;
        diagnostics::info(&format!(
            "Importación de ejecutables: {} nuevo(s), {} ya existente(s)",
            summary.added, summary.already_present
        ));
        Ok(summary)
    }

    pub fn import_steam(&mut self, scan: &SteamScan) -> Result<RegisterSummary> {
        self.remember_steam_root(&scan.steam_root, &scan.libraries);
        // Un juego que el usuario ya agregó a mano y está enlazado con ese AppID no se duplica.
        let discovered: Vec<DiscoveredGame> = scan
            .to_discovered()
            .into_iter()
            .filter(|game| {
                let Some(app_id) = game.steam_app_id else {
                    return true;
                };
                let already_steam = registry::find_game_by_source(
                    self.db.conn(),
                    PROVIDER_STEAM,
                    &game.external_id,
                )
                .ok()
                .flatten()
                .is_some();
                already_steam
                    || registry::find_game_by_steam_app_id(self.db.conn(), app_id)
                        .ok()
                        .flatten()
                        .is_none()
            })
            .collect();
        let summary = registry::register(self.db.conn_mut(), &discovered)?;
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

    /// Devuelve los AppID encontrados mediante archivos Lua que todavía
    /// no están registrados en la biblioteca de UGL.
    ///
    /// No registra juegos ni modifica la base de datos.
    pub fn steam_lua_candidates(&self, scan: &SteamScan) -> Result<Vec<u64>> {
        let mut candidates = Vec::new();

        for app_id in &scan.lua_app_ids {
            // Si el juego ya aparece como instalado en este escaneo,
            // no debe considerarse un candidato nuevo.
            if scan.games.iter().any(|game| game.app_id == *app_id) {
                continue;
            }

            // Si el AppID ya está asociado a un juego de UGL,
            // tampoco debe aparecer como candidato.
            if registry::find_game_by_steam_app_id(self.db.conn(), *app_id)?.is_some() {
                continue;
            }

            candidates.push(*app_id);
        }

        Ok(candidates)
    }

    /// Consulta los metadatos públicos de los AppID descubiertos mediante Steam Lua.
    ///
    /// No registra juegos ni modifica la base de datos.
    /// Los errores individuales se conservan para permitir reintentos posteriores.
    pub fn fetch_steam_lua_metadata(
        &self,
        scan: &SteamScan,
        work_dir: &Path,
    ) -> Result<Vec<SteamLuaCandidate>> {
        let app_ids = self.steam_lua_candidates(scan)?;
        let mut results = Vec::with_capacity(app_ids.len());

        for app_id in app_ids {
            let status = match crate::integrations::steam_store::fetch_info(app_id, work_dir) {
                Ok(Some(info)) if info.is_game() => {
                    SteamLuaCandidateStatus::Ready(info)
                }

                Ok(Some(info)) => SteamLuaCandidateStatus::NotGame {
                    product_type: if info.product_type.is_empty() {
                        "unknown".to_string()
                    } else {
                        info.product_type
                    },
                },

                Ok(None) => SteamLuaCandidateStatus::Unavailable,

                Err(err) => SteamLuaCandidateStatus::Failed {
                    message: err.to_string(),
                },
            };

            results.push(SteamLuaCandidate { app_id, status });
        }

        Ok(results)
    }

    /// Registra en la biblioteca los candidatos Lua seleccionados por el usuario.
    ///
    /// Solo incorpora candidatos cuya ficha pública confirmó que son juegos.
    /// No crea instalaciones: Steam se encargará de instalarlos posteriormente.
    pub fn register_steam_lua_candidates(
        &mut self,
        candidates: &[SteamLuaCandidate],
        selected_app_ids: &[u64],
    ) -> Result<RegisterSummary> {
        let mut summary = RegisterSummary::default();

        for app_id in selected_app_ids {
            let Some(candidate) = candidates.iter().find(|candidate| {
                candidate.app_id == *app_id
            }) else {
                continue;
            };

            let SteamLuaCandidateStatus::Ready(info) = &candidate.status else {
                continue;
            };

            let title = info.name.trim();

            if title.is_empty() || info.app_id != *app_id {
                continue;
            }

            let result = registry::register_uninstalled_steam_game(
                self.db.conn_mut(),
                *app_id,
                title,
            )?;

            summary.added += result.added;
            summary.already_present += result.already_present;
            summary.updated += result.updated;
        }

        diagnostics::info(&format!(
            "Importación de candidatos Steam Lua: {} nuevo(s), {} ya existente(s)",
            summary.added,
            summary.already_present
        ));

        Ok(summary)
    }

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
        let saved = settings::set(
            self.db.conn(),
            settings::KEY_STEAM_ROOT,
            &root.to_string_lossy(),
        )
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

    /// Juego de la biblioteca con ese AppID de Steam (importado de Steam o asociado a mano).
    pub fn find_steam_game(&self, app_id: u64) -> Option<GameId> {
        registry::find_game_by_steam_app_id(self.db.conn(), app_id)
            .ok()
            .flatten()
            .or_else(|| {
                registry::find_game_by_source(self.db.conn(), PROVIDER_STEAM, &app_id.to_string())
                    .ok()
                    .flatten()
            })
    }

    /// Juegos de Steam con logros cuyo progreso cambió en Steam desde la última sincronización
    /// (por ejemplo, logros ganados mientras UGL estaba cerrado).
    pub fn stale_achievement_jobs(&self) -> Vec<(GameId, SteamAchievementJob)> {
        let Some(root) = self.steam_root() else {
            return Vec::new();
        };

        let games = registry::list_games(self.db.conn(), "", SortOrder::Title)
            .unwrap_or_default();

        games
            .into_iter()
            .filter_map(|game| {
                let installation = game.installation.as_ref()?;

                if installation.launch_kind != LaunchKind::SteamUri
                    || game.achievements_total == 0
                {
                    return None;
                }

                let app_id = game
                    .steam_app_id
                    .or_else(|| installation.launch_target.trim().parse().ok())?;

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
            .filter_map(|game| {
                let installation = game.installation?;

                Some(IconJob {
                    game_id: game.id,
                    install_path: installation.install_path,
                    launch_kind: installation.launch_kind,
                    launch_target: installation.launch_target,
                    exe_path: installation.exe_path,
                })
            })
            .collect()
    }

    /// Anota el resultado: `Some(ruta)` si hay icono, `None` si se intentó y no lo hay.
    pub fn record_icon_result(&mut self, id: GameId, icon: Option<&Path>) {
        let value = icon
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Err(err) = registry::set_icon_path(self.db.conn(), id, &value) {
            diagnostics::warn(&format!(
                "No se pudo guardar el icono del juego {id}: {err}"
            ));
        }
    }

    // ---- Cuenta de itch.io (opcional) ----

    /// La cuenta está vinculada mientras haya una clave de API guardada.
    pub fn itch_account(&self) -> Option<ItchAccount> {
        let conn = self.db.conn();
        settings::get(conn, settings::KEY_ITCH_KEY).ok().flatten()?;
        Some(ItchAccount {
            username: settings::get(conn, settings::KEY_ITCH_USER)
                .ok()
                .flatten()
                .filter(|name| !name.is_empty()),
        })
    }

    /// Vincula la cuenta guardando la clave de la API cifrada con el usuario de Windows (DPAPI).
    /// Con otra clave se descarta lo que se sabía de la cuenta anterior.
    pub fn link_itch_account(&mut self, key: &str) -> Result<()> {
        let key = key.trim();
        if !itch::is_valid_api_key(key) {
            return Err(UglError::Invalid(
                "La clave no tiene el formato de una clave de itch.io (letras y números, sin espacios)."
                    .into(),
            ));
        }
        let encrypted = secrets::protect(key.as_bytes())?;
        let conn = self.db.conn();
        settings::remove(conn, settings::KEY_ITCH_USER)?;
        settings::remove(conn, settings::KEY_ITCH_SYNC)?;
        registry::clear_itch_games(conn)?;
        settings::set(conn, settings::KEY_ITCH_KEY, &secrets::to_hex(&encrypted))?;
        diagnostics::info("Cuenta de itch.io vinculada");
        Ok(())
    }

    pub fn set_itch_username(&mut self, name: &str) -> Result<()> {
        settings::set(self.db.conn(), settings::KEY_ITCH_USER, name.trim())
    }

    /// Desvincula la cuenta y borra la clave y la copia local de la biblioteca.
    pub fn unlink_itch_account(&mut self) -> Result<()> {
        let conn = self.db.conn();
        for key in [
            settings::KEY_ITCH_KEY,
            settings::KEY_ITCH_USER,
            settings::KEY_ITCH_SYNC,
        ] {
            settings::remove(conn, key)?;
        }
        registry::clear_itch_games(conn)?;
        diagnostics::info("Cuenta de itch.io desvinculada");
        Ok(())
    }

    /// La clave descifrada (solo para usarla al consultar la API; no se muestra ni se registra).
    pub fn itch_api_key(&self) -> Option<String> {
        let hex = settings::get(self.db.conn(), settings::KEY_ITCH_KEY)
            .ok()
            .flatten()?;
        let plain = secrets::unprotect(&secrets::from_hex(&hex)?).ok()?;
        String::from_utf8(plain).ok()
    }

    pub fn save_itch_games(&mut self, games: &[ItchGame]) -> Result<()> {
        registry::replace_itch_games(self.db.conn_mut(), games)?;
        settings::set(
            self.db.conn(),
            settings::KEY_ITCH_SYNC,
            &crate::domain::unix_now().to_string(),
        )?;
        diagnostics::info(&format!(
            "Biblioteca de itch.io sincronizada: {} juegos",
            games.len()
        ));
        Ok(())
    }

    /// Juegos de la cuenta de itch.io que no están en la biblioteca local.
    pub fn list_itch_games(&self, query: &str) -> Vec<ItchGame> {
        registry::list_itch_not_installed(self.db.conn(), query).unwrap_or_default()
    }

    /// (total en la cuenta, de ellos sin instalar).
    pub fn itch_counts(&self) -> (usize, usize) {
        registry::count_itch_games(self.db.conn()).unwrap_or((0, 0))
    }

    pub fn last_itch_sync(&self) -> Option<i64> {
        settings::get(self.db.conn(), settings::KEY_ITCH_SYNC)
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
    }

    /// Mostrar en la biblioteca los juegos de itch.io sin instalar (activado por defecto).
    pub fn show_itch_enabled(&self) -> bool {
        self.flag(settings::KEY_SHOW_ITCH, true)
    }

    pub fn set_show_itch_enabled(&mut self, enabled: bool) -> Result<()> {
        settings::set(
            self.db.conn(),
            settings::KEY_SHOW_ITCH,
            if enabled { "1" } else { "0" },
        )
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

    /// Descargar de Steam Store la ficha pública (nombre, descripción, portada…) de los juegos con AppID.
    pub fn fetch_metadata_enabled(&self) -> bool {
        self.flag(settings::KEY_FETCH_METADATA, true)
    }

    pub fn set_fetch_metadata_enabled(&mut self, enabled: bool) -> Result<()> {
        settings::set(
            self.db.conn(),
            settings::KEY_FETCH_METADATA,
            if enabled { "1" } else { "0" },
        )
    }

    /// Minimizar UGL a la bandeja al iniciar un juego y volver al cerrarlo (activado por defecto).
    pub fn minimize_on_launch_enabled(&self) -> bool {
        self.flag(settings::KEY_MINIMIZE_ON_LAUNCH, true)
    }

    pub fn set_minimize_on_launch_enabled(&mut self, enabled: bool) -> Result<()> {
        settings::set(
            self.db.conn(),
            settings::KEY_MINIMIZE_ON_LAUNCH,
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

        let app_id = match game.steam_app_id {
            Some(app_id) => app_id,
            None => {
                let installation = require_installation(&game)?;

                if installation.launch_kind != LaunchKind::SteamUri {
                    return Err(UglError::Invalid(
                        "Por ahora UGL solo puede leer logros de juegos de Steam.".into(),
                    ));
                }

                installation
                    .launch_target
                    .trim()
                    .parse()
                    .map_err(|_| {
                        UglError::Invalid(format!(
                            "Identificador de Steam no válido: {}",
                            installation.launch_target
                        ))
                    })?
            }
        };

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

    /// Inicia el juego y devuelve lo necesario para seguir su sesión.
    pub fn launch_game(&mut self, id: GameId) -> Result<LaunchOutcome> {
        let game = registry::get_game(self.db.conn(), id)?;

        // La ejecución requiere una instalación o un método de lanzamiento válido.
        let pid = process_manager::launch(&game)?;

        if let Err(err) = registry::touch_last_played(self.db.conn(), id) {
            diagnostics::warn(&format!("No se pudo anotar la última partida: {err}"));
        }

        let watch_dir = game
            .installation
            .as_ref()
            .filter(|installation| {
                installation.launch_kind == LaunchKind::SteamUri
                    && installation.install_path.is_dir()
            })
            .map(|installation| installation.install_path.clone());

        Ok(LaunchOutcome {
            title: game.title,
            pid,
            watch_dir,
        })
    }

    // ---- Información de Steam Store ----

    /// Juegos con AppID conocido a los que aún les falta la ficha de la tienda.
    pub fn metadata_jobs(&self, limit: usize) -> Vec<MetadataJob> {
        registry::games_needing_metadata(self.db.conn(), limit)
            .unwrap_or_default()
            .into_iter()
            .map(|(game_id, app_id)| MetadataJob { game_id, app_id })
            .collect()
    }

    pub fn save_metadata(&mut self, id: GameId, record: &MetadataRecord) -> Result<()> {
        registry::save_metadata(self.db.conn(), id, record)
    }

    pub fn metadata(&self, id: GameId) -> Option<MetadataRecord> {
        registry::get_metadata(self.db.conn(), id).ok().flatten()
    }

    /// Asocia manualmente un juego con una ficha de Steam. No cambia cómo se inicia; la ficha antigua
    /// se descarta para pedir la nueva.
    pub fn associate_steam(&mut self, id: GameId, app_id: u64) -> Result<()> {
        registry::set_steam_app_id(self.db.conn(), id, Some(app_id))?;
        registry::delete_metadata(self.db.conn(), id)?;
        diagnostics::info(&format!(
            "Juego {id} asociado con el AppID {app_id} de Steam"
        ));
        Ok(())
    }

    pub fn rename_game(&mut self, id: GameId, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Err(UglError::Invalid("El nombre no puede estar vacío.".into()));
        }
        registry::rename_game(self.db.conn(), id, title)
    }

    /// Cambia el método de lanzamiento de un juego: ejecutable directo (con argumentos y carpeta de
    /// trabajo), Steam u otra plataforma. Conserva los datos de los otros métodos para poder volver.
    pub fn update_launch(&mut self, id: GameId, settings: &LaunchSettings) -> Result<()> {
        let game = registry::get_game(self.db.conn(), id)?;
        let installation = require_installation(&game)?;

        let args = settings.args.trim().to_string();

        let working_dir = match &settings.working_dir {
            Some(dir) => {
                if !dir.is_dir() {
                    return Err(UglError::Invalid(
                        "La carpeta de trabajo elegida no existe.".into(),
                    ));
                }
                Some(dir.to_string_lossy().into_owned())
            }
            None => None,
        };

        match settings.kind {
            LaunchKind::Executable => {
                let exe: PathBuf = match &settings.exe {
                    Some(path) => path.clone(),
                    None => installation
                        .exe_path
                        .as_deref()
                        .map(|rel| installation.install_path.join(rel))
                        .ok_or_else(|| {
                            UglError::Invalid(
                                "Este juego no tiene un ejecutable conocido. Elige uno.".into(),
                            )
                        })?,
                };

                if process_manager::is_shortcut_file(&exe) {
                    return Err(UglError::Invalid(
                        "Elige el ejecutable (.exe), no un acceso directo.".into(),
                    ));
                }

                if !exe.is_file() {
                    return Err(UglError::NotFound(format!(
                        "el ejecutable {}",
                        exe.display()
                    )));
                }

                let stored = match exe.strip_prefix(&installation.install_path) {
                    Ok(relative) => relative.to_string_lossy().into_owned(),
                    Err(_) => exe.to_string_lossy().into_owned(),
                };

                registry::update_launch(
                    self.db.conn(),
                    id,
                    &registry::LaunchRow {
                        kind: LaunchKind::Executable,
                        target: &stored,
                        exe_path: Some(&stored),
                        args: &args,
                        working_dir: working_dir.as_deref(),
                        steam_app_id: game.steam_app_id,
                    },
                )?;

                registry::clear_icon(self.db.conn(), id)?;
            }

            LaunchKind::SteamUri => {
                let app_id = settings.steam_app_id.or(game.steam_app_id).ok_or_else(|| {
                    UglError::Invalid("Indica el AppID de Steam de este juego.".into())
                })?;

                registry::update_launch(
                    self.db.conn(),
                    id,
                    &registry::LaunchRow {
                        kind: LaunchKind::SteamUri,
                        target: &app_id.to_string(),
                        exe_path: installation.exe_path.as_deref(),
                        args: &args,
                        working_dir: working_dir.as_deref(),
                        steam_app_id: Some(app_id),
                    },
                )?;
            }

            LaunchKind::Uri => {
                let uri = settings
                    .uri
                    .as_deref()
                    .map(str::trim)
                    .filter(|uri| !uri.is_empty())
                    .ok_or_else(|| {
                        UglError::Invalid(
                            "Escribe la dirección de inicio de la plataforma.".into(),
                        )
                    })?;

                process_manager::validate_uri(uri)?;

                registry::update_launch(
                    self.db.conn(),
                    id,
                    &registry::LaunchRow {
                        kind: LaunchKind::Uri,
                        target: uri,
                        exe_path: installation.exe_path.as_deref(),
                        args: &args,
                        working_dir: working_dir.as_deref(),
                        steam_app_id: game.steam_app_id,
                    },
                )?;
            }
        }

        diagnostics::info(&format!(
            "Método de inicio del juego {id}: {}",
            settings.kind.as_str()
        ));

        Ok(())
    }


    /// Cambia el ejecutable asociado (debe estar dentro de la carpeta de instalación).
    pub fn set_game_executable(&mut self, id: GameId, exe: &Path) -> Result<()> {
        let game = registry::get_game(self.db.conn(), id)?;
        let installation = require_installation(&game)?;

        if installation.launch_kind != LaunchKind::Executable {
            return Err(UglError::Invalid(
                "Este juego se inicia a través de Steam y no tiene un ejecutable propio.".into(),
            ));
        }

        if !exe.is_file() {
            return Err(UglError::NotFound(format!("el archivo {}", exe.display())));
        }

        let relative = exe.strip_prefix(&installation.install_path).map_err(|_| {
            UglError::Invalid(
                "El ejecutable debe estar dentro de la carpeta de instalación del juego.".into(),
            )
        })?;

        registry::set_executable(self.db.conn(), id, &relative.to_string_lossy())?;
        registry::clear_icon(self.db.conn(), id)?;

        Ok(())
    }

    /// Actualiza la carpeta de instalación de un juego.
    pub fn set_game_install_path(&mut self, id: GameId, new_dir: &Path) -> Result<()> {
        let game = registry::get_game(self.db.conn(), id)?;
        let installation = require_installation(&game)?;

        if installation.launch_kind != LaunchKind::Executable {
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

    fn installation(game: &Game) -> &Installation {
        game.installation
            .as_ref()
            .expect("El juego de prueba debe tener una instalación")
    }

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
        assert_eq!(
            PathBuf::from(&installation(&game).launch_target),
            Path::new("bin").join("otro.exe")
        );

        assert!(core.set_game_executable(id, &outside).is_err());
    }

    #[test]
    fn several_executables_in_one_folder_import_as_independent_games() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Pack");
        fs::create_dir_all(&folder).unwrap();
        let (a, b) = (folder.join("uno.exe"), folder.join("dos.exe"));
        fs::write(&a, b"MZ").unwrap();
        fs::write(&b, b"MZ").unwrap();

        let mut core = Core::open_in_memory().unwrap();
        let preview = core.preview_import(&[a.clone(), b.clone()]);
        assert!(preview.iter().all(|c| c.status == CandidateStatus::New));

        let items: Vec<(ImportCandidate, String)> = preview
            .into_iter()
            .map(|c| {
                let name = c.name.clone();
                (c, name)
            })
            .collect();
        let summary = core.import_candidates(&items).unwrap();
        assert_eq!(summary.added, 2);
        assert_eq!(core.list_games("").unwrap().len(), 2);

        // Una segunda vista previa los reconoce como duplicados y no se importa nada más.
        let again = core.preview_import(&[a, b]);
        assert!(again.iter().all(|c| c.status == CandidateStatus::Duplicate));
        let items: Vec<(ImportCandidate, String)> =
            again.into_iter().map(|c| (c, String::new())).collect();
        assert_eq!(core.import_candidates(&items).unwrap().added, 0);
        assert_eq!(core.list_games("").unwrap().len(), 2);
    }

    #[test]
    fn importing_a_folder_game_and_then_its_exe_is_a_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();

        let preview = core.preview_import(&[dir.join("bin").join("juego.exe")]);
        assert_eq!(preview[0].status, CandidateStatus::Duplicate);
        // El otro .exe de la misma carpeta sigue siendo un juego distinto.
        let preview = core.preview_import(&[dir.join("bin").join("otro.exe")]);
        assert_eq!(preview[0].status, CandidateStatus::New);
    }

    #[test]
    fn executables_inside_an_imported_steam_game_are_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let apps = root.join("steamapps");
        let game_dir = apps.join("common").join("Half-Life 2");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("hl2.exe"), b"MZ").unwrap();
        fs::write(
            apps.join("appmanifest_220.acf"),
            "\"AppState\" { \"appid\" \"220\" \"name\" \"Half-Life 2\" \"installdir\" \"Half-Life 2\" \"StateFlags\" \"4\" }",
        )
        .unwrap();
        let mut core = Core::open_in_memory().unwrap();
        let scan = crate::integrations::steam::scan_steam_folder(&root).unwrap();
        core.import_steam(&scan).unwrap();

        let preview = core.preview_import(&[game_dir.join("hl2.exe")]);
        assert_eq!(preview[0].status, CandidateStatus::Duplicate);
        assert!(preview[0].note.contains("Steam"));
    }

    #[test]
    fn launch_method_can_be_changed_per_game_and_keeps_the_other_data() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();
        let id = core.list_games("").unwrap()[0].id;

        // Ejecutable directo con argumentos y carpeta de trabajo.
        core.update_launch(
            id,
            &LaunchSettings {
                kind: LaunchKind::Executable,
                exe: Some(dir.join("bin").join("otro.exe")),
                args: "  -windowed -nolauncher ".to_string(),
                working_dir: Some(dir.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let game = core.game(id).unwrap();
        assert_eq!(installation(&game).launch_kind, LaunchKind::Executable);
        assert_eq!(
            PathBuf::from(&installation(&game).launch_target),
            Path::new("bin").join("otro.exe")
        );
        assert_eq!(installation(&game).launch_args, "-windowed -nolauncher");
        assert_eq!(installation(&game).working_dir.as_deref(), Some(&*dir.to_string_lossy()));

        // Steam necesita un AppID; con él, el ejecutable sigue guardado.
        let err = core
            .update_launch(
                id,
                &LaunchSettings {
                    kind: LaunchKind::SteamUri,
                    ..Default::default()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("AppID"));
        core.update_launch(
            id,
            &LaunchSettings {
                kind: LaunchKind::SteamUri,
                steam_app_id: Some(440),
                ..Default::default()
            },
        )
        .unwrap();
        let game = core.game(id).unwrap();
        assert_eq!(installation(&game).launch_kind, LaunchKind::SteamUri);
        assert_eq!(installation(&game).launch_target, "440");
        assert_eq!(game.steam_app_id, Some(440));
        assert!(installation(&game).exe_path.is_some());

        // Otra plataforma: solo esquemas permitidos.
        assert!(core
            .update_launch(
                id,
                &LaunchSettings {
                    kind: LaunchKind::Uri,
                    uri: Some("file:///x".into()),
                    ..Default::default()
                }
            )
            .is_err());
        core.update_launch(
            id,
            &LaunchSettings {
                kind: LaunchKind::Uri,
                uri: Some("goggalaxy://openGameView/1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let game = core.game(id).unwrap();
        assert_eq!(installation(&game).launch_kind, LaunchKind::Uri);

        // Y se puede volver al ejecutable directo sin volver a elegirlo.
        core.update_launch(
            id,
            &LaunchSettings {
                kind: LaunchKind::Executable,
                ..Default::default()
            },
        )
        .unwrap();
        let game = core.game(id).unwrap();
        assert_eq!(installation(&game).launch_kind, LaunchKind::Executable);
        assert_eq!(
            PathBuf::from(&installation(&game).launch_target),
            Path::new("bin").join("otro.exe")
        );
    }

    #[test]
    fn executables_outside_the_install_folder_and_shortcuts_are_handled() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let outside = tmp.path().join("fuera.exe");
        fs::write(&outside, b"MZ").unwrap();
        let lnk = tmp.path().join("acceso.lnk");
        fs::write(&lnk, b"x").unwrap();

        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();
        let id = core.list_games("").unwrap()[0].id;

        core.update_launch(
            id,
            &LaunchSettings {
                kind: LaunchKind::Executable,
                exe: Some(outside.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let game = core.game(id).unwrap();
        assert_eq!(PathBuf::from(&installation(&game).launch_target), outside);

        assert!(core
            .update_launch(
                id,
                &LaunchSettings {
                    kind: LaunchKind::Executable,
                    exe: Some(lnk),
                    ..Default::default()
                },
            )
            .is_err());
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
        let schema = document(obj("220", obj("stats", obj("1", obj("bits", bit)))));
        fs::write(stats_dir.join("UserGameStatsSchema_220.bin"), schema).unwrap();
        let progress = document(obj(
            "cache",
            obj(
                "1",
                cat(&[
                    int("data", 1),
                    obj("AchievementTimes", int("0", 1_700_000_000)),
                ]),
            ),
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
        assert_eq!(
            (game.achievements_unlocked, game.achievements_total),
            (1, 1)
        );
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
    fn manual_association_keeps_the_launch_method_and_enables_achievements_and_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();
        let id = core.list_games("").unwrap()[0].id;
        assert!(core.metadata_jobs(10).is_empty()); // sin AppID no hay nada que pedir
        assert_eq!(core.find_steam_game(440), None);

        core.associate_steam(id, 440).unwrap();
        let game = core.game(id).unwrap();
        assert_eq!(
            installation(&game).launch_kind,
            LaunchKind::Executable
        ); // sigue iniciándose por su .exe
        assert_eq!(game.steam_app_id, Some(440));
        assert_eq!(core.find_steam_game(440), Some(id));
        assert_eq!(core.metadata_jobs(10).len(), 1);

        let record = MetadataRecord {
            steam_app_id: 440,
            available: true,
            name: "Team Fortress 2".into(),
            ..Default::default()
        };
        core.save_metadata(id, &record).unwrap();
        assert!(core.metadata_jobs(10).is_empty());
        assert_eq!(core.metadata(id).unwrap().name, "Team Fortress 2");

        // Cambiar la asociación descarta la ficha vieja.
        core.associate_steam(id, 570).unwrap();
        assert!(core.metadata(id).is_none());
        assert_eq!(core.metadata_jobs(10)[0].app_id, 570);

        // Con AppID conocido ya se pueden leer logros (aunque falte la carpeta de Steam, el error lo explica).
        assert!(core
            .steam_achievement_job(id)
            .unwrap_err()
            .to_string()
            .contains("Steam"));
    }

    #[test]
    fn steam_import_does_not_duplicate_a_game_already_linked_by_hand() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let apps = root.join("steamapps");
        let game_dir = apps.join("common").join("Half-Life 2");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("hl2.exe"), b"MZ").unwrap();
        fs::write(
            apps.join("appmanifest_220.acf"),
            "\"AppState\" { \"appid\" \"220\" \"name\" \"Half-Life 2\" \"installdir\" \"Half-Life 2\" \"StateFlags\" \"4\" }",
        )
        .unwrap();

        let mut core = Core::open_in_memory().unwrap();
        // El usuario agrega el .exe a mano: se enlaza solo con el AppID por su ubicación.
        let preview = core.preview_import(&[game_dir.join("hl2.exe")]);
        let items: Vec<(ImportCandidate, String)> = preview
            .into_iter()
            .map(|c| (c, "HL2".to_string()))
            .collect();
        assert_eq!(core.import_candidates(&items).unwrap().added, 1);
        assert_eq!(core.list_games("").unwrap()[0].steam_app_id, Some(220));

        // Al importar después la carpeta de Steam no aparece una segunda copia.
        let scan = crate::integrations::steam::scan_steam_folder(&root).unwrap();
        core.import_steam(&scan).unwrap();
        assert_eq!(core.list_games("").unwrap().len(), 1);
    }

    #[test]
    fn renaming_and_background_preferences() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Juego");
        let mut core = Core::open_in_memory().unwrap();
        core.import_folder_game(&dir, "Juego", Path::new("bin/juego.exe"))
            .unwrap();
        let id = core.list_games("").unwrap()[0].id;
        core.rename_game(id, "  Nombre Oficial ").unwrap();
        assert_eq!(core.game(id).unwrap().title, "Nombre Oficial");
        assert!(core.rename_game(id, "   ").is_err());

        assert!(core.fetch_metadata_enabled() && core.minimize_on_launch_enabled());
        core.set_fetch_metadata_enabled(false).unwrap();
        core.set_minimize_on_launch_enabled(false).unwrap();
        assert!(!core.fetch_metadata_enabled() && !core.minimize_on_launch_enabled());
    }

    fn itch_game(game_id: u64, title: &str) -> ItchGame {
        ItchGame {
            game_id,
            title: title.into(),
            author: "Autor".into(),
            url: format!("https://autor.itch.io/juego-{game_id}"),
            description: "Descripción".into(),
            cover_url: None,
        }
    }

    #[test]
    fn the_itch_account_is_optional_and_clean() {
        let mut core = Core::open_in_memory().unwrap();
        assert!(core.itch_account().is_none()); // sin cuenta, todo funciona igual
        assert!(core.list_itch_games("").is_empty());

        // Una clave con mal formato se rechaza antes de guardar nada.
        assert!(core.link_itch_account("corta").is_err());
        assert!(core
            .link_itch_account("una clave con espacios y \" comillas 1234567890")
            .is_err());
        assert!(core.itch_account().is_none());
        assert!(core.itch_api_key().is_none());

        core.save_itch_games(&[itch_game(1, "Juego Uno"), itch_game(2, "Juego Dos")])
            .unwrap();
        assert_eq!(core.itch_counts(), (2, 2));
        assert!(core.last_itch_sync().is_some());
        assert_eq!(core.list_itch_games("uno").len(), 1);

        core.unlink_itch_account().unwrap();
        assert_eq!(core.itch_counts(), (0, 0));
        assert!(core.last_itch_sync().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn the_itch_key_is_stored_encrypted_and_can_be_read_back() {
        let mut core = Core::open_in_memory().unwrap();
        let key = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd";
        core.save_itch_games(&[itch_game(1, "Viejo")]).unwrap();
        core.link_itch_account(&format!("  {key} ")).unwrap();
        assert!(core.itch_account().is_some());
        assert_eq!(core.itch_api_key().as_deref(), Some(key));
        // En la base de datos no aparece en claro, y vincular una clave descarta la biblioteca anterior.
        let stored = settings::get(core.db.conn(), settings::KEY_ITCH_KEY)
            .unwrap()
            .unwrap();
        assert!(!stored.contains(key));
        assert_eq!(core.itch_counts(), (0, 0));

        core.set_itch_username("  Ana ").unwrap();
        assert_eq!(
            core.itch_account().unwrap().username.as_deref(),
            Some("Ana")
        );
        core.unlink_itch_account().unwrap();
        assert!(core.itch_account().is_none() && core.itch_api_key().is_none());
    }

    #[test]
    fn an_itch_game_stops_being_listed_as_not_installed_once_it_is_in_the_library() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_game_dir(tmp.path(), "Mi Juego");
        let mut core = Core::open_in_memory().unwrap();
        core.save_itch_games(&[itch_game(1, "Mi Juego"), itch_game(2, "Otro Juego")])
            .unwrap();
        assert_eq!(core.list_itch_games("").len(), 2);

        core.import_folder_game(&dir, "mi juego", Path::new("bin/juego.exe"))
            .unwrap();
        let pending: Vec<String> = core
            .list_itch_games("")
            .into_iter()
            .map(|g| g.title)
            .collect();
        assert_eq!(pending, vec!["Otro Juego"]);
        assert_eq!(core.itch_counts(), (2, 1));
        assert!(core.show_itch_enabled());
        core.set_show_itch_enabled(false).unwrap();
        assert!(!core.show_itch_enabled());
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