use thiserror::Error;

#[derive(Debug, Error)]
pub enum UglError {
    #[error("error de base de datos: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("error de archivos: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("no encontrado: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, UglError>;
