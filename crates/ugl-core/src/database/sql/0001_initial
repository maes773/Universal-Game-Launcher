-- UGL esquema v1 (0.1 Foundation)
-- Separa el juego (games) de sus instalaciones (installations) y de su origen (game_sources),
-- para que un mismo juego pueda tener varias ubicaciones o proceder de varias plataformas.

CREATE TABLE platforms (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    -- Estilo de estantería previsto para la interfaz: caja moderna, cartucho o emulado.
    kind TEXT NOT NULL CHECK (kind IN ('modern', 'retro', 'emulated'))
);

INSERT INTO platforms (slug, name, kind) VALUES ('pc', 'PC (Windows)', 'modern');

CREATE TABLE games (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    title          TEXT    NOT NULL,
    sort_title     TEXT    NOT NULL,
    platform_id    INTEGER NOT NULL REFERENCES platforms(id),
    added_at       INTEGER NOT NULL,
    last_played_at INTEGER,
    hidden         INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1))
);

CREATE INDEX idx_games_sort_title  ON games(sort_title);
CREATE INDEX idx_games_last_played ON games(last_played_at);
CREATE INDEX idx_games_added_at    ON games(added_at);

CREATE TABLE installations (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id       INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    install_path  TEXT    NOT NULL,
    -- 'executable': launch_target = ruta del .exe relativa a install_path
    -- 'steam_uri' : launch_target = AppID de Steam
    launch_kind   TEXT    NOT NULL CHECK (launch_kind IN ('executable', 'steam_uri')),
    launch_target TEXT    NOT NULL,
    is_primary    INTEGER NOT NULL DEFAULT 1 CHECK (is_primary IN (0, 1)),
    created_at    INTEGER NOT NULL
);

CREATE INDEX idx_installations_game ON installations(game_id);

CREATE TABLE game_sources (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id     INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    provider    TEXT    NOT NULL,
    external_id TEXT    NOT NULL,
    UNIQUE (provider, external_id)
);

CREATE INDEX idx_game_sources_game ON game_sources(game_id);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
