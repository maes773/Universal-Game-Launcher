-- UGL esquema v3: icono de cada juego.
-- NULL = todavía no se ha intentado; '' = se intentó y no hay icono; otro valor = ruta del PNG en caché.
ALTER TABLE games ADD COLUMN icon_path TEXT;
