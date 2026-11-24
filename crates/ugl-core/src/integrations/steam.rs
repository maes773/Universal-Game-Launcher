//! Proveedor Steam: lectura 100 % local de `libraryfolders.vdf` y `appmanifest_*.acf`.
//! Sin inicio de sesión, sin red, sin APIs remotas.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::vdf::{self, Vdf, VdfError};
use crate::domain::{path_key, DiscoveredGame, LaunchKind};
use crate::error::{Result, UglError};

pub const PROVIDER_STEAM: &str = "steam";

/// Steamworks Common Redistributables: aparece como "juego" pero no lo es.
const IGNORED_APP_IDS: &[u64] = &[228980];
const IGNORED_NAME_PREFIXES: &[&str] = &[
    "Steamworks Common Redistributables",
    "Proton ",
    "Steam Linux Runtime",
];

#[derive(Debug, Clone)]
pub struct SteamGame {
    pub app_id: u64,
    pub name: String,
    pub install_dir_name: String,
    pub install_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SteamScan {
    pub steam_root: PathBuf,
    pub libraries: Vec<PathBuf>,
    pub games: Vec<SteamGame>,
    /// Manifiestos cuya carpeta de instalación no existe (descarga incompleta o juego borrado).
    pub skipped_not_installed: usize,
    pub skipped_ignored: usize,
    pub lua_app_ids: Vec<u64>,
    pub lua_app_ids_not_installed: Vec<u64>,
    pub warnings: Vec<String>,
}

impl SteamScan {
    pub fn to_discovered(&self) -> Vec<DiscoveredGame> {
        self.games
            .iter()
            .map(|game| DiscoveredGame {
                provider: PROVIDER_STEAM.to_string(),
                external_id: game.app_id.to_string(),
                title: game.name.clone(),
                platform_slug: "pc".to_string(),
                install_path: game.install_path.clone(),
                launch_kind: LaunchKind::SteamUri,
                launch_target: game.app_id.to_string(),
                steam_app_id: Some(game.app_id),
                ..Default::default()
            })
            .collect()
    }
}

/// Extrae las rutas de biblioteca de `libraryfolders.vdf` (formato nuevo y antiguo).
pub fn parse_library_folders(text: &str) -> std::result::Result<Vec<PathBuf>, VdfError> {
    let doc = vdf::parse(text)?;
    let folders = doc
        .get("libraryfolders")
        .ok_or(VdfError::Missing("libraryfolders"))?;

    let mut paths = Vec::new();
    for (key, value) in folders.entries() {
        // Solo las entradas numéricas son bibliotecas ("0", "1"…); el resto son metadatos.
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        match value {
            Vdf::Str(path) => paths.push(PathBuf::from(path)), // formato antiguo
            Vdf::Obj(_) => {
                if let Some(path) = value.get("path").and_then(Vdf::as_str) {
                    paths.push(PathBuf::from(path)); // formato nuevo
                }
            }
        }
    }
    Ok(paths)
}

struct Manifest {
    app_id: u64,
    name: String,
    install_dir: String,
    /// `StateFlags` de Steam: el bit 4 indica «instalado por completo».
    state_flags: Option<u32>,
}



fn app_id_from_file_name(file_name: &str) -> Option<u64> {
    let digits: String = file_name
        .chars()
        .skip("appmanifest_".len())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// `installdir` debe ser un nombre de carpeta relativo; así un manifiesto manipulado no puede
/// apuntar fuera de `steamapps\common`.
fn is_safe_relative(text: &str) -> bool {
    let path = Path::new(text);
    path.components().next().is_some()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn parse_manifest(text: &str, file_name: &str) -> std::result::Result<Manifest, String> {
    let doc = vdf::parse(text).map_err(|e| e.to_string())?;
    let state = doc
        .get("AppState")
        .ok_or_else(|| VdfError::Missing("AppState").to_string())?;

    let app_id = state
        .get("appid")
        .and_then(Vdf::as_str)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or_else(|| app_id_from_file_name(file_name))
        .ok_or_else(|| "no se pudo determinar el AppID".to_string())?;

    let install_dir = state
        .get("installdir")
        .and_then(Vdf::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "falta «installdir»".to_string())?
        .to_string();
    if !is_safe_relative(&install_dir) {
        return Err(format!("«installdir» no es válido: {install_dir}"));
    }

    let name = state
        .get("name")
        .and_then(Vdf::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(install_dir.as_str())
        .to_string();

    let state_flags = state
        .get("StateFlags")
        .and_then(Vdf::as_str)
        .and_then(|v| v.trim().parse::<u32>().ok());

    Ok(Manifest {
        app_id,
        name,
        install_dir,
        state_flags,
    })
}

fn steam_root_from_selection(selected: &Path) -> PathBuf {
    let is_steamapps = selected
        .file_name()
        .map_or(false, |n| n.eq_ignore_ascii_case("steamapps"));
    if is_steamapps {
        selected
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| selected.to_path_buf())
    } else {
        selected.to_path_buf()
    }
}

/// Escanea `config/lua` y extrae los AppID de los nombres de archivos `.lua`.
///
/// No lee ni ejecuta el contenido de los archivos.
pub fn scan_lua_app_ids(steam_root: &Path) -> std::io::Result<Vec<u64>> {
    let lua_dir = steam_root.join("config").join("lua");

    let entries = match fs::read_dir(&lua_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(err) => return Err(err),
    };

    let mut app_ids = HashSet::new();

    for entry in entries {
        let entry = entry?;

        if !entry.file_type()?.is_file() {
            continue;
        }

        let path = entry.path();

        let is_lua = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map_or(false, |ext| ext.eq_ignore_ascii_case("lua"));

        if !is_lua {
            continue;
        }

        let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };

        // El nombre completo debe ser numérico.
        if !stem.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        if let Ok(app_id) = stem.parse::<u64>() {
            if app_id > 0 {
                app_ids.insert(app_id);
            }
        }
    }

    let mut result: Vec<u64> = app_ids.into_iter().collect();
    result.sort_unstable();

    Ok(result)
}

fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
    let key = path_key(&path);
    if !list.iter().any(|p| path_key(p) == key) {
        list.push(path);
    }
}

/// Si `path` está dentro de `<biblioteca>\\steamapps\\common\\<carpeta>`, busca en el manifiesto de esa
/// carpeta el AppID del juego. Así un juego instalado desde Steam y agregado a mano queda
/// enlazado con su ficha sin preguntar nada.
pub fn app_id_for_path(path: &Path) -> Option<u64> {
    let components: Vec<Component<'_>> = path.components().collect();
    let common_index = components
        .iter()
        .enumerate()
        .position(|(index, component)| {
            index > 0
                && component.as_os_str().eq_ignore_ascii_case("common")
                && components[index - 1]
                    .as_os_str()
                    .eq_ignore_ascii_case("steamapps")
        })?;
    let steamapps: PathBuf = components[..common_index].iter().collect();
    let folder = components
        .get(common_index + 1)?
        .as_os_str()
        .to_string_lossy()
        .to_lowercase();

    for entry in fs::read_dir(&steamapps).ok()?.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let lower = file_name.to_lowercase();
        if !(lower.starts_with("appmanifest_") && lower.ends_with(".acf")) {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        if let Ok(manifest) = parse_manifest(&String::from_utf8_lossy(&bytes), &file_name) {
            if manifest.install_dir.to_lowercase() == folder {
                return Some(manifest.app_id);
            }
        }
    }
    None
}

/// Analiza la carpeta de Steam elegida por el usuario (la raíz, `steamapps` o una biblioteca
/// adicional). Pensado para ejecutarse en un hilo aparte.
pub fn scan_steam_folder(selected: &Path) -> Result<SteamScan> {
    if !selected.is_dir() {
        return Err(UglError::Invalid(
            "La ruta seleccionada no es una carpeta.".into(),
        ));
    }
    let root = steam_root_from_selection(selected);
    let mut warnings = Vec::new();

    // 1. Bibliotecas: la propia carpeta elegida + las que declare libraryfolders.vdf.
    let mut candidates = vec![root.clone()];
    let vdf_paths = [
        root.join("steamapps").join("libraryfolders.vdf"),
        root.join("config").join("libraryfolders.vdf"),
    ];
    for vdf_path in vdf_paths {
        if !vdf_path.is_file() {
            continue;
        }
        match fs::read(&vdf_path) {
            Ok(bytes) => match parse_library_folders(&String::from_utf8_lossy(&bytes)) {
                Ok(paths) => {
                    for path in paths {
                        push_unique(&mut candidates, path);
                    }
                }
                Err(err) => warnings.push(format!(
                    "No se pudo interpretar {}: {err}",
                    vdf_path.display()
                )),
            },
            Err(err) => warnings.push(format!("No se pudo leer {}: {err}", vdf_path.display())),
        }
    }

    let mut libraries = Vec::new();
    for candidate in candidates {
        if candidate.join("steamapps").is_dir() {
            libraries.push(candidate);
        } else if path_key(&candidate) != path_key(&root) {
            warnings.push(format!(
                "Biblioteca no disponible (¿unidad desconectada?): {}",
                candidate.display()
            ));
        }
    }
    if libraries.is_empty() {
        return Err(UglError::Invalid(
            "La carpeta seleccionada no parece una carpeta de Steam: no se encontró «steamapps»."
                .into(),
        ));
    }

    // 2. Manifiestos de cada biblioteca.
    let mut games = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped_not_installed = 0usize;
    let mut skipped_ignored = 0usize;

    for library in &libraries {
        let steamapps = library.join("steamapps");
        let entries = match fs::read_dir(&steamapps) {
            Ok(entries) => entries,
            Err(err) => {
                warnings.push(format!("No se pudo leer {}: {err}", steamapps.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let lower = file_name.to_lowercase();
            if !(lower.starts_with("appmanifest_") && lower.ends_with(".acf")) {
                continue;
            }
            let bytes = match fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(err) => {
                    warnings.push(format!("No se pudo leer {file_name}: {err}"));
                    continue;
                }
            };
            let manifest = match parse_manifest(&String::from_utf8_lossy(&bytes), &file_name) {
                Ok(manifest) => manifest,
                Err(message) => {
                    warnings.push(format!("{file_name}: {message}"));
                    continue;
                }
            };

            let ignored = IGNORED_APP_IDS.contains(&manifest.app_id)
                || IGNORED_NAME_PREFIXES
                    .iter()
                    .any(|prefix| manifest.name.starts_with(*prefix));
            if ignored {
                skipped_ignored += 1;
                continue;
            }

            // Instalación a medias (descarga en curso): se espera a que termine.
            if let Some(flags) = manifest.state_flags {
                if flags & 4 == 0 {
                    skipped_not_installed += 1;
                    continue;
                }
            }

            let install_path = steamapps.join("common").join(&manifest.install_dir);
            if !install_path.is_dir() {
                skipped_not_installed += 1;
                continue;
            }
            if !seen.insert(manifest.app_id) {
                continue; // ya vista en otra biblioteca
            }
            games.push(SteamGame {
                app_id: manifest.app_id,
                name: manifest.name,
                install_dir_name: manifest.install_dir,
                install_path,
            });
        }
    }

    games.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.app_id.cmp(&b.app_id))
    });

