//! Información pública de Steam Store: nombre, desarrolladores, descripción, portada…
//! No necesita Steam instalado ni inicio de sesión. Todo lo obtenido se guarda en local para que
//! UGL funcione sin conexión.
//!
//! AVISO: los formatos de respuesta salen de documentación de terceros (la API no está documentada
//! oficialmente); este código no se ha probado contra el servicio real.

use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::MetadataRecord;
use crate::error::{Result, UglError};
use crate::json::{self, Json};
use crate::net;

/// Idioma de las fichas de la tienda.
pub const LANGUAGE: &str = "spanish";

const IMAGE_HOSTS: &[&str] = &[
    "https://cdn.cloudflare.steamstatic.com",
    "https://cdn.akamai.steamstatic.com",
];
/// Primero la portada vertical; si no existe, la cabecera horizontal.
const COVER_FILES: &[&str] = &["library_600x900.jpg", "header.jpg"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreInfo {
    pub app_id: u64,
    pub name: String,
    pub short_description: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub genres: Vec<String>,
    pub release_date: String,
    pub website: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub app_id: u64,
    pub name: String,
}

/// Quita etiquetas HTML y entidades habituales, normaliza los espacios
/// y evita espacios incorrectos antes de los signos de puntuación.
pub fn clean_text(input: &str) -> String {
    let mut text = String::with_capacity(input.len());
    let mut in_tag = false;

    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }

    let decoded = text
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");

    let normalized = decoded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let mut cleaned = String::with_capacity(normalized.len());

    for c in normalized.chars() {
        if matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | '…' | '%' | ')' | ']' | '}' | '»' | '”' | '’'
        ) {
            while cleaned.ends_with(' ') {
                cleaned.pop();
            }
        }

        cleaned.push(c);
    }

    cleaned
}


/// Codifica un texto para usarlo en una dirección web.
pub fn url_encode(term: &str) -> String {
    let mut out = String::new();
    for byte in term.trim().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn app_details_url(app_id: u64) -> String {
    format!("https://store.steampowered.com/api/appdetails?appids={app_id}&cc=us&l={LANGUAGE}")
}

pub fn search_url(term: &str) -> String {
    format!(
        "https://store.steampowered.com/api/storesearch/?term={}&l={LANGUAGE}&cc=us",
        url_encode(term)
    )
}

fn string_list(node: Option<&Json>, field: Option<&str>) -> Vec<String> {
    node.and_then(Json::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match field {
                    Some(name) => item.get(name).and_then(Json::as_str),
                    None => item.as_str(),
                })
                .map(clean_text)
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// `Ok(None)` = Steam respondió que esa ficha no está disponible (retirada, restringida…).
pub fn parse_app_details(text: &str, app_id: u64) -> Result<Option<StoreInfo>> {
    let root = json::parse(text).map_err(|err| UglError::Invalid(err.to_string()))?;
    let entry = root
        .get(&app_id.to_string())
        .or_else(|| root.entries().first().map(|(_, value)| value))
        .ok_or_else(|| UglError::Invalid("La respuesta de Steam Store está vacía.".into()))?;
    if entry.get("success").and_then(Json::as_bool) != Some(true) {
        return Ok(None);
    }
    let data = entry
        .get("data")
        .ok_or_else(|| UglError::Invalid("La respuesta de Steam Store no trae datos.".into()))?;
    let name = data
        .get("name")
        .and_then(Json::as_str)
        .map(clean_text)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| UglError::Invalid("La ficha de Steam Store no tiene nombre.".into()))?;

    Ok(Some(StoreInfo {
        app_id,
        name,
        short_description: data
            .get("short_description")
            .and_then(Json::as_str)
            .map(clean_text)
            .unwrap_or_default(),
        developers: string_list(data.get("developers"), None),
        publishers: string_list(data.get("publishers"), None),
        genres: string_list(data.get("genres"), Some("description")),
        release_date: data
            .get("release_date")
            .and_then(|r| r.get("date"))
            .and_then(Json::as_str)
            .map(clean_text)
            .unwrap_or_default(),
        website: data
            .get("website")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|w| w.starts_with("http"))
            .map(str::to_string),
    }))
}

pub fn parse_search(text: &str) -> Result<Vec<SearchHit>> {
    let root = json::parse(text).map_err(|err| UglError::Invalid(err.to_string()))?;
    let items = root
        .get("items")
        .and_then(Json::as_array)
        .ok_or_else(|| UglError::Invalid("La búsqueda de Steam Store no devolvió resultados.".into()))?;
    Ok(items
        .iter()
        .filter(|item| {
            // Se descartan paquetes y otros tipos: solo aplicaciones.
            item.get("type").and_then(Json::as_str).map_or(true, |t| t == "app")
        })
        .filter_map(|item| {
            let app_id = item.get("id").and_then(Json::as_u64)?;
            let name = item.get("name").and_then(Json::as_str).map(clean_text)?;
            (!name.is_empty()).then_some(SearchHit { app_id, name })
        })
        .collect())
}

fn fetch_json(url: &str, work_dir: &Path, tag: &str) -> Result<String> {
    fs::create_dir_all(work_dir)?;
    let dest = work_dir.join(format!("store-{tag}.json"));
    net::download_file(url, &dest)?;
    let bytes = fs::read(&dest);
    let _ = fs::remove_file(&dest);
    Ok(String::from_utf8_lossy(&bytes?).into_owned())
}

