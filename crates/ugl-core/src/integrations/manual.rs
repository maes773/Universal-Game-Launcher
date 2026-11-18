//! Proveedor "manual": analiza la carpeta de un juego y propone ejecutables.
//! Solo lee. Nunca copia, mueve ni modifica nada de la instalación.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Result, UglError};

pub const PROVIDER_MANUAL: &str = "manual";

/// Profundidad máxima (la carpeta del juego cuenta como nivel 0).
const MAX_DEPTH: usize = 4;
/// Entradas de disco que se revisan como máximo, para no atascarse en carpetas enormes.
const MAX_ENTRIES: usize = 20_000;
const MAX_CANDIDATES: usize = 200;

/// Carpetas que nunca contienen el ejecutable principal (se compara en minúsculas).
const SKIPPED_DIRS: &[&str] = &[
    "_commonredist",
    "commonredist",
    "redist",
    "redistributable",
    "directx",
    "vcredist",
    "dotnet",
    ".git",
    "node_modules",
    "$recycle.bin",
    "system volume information",
    "__installer",
];

/// Nombres de ejecutables casi seguro irrelevantes: penalización fuerte.
const JUNK_NAMES: &[&str] = &[
    "unins",
    "uninstall",
    "setup",
    "install",
    "vcredist",
    "vc_redist",
    "redist",
    "dxsetup",
    "dotnet",
    "crashreport",
    "crashhandler",
    "crashpad",
];

/// Utilidades del juego que probablemente no son el ejecutable principal: penalización leve.
const MINOR_NAMES: &[&str] = &["update", "patch", "config", "settings", "helper", "benchmark"];

#[derive(Debug, Clone)]
pub struct ExeCandidate {
    /// Ruta relativa a la carpeta del juego.
    pub relative: PathBuf,
    pub size: u64,
    pub score: i32,
    /// Hay una propuesta razonable de ejecutable principal (solo el primero puede marcarse).
    pub suggested: bool,
}

#[derive(Debug, Clone)]
pub struct FolderScan {
    pub install_dir: PathBuf,
    pub suggested_name: String,
    /// Ordenados de más a menos probable.
    pub candidates: Vec<ExeCandidate>,
    pub truncated: bool,
    pub unreadable_dirs: usize,
}

impl FolderScan {
    /// Aviso para el usuario cuando el análisis no fue completo.
    pub fn note(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.unreadable_dirs > 0 {
            parts.push(format!(
                "{} carpeta(s) no se pudieron leer (permisos)",
                self.unreadable_dirs
            ));
        }
        if self.truncated {
            parts.push("el análisis se limitó por el tamaño de la carpeta".to_string());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("; "))
        }
    }

    /// Añade un ejecutable elegido a mano (por ejemplo, uno más profundo que el límite de análisis).
    /// Devuelve su posición en la lista.
    pub fn add_candidate(&mut self, exe: &Path) -> Result<usize> {
        if !exe.is_file() {
            return Err(UglError::NotFound(format!("el archivo {}", exe.display())));
        }
        let relative = exe
            .strip_prefix(&self.install_dir)
            .map_err(|_| {
                UglError::Invalid(
                    "El ejecutable debe estar dentro de la carpeta del juego.".into(),
                )
            })?
            .to_path_buf();
        if let Some(index) = self.candidates.iter().position(|c| c.relative == relative) {
            return Ok(index);
        }
        let size = fs::metadata(exe).map(|m| m.len()).unwrap_or(0);
        self.candidates.push(ExeCandidate {
            relative,
            size,
            score: 0,
            suggested: false,
        });
        Ok(self.candidates.len() - 1)
    }
}

/// Comprobaciones comunes para cualquier carpeta de juego.
pub fn validate_game_dir(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Err(UglError::Invalid(format!(
            "«{}» no es una carpeta accesible.",
            dir.display()
        )));
    }
    if dir.parent().is_none() {
        return Err(UglError::Invalid(
            "Selecciona la carpeta del juego, no la raíz de una unidad.".into(),
        ));
    }
    Ok(())
}

fn is_skipped_dir(name: &OsStr) -> bool {
    let lower = name.to_string_lossy().to_lowercase();
    SKIPPED_DIRS.contains(&lower.as_str())
}

