//! game_registry: altas, consultas y cambios de los juegos registrados.

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::domain::{
    unix_now, DiscoveredGame, Game, GameId, LaunchKind, RegisterSummary, SortOrder,
};
use crate::error::{Result, UglError};

const GAME_SELECT: &str = "
SELECT g.id,
       g.title,
       p.slug,
       COALESCE((SELECT provider FROM game_sources WHERE game_id = g.id ORDER BY id LIMIT 1), ''),
       i.install_path,
       i.launch_kind,
       i.launch_target,
       g.added_at,
       g.last_played_at,
       (SELECT COUNT(*) FROM achievements WHERE game_id = g.id),
       (SELECT COUNT(*) FROM achievements a JOIN unlocks u ON u.achievement_id = a.id
         WHERE a.game_id = g.id AND u.unlocked = 1),
       g.icon_path
FROM games g
JOIN platforms p ON p.id = g.platform_id
JOIN installations i ON i.id = (
    SELECT id FROM installations WHERE game_id = g.id AND is_primary = 1 ORDER BY id LIMIT 1
)
";

fn map_game(row: &Row<'_>) -> rusqlite::Result<Game> {
    let kind_text: String = row.get(5)?;
    let launch_kind = LaunchKind::parse(&kind_text).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(
            5,
            "launch_kind".to_string(),
            rusqlite::types::Type::Text,
        )
    })?;
    let install_path: String = row.get(4)?;
    Ok(Game {
        id: row.get(0)?,
        title: row.get(1)?,
        platform: row.get(2)?,
        source: row.get(3)?,
        install_path: install_path.into(),
        launch_kind,
        launch_target: row.get(6)?,
        added_at: row.get(7)?,
        last_played_at: row.get(8)?,
        achievements_total: row.get::<_, i64>(9)? as u32,
        achievements_unlocked: row.get::<_, i64>(10)? as u32,
        icon_path: row.get(11)?,
    })
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn list_games(conn: &Connection, query: &str, order: SortOrder) -> Result<Vec<Game>> {
    let order_by = match order {
        SortOrder::Title => "g.sort_title ASC, g.id ASC",
        SortOrder::RecentlyPlayed => {
            "g.last_played_at IS NULL, g.last_played_at DESC, g.sort_title ASC"
        }
        SortOrder::RecentlyAdded => "g.added_at DESC, g.id DESC",
    };
    let sql = format!(
        "{GAME_SELECT} WHERE g.hidden = 0 AND g.title LIKE ?1 ESCAPE '\\' ORDER BY {order_by}"
    );
    let pattern = format!("%{}%", escape_like(query.trim()));
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![pattern], map_game)?;
    let games = rows.collect::<rusqlite::Result<Vec<Game>>>()?;
    Ok(games)
}

pub fn get_game(conn: &Connection, id: GameId) -> Result<Game> {
    let sql = format!("{GAME_SELECT} WHERE g.id = ?1");
    conn.query_row(&sql, params![id], map_game)
        .optional()?
        .ok_or_else(|| UglError::NotFound(format!("el juego {id}")))
}

/// Registra juegos descubiertos por cualquier integración, en una sola transacción.
/// Evita duplicados por (proveedor, id externo).
pub fn register(conn: &mut Connection, games: &[DiscoveredGame]) -> Result<RegisterSummary> {
    let tx = conn.transaction()?;
    let mut summary = RegisterSummary::default();
    let now = unix_now();

    for game in games {
        // Nota: las rutas se guardan como texto; una ruta con caracteres no UTF-8 se convertiría con pérdida.
        let install_path = game.install_path.to_string_lossy().into_owned();

        let existing: Option<GameId> = tx
            .query_row(
                "SELECT game_id FROM game_sources WHERE provider = ?1 AND external_id = ?2",
                params![game.provider, game.external_id],
                |row| row.get(0),
            )
            .optional()?;

        match existing {
            Some(game_id) => {
                // Los juegos de Steam cambian de biblioteca al moverse; los manuales conservan
                // la ruta que el usuario haya fijado.
                let refreshed = game.launch_kind == LaunchKind::SteamUri
                    && tx.execute(
                        "UPDATE installations SET install_path = ?1
                         WHERE game_id = ?2 AND is_primary = 1 AND install_path <> ?1",
                        params![install_path, game_id],
                    )? > 0;
                if refreshed {
                    summary.updated += 1;
                } else {
                    summary.already_present += 1;
                }
            }
            None => {
                let platform_id: i64 = tx
                    .query_row(
                        "SELECT id FROM platforms WHERE slug = ?1",
                        params![game.platform_slug],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        UglError::Invalid(format!("Plataforma desconocida: {}", game.platform_slug))
                    })?;

                tx.execute(
                    "INSERT INTO games (title, sort_title, platform_id, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![game.title, game.title.to_lowercase(), platform_id, now],
                )?;
                let game_id = tx.last_insert_rowid();

                tx.execute(
                    "INSERT INTO installations
                         (game_id, install_path, launch_kind, launch_target, is_primary, created_at)
                     VALUES (?1, ?2, ?3, ?4, 1, ?5)",
                    params![
                        game_id,
                        install_path,
                        game.launch_kind.as_str(),
                        game.launch_target,
                        now
                    ],
                )?;
                tx.execute(
                    "INSERT INTO game_sources (game_id, provider, external_id) VALUES (?1, ?2, ?3)",
                    params![game_id, game.provider, game.external_id],
                )?;
                summary.added += 1;
            }
        }
    }

    tx.commit()?;
    Ok(summary)
}

