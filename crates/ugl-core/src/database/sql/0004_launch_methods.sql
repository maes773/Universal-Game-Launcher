-- UGL esquema v4: métodos de lanzamiento por juego.
--   exe_path     : ejecutable directo conocido (relativo a install_path, o absoluto). Se conserva
--                  aunque el juego se inicie por Steam, para poder cambiar de método.
--   launch_args  : argumentos de la línea de comandos.
--   working_dir  : directorio de trabajo (NULL = carpeta del ejecutable).
--   steam_app_id : AppID de Steam conocido, aunque el juego se inicie con su ejecutable.
-- launch_kind admite ahora 'uri' (otras plataformas). SQLite no permite cambiar un CHECK,
-- así que se reconstruye la tabla conservando todos los datos.

CREATE TABLE installations_new (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id       INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    install_path  TEXT    NOT NULL,
    launch_kind   TEXT    NOT NULL CHECK (launch_kind IN ('executable', 'steam_uri', 'uri')),
    launch_target TEXT    NOT NULL,
    exe_path      TEXT,
    launch_args   TEXT    NOT NULL DEFAULT '',
    working_dir   TEXT,
    steam_app_id  INTEGER,
    is_primary    INTEGER NOT NULL DEFAULT 1 CHECK (is_primary IN (0, 1)),
    created_at    INTEGER NOT NULL
);

INSERT INTO installations_new
    (id, game_id, install_path, launch_kind, launch_target, exe_path, steam_app_id, is_primary, created_at)
SELECT id, game_id, install_path, launch_kind, launch_target,
       CASE WHEN launch_kind = 'executable' THEN launch_target END,
       CASE WHEN launch_kind = 'steam_uri'  THEN CAST(launch_target AS INTEGER) END,
       is_primary, created_at
FROM installations;

DROP TABLE installations;
ALTER TABLE installations_new RENAME TO installations;
CREATE INDEX idx_installations_game ON installations(game_id);
