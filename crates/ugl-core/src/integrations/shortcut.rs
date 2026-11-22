//! Lectura de accesos directos de Windows (.lnk) y de Internet (.url) para encontrar el ejecutable
//! original, sus argumentos y su directorio de trabajo. Solo lectura y sin COM: UGL nunca inicia un
//! juego «a través» del acceso directo, sino directamente desde el .exe.
//!
//! Formato .lnk: [MS-SHLLINK]. AVISO: escrito siguiendo la especificación; las pruebas usan archivos
//! fabricados a mano, no accesos directos reales de Windows.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{Result, UglError};

const HEADER_SIZE: usize = 0x4C;
const LNK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];
const MAX_SHORTCUT_BYTES: u64 = 1_048_576;

// LinkFlags
const HAS_LINK_TARGET_ID_LIST: u32 = 1 << 0;
const HAS_LINK_INFO: u32 = 1 << 1;
const HAS_NAME: u32 = 1 << 2;
const HAS_RELATIVE_PATH: u32 = 1 << 3;
const HAS_WORKING_DIR: u32 = 1 << 4;
const HAS_ARGUMENTS: u32 = 1 << 5;
const IS_UNICODE: u32 = 1 << 7;
const FORCE_NO_LINK_INFO: u32 = 1 << 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutInfo {
    /// Ejecutable (u otro archivo) al que apunta el acceso directo.
    pub target: Option<PathBuf>,
    /// Dirección de un acceso `.url` (por ejemplo `steam://rungameid/220`).
    pub url: Option<String>,
    pub arguments: String,
    pub working_dir: Option<PathBuf>,
}

fn invalid() -> UglError {
    UglError::Invalid("El acceso directo (.lnk) no tiene un formato válido.".into())
}

pub fn is_shortcut(path: &Path) -> bool {
    matches!(extension_lowercase(path).as_str(), "lnk" | "url")
}

fn extension_lowercase(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Lee un acceso directo `.lnk` o `.url`.
pub fn read_shortcut(path: &Path) -> Result<ShortcutInfo> {
    let extension = extension_lowercase(path);
    if extension != "lnk" && extension != "url" {
        return Err(UglError::Invalid(
            "Solo se pueden leer accesos directos .lnk y .url.".into(),
        ));
    }
    if fs::metadata(path)?.len() > MAX_SHORTCUT_BYTES {
        return Err(UglError::Invalid(
            "El acceso directo es demasiado grande.".into(),
        ));
    }
    let bytes = fs::read(path)?;
    if extension == "lnk" {
        parse_lnk(&bytes, path.parent())
    } else {
        Ok(parse_url_file(&String::from_utf8_lossy(&bytes)))
    }
}

/// `steam://rungameid/220` o `steam://run/220` → 220.
pub fn steam_app_id_from_url(url: &str) -> Option<u64> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("steam://rungameid/")
        .or_else(|| lower.strip_prefix("steam://run/"))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// `-applaunch 220` (accesos directos que llaman a steam.exe) → 220.
pub fn steam_app_id_from_args(args: &str) -> Option<u64> {
    let mut parts = args.split_whitespace();
    while let Some(part) = parts.next() {
        if part.eq_ignore_ascii_case("-applaunch") {
            return parts.next()?.parse().ok();
        }
    }
    None
}

pub fn parse_url_file(text: &str) -> ShortcutInfo {
    let mut info = ShortcutInfo::default();
    for line in text.lines() {
        if let Some((key, value)) = line.trim().split_once('=') {
            match key.trim().to_ascii_lowercase().as_str() {
                "url" => info.url = Some(value.trim().to_string()),
                "workingdirectory" => info.working_dir = Some(PathBuf::from(value.trim())),
                _ => {}
            }
        }
    }
    info
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*data.get(offset)?, *data.get(offset + 1)?]))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *data.get(offset)?,
        *data.get(offset + 1)?,
        *data.get(offset + 2)?,
        *data.get(offset + 3)?,
    ]))
}

fn decode_utf16(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn ansi_cstring_at(data: &[u8], offset: usize) -> Option<String> {
    let rest = data.get(offset..)?;
    let len = rest.iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&rest[..len]).into_owned())
}

fn utf16_cstring_at(data: &[u8], offset: usize) -> Option<String> {
    let rest = data.get(offset..)?;
    let mut end = 0;
    while end + 1 < rest.len() && !(rest[end] == 0 && rest[end + 1] == 0) {
        end += 2;
    }
    if end + 1 >= rest.len() {
        return None; // sin terminador
    }
    Some(decode_utf16(&rest[..end]))
}