pub fn touch_last_played(conn: &Connection, id: GameId) -> Result<()> {
    conn.execute(
        "UPDATE games SET last_played_at = ?1 WHERE id = ?2",
        params![unix_now(), id],
    )?;
    Ok(())
}

pub fn set_executable(conn: &Connection, id: GameId, relative_path: &str) -> Result<()> {
    let changed = conn.execute(
        "UPDATE installations SET launch_target = ?1
         WHERE game_id = ?2 AND is_primary = 1 AND launch_kind = 'executable'",
        params![relative_path, id],
    )?;
    if changed == 0 {
        return Err(UglError::NotFound(format!(
            "la instalación con ejecutable del juego {id}"
        )));
    }
    Ok(())
}

pub fn set_install_path(conn: &Connection, id: GameId, path: &str) -> Result<()> {
    let changed = conn.execute(
        "UPDATE installations SET install_path = ?1
         WHERE game_id = ?2 AND is_primary = 1 AND launch_kind = 'executable'",
        params![path, id],
    )?;
    if changed == 0 {
        return Err(UglError::NotFound(format!(
            "la instalación manual del juego {id}"
        )));
    }
    Ok(())
}

/// Quita el juego de la biblioteca de UGL. No toca ningún archivo del disco.
pub fn remove_game(conn: &Connection, id: GameId) -> Result<()> {
    let changed = conn.execute("DELETE FROM games WHERE id = ?1", params![id])?;
    if changed == 0 {
        return Err(UglError::NotFound(format!("el juego {id}")));
    }
    Ok(())
}

/// Guarda la ruta del icono en caché. Una cadena vacía significa «se intentó y no hay icono».
pub fn set_icon_path(conn: &Connection, id: GameId, path: &str) -> Result<()> {
    conn.execute(
        "UPDATE games SET icon_path = ?1 WHERE id = ?2",
        params![path, id],
    )?;
    Ok(())
}

