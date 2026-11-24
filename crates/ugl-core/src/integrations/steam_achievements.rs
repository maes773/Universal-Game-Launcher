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
use crate::{diagnostics, net};

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
    /// Icono a color y en gris, si ya están en disco.
    pub icon: Option<PathBuf>,
    pub icon_gray: Option<PathBuf>,
    /// Identificadores de las imágenes (para localizarlas o descargarlas si faltan).
    pub icon_hash: Option<String>,
    pub icon_gray_hash: Option<String>,
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

/// «abc123.jpg» → «abc123». Solo caracteres seguros: el nombre acaba formando parte de una URL
/// y de una ruta de archivo.
pub fn normalize_icon_hash(name: &str) -> Option<String> {
    let trimmed = name.trim();
    let stem = trimmed
        .strip_suffix(".jpg")
        .or_else(|| trimmed.strip_suffix(".png"))
        .unwrap_or(trimmed);
    let valid = !stem.is_empty()
        && stem.len() <= 128
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then(|| stem.to_string())
}

/// Carpeta de UGL donde se guardan las imágenes de logros de un juego.
pub fn achievement_icon_dir(cache_dir: &Path, app_id: u64) -> PathBuf {
    cache_dir.join("achievements").join(app_id.to_string())
}

/// Marca de tiempo (Unix) del archivo de progreso más reciente de ese juego, si existe.
pub fn user_stats_modified(steam_root: &Path, app_id: u64) -> Option<i64> {
    let stats_dir = steam_root.join("appcache").join("stats");
    let path = newest_user_stats(&stats_dir, app_id)?;
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IconReport {
    pub from_cache: usize,
    pub downloaded: usize,
    pub failed: usize,
    pub still_missing: usize,
}

/// Servidores de imágenes de la comunidad de Steam, en orden de preferencia.
const ICON_HOSTS: &[&str] = &[
    "https://cdn.cloudflare.steamstatic.com",
    "https://cdn.akamai.steamstatic.com",
    "https://media.steampowered.com",
];

fn download_achievement_icon(app_id: u64, hash: &str, dest: &Path) -> Result<()> {
    let mut last_error = None;
    for host in ICON_HOSTS {
        let url = format!("{host}/steamcommunity/public/images/apps/{app_id}/{hash}.jpg");
        match net::download_image(&url, dest) {
            Ok(()) => return Ok(()),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| UglError::Invalid("no hay servidores de imágenes".into())))
}

/// Completa los iconos que faltan: primero la caché de UGL y, si `allow_download`, la descarga.
/// Tras tres fallos seguidos deja de intentarlo (sin conexión) y lo reintentará en otra ocasión.
pub fn resolve_missing_icons(
    set: &mut SteamAchievementSet,
    cache_dir: &Path,
    allow_download: bool,
) -> IconReport {
    let dir = achievement_icon_dir(cache_dir, set.app_id);
    let app_id = set.app_id;
    let mut report = IconReport::default();
    let mut fail_streak = 0u32;

    for achievement in set.achievements.iter_mut() {
        let slots = [
            (achievement.icon_hash.clone(), &mut achievement.icon),
            (achievement.icon_gray_hash.clone(), &mut achievement.icon_gray),
        ];
        for (hash, slot) in slots {
            if slot.is_some() {
                continue;
            }
            let Some(hash) = hash else {
                continue;
            };
            let target = dir.join(format!("{hash}.jpg"));
            if target.is_file() {
                *slot = Some(target);
                report.from_cache += 1;
                continue;
            }
            if !allow_download || fail_streak >= 3 {
                report.still_missing += 1;
                continue;
            }
            match download_achievement_icon(app_id, &hash, &target) {
                Ok(()) => {
                    *slot = Some(target);
                    report.downloaded += 1;
                    fail_streak = 0;
                }
                Err(err) => {
                    diagnostics::warn(&format!("Imagen de logro {hash} (juego {app_id}): {err}"));
                    report.failed += 1;
                    report.still_missing += 1;
                    fail_streak += 1;
                }
            }
        }
    }
    report
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
            let icon_hash = entry.icon.as_deref().and_then(normalize_icon_hash);
            let icon_gray_hash = entry.icon_gray.as_deref().and_then(normalize_icon_hash);
            Some(SteamAchievement {
                api_name: entry.api_name,
                name: entry.name,
                description: entry.description,
                hidden: entry.hidden,
                icon,
                icon_gray,
                icon_hash,
                icon_gray_hash,
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
    fn icon_hashes_are_normalised_and_sanitised() {
        assert_eq!(normalize_icon_hash("abc123.jpg"), Some("abc123".to_string()));
        assert_eq!(normalize_icon_hash("  abc-1_x "), Some("abc-1_x".to_string()));
        assert_eq!(normalize_icon_hash(""), None);
        assert_eq!(normalize_icon_hash("../x"), None);
        assert_eq!(normalize_icon_hash("a b"), None);
    }

    fn achievement_with_hashes(color: &str, gray: &str) -> SteamAchievement {
        SteamAchievement {
            api_name: "A".into(),
            name: "A".into(),
            description: String::new(),
            hidden: false,
            icon: None,
            icon_gray: None,
            icon_hash: Some(color.into()),
            icon_gray_hash: Some(gray.into()),
            unlocked: false,
            unlocked_at: None,
        }
    }

    #[test]
    fn missing_icons_are_filled_from_the_ugl_cache_without_downloading() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = achievement_icon_dir(tmp.path(), 220);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("color.jpg"), b"jpg").unwrap();

        let mut set = SteamAchievementSet {
            app_id: 220,
            achievements: vec![achievement_with_hashes("color", "gris")],
            progress_found: true,
            warnings: vec![],
        };
        let report = resolve_missing_icons(&mut set, tmp.path(), false);

        assert!(set.achievements[0].icon.is_some());
        assert!(set.achievements[0].icon_gray.is_none());
        assert_eq!(report.from_cache, 1);
        assert_eq!(report.still_missing, 1);
        assert_eq!(report.downloaded, 0);
    }

    #[test]
    fn user_stats_modified_reports_the_newest_progress_file() {
        let tmp = tempfile::tempdir().unwrap();
        let stats_dir = tmp.path().join("appcache").join("stats");
        fs::create_dir_all(&stats_dir).unwrap();
        assert!(user_stats_modified(tmp.path(), 220).is_none());
        fs::write(stats_dir.join("UserGameStats_1_220.bin"), b"x").unwrap();
        assert!(user_stats_modified(tmp.path(), 220).unwrap() > 1_600_000_000);
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
