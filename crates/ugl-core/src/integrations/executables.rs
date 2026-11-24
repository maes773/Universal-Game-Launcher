//! Importación de ejecutables sueltos: `.exe`, accesos directos `.lnk` y accesos `.url` de Steam.
//! Cada ejecutable es un juego independiente, aunque varios estén en la misma carpeta. Los accesos
//! directos nunca se guardan como método de inicio: se resuelven al ejecutable real, con sus
//! argumentos y su directorio de trabajo.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::manual::PROVIDER_MANUAL;
use super::shortcut::{self, ShortcutInfo};
use crate::domain::{path_key, DiscoveredGame, LaunchKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateStatus {
    /// Se puede importar.
    New,
    /// Ya está en la biblioteca (o repetido en la selección).
    Duplicate,
    /// No se puede importar; `note` explica por qué.
    Invalid,
}

#[derive(Debug, Clone)]
pub struct ImportCandidate {
    /// Lo que eligió el usuario (.exe, .lnk o .url).
    pub source: PathBuf,
    pub kind: LaunchKind,
    /// Ejecutable real (ya resuelto si el origen era un acceso directo).
    pub exe: Option<PathBuf>,
    pub install_dir: PathBuf,
    /// Nombre propuesto; el usuario puede editarlo antes de importar.
    pub name: String,
    pub args: String,
    pub working_dir: Option<PathBuf>,
    pub steam_app_id: Option<u64>,
    pub status: CandidateStatus,
    pub note: String,
}

fn pretty_name(stem: &str) -> String {
    stem.replace('_', " ").trim().to_string()
}

fn extension_lowercase(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// ¿`child` está dentro de `parent`? Sin distinguir mayúsculas ni tipo de barra (Windows).
pub fn is_inside(child: &Path, parent: &Path) -> bool {
    let normalize = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
    let parent = normalize(parent);
    let parent = parent.trim_end_matches('\\');
    !parent.is_empty() && normalize(child).starts_with(&format!("{parent}\\"))
}

/// Analiza un archivo elegido por el usuario. Solo mira el disco; no consulta la biblioteca.
pub fn inspect(path: &Path) -> ImportCandidate {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut candidate = ImportCandidate {
        source: path.to_path_buf(),
        kind: LaunchKind::Executable,
        exe: None,
        install_dir: PathBuf::new(),
        name: pretty_name(&stem),
        args: String::new(),
        working_dir: None,
        steam_app_id: None,
        status: CandidateStatus::Invalid,
        note: String::new(),
    };

    match extension_lowercase(path).as_str() {
        "exe" => from_exe(&mut candidate, path.to_path_buf()),
        "lnk" | "url" => match shortcut::read_shortcut(path) {
            Ok(info) => {
                if extension_lowercase(path) == "lnk" {
                    from_lnk(&mut candidate, info);
                } else {
                    from_url(&mut candidate, info);
                }
            }
            Err(err) => candidate.note = err.to_string(),
        },
        _ => candidate.note = "Solo se admiten archivos .exe, .lnk y .url.".to_string(),
    }
    candidate
}

fn from_exe(candidate: &mut ImportCandidate, exe: PathBuf) {
    if !exe.is_file() {
        candidate.note = "No se encontró el archivo.".to_string();
        return;
    }
    candidate.install_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    candidate.exe = Some(exe);
    candidate.status = CandidateStatus::New;
}

fn from_lnk(candidate: &mut ImportCandidate, info: ShortcutInfo) {
    candidate.args = info.arguments.clone();
    candidate.working_dir = info.working_dir.filter(|dir| dir.is_dir());

    // Accesos directos que llaman a Steam («steam.exe -applaunch 220»): el juego se inicia por Steam.
    if let Some(app_id) = shortcut::steam_app_id_from_args(&candidate.args) {
        candidate.kind = LaunchKind::SteamUri;
        candidate.steam_app_id = Some(app_id);
        candidate.status = CandidateStatus::New;
        return;
    }

    let Some(target) = info.target else {
        candidate.note = "El acceso directo no indica a qué ejecutable apunta.".to_string();
        return;
    };
    if extension_lowercase(&target) != "exe" {
        candidate.note = format!(
            "El acceso directo apunta a «{}», que no es un ejecutable (.exe).",
            target.display()
        );
        return;
    }
    if !target.is_file() {
        candidate.note = format!(
            "El ejecutable al que apunta no existe: {}",
            target.display()
        );
        return;
    }
    candidate.install_dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    candidate.exe = Some(target);
    candidate.status = CandidateStatus::New;
}

fn from_url(candidate: &mut ImportCandidate, info: ShortcutInfo) {
    match info.url.as_deref().and_then(shortcut::steam_app_id_from_url) {
        Some(app_id) => {
            candidate.kind = LaunchKind::SteamUri;
            candidate.steam_app_id = Some(app_id);
            candidate.status = CandidateStatus::New;
        }
        None => {
            candidate.note = "Solo se admiten accesos .url de Steam (steam://…).".to_string();
        }
    }
}

/// Analiza toda la selección y marca los repetidos dentro de ella.
pub fn inspect_all(paths: &[PathBuf]) -> Vec<ImportCandidate> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut candidates: Vec<ImportCandidate> = paths.iter().map(|p| inspect(p)).collect();
    for candidate in candidates.iter_mut() {
        if candidate.status != CandidateStatus::New {
            continue;
        }
        let key = match (&candidate.exe, candidate.steam_app_id) {
            (Some(exe), _) => path_key(exe),
            (None, Some(app_id)) => format!("steam:{app_id}"),
            (None, None) => continue,
        };
        if !seen.insert(key) {
            candidate.status = CandidateStatus::Duplicate;
            candidate.note = "Repetido en la selección.".to_string();
        }
    }
    candidates
}

/// Accesos directos (.lnk/.url) de una carpeta, sin entrar en subcarpetas. Leer la carpeta evita que
/// el selector de archivos de Windows sustituya el acceso directo por su destino (y pierda los
/// argumentos).
pub fn list_shortcuts(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && shortcut::is_shortcut(path))
            .collect(),
        Err(_) => Vec::new(),
    };
    found.sort();
    found.truncate(limit);
    found
}

