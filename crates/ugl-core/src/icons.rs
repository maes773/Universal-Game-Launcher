//! Iconos de los juegos: el mismo que muestra el ejecutable en Windows.
//! Se extrae una sola vez en segundo plano y se guarda como PNG en la caché de UGL.

use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::{GameId, LaunchKind};
use crate::error::{Result, UglError};
use crate::integrations::manual;
use crate::png;

/// Lado del icono que se pide al sistema (se guarda tal cual).
pub const ICON_SIZE: u32 = 64;

pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// RGBA sin premultiplicar.
    pub pixels: Vec<u8>,
}

/// Lo necesario para obtener el icono de un juego fuera del hilo de la interfaz.
#[derive(Debug, Clone)]
pub struct IconJob {
    pub game_id: GameId,
    pub install_path: PathBuf,
    pub launch_kind: LaunchKind,
    pub launch_target: String,
    /// Ejecutable directo conocido (aunque el juego se inicie por Steam).
    pub exe_path: Option<String>,
}

pub fn icon_file(cache_dir: &Path, game_id: GameId) -> PathBuf {
    cache_dir.join("icons").join(format!("game-{game_id}.png"))
}

/// Ejecutable cuyo icono representa al juego. Los juegos propios usan el que el usuario eligió;
/// los de Steam, el mejor candidato de su carpeta (solo si hay una propuesta razonable).
pub fn icon_source_exe(job: &IconJob) -> Option<PathBuf> {
    // 1) El ejecutable que ya se conoce (el elegido por el usuario o el de la importación).
    let known = match job.launch_kind {
        LaunchKind::Executable => Some(job.launch_target.as_str()),
        _ => job.exe_path.as_deref(),
    };
    if let Some(relative) = known.filter(|path| !path.is_empty()) {
        let exe = job.install_path.join(relative);
        if exe.is_file() {
            return Some(exe);
        }
        if job.launch_kind == LaunchKind::Executable {
            return None;
        }
    }
    // 2) Juego de Steam sin ejecutable conocido: el mejor candidato de su carpeta.
    if job.launch_kind == LaunchKind::SteamUri {
        let scan = manual::scan_game_folder(&job.install_path).ok()?;
        let best = scan.candidates.first().filter(|c| c.suggested)?;
        return Some(job.install_path.join(&best.relative));
    }
    None
}