/// Ruta local de destino que guarda la sección LinkInfo.
fn link_info_target(info: &[u8]) -> Option<PathBuf> {
    let header_size = u32_at(info, 4)? as usize;
    let flags = u32_at(info, 8)?;
    if flags & 1 == 0 {
        return None; // sin ruta local (p. ej. un recurso de red)
    }
    let local_offset = u32_at(info, 16)? as usize;
    let suffix_offset = u32_at(info, 24)? as usize;

    let (base, suffix) = if header_size >= 0x24 && u32_at(info, 28)? != 0 {
        let local_unicode = u32_at(info, 28)? as usize;
        let suffix_unicode = u32_at(info, 32)? as usize;
        (
            utf16_cstring_at(info, local_unicode)?,
            if suffix_unicode != 0 {
                utf16_cstring_at(info, suffix_unicode).unwrap_or_default()
            } else {
                String::new()
            },
        )
    } else {
        (
            ansi_cstring_at(info, local_offset)?,
            ansi_cstring_at(info, suffix_offset).unwrap_or_default(),
        )
    };

    if suffix.is_empty() {
        Some(PathBuf::from(base))
    } else if base.ends_with('\\') || base.ends_with('/') {
        Some(PathBuf::from(format!("{base}{suffix}")))
    } else {
        Some(PathBuf::from(format!("{base}\\{suffix}")))
    }
}

/// Resuelve `..` y `.` de forma léxica (sin tocar el disco).
fn resolve_relative(base: &Path, relative: &str) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    let joined = base.join(relative);
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else {
                    parts.push(component);
                }
            }
            Component::CurDir => {}
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// Interpreta los bytes de un `.lnk`. `base_dir` es la carpeta del acceso directo (para destinos
/// relativos).
pub fn parse_lnk(data: &[u8], base_dir: Option<&Path>) -> Result<ShortcutInfo> {
    if data.len() < HEADER_SIZE
        || u32_at(data, 0) != Some(HEADER_SIZE as u32)
        || data[4..20] != LNK_CLSID
    {
        return Err(invalid());
    }
    let flags = u32_at(data, 0x14).ok_or_else(invalid)?;
    let unicode = flags & IS_UNICODE != 0;

    let mut offset = HEADER_SIZE;
    if flags & HAS_LINK_TARGET_ID_LIST != 0 {
        let size = u16_at(data, offset).ok_or_else(invalid)? as usize;
        offset = offset.checked_add(2 + size).ok_or_else(invalid)?;
    }

    let mut target = None;
    if flags & HAS_LINK_INFO != 0 && flags & FORCE_NO_LINK_INFO == 0 {
        let info_size = u32_at(data, offset).ok_or_else(invalid)? as usize;
        let end = offset.checked_add(info_size).ok_or_else(invalid)?;
        if info_size < 0x1C || end > data.len() {
            return Err(invalid());
        }
        target = link_info_target(&data[offset..end]);
        offset = end;
    }

    // Cadenas de longitud fija: nombre, ruta relativa, directorio de trabajo, argumentos…
    let mut read_string = |present: bool| -> Result<Option<String>> {
        if !present {
            return Ok(None);
        }
        let count = u16_at(data, offset).ok_or_else(invalid)? as usize;
        offset += 2;
        let byte_len = if unicode { count * 2 } else { count };
        let end = offset
            .checked_add(byte_len)
            .filter(|end| *end <= data.len())
            .ok_or_else(invalid)?;
        let raw = &data[offset..end];
        offset = end;
        Ok(Some(if unicode {
            decode_utf16(raw)
        } else {
            String::from_utf8_lossy(raw).into_owned()
        }))
    };
    let _name = read_string(flags & HAS_NAME != 0)?;
    let relative = read_string(flags & HAS_RELATIVE_PATH != 0)?;
    let working_dir = read_string(flags & HAS_WORKING_DIR != 0)?;
    let arguments = read_string(flags & HAS_ARGUMENTS != 0)?;

    if target.is_none() {
        if let (Some(relative), Some(base)) = (relative.as_deref(), base_dir) {
            if !relative.is_empty() {
                target = Some(resolve_relative(base, relative));
            }
        }
    }

    Ok(ShortcutInfo {
        target,
        url: None,
        arguments: arguments.unwrap_or_default().trim().to_string(),
        working_dir: working_dir
            .filter(|dir| !dir.trim().is_empty())
            .map(PathBuf::from),
    })
}

