//! achievement_engine: almacén unificado de logros, sea cual sea su origen.
//! Cada logro guarda su procedencia; nunca se presenta como «oficial» algo importado.

use std::path::PathBuf;

use rusqlite::{params, Connection};

use crate::domain::{unix_now, GameId};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Official,
    Imported,
    Local,
    Unverified,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Official => "official",
            Provenance::Imported => "imported",
            Provenance::Local => "local",
            Provenance::Unverified => "unverified",
        }
    }

    /// Ante la duda, lo menos confiable.
    pub fn parse(value: &str) -> Self {
        match value {
            "official" => Provenance::Official,
            "imported" => Provenance::Imported,
            "local" => Provenance::Local,
            _ => Provenance::Unverified,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provenance::Official => "Oficial",
            Provenance::Imported => "Importado",
            Provenance::Local => "Local",
            Provenance::Unverified => "Sin verificar",
        }
    }
}

/// Lo que una integración entrega para guardar.
#[derive(Debug, Clone)]
pub struct AchievementRecord {
    pub external_id: String,
    pub name: String,
    pub description: String,
    pub hidden: bool,
    pub icon_path: Option<String>,
    pub icon_gray_path: Option<String>,
    pub unlocked: bool,
    pub unlocked_at: Option<i64>,
}

/// Lo que la interfaz muestra.
#[derive(Debug, Clone)]
pub struct AchievementView {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub hidden: bool,
    pub icon: Option<PathBuf>,
    pub icon_gray: Option<PathBuf>,
    pub provenance: Provenance,
    pub source: String,
    pub unlocked: bool,
    pub unlocked_at: Option<i64>,
}

