//! Migraciones versionadas con `PRAGMA user_version`. Solo se añaden; nunca se editan las ya publicadas.

use std::fs;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::diagnostics;
use crate::domain::path_key;
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
    Migration {
        version: 4,
        name: "métodos de lanzamiento",
        sql: include_str!("sql/0004_launch_methods.sql"),
    },
    Migration {
        version: 5,
        name: "información de la tienda",
        sql: include_str!("sql/0005_game_metadata.sql"),
    },
    Migration {
        version: 6,
        name: "biblioteca de la cuenta de Steam",
        sql: include_str!("sql/0006_account_games.sql"),
    },
];

pub fn latest_version() -> i32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

pub fn migrate(conn: &mut Connection, db_path: Option<&Path>) -> Result<()> {
    migrate_to(conn, db_path, latest_version())
}

fn migrate_to(conn: &mut Connection, db_path: Option<&Path>, target: i32) -> Result<()> {
    let current: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let latest = latest_version();

    if current > latest {
        return Err(UglError::Invalid(format!(
            "La base de datos pertenece a una versión más reciente de UGL (esquema {current}; \
             esta versión entiende hasta el {latest}). Actualiza UGL."
        )));
    }
    if current >= target {
        return Ok(());
    }

    // No perder datos del usuario: copia de seguridad antes de migrar una base existente.
    if current > 0 {
        if let Some(path) = db_path {
            backup_before_migration(conn, path, current)?;
        }
    }

    for migration in MIGRATIONS
        .iter()
        .filter(|m| m.version > current && m.version <= target)
    {
        diagnostics::info(&format!(
            "Aplicando migración {}: {}",
            migration.version, migration.name
        ));
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        if migration.version == 4 {
            fixup_manual_sources(&tx)?;
        }
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }
    Ok(())
}

/// Une una carpeta y una ruta de ejecutable con barra de Windows (la ruta puede ser absoluta).
fn join_windows(base: &str, target: &str) -> String {
    let absolute = target.starts_with("\\\\") || target.as_bytes().get(1) == Some(&b':');
    if absolute {
        target.to_string()
    } else {
        format!(
            "{}\\{}",
            base.trim_end_matches(|c| c == '\\' || c == '/'),
            target
        )
    }
}

/// Hasta la v3 un juego manual se identificaba por su carpeta; ahora, por su ejecutable (así varios
/// ejecutables de una misma carpeta pueden ser juegos distintos). Se actualizan los registros viejos.
fn fixup_manual_sources(conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.id, i.install_path, i.launch_target
             FROM game_sources s
             JOIN installations i ON i.game_id = s.game_id AND i.is_primary = 1
             WHERE s.provider = 'manual' AND i.launch_kind = 'executable'",
        )?;
        let mapped =
            stmt.query_map(params![], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, install_path, target) in rows {
        let key = path_key(Path::new(&join_windows(&install_path, &target)));
        conn.execute(
            "UPDATE OR IGNORE game_sources SET external_id = ?1 WHERE id = ?2",
            params![key, id],
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_data_survives_the_launch_methods_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_to(&mut conn, None, 3).unwrap();

        conn.execute_batch(
            "INSERT INTO games (id, title, sort_title, platform_id, added_at)
                 VALUES (1, 'Mi Juego', 'mi juego', 1, 10), (2, 'Half-Life 2', 'half-life 2', 1, 11);
             INSERT INTO installations (game_id, install_path, launch_kind, launch_target, is_primary, created_at)
                 VALUES (1, 'D:\\Juegos\\Mi Juego', 'executable', 'bin\\juego.exe', 1, 10),
                        (2, 'C:\\Steam\\steamapps\\common\\HL2', 'steam_uri', '220', 1, 11);
             INSERT INTO game_sources (game_id, provider, external_id)
                 VALUES (1, 'manual', 'd:\\juegos\\mi juego'), (2, 'steam', '220');",
        )
        .unwrap();

        migrate_to(&mut conn, None, 4).unwrap();

        let exe: String = conn
            .query_row(
                "SELECT exe_path FROM installations WHERE game_id = 1",
                params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exe, "bin\\juego.exe");
        let app_id: i64 = conn
            .query_row(
                "SELECT steam_app_id FROM installations WHERE game_id = 2",
                params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(app_id, 220);
        let args: String = conn
            .query_row(
                "SELECT launch_args FROM installations WHERE game_id = 1",
                params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(args, "");

        // La identidad del juego manual pasa a ser su ejecutable.
        let key: String = conn
            .query_row(
                "SELECT external_id FROM game_sources WHERE game_id = 1",
                params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(key, "d:\\juegos\\mi juego\\bin\\juego.exe");

        // Los datos siguen enlazados y el nuevo tipo 'uri' ya está permitido.
        let games: i64 = conn
            .query_row("SELECT COUNT(*) FROM installations", params![], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(games, 2);
        conn.execute(
            "UPDATE installations SET launch_kind = 'uri' WHERE game_id = 1",
            params![],
        )
        .unwrap();
    }

    #[test]
    fn deleting_a_game_still_cascades_after_the_rebuild() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate(&mut conn, None).unwrap();
        conn.execute_batch(
            "INSERT INTO games (id, title, sort_title, platform_id, added_at) VALUES (1, 'A', 'a', 1, 1);
             INSERT INTO installations (game_id, install_path, launch_kind, launch_target, created_at)
                 VALUES (1, 'C:\\a', 'executable', 'a.exe', 1);",
        )
        .unwrap();
        conn.execute("DELETE FROM games WHERE id = 1", params![])
            .unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM installations", params![], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(left, 0);
    }
}
