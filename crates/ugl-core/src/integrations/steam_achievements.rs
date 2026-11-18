//! Logros de Steam leídos de la caché local del cliente (sin red y sin iniciar sesión).
//!
//! Steam guarda dos archivos por juego en `<Steam>\appcache\stats\`:
//! - `UserGameStatsSchema_<appid>.bin`: lista de logros (nombre, descripción, iconos, bit).
//! - `UserGameStats_<cuenta>_<appid>.bin`: progreso (máscara de bits por estadística y fechas).
//!
//! AVISO: el formato está documentado solo por herramientas de terceros; este lector no se ha
//! probado con archivos reales de Steam. Si algo no coincide, falla con un mensaje claro en lugar
//! de mostrar datos inventados.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::binary_kv::{self, Bkv};
use crate::error::{Result, UglError};

pub const SOURCE_STEAM_LOCAL: &str = "steam_local";

const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const PREFERRED_LANGUAGES: &[&str] = &["spanish", "latam", "english"];

#[derive(Debug, Clone)]
pub struct SchemaEntry {
    pub stat_id: u32,
    pub bit: u32,
    pub api_name: String,
    pub name: String,
    pub description: String,
    pub hidden: bool,
    pub archived: bool,
    pub icon: Option<String>,
    pub icon_gray: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UserStats {
    /// Máscara de logros desbloqueados por identificador de estadística.
    pub masks: HashMap<u32, u32>,
    /// Fecha (Unix) de desbloqueo por (estadística, bit).
    pub times: HashMap<(u32, u32), i64>,
}

#[derive(Debug, Clone)]
pub struct SteamAchievement {
    pub api_name: String,
    pub name: String,
    pub description: String,
    pub hidden: bool,
    pub icon: Option<PathBuf>,
    pub icon_gray: Option<PathBuf>,
    pub unlocked: bool,
    pub unlocked_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SteamAchievementSet {
    pub app_id: u64,
    pub achievements: Vec<SteamAchievement>,
    /// Si se encontró y se pudo leer el archivo de progreso del usuario.
    pub progress_found: bool,
    pub warnings: Vec<String>,
}

fn localized(node: &Bkv) -> Option<String> {
    match node {
        Bkv::Obj(entries) => {
            for lang in PREFERRED_LANGUAGES {
                if let Some(text) = node.get(lang).and_then(Bkv::as_text) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
            entries
                .iter()
                .filter(|(key, _)| !key.eq_ignore_ascii_case("token"))
                .find_map(|(_, value)| value.as_text())
                .filter(|text| !text.trim().is_empty())
        }
        other => other.as_text().filter(|text| !text.trim().is_empty()),
    }
}

pub fn parse_schema(bytes: &[u8], app_id: u64) -> Result<Vec<SchemaEntry>> {
    let root = binary_kv::parse(bytes).map_err(|err| {
        UglError::Invalid(format!("El esquema de logros no se pudo interpretar: {err}"))
    })?;
    let app = root
        .get(&app_id.to_string())
        .or_else(|| {
            root.entries()
                .iter()
                .map(|(_, value)| value)
                .find(|value| matches!(value, Bkv::Obj(_)))
        })
        .ok_or_else(|| UglError::Invalid("El esquema de logros está vacío.".into()))?;
    let stats = app.get("stats").ok_or_else(|| {
        UglError::Invalid("El esquema de logros no contiene la sección «stats».".into())
    })?;

    let mut entries = Vec::new();
    for (stat_key, stat) in stats.entries() {
        let Ok(stat_id) = stat_key.parse::<u32>() else {
            continue;
        };
        let Some(bits) = stat.get("bits") else {
            continue;
        };
        for (bit_key, bit) in bits.entries() {
            let Ok(bit_index) = bit_key.parse::<u32>() else {
                continue;
            };
            let Some(api_name) = bit.get("name").and_then(Bkv::as_text) else {
                continue;
            };
            let display = bit.get("display");
            let name = display
                .and_then(|d| d.get("name"))
                .and_then(localized)
                .unwrap_or_else(|| api_name.clone());
            let description = display
                .and_then(|d| d.get("desc"))
                .and_then(localized)
                .unwrap_or_default();
            let hidden = display
                .and_then(|d| d.get("hidden"))
                .or_else(|| bit.get("hidden"))
                .map_or(false, Bkv::truthy);
            let archived = bit.get("archived").map_or(false, Bkv::truthy);
            let icon = display
                .and_then(|d| d.get("icon"))
                .and_then(Bkv::as_text)
                .filter(|text| !text.is_empty());
            let icon_gray = display
                .and_then(|d| d.get("icon_gray"))
                .and_then(Bkv::as_text)
                .filter(|text| !text.is_empty());

            entries.push(SchemaEntry {
                stat_id,
                bit: bit_index,
                api_name,
                name,
                description,
                hidden,
                archived,
                icon,
                icon_gray,
            });
        }
    }
    entries.sort_by_key(|entry| (entry.stat_id, entry.bit));
    Ok(entries)
}

pub fn parse_user_stats(bytes: &[u8]) -> Result<UserStats> {
    let root = binary_kv::parse(bytes).map_err(|err| {
        UglError::Invalid(format!("El archivo de progreso no se pudo interpretar: {err}"))
    })?;
    let cache = root.get("cache").ok_or_else(|| {
        UglError::Invalid("El archivo de progreso no contiene la sección «cache».".into())
    })?;

    let mut stats = UserStats::default();
    for (key, node) in cache.entries() {
        // Solo las claves numéricas son estadísticas ("crc", "PendingChanges"… se ignoran).
        let Ok(stat_id) = key.parse::<u32>() else {
            continue;
        };
        if let Some(data) = node.get("data").and_then(Bkv::as_i64) {
            stats.masks.insert(stat_id, data as u32);
        }
        if let Some(times) = node.get("AchievementTimes") {
            for (bit_key, value) in times.entries() {
                if let (Ok(bit), Some(time)) = (bit_key.parse::<u32>(), value.as_i64()) {
                    stats.times.insert((stat_id, bit), time);
                }
            }
        }
    }
    Ok(stats)
}

fn read_capped(path: &Path) -> Result<Vec<u8>> {
    let size = fs::metadata(path)?.len();
    if size > MAX_FILE_BYTES {
        return Err(UglError::Invalid(format!(
            "{} es demasiado grande para ser una caché de logros.",
            path.display()
        )));
    }
    Ok(fs::read(path)?)
}

/// Si hay varias cuentas de Steam en el equipo, se usa la del archivo modificado más recientemente.
fn newest_user_stats(stats_dir: &Path, app_id: u64) -> Option<PathBuf> {
    let suffix = format!("_{app_id}.bin");
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(stats_dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("UserGameStats_") || !name.ends_with(&suffix) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let newer = best.as_ref().map_or(true, |(time, _)| modified > *time);
        if newer {
            best = Some((modified, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

fn resolve_icon(icon_dir: &Path, name: &str) -> Option<PathBuf> {
    // Nunca salir de la carpeta de caché por un nombre manipulado.
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    [name.to_string(), format!("{name}.jpg")]
        .iter()
        .map(|file| icon_dir.join(file))
        .find(|path| path.is_file())
}

/// Lee los logros de un juego desde la caché local de Steam. Pensado para un hilo aparte.
pub fn read_local_achievements(steam_root: &Path, app_id: u64) -> Result<SteamAchievementSet> {
    let stats_dir = steam_root.join("appcache").join("stats");
    let schema_path = stats_dir.join(format!("UserGameStatsSchema_{app_id}.bin"));
    if !schema_path.is_file() {
        return Err(UglError::NotFound(format!(
            "la caché de logros de Steam para este juego ({})",
            schema_path.display()
        )));
    }
    let schema = parse_schema(&read_capped(&schema_path)?, app_id)?;
    if schema.is_empty() {
        return Err(UglError::Invalid(
            "La caché de Steam no contiene logros para este juego.".into(),
        ));
    }

    let mut warnings = Vec::new();
    let mut progress_found = false;
    let mut stats = UserStats::default();
    match newest_user_stats(&stats_dir, app_id) {
        Some(path) => match read_capped(&path).and_then(|bytes| parse_user_stats(&bytes)) {
            Ok(parsed) => {
                stats = parsed;
                progress_found = true;
            }
            Err(err) => warnings.push(format!(
                "No se pudo leer el progreso ({}): {err}",
                path.display()
            )),
        },
        None => warnings.push("No se encontró el archivo de progreso del usuario.".to_string()),
    }

    let icon_dir = steam_root
        .join("appcache")
        .join("librarycache")
        .join(app_id.to_string());

    let achievements = schema
        .into_iter()
        .filter_map(|entry| {
            let mask = stats.masks.get(&entry.stat_id).copied().unwrap_or(0);
            let unlocked = entry.bit < 32 && (mask >> entry.bit) & 1 == 1;
            // Un logro archivado que no se tiene ya no se puede conseguir: no se muestra.
            if entry.archived && !unlocked {
                return None;
            }
            let unlocked_at = if unlocked {
                stats
                    .times
                    .get(&(entry.stat_id, entry.bit))
                    .copied()
                    .filter(|time| *time > 0)
            } else {
                None
            };
            let icon = entry.icon.as_deref().and_then(|n| resolve_icon(&icon_dir, n));
            let icon_gray = entry
                .icon_gray
                .as_deref()
                .and_then(|n| resolve_icon(&icon_dir, n));
            Some(SteamAchievement {
                api_name: entry.api_name,
                name: entry.name,
                description: entry.description,
                hidden: entry.hidden,
                icon,
                icon_gray,
                unlocked,
                unlocked_at,
            })
        })
        .collect();

    Ok(SteamAchievementSet {
        app_id,
        achievements,
        progress_found,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::super::binary_kv::test_util::*;
    use super::*;

    fn schema_bytes() -> Vec<u8> {
        let first = obj(
            "0",
            cat(&[
                st("name", "ACH_ONE"),
                obj(
                    "display",
                    cat(&[
                        obj("name", cat(&[st("english", "First"), st("spanish", "Primero")])),
                        obj("desc", st("english", "Do it")),
                        st("icon", "aaa.jpg"),
                        st("icon_gray", "bbb.jpg"),
                    ]),
                ),
            ]),
        );
        let second = obj(
            "1",
            cat(&[
                st("name", "ACH_TWO"),
                obj(
                    "display",
                    cat(&[
                        obj("name", st("english", "Secret")),
                        obj("desc", st("english", "Shh")),
                        st("hidden", "1"),
                    ]),
                ),
            ]),
        );
        let archived = obj(
            "2",
            cat(&[st("name", "ACH_OLD"), int("archived", 1)]),
        );
        let stat = obj("1", cat(&[st("type", "4"), obj("bits", cat(&[first, second, archived]))]));
        document(obj("220", obj("stats", stat)))
    }

    fn stats_bytes(mask: i32) -> Vec<u8> {
        document(obj(
            "cache",
            cat(&[
                int("crc", 123),
                obj(
                    "1",
                    cat(&[
                        int("data", mask),
                        obj("AchievementTimes", int("0", 1_700_000_000)),
                    ]),
                ),
            ]),
        ))
    }

    #[test]
    fn schema_prefers_spanish_and_reads_flags() {
        let entries = parse_schema(&schema_bytes(), 220).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "Primero");
        assert_eq!(entries[0].description, "Do it");
        assert_eq!(entries[0].icon.as_deref(), Some("aaa.jpg"));
        assert!(entries[1].hidden);
        assert_eq!(entries[1].name, "Secret");
        assert!(entries[2].archived);
        assert_eq!(entries[2].name, "ACH_OLD"); // sin «display»: se usa el nombre técnico
    }

    #[test]
    fn user_stats_reads_mask_and_times() {
        let stats = parse_user_stats(&stats_bytes(0b01)).unwrap();
        assert_eq!(stats.masks.get(&1), Some(&1));
        assert_eq!(stats.times.get(&(1, 0)), Some(&1_700_000_000));
    }

    #[test]
    fn negative_mask_means_all_bits_set() {
        let stats = parse_user_stats(&stats_bytes(-1)).unwrap();
        assert_eq!(stats.masks.get(&1), Some(&u32::MAX));
    }

    #[test]
    fn reads_from_a_fake_steam_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let stats_dir = tmp.path().join("appcache").join("stats");
        let icons = tmp.path().join("appcache").join("librarycache").join("220");
        fs::create_dir_all(&stats_dir).unwrap();
        fs::create_dir_all(&icons).unwrap();
        fs::write(stats_dir.join("UserGameStatsSchema_220.bin"), schema_bytes()).unwrap();
        fs::write(stats_dir.join("UserGameStats_12345_220.bin"), stats_bytes(0b01)).unwrap();
        fs::write(icons.join("aaa.jpg"), b"jpg").unwrap();

        let set = read_local_achievements(tmp.path(), 220).unwrap();
        assert!(set.progress_found);
        // El logro archivado y sin conseguir no aparece.
        assert_eq!(set.achievements.len(), 2);
        let one = &set.achievements[0];
        assert!(one.unlocked);
        assert_eq!(one.unlocked_at, Some(1_700_000_000));
        assert!(one.icon.is_some());
        assert!(one.icon_gray.is_none()); // bbb.jpg no existe en disco
        let two = &set.achievements[1];
        assert!(!two.unlocked);
        assert!(two.hidden);
    }

    #[test]
    fn missing_schema_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_local_achievements(tmp.path(), 220),
            Err(UglError::NotFound(_))
        ));
    }

    #[test]
    fn icon_names_cannot_escape_the_cache_folder() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_icon(tmp.path(), "../secreto.jpg").is_none());
    }
}
