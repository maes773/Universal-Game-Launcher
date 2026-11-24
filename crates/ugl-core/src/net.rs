//! Descargas mínimas de imágenes. En Windows usa `urlmon`, que viene con el sistema: UGL no lleva
//! su propia pila TLS ni cliente HTTP. Fuera de Windows no hay descargas todavía.

use std::fs;
use std::path::Path;

use crate::error::{Result, UglError};

/// ¿Empieza como JPEG o PNG? Evita guardar páginas de error HTML como si fueran imágenes.
pub fn looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF]) || bytes.starts_with(&[0x89, b'P', b'N', b'G'])
}

/// Petición GET a la API de itch.io con la clave en la cabecera `Authorization: Bearer`. Se usa
/// `curl` (incluido en Windows 10 y 11) y la cabecera se le entrega por la entrada estándar, así la
/// clave no aparece en la línea de comandos ni queda en la caché de internet de Windows.
pub fn get_with_bearer(url: &str, token: &str) -> Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    #[cfg(windows)]
    const CURL: &str = "curl.exe";
    #[cfg(not(windows))]
    const CURL: &str = "curl";

    if !url.starts_with("https://api.itch.io/") {
        return Err(UglError::Invalid("Petición no permitida.".into()));
    }
    // La clave se escribe entre comillas en un archivo de configuración de curl: solo caracteres seguros.
    if token.is_empty()
        || !token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(UglError::Invalid("La clave tiene caracteres no válidos.".into()));
    }

    let mut command = Command::new(CURL);
    command
        .args(["--silent", "--show-error", "--max-time", "30", "--config", "-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // sin ventana de consola
    }
    let mut child = command
        .spawn()
        .map_err(|err| UglError::Invalid(format!("No se pudo ejecutar curl: {err}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let config = format!(
            "header = \"Authorization: Bearer {token}\"\nuser-agent = \"UGL (Universal Game Launcher)\"\n"
        );
        stdin.write_all(config.as_bytes())?;
        // Al salir del bloque se cierra la entrada y curl empieza.
    }
    let output = child
        .wait_with_output()
        .map_err(|err| UglError::Invalid(format!("curl no terminó bien: {err}")))?;
    if !output.status.success() {
        return Err(UglError::Invalid(
            "No se pudo consultar itch.io (¿sin conexión?).".into(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Descarga `url` en `dest` (escritura atómica) sin interpretar el contenido (JSON, etc.).
pub fn download_file(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    let _ = fs::remove_file(&part);
    platform::download(url, &part)?;
    fs::rename(&part, dest)?;
    Ok(())
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
