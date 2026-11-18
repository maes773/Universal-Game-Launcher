//! Preferencias del usuario, guardadas como pares clave/valor en SQLite.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;

pub const KEY_SORT_ORDER: &str = "library.sort_order";
pub const KEY_LAST_IMPORT_DIR: &str = "import.last_dir";

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let value = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(value)
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}
