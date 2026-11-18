//! Descargas mínimas de imágenes. En Windows usa `urlmon`, que viene con el sistema: UGL no lleva
//! su propia pila TLS ni cliente HTTP. Fuera de Windows no hay descargas todavía.

use std::fs;
use std::path::Path;

use crate::error::{Result, UglError};

/// ¿Empieza como JPEG o PNG? Evita guardar páginas de error HTML como si fueran imágenes.
pub fn looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF]) || bytes.starts_with(&[0x89, b'P', b'N', b'G'])
}

/// Descarga `url` en `dest` (escritura atómica) y comprueba que sea una imagen.
pub fn download_image(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    let _ = fs::remove_file(&part);

    platform::download(url, &part)?;

    let mut head = [0u8; 8];
    let read = {
        use std::io::Read;
        fs::File::open(&part).and_then(|mut f| f.read(&mut head))?
    };
    if read < 4 || !looks_like_image(&head[..read]) {
        let _ = fs::remove_file(&part);
        return Err(UglError::Invalid(format!("{url} no devolvió una imagen")));
    }
    fs::rename(&part, dest)?;
    Ok(())
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use crate::error::{Result, UglError};

    #[link(name = "urlmon")]
    extern "system" {
        fn URLDownloadToFileW(
            caller: *mut c_void,
            url: *const u16,
            file: *const u16,
            reserved: u32,
            callback: *mut c_void,
        ) -> i32;
    }

    pub fn download(url: &str, dest: &Path) -> Result<()> {
        let wide_url: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        let wide_file: Vec<u16> = dest
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: ambos búferes terminan en cero y viven durante la llamada.
        let result = unsafe {
            URLDownloadToFileW(
                std::ptr::null_mut(),
                wide_url.as_ptr(),
                wide_file.as_ptr(),
                0,
                std::ptr::null_mut(),
            )
        };
        if result != 0 {
            let _ = std::fs::remove_file(dest);
            return Err(UglError::Invalid(format!(
                "no se pudo descargar {url} (código 0x{:08X})",
                result as u32
            )));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    use crate::error::{Result, UglError};

    pub fn download(_url: &str, _dest: &Path) -> Result<()> {
        Err(UglError::Invalid(
            "las descargas de imágenes solo están disponibles en Windows".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_jpeg_and_png_but_not_html() {
        assert!(looks_like_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(looks_like_image(&[0x89, b'P', b'N', b'G', 0x0D]));
        assert!(!looks_like_image(b"<!DOCTYPE html>"));
        assert!(!looks_like_image(&[]));
    }
}