    // 3. AppID de archivos Lua: se leen únicamente los nombres, nunca su contenido.
    let lua_app_ids = match scan_lua_app_ids(&root) {
        Ok(app_ids) => app_ids,
        Err(err) => {
            warnings.push(format!("No se pudo leer config/lua: {err}"));
            Vec::new()
        }
    };

    // 4. Separa los AppID Lua que no corresponden a juegos instalados detectados.
    let installed_app_ids: Vec<u64> = games.iter().map(|game| game.app_id).collect();
    let lua_app_ids_not_installed =
        find_uninstalled_lua_app_ids(&lua_app_ids, &installed_app_ids);

    Ok(SteamScan {
        steam_root: root,
        libraries,
        games,
        skipped_not_installed,
        skipped_ignored,
        lua_app_ids,
        lua_app_ids_not_installed,
        warnings,
    })
}

fn find_uninstalled_lua_app_ids(
    lua_app_ids: &[u64],
    installed_app_ids: &[u64],
) -> Vec<u64> {
    lua_app_ids
        .iter()
        .copied()
        .filter(|app_id| !installed_app_ids.contains(app_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY_FOLDERS_NEW: &str = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t\t\"label\"\t\t\"\"\n\t\t\"apps\"\n\t\t{\n\t\t\t\"228980\"\t\t\"123\"\n\t\t}\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}\n";

    const LIBRARY_FOLDERS_OLD: &str = "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\t\"1234\"\n\t\"ContentStatsID\"\t\t\"-1\"\n\t\"1\"\t\t\"D:\\\\Games\\\\Steam\"\n}\n";

    const ACF_HL2: &str = "\"AppState\"\n{\n\t\"appid\"\t\t\"220\"\n\t\"Universe\"\t\t\"1\"\n\t\"name\"\t\t\"Half-Life 2\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"Half-Life 2\"\n}\n";

    const ACF_NOT_INSTALLED: &str = "\"AppState\"\n{\n\t\"appid\"\t\t\"999\"\n\t\"name\"\t\t\"Fantasma\"\n\t\"installdir\"\t\t\"Fantasma\"\n}\n";

    const ACF_REDIST: &str = "\"AppState\"\n{\n\t\"appid\"\t\t\"228980\"\n\t\"name\"\t\t\"Steamworks Common Redistributables\"\n\t\"installdir\"\t\t\"Steamworks Shared\"\n}\n";

    #[test]
    fn scans_lua_files_by_app_id() {
        let tmp = tempfile::tempdir().unwrap();
        let lua_dir = tmp.path().join("config").join("lua");

        fs::create_dir_all(&lua_dir).unwrap();

        fs::write(lua_dir.join("201830.lua"), "").unwrap();
        fs::write(lua_dir.join("220.lua"), "").unwrap();
        fs::write(lua_dir.join("999.LUA"), "").unwrap();

        // Estos nombres deben ignorarse.
        fs::write(lua_dir.join("invalid.lua"), "").unwrap();
        fs::write(lua_dir.join("123abc.lua"), "").unwrap();
        fs::write(lua_dir.join("0.lua"), "").unwrap();
        fs::write(lua_dir.join("readme.txt"), "").unwrap();

        let result = scan_lua_app_ids(tmp.path()).unwrap();

        assert_eq!(
            result,
            vec![220, 999, 201830].into_iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn reads_new_library_folders_format() {
        let paths = parse_library_folders(LIBRARY_FOLDERS_NEW).unwrap();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("C:\\Program Files (x86)\\Steam"),
                PathBuf::from("D:\\SteamLibrary")
            ]
        );
    }

    #[test]
    fn reads_old_library_folders_format() {
        let paths = parse_library_folders(LIBRARY_FOLDERS_OLD).unwrap();
        assert_eq!(paths, vec![PathBuf::from("D:\\Games\\Steam")]);
    }

    #[test]
    fn manifest_with_path_traversal_is_rejected() {
        let acf =
            "\"AppState\" { \"appid\" \"1\" \"name\" \"X\" \"installdir\" \"../../Windows\" }";
        assert!(parse_manifest(acf, "appmanifest_1.acf").is_err());
    }

    #[test]
    fn scans_a_steam_folder_and_skips_noise() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Steam");
        let apps = root.join("steamapps");
        fs::create_dir_all(apps.join("common").join("Half-Life 2")).unwrap();
        fs::create_dir_all(apps.join("common").join("Steamworks Shared")).unwrap();
        fs::write(apps.join("appmanifest_220.acf"), ACF_HL2).unwrap();
        fs::write(apps.join("appmanifest_999.acf"), ACF_NOT_INSTALLED).unwrap();
        fs::write(apps.join("appmanifest_228980.acf"), ACF_REDIST).unwrap();

        let scan = scan_steam_folder(&root).unwrap();
        assert_eq!(scan.games.len(), 1);
        assert_eq!(scan.games[0].app_id, 220);
        assert_eq!(scan.games[0].name, "Half-Life 2");
        assert_eq!(scan.skipped_not_installed, 1);
        assert_eq!(scan.skipped_ignored, 1);

        // Elegir directamente «steamapps» también funciona.
        let scan2 = scan_steam_folder(&apps).unwrap();
        assert_eq!(scan2.games.len(), 1);

        let discovered = scan.to_discovered();
        assert_eq!(discovered[0].launch_target, "220");
    }

    #[test]
    fn games_still_downloading_are_not_imported_yet() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("steamapps");
        fs::create_dir_all(apps.join("common").join("Half-Life 2")).unwrap();
        let downloading = ACF_HL2.replace("\"StateFlags\"\t\t\"4\"", "\"StateFlags\"\t\t\"1026\"");
        assert_ne!(downloading, ACF_HL2);
        fs::write(apps.join("appmanifest_220.acf"), downloading).unwrap();

        let scan = scan_steam_folder(tmp.path()).unwrap();
        assert!(scan.games.is_empty());
        assert_eq!(scan.skipped_not_installed, 1);

        // Al terminar la instalación (StateFlags 4) aparece.
        fs::write(apps.join("appmanifest_220.acf"), ACF_HL2).unwrap();
        assert_eq!(scan_steam_folder(tmp.path()).unwrap().games.len(), 1);
    }

    #[test]
fn finds_lua_games_not_installed() {
    let lua_app_ids: Vec<u64> = vec![10, 20, 30, 40];
    let installed_app_ids: Vec<u64> = vec![10, 30];

    let result =
        find_uninstalled_lua_app_ids(&lua_app_ids, &installed_app_ids);

    assert_eq!(result, vec![20, 40]);
}

    #[test]
    fn finds_the_app_id_of_a_game_by_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("Biblioteca").join("SteamApps");
        let game = apps.join("Common").join("Half-Life 2");
        fs::create_dir_all(game.join("bin")).unwrap();
        fs::write(apps.join("appmanifest_220.acf"), ACF_HL2).unwrap();

        assert_eq!(
            app_id_for_path(&game.join("bin").join("hl2.exe")),
            Some(220)
        );
        assert_eq!(app_id_for_path(&game), Some(220));
        // Fuera de steamapps\common no se asume nada.
        assert_eq!(
            app_id_for_path(&tmp.path().join("Otros").join("x.exe")),
            None
        );
        // Carpeta sin manifiesto.
        assert_eq!(
            app_id_for_path(&apps.join("common").join("Desconocido").join("a.exe")),
            None
        );
    }

    #[test]
    fn rejects_a_folder_that_is_not_steam() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan_steam_folder(tmp.path()).is_err());
    }
}
