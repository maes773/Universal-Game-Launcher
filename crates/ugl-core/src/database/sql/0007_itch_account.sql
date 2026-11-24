-- UGL esquema v7: la cuenta vinculada pasa de Steam a itch.io.
-- Se elimina la copia local de la biblioteca de Steam y todo lo guardado de aquella cuenta
-- (identidad, nombre y clave cifrada de la API de Steam).
DROP TABLE IF EXISTS account_games;
DELETE FROM settings WHERE key LIKE 'steam.account.%';

-- Biblioteca de la cuenta de itch.io vinculada (copia local de lo que devuelve la API).
CREATE TABLE itch_games (
    game_id     INTEGER PRIMARY KEY,
    title       TEXT    NOT NULL,
    author      TEXT    NOT NULL DEFAULT '',
    url         TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    cover_url   TEXT,
    synced_at   INTEGER NOT NULL
);

CREATE INDEX idx_itch_games_title ON itch_games(title);