/// Olvida el icono para que se vuelva a extraer (por ejemplo, tras cambiar el ejecutable).
pub fn clear_icon(conn: &Connection, id: GameId) -> Result<()> {
    conn.execute("UPDATE games SET icon_path = NULL WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn games_without_icon(conn: &Connection, limit: usize) -> Result<Vec<Game>> {
    let sql = format!(
        "{GAME_SELECT} WHERE g.hidden = 0 AND g.icon_path IS NULL ORDER BY g.id LIMIT ?1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![limit as i64], map_game)?;
    let games = rows.collect::<rusqlite::Result<Vec<Game>>>()?;
    Ok(games)
}

pub fn find_game_by_source(
    conn: &Connection,
    provider: &str,
    external_id: &str,
) -> Result<Option<GameId>> {
    let found = conn
        .query_row(
            "SELECT game_id FROM game_sources WHERE provider = ?1 AND external_id = ?2",
            params![provider, external_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::database::Database;

    fn steam_game(app_id: u64, name: &str, path: &str) -> DiscoveredGame {
        DiscoveredGame {
            provider: "steam".into(),
            external_id: app_id.to_string(),
            title: name.into(),
            platform_slug: "pc".into(),
            install_path: PathBuf::from(path),
            launch_kind: LaunchKind::SteamUri,
            launch_target: app_id.to_string(),
        }
    }

    #[test]
    fn register_twice_does_not_duplicate() {
        let mut db = Database::open_in_memory().unwrap();
        let games = [steam_game(220, "Half-Life 2", "C:\\Steam\\common\\HL2")];

        let first = register(db.conn_mut(), &games).unwrap();
        assert_eq!(first.added, 1);
        let second = register(db.conn_mut(), &games).unwrap();
        assert_eq!(second.added, 0);
        assert_eq!(second.already_present, 1);

        assert_eq!(
            list_games(db.conn(), "", SortOrder::Title).unwrap().len(),
            1
        );
    }

    #[test]
    fn steam_reimport_refreshes_install_path() {
        let mut db = Database::open_in_memory().unwrap();
        register(
            db.conn_mut(),
            &[steam_game(220, "Half-Life 2", "C:\\Steam\\common\\HL2")],
        )
        .unwrap();
        let summary = register(
            db.conn_mut(),
            &[steam_game(220, "Half-Life 2", "D:\\Lib\\common\\HL2")],
        )
        .unwrap();
        assert_eq!(summary.updated, 1);

        let game = list_games(db.conn(), "", SortOrder::Title).unwrap().remove(0);
        assert_eq!(game.install_path, PathBuf::from("D:\\Lib\\common\\HL2"));
    }

    #[test]
    fn search_treats_percent_literally() {
        let mut db = Database::open_in_memory().unwrap();
        register(
            db.conn_mut(),
            &[
                steam_game(1, "100% Orange Juice", "C:\\a"),
                steam_game(2, "Half-Life", "C:\\b"),
            ],
        )
        .unwrap();
        let found = list_games(db.conn(), "%", SortOrder::Title).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "100% Orange Juice");
    }

    #[test]
    fn remove_cascades_to_installations_and_sources() {
        let mut db = Database::open_in_memory().unwrap();
        register(db.conn_mut(), &[steam_game(220, "Half-Life 2", "C:\\a")]).unwrap();
        let id = list_games(db.conn(), "", SortOrder::Title).unwrap()[0].id;
        remove_game(db.conn(), id).unwrap();

        let installs: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM installations", params![], |r| r.get(0))
            .unwrap();
        let sources: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM game_sources", params![], |r| r.get(0))
            .unwrap();
        assert_eq!(installs, 0);
        assert_eq!(sources, 0);
    }

    #[test]
    fn icon_state_goes_from_unknown_to_known() {
        let mut db = Database::open_in_memory().unwrap();
        register(db.conn_mut(), &[steam_game(1, "Alfa", "C:\\a")]).unwrap();
        let id = list_games(db.conn(), "", SortOrder::Title).unwrap()[0].id;

        assert_eq!(games_without_icon(db.conn(), 10).unwrap().len(), 1);
        set_icon_path(db.conn(), id, "").unwrap(); // se intentó y no hay icono
        assert!(games_without_icon(db.conn(), 10).unwrap().is_empty());
        assert_eq!(get_game(db.conn(), id).unwrap().icon_path.as_deref(), Some(""));

        clear_icon(db.conn(), id).unwrap();
        assert_eq!(games_without_icon(db.conn(), 10).unwrap().len(), 1);
    }

    #[test]
    fn finds_a_game_by_its_source() {
        let mut db = Database::open_in_memory().unwrap();
        register(db.conn_mut(), &[steam_game(220, "Half-Life 2", "C:\\a")]).unwrap();
        assert!(find_game_by_source(db.conn(), "steam", "220").unwrap().is_some());
        assert!(find_game_by_source(db.conn(), "steam", "999").unwrap().is_none());
    }

    #[test]
    fn recently_played_orders_played_games_first() {
        let mut db = Database::open_in_memory().unwrap();
        register(
            db.conn_mut(),
            &[steam_game(1, "Alfa", "C:\\a"), steam_game(2, "Beta", "C:\\b")],
        )
        .unwrap();
        let beta = list_games(db.conn(), "beta", SortOrder::Title).unwrap()[0].id;
        touch_last_played(db.conn(), beta).unwrap();

        let ordered = list_games(db.conn(), "", SortOrder::RecentlyPlayed).unwrap();
        assert_eq!(ordered[0].title, "Beta");
    }
}