/// Constructor de `.lnk` mínimos para pruebas.
#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    pub fn build_lnk(target: &str, working_dir: Option<&str>, args: Option<&str>) -> Vec<u8> {
        let mut flags: u32 = HAS_LINK_INFO | IS_UNICODE;
        if working_dir.is_some() {
            flags |= HAS_WORKING_DIR;
        }
        if args.is_some() {
            flags |= HAS_ARGUMENTS;
        }
        let mut out = vec![0u8; HEADER_SIZE];
        out[0..4].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        out[4..20].copy_from_slice(&LNK_CLSID);
        out[0x14..0x18].copy_from_slice(&flags.to_le_bytes());

        // LinkInfo con cabecera de 0x1C bytes (sin offsets Unicode).
        let volume_id = [0u8; 16];
        let base = target.as_bytes();
        let local_offset = 0x1C + volume_id.len();
        let suffix_offset = local_offset + base.len() + 1;
        let info_size = suffix_offset + 1;
        let mut info = Vec::new();
        info.extend_from_slice(&(info_size as u32).to_le_bytes());
        info.extend_from_slice(&0x1Cu32.to_le_bytes());
        info.extend_from_slice(&1u32.to_le_bytes()); // VolumeIDAndLocalBasePath
        info.extend_from_slice(&0x1Cu32.to_le_bytes()); // VolumeIDOffset
        info.extend_from_slice(&(local_offset as u32).to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes()); // CommonNetworkRelativeLinkOffset
        info.extend_from_slice(&(suffix_offset as u32).to_le_bytes());
        info.extend_from_slice(&volume_id);
        info.extend_from_slice(base);
        info.push(0); // fin de la ruta base
        info.push(0); // sufijo vacío
        out.extend(info);

        for text in [working_dir, args].into_iter().flatten() {
            let units: Vec<u16> = text.encode_utf16().collect();
            out.extend_from_slice(&(units.len() as u16).to_le_bytes());
            for unit in units {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::build_lnk;
    use super::*;

    #[test]
    fn reads_target_arguments_and_working_dir_from_a_lnk() {
        let bytes = build_lnk(
            "C:\\Juegos\\Doom\\doom.exe",
            Some("C:\\Juegos\\Doom"),
            Some("-skill 4 +map E1M1"),
        );
        let info = parse_lnk(&bytes, None).unwrap();
        assert_eq!(info.target, Some(PathBuf::from("C:\\Juegos\\Doom\\doom.exe")));
        assert_eq!(info.arguments, "-skill 4 +map E1M1");
        assert_eq!(info.working_dir, Some(PathBuf::from("C:\\Juegos\\Doom")));
    }

    #[test]
    fn a_lnk_without_optional_strings_still_gives_the_target() {
        let info = parse_lnk(&build_lnk("D:\\a.exe", None, None), None).unwrap();
        assert_eq!(info.target, Some(PathBuf::from("D:\\a.exe")));
        assert_eq!(info.arguments, "");
        assert_eq!(info.working_dir, None);
    }

    #[test]
    fn rejects_garbage_and_truncated_data() {
        assert!(parse_lnk(b"esto no es un acceso directo", None).is_err());
        assert!(parse_lnk(&[], None).is_err());
        let mut bytes = build_lnk("C:\\a.exe", Some("C:\\"), Some("-x"));
        bytes.truncate(bytes.len() - 3);
        assert!(parse_lnk(&bytes, None).is_err());
    }

    #[test]
    fn parses_internet_shortcuts() {
        let info = parse_url_file(
            "\u{feff}[InternetShortcut]\r\nURL=steam://rungameid/220\r\nIconFile=C:\\x.ico\r\n",
        );
        assert_eq!(info.url.as_deref(), Some("steam://rungameid/220"));
        assert_eq!(steam_app_id_from_url("steam://rungameid/220"), Some(220));
        assert_eq!(steam_app_id_from_url("STEAM://run/730/"), Some(730));
        assert_eq!(steam_app_id_from_url("https://example.com"), None);
    }

    #[test]
    fn finds_the_app_id_in_steam_exe_arguments() {
        assert_eq!(steam_app_id_from_args("-applaunch 220 -windowed"), Some(220));
        assert_eq!(steam_app_id_from_args("-windowed"), None);
        assert_eq!(steam_app_id_from_args("-applaunch"), None);
    }

    #[test]
    fn relative_targets_are_resolved_lexically() {
        let base = Path::new("juegos").join("doom");
        let resolved = resolve_relative(&base, "../quake/quake.exe");
        assert_eq!(resolved, Path::new("juegos").join("quake").join("quake.exe"));
    }

    #[test]
    fn reads_shortcut_files_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let lnk = tmp.path().join("Doom.lnk");
        fs::write(&lnk, build_lnk("C:\\Doom\\doom.exe", None, Some("-fast"))).unwrap();
        let info = read_shortcut(&lnk).unwrap();
        assert_eq!(info.arguments, "-fast");

        let url = tmp.path().join("Half-Life 2.url");
        fs::write(&url, "[InternetShortcut]\nURL=steam://rungameid/220\n").unwrap();
        assert_eq!(steam_app_id_from_url(&read_shortcut(&url).unwrap().url.unwrap()), Some(220));

        assert!(read_shortcut(&tmp.path().join("a.txt")).is_err());
        assert!(is_shortcut(&lnk) && is_shortcut(&url));
    }
}
