//! Preferencias del usuario, guardadas como pares clave/valor en SQLite.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;

pub const KEY_SORT_ORDER: &str = "library.sort_order";
pub const KEY_LAST_IMPORT_DIR: &str = "import.last_dir";
pub const KEY_STEAM_ROOT: &str = "steam.root";
pub const KEY_STEAM_LIBRARIES: &str = "steam.libraries";
pub const KEY_AUTO_UPDATE: &str = "auto.update";
pub const KEY_DOWNLOAD_ICONS: &str = "net.download_icons";
pub const KEY_FETCH_METADATA: &str = "net.fetch_metadata";
pub const KEY_ACCOUNT_ID: &str = "steam.account.id";
pub const KEY_ACCOUNT_PERSONA: &str = "steam.account.persona";
pub const KEY_ACCOUNT_VERIFIED: &str = "steam.account.verified";
/// Clave de la Web API cifrada (DPAPI) y en hexadecimal; nunca en claro.
pub const KEY_ACCOUNT_KEY: &str = "steam.account.key";
pub const KEY_ACCOUNT_SYNC: &str = "steam.account.last_sync";
pub const KEY_SHOW_OWNED: &str = "steam.account.show_owned";
pub const KEY_MINIMIZE_ON_LAUNCH: &str = "bg.minimize_on_launch";

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

pub fn remove(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
    Ok(())
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}
