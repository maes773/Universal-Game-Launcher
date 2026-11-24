
//! Tipos del dominio. Sin acceso a disco ni a base de datos.

use std::path::{Path, PathBuf};

pub type GameId = i64;

/// Cómo se inicia un juego.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaunchKind {
    /// Ejecutable propio; `launch_target` es la ruta del .exe (relativa a la carpeta de instalación
    /// o absoluta).
    #[default]
    Executable,
    /// A través de Steam; `launch_target` es el AppID.
    SteamUri,
    /// A través de otra plataforma; `launch_target` es una URI de una lista de esquemas permitidos.
    Uri,
}

impl LaunchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchKind::Executable => "executable",
            LaunchKind::SteamUri => "steam_uri",
            LaunchKind::Uri => "uri",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "executable" => Some(LaunchKind::Executable),
            "steam_uri" => Some(LaunchKind::SteamUri),
            "uri" => Some(LaunchKind::Uri),
            _ => None,
        }
    }

    /// Posición en el desplegable de la interfaz.
    pub fn index(self) -> i32 {
        match self {
            LaunchKind::Executable => 0,
            LaunchKind::SteamUri => 1,
            LaunchKind::Uri => 2,
        }
    }

    pub fn from_index(index: i32) -> Self {
        match index {
            1 => LaunchKind::SteamUri,
            2 => LaunchKind::Uri,
            _ => LaunchKind::Executable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Title,
    RecentlyPlayed,
    RecentlyAdded,
}

impl SortOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            SortOrder::Title => "title",
            SortOrder::RecentlyPlayed => "recently_played",
            SortOrder::RecentlyAdded => "recently_added",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "title" => Some(SortOrder::Title),
            "recently_played" => Some(SortOrder::RecentlyPlayed),
            "recently_added" => Some(SortOrder::RecentlyAdded),
            _ => None,
        }
    }

    /// Posición en el desplegable.
    pub fn index(self) -> i32 {
        match self {
            SortOrder::Title => 0,
            SortOrder::RecentlyPlayed => 1,
            SortOrder::RecentlyAdded => 2,
        }
    }

    pub fn from_index(index: i32) -> Self {
        match index {
            1 => SortOrder::RecentlyPlayed,
            2 => SortOrder::RecentlyAdded,
            _ => SortOrder::Title,
        }
    }
}

/// Una instalación real de un juego.
///
/// Un juego puede tener ninguna, una o varias instalaciones.
#[derive(Debug, Clone)]
pub struct Installation {
    pub install_path: PathBuf,
    pub launch_kind: LaunchKind,
    pub launch_target: String,
    /// Ejecutable directo conocido (relativo a `install_path` o absoluto),
    /// aunque el juego se inicie por Steam.
    pub exe_path: Option<String>,
    pub launch_args: String,
    /// Directorio de trabajo; `None` = carpeta del ejecutable.
    pub working_dir: Option<String>,
    /// AppID de Steam asociado a esta instalación, si corresponde.
    pub steam_app_id: Option<u64>,
}

/// Un juego de la biblioteca, independientemente de que esté instalado.
#[derive(Debug, Clone)]
pub struct Game {
    pub id: GameId,
    pub title: String,
    pub platform: String,
    /// Proveedor que lo registró: "manual", "steam"…
    pub source: String,
    /// Instalación principal. `None` significa que el juego no está instalado.
    pub installation: Option<Installation>,
    pub added_at: i64,
    pub last_played_at: Option<i64>,
    pub achievements_total: u32,
    pub achievements_unlocked: u32,
    /// None = sin intentar; Some("") = sin icono; Some(ruta) = PNG en la caché.
    pub icon_path: Option<String>,
    /// AppID de Steam conocido, aunque el juego no esté instalado o se inicie
    /// mediante su propio ejecutable.
    pub steam_app_id: Option<u64>,
}

/// Lo que una integración entrega al registro. Es el único contrato entre
/// los proveedores (manual, Steam, futuros) y el núcleo.
///
/// Este tipo representa siempre un juego que ya tiene una instalación real.
#[derive(Debug, Clone, Default)]
pub struct DiscoveredGame {
    pub provider: String,
    /// Identificador estable dentro del proveedor (AppID de Steam, ruta normalizada…).
    pub external_id: String,
    pub title: String,
    pub platform_slug: String,
    pub install_path: PathBuf,
    pub launch_kind: LaunchKind,
    pub launch_target: String,
    pub exe_path: Option<String>,
    pub launch_args: String,
    pub working_dir: Option<String>,
    pub steam_app_id: Option<u64>,
}