/// Convierte un candidato aceptado en un juego para el registro. Solo los de ejecutable directo;
/// los de Steam se registran importando la carpeta de Steam (solo los instalados).
pub fn to_discovered(candidate: &ImportCandidate, name: &str) -> Option<DiscoveredGame> {
    if candidate.kind != LaunchKind::Executable {
        return None;
    }
    let exe = candidate.exe.as_ref()?;
    let file_name = exe.file_name()?.to_string_lossy().into_owned();
    let title = name.trim();
    let title = if title.is_empty() {
        candidate.name.clone()
    } else {
        title.to_string()
    };
    Some(DiscoveredGame {
        provider: PROVIDER_MANUAL.to_string(),
        external_id: path_key(exe),
        title,
        platform_slug: "pc".to_string(),
        install_path: candidate.install_dir.clone(),
        launch_kind: LaunchKind::Executable,
        launch_target: file_name.clone(),
        exe_path: Some(file_name),
        launch_args: candidate.args.clone(),
        working_dir: candidate
            .working_dir
            .as_ref()
            .map(|dir| dir.to_string_lossy().into_owned()),
        steam_app_id: super::steam::app_id_for_path(exe),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::integrations::shortcut::test_util::build_lnk;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"MZ").unwrap();
    }

    #[test]
    fn two_executables_in_one_folder_are_two_independent_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("Pack").join("juego_uno.exe");
        let b = tmp.path().join("Pack").join("juego_dos.exe");
        touch(&a);
        touch(&b);

        let list = inspect_all(&[a.clone(), b.clone()]);
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|c| c.status == CandidateStatus::New));
        assert_eq!(list[0].name, "juego uno");
        assert_eq!(list[0].install_dir, tmp.path().join("Pack"));

        let ids: Vec<String> = list
            .iter()
            .map(|c| to_discovered(c, "").unwrap().external_id)
            .collect();
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn repeated_selection_and_unsupported_files_are_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("a.exe");
        touch(&exe);
        let text = tmp.path().join("leeme.txt");
        fs::write(&text, b"hola").unwrap();

        let list = inspect_all(&[
            exe.clone(),
            exe.clone(),
            text,
            tmp.path().join("no_existe.exe"),
        ]);
        assert_eq!(list[0].status, CandidateStatus::New);
        assert_eq!(list[1].status, CandidateStatus::Duplicate);
        assert_eq!(list[2].status, CandidateStatus::Invalid);
        assert_eq!(list[3].status, CandidateStatus::Invalid);
    }

    #[test]
    fn a_lnk_is_resolved_to_the_real_executable_with_its_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("Juego").join("game.exe");
        touch(&exe);
        let lnk = tmp.path().join("Mi Juego.lnk");
        fs::write(
            &lnk,
            build_lnk(
                &exe.to_string_lossy(),
                Some(&tmp.path().join("Juego").to_string_lossy()),
                Some("-windowed -nolauncher"),
            ),
        )
        .unwrap();

        let candidate = inspect(&lnk);
        assert_eq!(candidate.status, CandidateStatus::New);
        assert_eq!(candidate.exe.as_deref(), Some(exe.as_path()));
        assert_eq!(candidate.name, "Mi Juego"); // el nombre del acceso directo
        assert_eq!(candidate.args, "-windowed -nolauncher");
        assert_eq!(candidate.working_dir, Some(tmp.path().join("Juego")));

        let game = to_discovered(&candidate, "Mi Juego").unwrap();
        assert_eq!(game.launch_target, "game.exe"); // nunca la ruta del .lnk
        assert_eq!(game.launch_args, "-windowed -nolauncher");
    }

    #[test]
    fn a_lnk_to_a_missing_or_non_exe_target_is_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("falta.lnk");
        fs::write(&missing, build_lnk("C:\\no\\existe\\x.exe", None, None)).unwrap();
        assert_eq!(inspect(&missing).status, CandidateStatus::Invalid);

        let doc = tmp.path().join("doc.lnk");
        fs::write(&doc, build_lnk("C:\\docs\\leeme.txt", None, None)).unwrap();
        let candidate = inspect(&doc);
        assert_eq!(candidate.status, CandidateStatus::Invalid);
        assert!(candidate.note.contains("no es un ejecutable"));
    }

    #[test]
    fn steam_shortcuts_become_steam_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let url = tmp.path().join("Half-Life 2.url");
        fs::write(&url, "[InternetShortcut]\nURL=steam://rungameid/220\n").unwrap();
        let from_url = inspect(&url);
        assert_eq!(from_url.kind, LaunchKind::SteamUri);
        assert_eq!(from_url.steam_app_id, Some(220));
        assert!(to_discovered(&from_url, "x").is_none());

        let lnk = tmp.path().join("HL2.lnk");
        fs::write(
            &lnk,
            build_lnk("C:\\Steam\\steam.exe", None, Some("-applaunch 220")),
        )
        .unwrap();
        let from_lnk = inspect(&lnk);
        assert_eq!(from_lnk.kind, LaunchKind::SteamUri);
        assert_eq!(from_lnk.steam_app_id, Some(220));
    }

    #[test]
    fn lists_only_shortcuts_of_a_folder() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.lnk"), b"x").unwrap();
        fs::write(tmp.path().join("b.URL"), b"x").unwrap();
        fs::write(tmp.path().join("c.txt"), b"x").unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("sub").join("d.lnk"), b"x").unwrap();

        let found = list_shortcuts(tmp.path(), 100);
        assert_eq!(found.len(), 2);
        assert_eq!(list_shortcuts(tmp.path(), 1).len(), 1);
        assert!(list_shortcuts(&tmp.path().join("no_existe"), 10).is_empty());
    }

    #[test]
    fn is_inside_ignores_case_and_separator_style() {
        assert!(is_inside(
            Path::new("C:/Steam/steamapps/common/HL2/hl2.exe"),
            Path::new("c:\\steam\\steamapps\\common\\hl2\\")
        ));
        assert!(!is_inside(
            Path::new("C:\\Steam\\steamapps\\common\\HL22\\x.exe"),
            Path::new("C:\\Steam\\steamapps\\common\\HL2")
        ));
    }
}
