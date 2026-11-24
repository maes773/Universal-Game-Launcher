//! Cuenta de itch.io (opcional): lista los juegos de tu biblioteca (comprados o reclamados), estén o
//! no instalados. Se usa la API oficial de itch.io con una clave de API personal que el usuario
//! genera en su cuenta; UGL nunca ve tu contraseña. La clave viaja en una cabecera de autorización.
//!
//! AVISO: escrito siguiendo la documentación y lo que publican otras herramientas; no se ha probado
//! contra el servicio real.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::domain::ItchGame;
use crate::error::{Result, UglError};
use crate::integrations::steam_store::clean_text;
use crate::json::{self, Json};
use crate::net;

pub const API_BASE: &str = "https://api.itch.io";
/// Página donde cada persona genera su clave de API.
pub const API_KEY_PAGE: &str = "https://itch.io/user/settings/api-keys";
const MAX_PAGES: u32 = 200;
const COVER_HOST_PREFIX: &str = "https://img.itch.zone/";

/// Las claves de la API son cadenas alfanuméricas largas.
pub fn is_valid_api_key(key: &str) -> bool {
    (20..=100).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn api_errors(root: &Json) -> Option<String> {
    let errors = root.get("errors").and_then(Json::as_array)?;
    let message = errors
        .iter()
        .filter_map(Json::as_str)
        .collect::<Vec<_>>()
        .join("; ");
    Some(if message.contains("invalid key") {
        "itch.io rechazó la clave: no es válida.".to_string()
    } else if message.is_empty() {
        "itch.io devolvió un error.".to_string()
    } else {
        format!("itch.io respondió: {message}")
    })
}

/// Interpreta una página de `profile/owned-keys`. Una página vacía (incluida la forma `{}` con la
/// que el servidor representa una lista vacía) devuelve `Ok` con una lista vacía.
pub fn parse_owned_keys(text: &str) -> Result<Vec<ItchGame>> {
    let root = json::parse(text).map_err(|err| UglError::Invalid(err.to_string()))?;
    if let Some(message) = api_errors(&root) {
        return Err(UglError::Invalid(message));
    }
    let keys = root
        .get("owned_keys")
        .and_then(Json::as_array)
        .unwrap_or(&[]);

    let mut seen = HashSet::new();
    Ok(keys
        .iter()
        .filter_map(|key| {
            let game = key.get("game")?;
            let game_id = game
                .get("id")
                .and_then(Json::as_u64)
                .or_else(|| key.get("game_id").and_then(Json::as_u64))?;
            if !seen.insert(game_id) {
                return None; // varias claves para un mismo juego
            }
            let title = clean_text(game.get("title").and_then(Json::as_str)?);
            let url = game.get("url").and_then(Json::as_str)?.trim().to_string();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let author = game
                .get("user")
                .and_then(|user| {
                    user.get("display_name")
                        .and_then(Json::as_str)
                        .or_else(|| user.get("username").and_then(Json::as_str))
                })
                .map(clean_text)
                .unwrap_or_default();
            Some(ItchGame {
                game_id,
                title,
                author,
                url,
                description: game
                    .get("short_text")
                    .and_then(Json::as_str)
                    .map(clean_text)
                    .unwrap_or_default(),
                cover_url: game
                    .get("cover_url")
                    .and_then(Json::as_str)
                    .map(str::to_string)
                    .filter(|url| !url.is_empty()),
            })
        })
        .collect())
}

/// Nombre de usuario de la respuesta de `profile`.
pub fn parse_profile(text: &str) -> Result<Option<String>> {
    let root = json::parse(text).map_err(|err| UglError::Invalid(err.to_string()))?;
    if let Some(message) = api_errors(&root) {
        return Err(UglError::Invalid(message));
    }
    Ok(root.get("user").and_then(|user| {
        user.get("display_name")
            .and_then(Json::as_str)
            .filter(|name| !name.trim().is_empty())
            .or_else(|| user.get("username").and_then(Json::as_str))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
    }))
}

pub fn fetch_username(key: &str) -> Result<Option<String>> {
    parse_profile(&net::get_with_bearer(&format!("{API_BASE}/profile"), key)?)
}

/// Descarga toda la biblioteca, página a página, hasta que llega una vacía (o sin juegos nuevos).
/// Bloquea: ejecutar en un hilo aparte.
pub fn fetch_library(key: &str) -> Result<Vec<ItchGame>> {
    let mut games: Vec<ItchGame> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    for page in 1..=MAX_PAGES {
        let url = format!("{API_BASE}/profile/owned-keys?page={page}");
        let batch = parse_owned_keys(&net::get_with_bearer(&url, key)?)?;
        if batch.is_empty() {
            break;
        }
        let before = games.len();
        for game in batch {
            if seen.insert(game.game_id) {
                games.push(game);
            }
        }
        if games.len() == before {
            break; // la página no aportó nada nuevo: no seguir
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(games)
}

fn cover_extension(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "jpg"
    } else if path.ends_with(".gif") {
        "gif"
    } else if path.ends_with(".webp") {
        "webp"
    } else {
        "png"
    }
}

pub fn cover_file(cache_dir: &Path, game: &ItchGame) -> Option<PathBuf> {
    let url = game.cover_url.as_deref()?;
    Some(
        cache_dir
            .join("itch-covers")
            .join(format!("{}.{}", game.game_id, cover_extension(url))),
    )
}

/// Descarga (una sola vez) la portada de un juego. Solo se aceptan imágenes del servidor de
/// imágenes de itch.io.
pub fn ensure_cover(game: &ItchGame, cache_dir: &Path) -> Result<PathBuf> {
    let url = game
        .cover_url
        .as_deref()
        .filter(|url| url.starts_with(COVER_HOST_PREFIX))
        .ok_or_else(|| UglError::NotFound("la portada de este juego".into()))?;
    let dest = cover_file(cache_dir, game)
        .ok_or_else(|| UglError::NotFound("la portada de este juego".into()))?;
    if dest.is_file() {
        return Ok(dest);
    }
    net::download_image(url, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"{"per_page":50,"page":1,"owned_keys":[
        {"id":1,"game_id":11,"game":{"id":11,"title":"Volcanic &amp; Sinkhole","url":"https://gurkenlabs.itch.io/volcanic",
          "short_text":"Un mapa <b>de batalla</b>.","cover_url":"https://img.itch.zone/aW1n/315x250%23c/y2h4Jk.png",
          "user":{"username":"gurkenlabs","display_name":"Gurken Labs","url":"https://gurkenlabs.itch.io"}}},
        {"id":2,"game_id":11,"game":{"id":11,"title":"Repetido","url":"https://x.itch.io/r"}},
        {"id":3,"game":{"id":22,"title":"Sin autor ni portada","url":"https://y.itch.io/s","user":{"username":"yuser"}}},
        {"id":4,"game":{"title":"Sin identificador","url":"https://z.itch.io/z"}}]}"#;

    #[test]
    fn parses_a_library_page() {
        let games = parse_owned_keys(PAGE).unwrap();
        assert_eq!(games.len(), 2); // el repetido y el que no tiene id se descartan
        assert_eq!(games[0].game_id, 11);
        assert_eq!(games[0].title, "Volcanic & Sinkhole");
        assert_eq!(games[0].author, "Gurken Labs");
        assert_eq!(games[0].description, "Un mapa de batalla.");
        assert!(games[0].cover_url.as_deref().unwrap().starts_with("https://img.itch.zone/"));
        assert_eq!(games[1].author, "yuser");
        assert_eq!(games[1].cover_url, None);
    }

    #[test]
    fn the_end_of_the_library_is_an_empty_page_in_either_shape() {
        assert!(parse_owned_keys(r#"{"page":9,"owned_keys":[]}"#).unwrap().is_empty());
        // El servidor escribe una lista vacía como objeto.
        assert!(parse_owned_keys(r#"{"page":9,"owned_keys":{}}"#).unwrap().is_empty());
        assert!(parse_owned_keys(r#"{}"#).unwrap().is_empty());
    }

    #[test]
    fn api_errors_are_reported_even_with_http_200() {
        let err = parse_owned_keys(r#"{"errors":["invalid key"]}"#).unwrap_err().to_string();
        assert!(err.contains("no es válida"));
        assert!(parse_owned_keys(r#"{"errors":["rate limited"]}"#).unwrap_err().to_string().contains("rate limited"));
        assert!(parse_owned_keys("<html>").is_err());
    }

    #[test]
    fn reads_the_user_name_from_the_profile() {
        assert_eq!(
            parse_profile(r#"{"user":{"id":1,"username":"ana","display_name":"Ana G"}}"#).unwrap(),
            Some("Ana G".to_string())
        );
        assert_eq!(parse_profile(r#"{"user":{"username":"ana"}}"#).unwrap(), Some("ana".to_string()));
        assert_eq!(parse_profile(r#"{}"#).unwrap(), None);
    }

    #[test]
    fn api_keys_have_a_sane_shape() {
        assert!(is_valid_api_key("AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd"));
        assert!(!is_valid_api_key("corta"));
        assert!(!is_valid_api_key("una clave con espacios y comillas \" no vale"));
    }

    #[test]
    fn covers_only_come_from_the_itch_image_server() {
        let tmp = tempfile::tempdir().unwrap();
        let mut game = parse_owned_keys(PAGE).unwrap().remove(0);
        let file = cover_file(tmp.path(), &game).unwrap();
        assert_eq!(file, tmp.path().join("itch-covers").join("11.png"));

        game.cover_url = Some("https://evil.example/portada.png".into());
        assert!(ensure_cover(&game, tmp.path()).is_err());
        game.cover_url = None;
        assert!(ensure_cover(&game, tmp.path()).is_err());
        assert_eq!(cover_extension("https://img.itch.zone/a/b.JPG?x=1"), "jpg");
        assert_eq!(cover_extension("https://img.itch.zone/a/b.gif"), "gif");
    }
}