/// Cambios pedidos al método de lanzamiento de un juego
/// (los valida `Core::update_launch`).
#[derive(Debug, Clone, Default)]
pub struct LaunchSettings {
    pub kind: LaunchKind,
    /// Ejecutable elegido; `None` = conservar el que ya se conoce.
    pub exe: Option<PathBuf>,
    pub args: String,
    pub working_dir: Option<PathBuf>,
    pub steam_app_id: Option<u64>,
    pub uri: Option<String>,
}

/// Ficha de la tienda guardada en local para un juego.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataRecord {
    pub steam_app_id: u64,
    /// `false` = Steam no ofrece ficha para ese AppID.
    pub available: bool,
    pub name: String,
    pub short_description: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub genres: Vec<String>,
    pub release_date: String,
    pub website: Option<String>,
    pub cover_path: Option<String>,
}

/// Juego de la biblioteca de la cuenta de itch.io vinculada (esté o no instalado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItchGame {
    pub game_id: u64,
    pub title: String,
    pub author: String,
    /// Dirección de la página del juego en itch.io.
    pub url: String,
    pub description: String,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegisterSummary {
    pub added: usize,
    pub already_present: usize,
    pub updated: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    /// Tiene una instalación registrada y está listo para iniciarse.
    Ready,
    /// Está en la biblioteca, pero todavía no tiene ninguna instalación.
    NotInstalled,
    /// La carpeta de una instalación registrada no se encuentra.
    MissingFolder,
    /// La carpeta existe, pero falta el ejecutable configurado.
    MissingExecutable,
}

impl GameStatus {
    pub fn message(self) -> &'static str {
        match self {
            GameStatus::Ready => "Listo para jugar",
            GameStatus::NotInstalled => "No instalado",
            GameStatus::MissingFolder => {
                "No se encuentra la carpeta del juego (¿se movió o está en un disco desconectado?)"
            }
            GameStatus::MissingExecutable => {
                "No se encuentra el ejecutable en la carpeta del juego"
            }
        }
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fecha `dd/mm/aaaa` (UTC) a partir de una marca de tiempo Unix.
pub fn format_date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    // Algoritmo de días civiles (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{day:02}/{month:02}/{year}")
}

/// Clave normalizada de una ruta para detectar duplicados
/// (Windows no distingue mayúsculas).
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .to_lowercase()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_key_ignores_case_and_trailing_separator() {
        assert_eq!(
            path_key(Path::new("D:\\Juegos\\Doom\\")),
            path_key(Path::new("d:\\juegos\\doom"))
        );
    }

    #[test]
    fn path_key_treats_both_separators_alike() {
        assert_eq!(
            path_key(Path::new("D:\\Juegos/Doom\\bin/doom.exe")),
            path_key(Path::new("d:\\juegos\\doom\\bin\\doom.exe"))
        );
    }

    #[test]
    fn formats_unix_dates() {
        assert_eq!(format_date(0), "01/01/1970");
        assert_eq!(format_date(1_700_000_000), "14/11/2023");
    }

    #[test]
    fn launch_kind_roundtrip() {
        for kind in [
            LaunchKind::Executable,
            LaunchKind::SteamUri,
            LaunchKind::Uri,
        ] {
            assert_eq!(LaunchKind::parse(kind.as_str()), Some(kind));
            assert_eq!(LaunchKind::from_index(kind.index()), kind);
        }
        assert_eq!(LaunchKind::parse("lnk"), None);
    }

    #[test]
    fn sort_order_roundtrip() {
        for order in [
            SortOrder::Title,
            SortOrder::RecentlyPlayed,
            SortOrder::RecentlyAdded,
        ] {
            assert_eq!(SortOrder::parse(order.as_str()), Some(order));
            assert_eq!(SortOrder::from_index(order.index()), order);
        }
    }

    #[test]
    fn game_status_distinguishes_uninstalled_games() {
        assert_eq!(GameStatus::NotInstalled.message(), "No instalado");
    }
}
