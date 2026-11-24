//! Guardado seguro de secretos (la clave de la API de itch.io). En Windows se cifra con DPAPI:
//! solo puede descifrarla el mismo usuario de Windows en este equipo. Fuera de Windows no se
//! guarda nada (se rechaza), para no dejar una clave en claro en la base de datos.

use crate::error::{Result, UglError};

/// Texto de «entropía» adicional: ata el secreto a UGL.
#[cfg_attr(not(windows), allow(dead_code))]
const ENTROPY: &[u8] = b"UGL-itch-api-key-v1";

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
    win::protect(data)
}

#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    win::unprotect(data)
}

#[cfg(not(windows))]
pub fn protect(_data: &[u8]) -> Result<Vec<u8>> {
    Err(unsupported())
}

#[cfg(not(windows))]
pub fn unprotect(_data: &[u8]) -> Result<Vec<u8>> {
    Err(unsupported())
}

#[cfg(not(windows))]
fn unsupported() -> UglError {
    UglError::Invalid(
        "El guardado cifrado de la clave solo está disponible en Windows.".into(),
    )
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use super::ENTROPY;
    use crate::error::{Result, UglError};

    #[repr(C)]
    struct DataBlob {
        size: u32,
        data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            description: *const u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const DataBlob,
            description: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    fn blob(data: &[u8]) -> DataBlob {
        DataBlob {
            size: data.len() as u32,
            data: data.as_ptr() as *mut u8,
        }
    }

    fn run(data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut output = DataBlob {
            size: 0,
            data: std::ptr::null_mut(),
        };
        // SAFETY: los búferes de entrada viven durante la llamada; la salida la reserva Windows y se
        // libera con LocalFree tras copiarla.
        let ok = unsafe {
            if encrypt {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    &entropy,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    &entropy,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            }
        };
        if ok == 0 || output.data.is_null() {
            return Err(UglError::Invalid(
                "Windows no pudo cifrar o descifrar la clave.".into(),
            ));
        }
        let bytes = unsafe {
            let copy = std::slice::from_raw_parts(output.data, output.size as usize).to_vec();
            LocalFree(output.data as *mut c_void);
            copy
        };
        Ok(bytes)
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        run(data, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let bytes = vec![0u8, 1, 15, 16, 255];
        assert_eq!(to_hex(&bytes), "00010f10ff");
        assert_eq!(from_hex("00010F10ff"), Some(bytes));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex(""), Some(vec![]));
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip() {
        let secret = b"0123456789ABCDEF0123456789ABCDEF";
        let encrypted = protect(secret).unwrap();
        assert_ne!(encrypted.as_slice(), secret.as_slice());
        assert_eq!(unprotect(&encrypted).unwrap(), secret);
    }

    #[cfg(not(windows))]
    #[test]
    fn secrets_are_refused_outside_windows() {
        assert!(protect(b"x").is_err());
    }
}
