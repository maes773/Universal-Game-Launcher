//! Tipos del dominio. Sin acceso a disco ni a base de datos.

use std::path::{Path, PathBuf};

pub type GameId = i64;

/// Cómo se inicia un juego.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKind {
    /// Ejecutable propio; `launch_target` es la ruta relativa a la carpeta de instalación.
    Executable,
    /// A través de Steam; `launch_target` es el AppID.
    SteamUri,
}

impl LaunchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchKind::Executable => "executable",
            LaunchKind::SteamUri => "steam_uri",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "executable" => Some(LaunchKind::Executable),
            "steam_uri" => Some(LaunchKind::SteamUri),
            _ => None,
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

    /// Posición en el desplegable de la interfaz.
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

/// Un juego de la biblioteca junto con su instalación principal.
#[derive(Debug, Clone)]
pub struct Game {
    pub id: GameId,
    pub title: String,
    pub platform: String,
    /// Proveedor que lo registró: "manual", "steam"…
    pub source: String,
    pub install_path: PathBuf,
    pub launch_kind: LaunchKind,
    pub launch_target: String,
    pub added_at: i64,
    pub last_played_at: Option<i64>,
    pub achievements_total: u32,
    pub achievements_unlocked: u32,
    /// None = sin intentar; Some("") = sin icono; Some(ruta) = PNG en la caché.
    pub icon_path: Option<String>,
}

/// Lo que una integración entrega al registro. Es el único contrato entre
/// los proveedores (manual, Steam, futuros) y el núcleo.
#[derive(Debug, Clone)]
pub struct DiscoveredGame {
    pub provider: String,
    /// Identificador estable dentro del proveedor (AppID de Steam, ruta normalizada…).
    pub external_id: String,
    pub title: String,
    pub platform_slug: String,
    pub install_path: PathBuf,
    pub launch_kind: LaunchKind,
    pub launch_target: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegisterSummary {
    pub added: usize,
    pub already_present: usize,
    pub updated: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    Ready,
    MissingFolder,
    MissingExecutable,
}

impl GameStatus {
    pub fn message(self) -> &'static str {
        match self {
            GameStatus::Ready => "Listo para jugar",
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

/// Clave normalizada de una ruta para detectar duplicados (Windows no distingue mayúsculas).
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .to_lowercase()
        .trim_end_matches(|c| c == '\\' || c == '/')
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
    fn formats_unix_dates() {
        assert_eq!(format_date(0), "01/01/1970");
        assert_eq!(format_date(1_700_000_000), "14/11/2023");
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
}