/// Extrae el icono y lo guarda en la caché. `Ok(None)` = el juego no tiene ejecutable del que
/// sacar un icono.
pub fn extract_and_cache(job: &IconJob, cache_dir: &Path) -> Result<Option<PathBuf>> {
    let Some(exe) = icon_source_exe(job) else {
        return Ok(None);
    };
    let image = extract_exe_icon(&exe, ICON_SIZE)?;
    let bytes = png::encode_rgba(image.width, image.height, &image.pixels)
        .ok_or_else(|| UglError::Invalid("el icono tiene dimensiones no válidas".into()))?;

    let dest = icon_file(cache_dir, job.game_id);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("png.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &dest)?;
    Ok(Some(dest))
}

#[cfg(windows)]
pub fn extract_exe_icon(path: &Path, size: u32) -> Result<RgbaImage> {
    win::extract(path, size)
}

#[cfg(not(windows))]
pub fn extract_exe_icon(_path: &Path, _size: u32) -> Result<RgbaImage> {
    Err(UglError::Invalid(
        "la extracción de iconos de ejecutables solo está disponible en Windows".into(),
    ))
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use super::RgbaImage;
    use crate::error::{Result, UglError};

    // Los identificadores de Windows (HICON, HBITMAP, HDC…) tienen el tamaño de un puntero.
    type Handle = isize;

    #[repr(C)]
    struct IconInfo {
        f_icon: i32,
        x_hotspot: u32,
        y_hotspot: u32,
        hbm_mask: Handle,
        hbm_color: Handle,
    }

    #[repr(C)]
    struct BitmapHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        x_pels_per_meter: i32,
        y_pels_per_meter: i32,
        clr_used: u32,
        clr_important: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        header: BitmapHeader,
        colors: [u32; 1],
    }

    #[repr(C)]
    struct BitmapObject {
        bm_type: i32,
        width: i32,
        height: i32,
        width_bytes: i32,
        planes: u16,
        bits_pixel: u16,
        bits: *mut c_void,
    }

    #[link(name = "user32")]
    extern "system" {
        fn PrivateExtractIconsW(
            file: *const u16,
            index: i32,
            cx: i32,
            cy: i32,
            icons: *mut Handle,
            ids: *mut u32,
            count: u32,
            flags: u32,
        ) -> u32;
        fn GetIconInfo(icon: Handle, info: *mut IconInfo) -> i32;
        fn DestroyIcon(icon: Handle) -> i32;
        fn GetDC(window: Handle) -> Handle;
        fn ReleaseDC(window: Handle, dc: Handle) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn GetObjectW(handle: Handle, size: i32, out: *mut c_void) -> i32;
        fn GetDIBits(
            dc: Handle,
            bitmap: Handle,
            start: u32,
            lines: u32,
            bits: *mut c_void,
            info: *mut BitmapInfo,
            usage: u32,
        ) -> i32;
        fn DeleteObject(object: Handle) -> i32;
    }

    pub fn extract(path: &Path, size: u32) -> Result<RgbaImage> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut icon: Handle = 0;
        // SAFETY: `wide` termina en cero; `icon` recibe como mucho un identificador.
        let count = unsafe {
            PrivateExtractIconsW(
                wide.as_ptr(),
                0,
                size as i32,
                size as i32,
                &mut icon,
                std::ptr::null_mut(),
                1,
                0,
            )
        };
        if count == 0 || count == u32::MAX || icon == 0 {
            return Err(UglError::NotFound(format!(
                "un icono en {}",
                path.display()
            )));
        }
        // SAFETY: `icon` es un identificador válido que se destruye justo después.
        let result = unsafe { icon_to_rgba(icon) };
        unsafe {
            DestroyIcon(icon);
        }
        result
    }

    unsafe fn icon_to_rgba(icon: Handle) -> Result<RgbaImage> {
        let mut info = IconInfo {
            f_icon: 0,
            x_hotspot: 0,
            y_hotspot: 0,
            hbm_mask: 0,
            hbm_color: 0,
        };
        if GetIconInfo(icon, &mut info) == 0 {
            return Err(UglError::Invalid(
                "Windows no pudo leer la información del icono".into(),
            ));
        }
        let result = read_bitmaps(&info);
        if info.hbm_color != 0 {
            DeleteObject(info.hbm_color);
        }
        if info.hbm_mask != 0 {
            DeleteObject(info.hbm_mask);
        }
        result
    }

    unsafe fn read_bitmaps(info: &IconInfo) -> Result<RgbaImage> {
        if info.hbm_color == 0 {
            return Err(UglError::Invalid(
                "el icono es monocromo y no está soportado".into(),
            ));
        }
        let mut object = BitmapObject {
            bm_type: 0,
            width: 0,
            height: 0,
            width_bytes: 0,
            planes: 0,
            bits_pixel: 0,
            bits: std::ptr::null_mut(),
        };
        let filled = GetObjectW(
            info.hbm_color,
            std::mem::size_of::<BitmapObject>() as i32,
            &mut object as *mut BitmapObject as *mut c_void,
        );
        let (width, height) = (object.width, object.height);
        if filled == 0 || width <= 0 || height <= 0 || width > 512 || height > 512 {
            return Err(UglError::Invalid(
                "el icono tiene un tamaño no válido".into(),
            ));
        }

        let dc = GetDC(0);
        if dc == 0 {
            return Err(UglError::Invalid(
                "no se pudo obtener un contexto gráfico".into(),
            ));
        }
        let color = dib_bgra(dc, info.hbm_color, width, height);
        let mask = dib_bgra(dc, info.hbm_mask, width, height);
        ReleaseDC(0, dc);

        let mut pixels =
            color.ok_or_else(|| UglError::Invalid("no se pudieron leer los píxeles del icono".into()))?;

        // Algunos iconos antiguos no traen canal alfa: la transparencia viene de la máscara
        // (negro = opaco).
        let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
        if !has_alpha {
            match mask {
                Some(mask) => {
                    for (p, m) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                        p[3] = if m[0] == 0 { 255 } else { 0 };
                    }
                }
                None => {
                    for p in pixels.chunks_exact_mut(4) {
                        p[3] = 255;
                    }
                }
            }
        }
        // BGRA → RGBA
        for p in pixels.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        Ok(RgbaImage {
            width: width as u32,
            height: height as u32,
            pixels,
        })
    }

    /// Lee un mapa de bits como BGRA de 32 bits, de arriba abajo.
    unsafe fn dib_bgra(dc: Handle, bitmap: Handle, width: i32, height: i32) -> Option<Vec<u8>> {
        if bitmap == 0 {
            return None;
        }
        let mut info = BitmapInfo {
            header: BitmapHeader {
                size: std::mem::size_of::<BitmapHeader>() as u32,
                width,
                height: -height, // negativo = de arriba abajo
                planes: 1,
                bit_count: 32,
                compression: 0, // BI_RGB
                size_image: 0,
                x_pels_per_meter: 0,
                y_pels_per_meter: 0,
                clr_used: 0,
                clr_important: 0,
            },
            colors: [0],
        };
        let mut buffer = vec![0u8; width as usize * height as usize * 4];
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            buffer.as_mut_ptr() as *mut c_void,
            &mut info,
            0, // DIB_RGB_COLORS
        );
        if lines == 0 {
            None
        } else {
            Some(buffer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(dir: &Path, kind: LaunchKind, target: &str) -> IconJob {
        IconJob {
            game_id: 7,
            install_path: dir.to_path_buf(),
            launch_kind: kind,
            launch_target: target.to_string(),
            exe_path: None,
        }
    }

    #[test]
    fn manual_games_use_their_chosen_executable() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("bin")).unwrap();
        fs::write(tmp.path().join("bin").join("juego.exe"), b"MZ").unwrap();

        let found = icon_source_exe(&job(tmp.path(), LaunchKind::Executable, "bin/juego.exe"));
        assert_eq!(found, Some(tmp.path().join("bin/juego.exe")));
        assert!(icon_source_exe(&job(tmp.path(), LaunchKind::Executable, "no_existe.exe")).is_none());
    }

    #[test]
    fn steam_games_use_the_best_candidate_in_their_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Half-Life 2");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("hl2.exe"), b"MZ").unwrap();
        fs::write(dir.join("unins000.exe"), b"MZ").unwrap();

        let found = icon_source_exe(&job(&dir, LaunchKind::SteamUri, "220"));
        assert_eq!(found, Some(dir.join("hl2.exe")));
    }

    #[test]
    fn a_known_executable_wins_over_the_steam_heuristic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Juego");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("juego.exe"), b"MZ").unwrap();
        fs::write(dir.join("otro.exe"), b"MZ").unwrap();

        let mut steam_job = job(&dir, LaunchKind::SteamUri, "1");
        steam_job.exe_path = Some("otro.exe".to_string());
        assert_eq!(icon_source_exe(&steam_job), Some(dir.join("otro.exe")));
    }

    #[test]
    fn steam_folder_with_only_junk_has_no_icon_source() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Algo");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("unins000.exe"), b"MZ").unwrap();
        assert!(icon_source_exe(&job(&dir, LaunchKind::SteamUri, "1")).is_none());
    }

    #[test]
    fn icon_files_live_in_the_cache_folder() {
        assert_eq!(
            icon_file(Path::new("cache"), 7),
            Path::new("cache").join("icons").join("game-7.png")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn extraction_is_reported_as_unsupported_outside_windows() {
        assert!(extract_exe_icon(Path::new("x.exe"), 64).is_err());
    }
}
