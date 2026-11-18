//! Ubicaciones de los datos del usuario. Nunca dependen de la carpeta desde la
//! que se ejecuta UGL.

use std::fs;
use std::path::PathBuf;

use crate::error::{Result, UglError};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    /// Por defecto: `%LOCALAPPDATA%\UGL`. La variable de entorno `UGL_DATA_DIR`
    /// permite redirigirla (útil para pruebas o para un modo portable).
    pub fn resolve() -> Result<Self> {
        let data_dir = match std::env::var_os("UGL_DATA_DIR") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => dirs::data_local_dir()
                .ok_or_else(|| {
                    UglError::Invalid(
                        "No se pudo determinar la carpeta de datos del usuario.".into(),
                    )
                })?
                .join("UGL"),
        };
        Ok(Self::from_data_dir(data_dir))
    }

    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        Self {
            db_path: data_dir.join("ugl.db"),
            log_path: data_dir.join("logs").join("ugl.log"),
            cache_dir: data_dir.join("cache"),
            data_dir,
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(&self.cache_dir)?;
        if let Some(log_dir) = self.log_path.parent() {
            fs::create_dir_all(log_dir)?;
        }
        Ok(())
    }
}
