-- UGL esquema v5: información de la tienda (Steam Store) guardada en local.
-- status 'unavailable' = Steam no ofrece ficha para ese AppID (no se vuelve a pedir).
CREATE TABLE game_metadata (
    game_id           INTEGER PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    steam_app_id      INTEGER NOT NULL,
    status            TEXT    NOT NULL CHECK (status IN ('ok', 'unavailable')),
    name              TEXT,
    short_description TEXT,
    developers        TEXT,
    publishers        TEXT,
    genres            TEXT,
    release_date      TEXT,
    website           TEXT,
    cover_path        TEXT,
    language          TEXT    NOT NULL DEFAULT 'spanish',
    fetched_at        INTEGER NOT NULL
);

CREATE INDEX idx_installations_steam ON installations(steam_app_id);