/// Guarda o actualiza logros en una sola transacción. Un logro ya desbloqueado nunca vuelve a
/// bloquearse por una lectura posterior (una caché atrasada no debe borrar progreso real).
pub fn store(
    conn: &mut Connection,
    game_id: GameId,
    source: &str,
    provenance: Provenance,
    records: &[AchievementRecord],
) -> Result<usize> {
    let tx = conn.transaction()?;
    let now = unix_now();
    for record in records {
        let achievement_id: i64 = tx.query_row(
            "INSERT INTO achievements
                 (game_id, source, external_id, name, description, hidden,
                  icon_path, icon_gray_path, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(game_id, source, external_id) DO UPDATE SET
                 name = excluded.name,
                 description = excluded.description,
                 hidden = excluded.hidden,
                 icon_path = excluded.icon_path,
                 icon_gray_path = excluded.icon_gray_path,
                 provenance = excluded.provenance
             RETURNING id",
            params![
                game_id,
                source,
                record.external_id,
                record.name,
                record.description,
                record.hidden as i64,
                record.icon_path,
                record.icon_gray_path,
                provenance.as_str()
            ],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO unlocks (achievement_id, unlocked, unlocked_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(achievement_id) DO UPDATE SET
                 unlocked = MAX(unlocks.unlocked, excluded.unlocked),
                 unlocked_at = COALESCE(excluded.unlocked_at, unlocks.unlocked_at),
                 updated_at = excluded.updated_at",
            params![achievement_id, record.unlocked as i64, record.unlocked_at, now],
        )?;
    }
    tx.commit()?;
    Ok(records.len())
}

/// Cuándo se sincronizaron por última vez los logros de este juego (Unix), si alguna vez.
pub fn synced_at(conn: &Connection, game_id: GameId) -> Result<Option<i64>> {
    let value: Option<i64> = conn.query_row(
        "SELECT MAX(u.updated_at)
         FROM unlocks u JOIN achievements a ON a.id = u.achievement_id
         WHERE a.game_id = ?1",
        params![game_id],
        |row| row.get(0),
    )?;
    Ok(value)
}

/// Desbloqueados primero (los más recientes arriba), después los pendientes en su orden original.
pub fn list(conn: &Connection, game_id: GameId) -> Result<Vec<AchievementView>> {
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.name, a.description, a.hidden, a.icon_path, a.icon_gray_path,
                a.provenance, a.source, COALESCE(u.unlocked, 0), u.unlocked_at
         FROM achievements a
         LEFT JOIN unlocks u ON u.achievement_id = a.id
         WHERE a.game_id = ?1
         ORDER BY COALESCE(u.unlocked, 0) DESC, u.unlocked_at DESC, a.id ASC",
    )?;
    let rows = stmt.query_map(params![game_id], |row| {
        let icon: Option<String> = row.get(4)?;
        let icon_gray: Option<String> = row.get(5)?;
        let provenance: String = row.get(6)?;
        Ok(AchievementView {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            hidden: row.get::<_, i64>(3)? != 0,
            icon: icon.map(PathBuf::from),
            icon_gray: icon_gray.map(PathBuf::from),
            provenance: Provenance::parse(&provenance),
            source: row.get(7)?,
            unlocked: row.get::<_, i64>(8)? != 0,
            unlocked_at: row.get(9)?,
        })
    })?;
    let views = rows.collect::<rusqlite::Result<Vec<AchievementView>>>()?;
    Ok(views)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::database::Database;
    use crate::domain::{DiscoveredGame, LaunchKind, SortOrder};
    use crate::registry;

    fn setup() -> (Database, GameId) {
        let mut db = Database::open_in_memory().unwrap();
        registry::register(
            db.conn_mut(),
            &[DiscoveredGame {
                provider: "steam".into(),
                external_id: "220".into(),
                title: "Half-Life 2".into(),
                platform_slug: "pc".into(),
                install_path: PathBuf::from("C:\\a"),
                launch_kind: LaunchKind::SteamUri,
                launch_target: "220".into(),
            }],
        )
        .unwrap();
        let id = registry::list_games(db.conn(), "", SortOrder::Title).unwrap()[0].id;
        (db, id)
    }

    fn record(name: &str, unlocked: bool, at: Option<i64>) -> AchievementRecord {
        AchievementRecord {
            external_id: name.to_string(),
            name: name.to_string(),
            description: String::new(),
            hidden: false,
            icon_path: None,
            icon_gray_path: None,
            unlocked,
            unlocked_at: at,
        }
    }

    #[test]
    fn unlocked_come_first_and_counts_reach_the_game_list() {
        let (mut db, id) = setup();
        store(
            db.conn_mut(),
            id,
            "steam_local",
            Provenance::Imported,
            &[
                record("A", false, None),
                record("B", true, Some(100)),
                record("C", true, Some(200)),
            ],
        )
        .unwrap();

        let list = list(db.conn(), id).unwrap();
        let names: Vec<&str> = list.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["C", "B", "A"]);
        assert!(list.iter().all(|a| a.provenance == Provenance::Imported));

        let game = registry::get_game(db.conn(), id).unwrap();
        assert_eq!(game.achievements_total, 3);
        assert_eq!(game.achievements_unlocked, 2);
    }

    #[test]
    fn stale_reads_never_relock_an_achievement() {
        let (mut db, id) = setup();
        store(db.conn_mut(), id, "steam_local", Provenance::Imported, &[record("A", true, Some(50))]).unwrap();
        // Una lectura posterior con la caché atrasada dice «bloqueado» y sin fecha.
        store(db.conn_mut(), id, "steam_local", Provenance::Imported, &[record("A", false, None)]).unwrap();

        let list = list(db.conn(), id).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].unlocked);
        assert_eq!(list[0].unlocked_at, Some(50));
    }

    #[test]
    fn removing_a_game_removes_its_achievements() {
        let (mut db, id) = setup();
        store(db.conn_mut(), id, "steam_local", Provenance::Imported, &[record("A", true, None)]).unwrap();
        registry::remove_game(db.conn(), id).unwrap();
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM unlocks", params![], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
