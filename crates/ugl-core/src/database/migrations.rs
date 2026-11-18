//! Migraciones versionadas con `PRAGMA user_version`. Solo se añaden; nunca se editan las ya publicadas.

use std::fs;
use std::path::Path;

use rusqlite::Connection;

use crate::diagnostics;
use crate::error::{Result, UglError};

struct Migration {
    version: i32,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "esquema inicial",
        sql: include_str!("sql/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "logros",
        sql: include_str!("sql/0002_achievements.sql"),
    },
    Migration {
        version: 3,
        name: "iconos de juegos",
        sql: include_str!("sql/0003_game_icons.sql"),
    },
];

pub fn latest_version() -> i32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

pub fn migrate(conn: &mut Connection, db_path: Option<&Path>) -> Result<()> {
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let latest = latest_version();

    if current > latest {
        return Err(UglError::Invalid(format!(
            "La base de datos pertenece a una versión más reciente de UGL (esquema {current}; \
             esta versión entiende hasta el {latest}). Actualiza UGL."
        )));
    }
    if current == latest {
        return Ok(());
    }

    // No perder datos del usuario: copia de seguridad antes de migrar una base existente.
    if current > 0 {
        if let Some(path) = db_path {
            backup_before_migration(conn, path, current)?;
        }
    }

    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        diagnostics::info(&format!(
            "Aplicando migración {}: {}",
            migration.version, migration.name
        ));
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }
    Ok(())
}

fn backup_before_migration(conn: &Connection, path: &Path, from_version: i32) -> Result<()> {
    // Vuelca el WAL al archivo principal para que la copia sea consistente.
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    let backup = path.with_extension(format!("db.bak-v{from_version}"));
    fs::copy(path, &backup)?;
    diagnostics::info(&format!("Copia de seguridad creada: {}", backup.display()));
    Ok(())
}