fn has_exe_extension(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map_or(false, |ext| ext.eq_ignore_ascii_case("exe"))
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn score_candidate(relative: &Path, size: u64, folder_name: &str) -> i32 {
    let depth = relative.components().count().saturating_sub(1) as i32;
    let file_name = relative
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = relative
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut score = -10 * depth;

    if JUNK_NAMES.iter().any(|junk| file_name.contains(junk)) {
        score -= 100;
    } else if MINOR_NAMES.iter().any(|minor| file_name.contains(minor)) {
        score -= 30;
    }

    let stem_key = normalize(&stem);
    let folder_key = normalize(folder_name);
    if !stem_key.is_empty() && !folder_key.is_empty() {
        if stem_key == folder_key {
            score += 60;
        } else if stem_key.len() >= 4
            && (folder_key.contains(&stem_key) || stem_key.contains(&folder_key))
        {
            score += 30;
        }
    }

    // Los ejecutables principales suelen pesar más que los auxiliares.
    score += (size / 1_048_576).min(40) as i32;
    score
}

/// Analiza la carpeta de un juego. Pensado para ejecutarse en un hilo aparte (hace E/S de disco).
pub fn scan_game_folder(dir: &Path) -> Result<FolderScan> {
    validate_game_dir(dir)?;

    let folder_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut candidates = Vec::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((dir.to_path_buf(), 0));
    let mut visited = 0usize;
    let mut unreadable_dirs = 0usize;
    let mut truncated = false;

    // Recorrido por niveles (primero lo superficial) con límites de profundidad y de entradas.
    'walk: while let Some((current, depth)) = queue.pop_front() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => {
                unreadable_dirs += 1;
                continue;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                unreadable_dirs += 1;
                continue;
            };
            visited += 1;
            if visited > MAX_ENTRIES {
                truncated = true;
                break 'walk;
            }
            // `file_type` de DirEntry no sigue enlaces simbólicos: no se entra en ellos.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if depth + 1 < MAX_DEPTH && !is_skipped_dir(&entry.file_name()) {
                    queue.push_back((path, depth + 1));
                }
            } else if file_type.is_file() && has_exe_extension(&path) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let relative = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
                let score = score_candidate(&relative, size, &folder_name);
                candidates.push(ExeCandidate {
                    relative,
                    size,
                    score,
                    suggested: false,
                });
            }
        }
    }

    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.relative.cmp(&b.relative))
    });
    if candidates.len() > MAX_CANDIDATES {
        candidates.truncate(MAX_CANDIDATES);
        truncated = true;
    }
    // Solo se sugiere si el mejor candidato no parece un desinstalador o similar.
    if let Some(first) = candidates.first_mut() {
        first.suggested = first.score > -50;
    }

    Ok(FolderScan {
        install_dir: dir.to_path_buf(),
        suggested_name: folder_name.replace('_', " ").trim().to_string(),
        candidates,
        truncated,
        unreadable_dirs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn ranks_main_executable_first_and_skips_redist() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("The_Witcher_3");
        touch(&dir.join("witcher3.exe"), b"MZ");
        touch(&dir.join("unins000.exe"), b"MZ");
        touch(&dir.join("bin").join("x64").join("witcher3.exe"), b"MZ");
        touch(&dir.join("redist").join("vcredist_x64.exe"), b"MZ");
        touch(&dir.join("readme.txt"), b"hola");

        let scan = scan_game_folder(&dir).unwrap();
        assert_eq!(scan.suggested_name, "The Witcher 3");
        assert_eq!(scan.candidates.len(), 3);
        assert_eq!(scan.candidates[0].relative, PathBuf::from("witcher3.exe"));
        assert!(scan.candidates[0].suggested);
        assert!(scan
            .candidates
            .iter()
            .all(|c| !c.relative.to_string_lossy().contains("redist")));
    }

    #[test]
    fn does_not_suggest_when_only_junk_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Algo");
        touch(&dir.join("unins000.exe"), b"MZ");
        let scan = scan_game_folder(&dir).unwrap();
        assert_eq!(scan.candidates.len(), 1);
        assert!(!scan.candidates[0].suggested);
    }

    #[test]
    fn rejects_missing_folder_and_drive_root() {
        assert!(validate_game_dir(Path::new("/ruta/que/no/existe")).is_err());
        assert!(validate_game_dir(Path::new("/")).is_err());
    }

    #[test]
    fn manual_candidate_must_live_inside_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Juego");
        touch(&dir.join("a.exe"), b"MZ");
        touch(&tmp.path().join("fuera.exe"), b"MZ");

        let mut scan = scan_game_folder(&dir).unwrap();
        assert!(scan.add_candidate(&tmp.path().join("fuera.exe")).is_err());
        let index = scan.add_candidate(&dir.join("a.exe")).unwrap();
        assert_eq!(index, 0); // ya estaba en la lista
    }
}
