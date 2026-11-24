//! game_registry: altas, consultas y cambios de los juegos registrados.

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::domain::{
    unix_now, DiscoveredGame, Game, GameId, ItchGame, LaunchKind, MetadataRecord, RegisterSummary,
    SortOrder,
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
       g.icon_path,
       i.exe_path,
       i.launch_args,
       i.working_dir,
       i.steam_app_id
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
        exe_path: row.get(12)?,
        launch_args: row.get(13)?,
        working_dir: row.get(14)?,
        steam_app_id: row.get::<_, Option<i64>>(15)?.map(|id| id as u64),
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
                         (game_id, install_path, launch_kind, launch_target, exe_path,
                          launch_args, working_dir, steam_app_id, is_primary, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)",
                    params![
                        game_id,
                        install_path,
                        game.launch_kind.as_str(),
                        game.launch_target,
                        game.exe_path,
                        game.launch_args,
                        game.working_dir,
                        game.steam_app_id.map(|id| id as i64),
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
        "UPDATE installations SET launch_target = ?1, exe_path = ?1
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

fn join_list(items: &[String]) -> String {
    items.join("\n")
}

fn split_list(text: Option<String>) -> Vec<String> {
    text.unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Guarda (o reemplaza) la ficha de la tienda de un juego.
pub fn save_metadata(conn: &Connection, game_id: GameId, record: &MetadataRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO game_metadata
             (game_id, steam_app_id, status, name, short_description, developers, publishers,
              genres, release_date, website, cover_path, language, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'spanish', ?12)
         ON CONFLICT(game_id) DO UPDATE SET
             steam_app_id = excluded.steam_app_id, status = excluded.status, name = excluded.name,
             short_description = excluded.short_description, developers = excluded.developers,
             publishers = excluded.publishers, genres = excluded.genres,
             release_date = excluded.release_date, website = excluded.website,
             cover_path = excluded.cover_path, fetched_at = excluded.fetched_at",
        params![
            game_id,
            record.steam_app_id as i64,
            if record.available { "ok" } else { "unavailable" },
            record.name,
            record.short_description,
            join_list(&record.developers),
            join_list(&record.publishers),
            join_list(&record.genres),
            record.release_date,
            record.website,
            record.cover_path,
            unix_now()
        ],
    )?;
    Ok(())
}

pub fn get_metadata(conn: &Connection, game_id: GameId) -> Result<Option<MetadataRecord>> {
    let found = conn
        .query_row(
            "SELECT steam_app_id, status, name, short_description, developers, publishers,
                    genres, release_date, website, cover_path
             FROM game_metadata WHERE game_id = ?1",
            params![game_id],
            |row| {
                Ok(MetadataRecord {
                    steam_app_id: row.get::<_, i64>(0)? as u64,
                    available: row.get::<_, String>(1)? == "ok",
                    name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    short_description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    developers: split_list(row.get(4)?),
                    publishers: split_list(row.get(5)?),
                    genres: split_list(row.get(6)?),
                    release_date: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    website: row.get(8)?,
                    cover_path: row.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(found)
}

pub fn delete_metadata(conn: &Connection, game_id: GameId) -> Result<()> {
    conn.execute("DELETE FROM game_metadata WHERE game_id = ?1", params![game_id])?;
    Ok(())
}

/// Juegos con AppID conocido cuya ficha aún no está guardada (o es de otro AppID).
pub fn games_needing_metadata(conn: &Connection, limit: usize) -> Result<Vec<(GameId, u64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT i.game_id, i.steam_app_id
         FROM installations i
         JOIN games g ON g.id = i.game_id
         LEFT JOIN game_metadata m ON m.game_id = i.game_id AND m.steam_app_id = i.steam_app_id
         WHERE i.is_primary = 1 AND i.steam_app_id IS NOT NULL AND g.hidden = 0 AND m.game_id IS NULL
         ORDER BY i.game_id
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok((row.get::<_, GameId>(0)?, row.get::<_, i64>(1)? as u64))
    })?;
    let jobs = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(jobs)
}

/// Asocia (o desasocia) un juego con una ficha de Steam sin cambiar su método de inicio.
pub fn set_steam_app_id(conn: &Connection, id: GameId, app_id: Option<u64>) -> Result<()> {
    let changed = conn.execute(
        "UPDATE installations SET steam_app_id = ?1 WHERE game_id = ?2 AND is_primary = 1",
        params![app_id.map(|v| v as i64), id],
    )?;
    if changed == 0 {
        return Err(UglError::NotFound(format!("el juego {id}")));
    }
    Ok(())
}

pub fn find_game_by_steam_app_id(conn: &Connection, app_id: u64) -> Result<Option<GameId>> {
    let found = conn
        .query_row(
            "SELECT game_id FROM installations
             WHERE steam_app_id = ?1 AND is_primary = 1 ORDER BY game_id LIMIT 1",
            params![app_id as i64],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found)
}

pub fn rename_game(conn: &Connection, id: GameId, title: &str) -> Result<()> {
    let changed = conn.execute(
        "UPDATE games SET title = ?1, sort_title = ?2 WHERE id = ?3",
        params![title, title.to_lowercase(), id],
    )?;
    if changed == 0 {
        return Err(UglError::NotFound(format!("el juego {id}")));
    }
    Ok(())
}

/// Condición SQL: el juego de itch.io (alias `a`) no está ya en la biblioteca local con ese título.
const ITCH_NOT_IN_LIBRARY: &str =
    "NOT EXISTS (SELECT 1 FROM games g WHERE lower(g.title) = lower(a.title))";

/// Sustituye la copia local de la biblioteca de itch.io por la que devuelve la API.
pub fn replace_itch_games(conn: &mut Connection, games: &[ItchGame]) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM itch_games", params![])?;
    let now = unix_now();
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO itch_games
                 (game_id, title, author, url, description, cover_url, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for game in games {
            stmt.execute(params![
                game.game_id as i64,
                game.title,
                game.author,
                game.url,
                game.description,
                game.cover_url,
                now
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn clear_itch_games(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM itch_games", params![])?;
    Ok(())
}

/// Juegos de la cuenta de itch.io que no están en la biblioteca local (los «no instalados»).
pub fn list_itch_not_installed(conn: &Connection, query: &str) -> Result<Vec<ItchGame>> {
    let sql = format!(
        "SELECT a.game_id, a.title, a.author, a.url, a.description, a.cover_url
         FROM itch_games a
         WHERE a.title LIKE ?1 ESCAPE '\\' AND {ITCH_NOT_IN_LIBRARY}
         ORDER BY lower(a.title) ASC"
    );
    let pattern = format!("%{}%", escape_like(query.trim()));
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![pattern], |row| {
        Ok(ItchGame {
            game_id: row.get::<_, i64>(0)? as u64,
            title: row.get(1)?,
            author: row.get(2)?,
            url: row.get(3)?,
            description: row.get(4)?,
            cover_url: row.get(5)?,
        })
    })?;
    let games = rows.collect::<rusqlite::Result<Vec<ItchGame>>>()?;
    Ok(games)
}

/// (total en la cuenta, de ellos sin instalar).
pub fn count_itch_games(conn: &Connection) -> Result<(usize, usize)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM itch_games", params![], |r| r.get(0))?;
    let missing: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM itch_games a WHERE {ITCH_NOT_IN_LIBRARY}"),
        params![],
        |r| r.get(0),
    )?;
    Ok((total as usize, missing as usize))
}

/// Método de lanzamiento completo de un juego (lo valida `Core::update_launch`).
pub struct LaunchRow<'a> {
    pub kind: LaunchKind,
    pub target: &'a str,
    pub exe_path: Option<&'a str>,
    pub args: &'a str,
    pub working_dir: Option<&'a str>,
    pub steam_app_id: Option<u64>,
}

pub fn update_launch(conn: &Connection, id: GameId, row: &LaunchRow<'_>) -> Result<()> {
    let changed = conn.execute(
        "UPDATE installations
         SET launch_kind = ?1, launch_target = ?2, exe_path = ?3,
             launch_args = ?4, working_dir = ?5, steam_app_id = ?6
         WHERE game_id = ?7 AND is_primary = 1",
        params![
            row.kind.as_str(),
            row.target,
            row.exe_path,
            row.args,
            row.working_dir,
            row.steam_app_id.map(|app_id| app_id as i64),
            id
        ],
    )?;
    if changed == 0 {
        return Err(UglError::NotFound(format!(
            "la instalación principal del juego {id}"
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
            steam_app_id: Some(app_id),
            ..Default::default()
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

    fn sample_record(app_id: u64) -> MetadataRecord {
        MetadataRecord {
            steam_app_id: app_id,
            available: true,
            name: "Half-Life 2".into(),
            short_description: "Descripción".into(),
            developers: vec!["Valve".into()],
            publishers: vec!["Valve".into(), "Otro".into()],
            genres: vec!["Acción".into()],
            release_date: "16 NOV 2004".into(),
            website: Some("http://x".into()),
            cover_path: Some("c.jpg".into()),
        }
    }

    #[test]
    fn metadata_roundtrip_and_pending_jobs() {
        let mut db = Database::open_in_memory().unwrap();
        register(db.conn_mut(), &[steam_game(220, "Half-Life 2", "C:\\a")]).unwrap();
        let id = list_games(db.conn(), "", SortOrder::Title).unwrap()[0].id;

        assert_eq!(games_needing_metadata(db.conn(), 10).unwrap(), vec![(id, 220)]);
        save_metadata(db.conn(), id, &sample_record(220)).unwrap();
        assert!(games_needing_metadata(db.conn(), 10).unwrap().is_empty());
        assert_eq!(get_metadata(db.conn(), id).unwrap(), Some(sample_record(220)));

        // Cambiar la asociación manualmente obliga a pedir la ficha nueva.
        set_steam_app_id(db.conn(), id, Some(70)).unwrap();
        assert_eq!(games_needing_metadata(db.conn(), 10).unwrap(), vec![(id, 70)]);
        delete_metadata(db.conn(), id).unwrap();
        assert!(get_metadata(db.conn(), id).unwrap().is_none());
    }

    #[test]
    fn unavailable_pages_are_remembered_and_games_can_be_found_by_app_id() {
        let mut db = Database::open_in_memory().unwrap();
        register(db.conn_mut(), &[steam_game(5, "X", "C:\\a")]).unwrap();
        let id = list_games(db.conn(), "", SortOrder::Title).unwrap()[0].id;
        let record = MetadataRecord { steam_app_id: 5, available: false, ..Default::default() };
        save_metadata(db.conn(), id, &record).unwrap();
        assert!(games_needing_metadata(db.conn(), 10).unwrap().is_empty());
        assert!(!get_metadata(db.conn(), id).unwrap().unwrap().available);
        assert_eq!(find_game_by_steam_app_id(db.conn(), 5).unwrap(), Some(id));
        assert_eq!(find_game_by_steam_app_id(db.conn(), 6).unwrap(), None);
    }

    #[test]
    fn renaming_keeps_the_sort_title_in_sync() {
        let mut db = Database::open_in_memory().unwrap();
        register(db.conn_mut(), &[steam_game(1, "zzz", "C:\\a"), steam_game(2, "mmm", "C:\\b")]).unwrap();
        let id = list_games(db.conn(), "zzz", SortOrder::Title).unwrap()[0].id;
        rename_game(db.conn(), id, "Aaa Nuevo").unwrap();
        let first = list_games(db.conn(), "", SortOrder::Title).unwrap();
        assert_eq!(first[0].title, "Aaa Nuevo");
    }

    fn itch(game_id: u64, title: &str) -> ItchGame {
        ItchGame {
            game_id,
            title: title.into(),
            author: "Autor".into(),
            url: format!("https://autor.itch.io/juego-{game_id}"),
            description: String::new(),
            cover_url: None,
        }
    }

    #[test]
    fn itch_games_hide_what_is_already_in_the_library_by_title() {
        let mut db = Database::open_in_memory().unwrap();
        replace_itch_games(db.conn_mut(), &[itch(1, "Celeste Clon"), itch(2, "Zeta Quest"), itch(3, "alfa")]).unwrap();
        assert_eq!(count_itch_games(db.conn()).unwrap(), (3, 3));

        // «Alfa» ya está instalado (aunque con otras mayúsculas): deja de ser «no instalado».
        register(db.conn_mut(), &[steam_game(10, "Alfa", "C:\\a")]).unwrap();
        let titles: Vec<String> = list_itch_not_installed(db.conn(), "")
            .unwrap()
            .into_iter()
            .map(|g| g.title)
            .collect();
        assert_eq!(titles, vec!["Celeste Clon", "Zeta Quest"]);
        assert_eq!(count_itch_games(db.conn()).unwrap(), (3, 2));
        assert_eq!(list_itch_not_installed(db.conn(), "zeta").unwrap().len(), 1);
        assert_eq!(list_itch_not_installed(db.conn(), "%").unwrap().len(), 0);
    }

    #[test]
    fn replacing_the_itch_library_discards_the_old_copy() {
        let mut db = Database::open_in_memory().unwrap();
        replace_itch_games(db.conn_mut(), &[itch(1, "A"), itch(2, "B")]).unwrap();
        replace_itch_games(db.conn_mut(), &[itch(3, "C")]).unwrap();
        assert_eq!(count_itch_games(db.conn()).unwrap(), (1, 1));
        clear_itch_games(db.conn()).unwrap();
        assert_eq!(count_itch_games(db.conn()).unwrap(), (0, 0));
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
