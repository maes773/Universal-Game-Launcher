-- UGL esquema v2: logros unificados.
-- `source` identifica la integración que los aportó (p. ej. 'steam_local').
-- `provenance` deja claro si el dato es oficial, importado, local o sin verificar.

CREATE TABLE achievements (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    source         TEXT    NOT NULL,
    external_id    TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    description    TEXT    NOT NULL DEFAULT '',
    hidden         INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1)),
    icon_path      TEXT,
    icon_gray_path TEXT,
    provenance     TEXT    NOT NULL
                   CHECK (provenance IN ('official', 'imported', 'local', 'unverified')),
    UNIQUE (game_id, source, external_id)
);

CREATE TABLE unlocks (
    achievement_id INTEGER PRIMARY KEY REFERENCES achievements(id) ON DELETE CASCADE,
    unlocked       INTEGER NOT NULL CHECK (unlocked IN (0, 1)),
    unlocked_at    INTEGER,
    updated_at     INTEGER NOT NULL
);