pub fn fetch_info(app_id: u64, work_dir: &Path) -> Result<Option<StoreInfo>> {
    let text = fetch_json(&app_details_url(app_id), work_dir, &app_id.to_string())?;
    parse_app_details(&text, app_id)
}

pub fn search(term: &str, work_dir: &Path) -> Result<Vec<SearchHit>> {
    let text = fetch_json(&search_url(term), work_dir, "search")?;
    parse_search(&text)
}

pub fn cover_path(cache_dir: &Path, app_id: u64) -> PathBuf {
    cache_dir.join("covers").join(format!("{app_id}.jpg"))
}

/// Descarga (una sola vez) la portada de un juego a la caché de UGL.
pub fn ensure_cover(app_id: u64, cache_dir: &Path) -> Result<PathBuf> {
    let dest = cover_path(cache_dir, app_id);
    if dest.is_file() {
        return Ok(dest);
    }
    let mut last_error = None;
    for file in COVER_FILES {
        for host in IMAGE_HOSTS {
            let url = format!("{host}/steam/apps/{app_id}/{file}");
            match net::download_image(&url, &dest) {
                Ok(()) => return Ok(dest),
                Err(err) => last_error = Some(err),
            }
        }
    }
    Err(last_error.unwrap_or_else(|| UglError::Invalid("no hay servidores de imágenes".into())))
}

/// Descarga la ficha y la portada de un juego. Un error de red es `Err` (se reintentará más
/// tarde); una ficha que Steam no ofrece es `Ok` con `available = false` (no se vuelve a pedir).
pub fn fetch_record(app_id: u64, cache_dir: &Path) -> Result<MetadataRecord> {
    let work_dir = cache_dir.join("tmp");
    match fetch_info(app_id, &work_dir)? {
        None => Ok(MetadataRecord {
            steam_app_id: app_id,
            available: false,
            ..Default::default()
        }),
        Some(info) => {
            let cover = ensure_cover(app_id, cache_dir)
                .ok()
                .map(|path| path.to_string_lossy().into_owned());
            Ok(MetadataRecord {
                steam_app_id: app_id,
                available: true,
                name: info.name,
                short_description: info.short_description,
                developers: info.developers,
                publishers: info.publishers,
                genres: info.genres,
                release_date: info.release_date,
                website: info.website,
                cover_path: cover,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETAILS: &str = r#"{"220":{"success":true,"data":{"type":"game","name":"Half-Life 2","steam_appid":220,
        "short_description":"The &quot;Game&quot; of the year &amp; more.<br>Segunda l\u00ednea","developers":["Valve"],
        "publishers":["Valve"],"genres":[{"id":"1","description":"Acci\u00f3n"}],
        "release_date":{"coming_soon":false,"date":"16 NOV 2004"},"website":"http://www.half-life2.com",
        "header_image":"https://cdn.akamai.steamstatic.com/steam/apps/220/header.jpg"}}}"#;

    #[test]
fn cleans_spaces_before_punctuation() {
    assert_eq!(
        clean_text("<b>Un mapa de batalla</b>."),
        "Un mapa de batalla."
    );

    assert_eq!(
        clean_text("Hola </i> , mundo !"),
        "Hola, mundo!"
    );
}
        
    #[test]
    fn parses_a_store_page() {
        let info = parse_app_details(DETAILS, 220).unwrap().unwrap();
        assert_eq!(info.name, "Half-Life 2");
        assert_eq!(info.short_description, "The \"Game\" of the year & more. Segunda línea");
        assert_eq!(info.developers, vec!["Valve"]);
        assert_eq!(info.genres, vec!["Acción"]);
        assert_eq!(info.release_date, "16 NOV 2004");
        assert_eq!(info.website.as_deref(), Some("http://www.half-life2.com"));
    }

    #[test]
    fn unavailable_pages_are_not_an_error() {
        assert_eq!(parse_app_details(r#"{"999":{"success":false}}"#, 999).unwrap(), None);
    }

    #[test]
    fn garbage_is_an_error_so_the_request_can_be_retried() {
        assert!(parse_app_details("<html>mantenimiento</html>", 1).is_err());
        assert!(parse_app_details(r#"{"1":{"success":true,"data":{}}}"#, 1).is_err());
    }

    #[test]
    fn search_results_keep_only_apps() {
        let hits = parse_search(
            r#"{"total":3,"items":[{"type":"app","name":"Half-Life 2","id":220},
                {"type":"bundle","name":"Paquete","id":5},{"name":"Sin tipo","id":7}]}"#,
        )
        .unwrap();
        assert_eq!(
            hits,
            vec![
                SearchHit { app_id: 220, name: "Half-Life 2".into() },
                SearchHit { app_id: 7, name: "Sin tipo".into() }
            ]
        );
    }

    #[test]
    fn builds_urls_safely() {
        assert_eq!(url_encode("Half-Life 2: Ep. 1 & más"), "Half-Life%202%3A%20Ep.%201%20%26%20m%C3%A1s");
        assert!(search_url("a b").contains("term=a%20b&l=spanish"));
        assert!(app_details_url(220).contains("appids=220"));
    }

    #[test]
    fn cleans_html() {
        assert_eq!(clean_text("<p>Hola&nbsp;<b>mundo</b></p>\n  fin"), "Hola mundo fin");
    }
}
