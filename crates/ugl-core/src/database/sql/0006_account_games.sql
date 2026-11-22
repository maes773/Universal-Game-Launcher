-- UGL esquema v6: biblioteca de la cuenta de Steam vinculada (opcional).
-- Es una copia local de lo que devuelve la API; los juegos instalados siguen viviendo en `games`.
CREATE TABLE account_games (
    app_id           INTEGER PRIMARY KEY,
    name             TEXT    NOT NULL,
    playtime_minutes INTEGER NOT NULL DEFAULT 0,
    last_played_at   INTEGER,
    synced_at        INTEGER NOT NULL
);

CREATE INDEX idx_account_games_name ON account_games(name);
