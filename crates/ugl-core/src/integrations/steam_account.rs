//! Cuenta de Steam (opcional). Dos cosas distintas:
//! 1. **Identificar** al usuario con «Iniciar sesión con Steam» (OpenID 2.0, la vía oficial para
//!    aplicaciones de terceros): el inicio de sesión ocurre en el navegador, en la página de Steam.
//!    UGL nunca ve usuario ni contraseña; solo recibe el SteamID64.
//! 2. **Leer la biblioteca** con la API web de Steam, que exige la clave de API del propio usuario
//!    y que los detalles de juego del perfil sean visibles.
//!
//! AVISO: escrito siguiendo la documentación pública de Steam, sin poder probarlo contra el servicio.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::domain::OwnedGame;
use crate::error::{Result, UglError};
use crate::integrations::steam_store::url_encode;
use crate::json::{self, Json};
use crate::net;

pub const OPENID_ENDPOINT: &str = "https://steamcommunity.com/openid/login";
const OPENID_NS: &str = "http://specs.openid.net/auth/2.0";
const IDENTIFIER_SELECT: &str = "http://specs.openid.net/auth/2.0/identifier_select";
const CLAIMED_ID_PREFIX: &str = "https://steamcommunity.com/openid/id/";
const CALLBACK_PATH: &str = "/steam-callback";
/// Primer SteamID64 de una cuenta individual.
const MIN_STEAM_ID: u64 = 76_561_197_960_265_728;
pub const API_KEY_PAGE: &str = "https://steamcommunity.com/dev/apikey";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamIdentity {
    pub steam_id: u64,
    /// `true` si Steam confirmó la respuesta (`check_authentication`).
    pub verified: bool,
}

// ---------------------------------------------------------------------------
// Texto de direcciones
// ---------------------------------------------------------------------------

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(value) => {
                        out.push(value);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key), percent_decode(value)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

pub fn build_login_url(return_to: &str, realm: &str) -> String {
    format!(
        "{OPENID_ENDPOINT}?openid.ns={}&openid.mode=checkid_setup&openid.return_to={}&openid.realm={}\
         &openid.identity={}&openid.claimed_id={}",
        url_encode(OPENID_NS),
        url_encode(return_to),
        url_encode(realm),
        url_encode(IDENTIFIER_SELECT),
        url_encode(IDENTIFIER_SELECT),
    )
}

fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let a = RandomState::new().build_hasher().finish();
    let b = RandomState::new().build_hasher().finish();
    format!("{a:016x}{b:016x}")
}

/// Comprueba la respuesta de Steam (sin red) y extrae el SteamID64. Devuelve también los
/// parámetros `openid.*` para pedir a Steam que confirme la respuesta.
pub fn parse_callback(
    query: &str,
    expected_return_to: &str,
    expected_state: &str,
) -> Result<(u64, Vec<(String, String)>)> {
    let params = parse_query(query);
    let get = |key: &str| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let fail = |message: &str| Err(UglError::Invalid(message.to_string()));

    match get("openid.mode") {
        Some("id_res") => {}
        Some("cancel") => return fail("Cancelaste el inicio de sesión en Steam."),
        _ => return fail("La respuesta de Steam no es válida."),
    }
    if get("state") != Some(expected_state) {
        return fail("La respuesta no corresponde a este inicio de sesión.");
    }
    if get("openid.ns") != Some(OPENID_NS)
        || get("openid.op_endpoint") != Some(OPENID_ENDPOINT)
        || get("openid.return_to") != Some(expected_return_to)
    {
        return fail("La respuesta de Steam no coincide con lo solicitado.");
    }
    let claimed = get("openid.claimed_id").unwrap_or_default();
    if get("openid.identity") != Some(claimed) {
        return fail("La identidad devuelta por Steam no es coherente.");
    }
    if get("openid.signed").is_none() || get("openid.sig").is_none() {
        return fail("La respuesta de Steam no está firmada.");
    }
    let digits = claimed
        .strip_prefix(CLAIMED_ID_PREFIX)
        .or_else(|| claimed.strip_prefix("http://steamcommunity.com/openid/id/"))
        .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
    let steam_id: u64 = digits
        .and_then(|d| d.parse().ok())
        .filter(|id| *id >= MIN_STEAM_ID)
        .ok_or_else(|| UglError::Invalid("Steam no devolvió un SteamID válido.".into()))?;

    let openid_params = params
        .iter()
        .filter(|(k, _)| k.starts_with("openid."))
        .cloned()
        .collect();
    Ok((steam_id, openid_params))
}

/// Pide a Steam que confirme la respuesta (`check_authentication`).
fn verify_with_steam(openid_params: &[(String, String)]) -> Result<bool> {
    let mut body = String::from("openid.mode=check_authentication");
    for (key, value) in openid_params {
        if key != "openid.mode" {
            body.push('&');
            body.push_str(&url_encode(key));
            body.push('=');
            body.push_str(&url_encode(value));
        }
    }
    let answer = net::post_form(OPENID_ENDPOINT, &body)?;
    Ok(answer.lines().any(|line| line.trim() == "is_valid:true"))
}

// ---------------------------------------------------------------------------
// Inicio de sesión: servidor local de un solo uso
// ---------------------------------------------------------------------------

pub struct LoginSession {
    listener: TcpListener,
    /// Dirección que hay que abrir en el navegador.
    pub login_url: String,
    return_to: String,
    state: String,
}

/// Abre un puerto local (solo 127.0.0.1) donde Steam devolverá al usuario tras iniciar sesión.
pub fn begin_login() -> Result<LoginSession> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let state = random_token();
    let realm = format!("http://127.0.0.1:{port}/");
    let return_to = format!("http://127.0.0.1:{port}{CALLBACK_PATH}?state={state}");
    let login_url = build_login_url(&return_to, &realm);
    Ok(LoginSession {
        listener,
        login_url,
        return_to,
        state,
    })
}

const PAGE: &str = "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><title>UGL</title></head>\
<body style=\"font-family:sans-serif;background:#1c1511;color:#f2e6d4;text-align:center;padding-top:15vh\">\
<h2>UGL</h2><p>Inicio de sesión recibido. Ya puedes cerrar esta pestaña y volver a UGL.</p></body></html>";

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Lee una petición; si es la devolución de Steam, responde y entrega su cadena de consulta.
fn read_callback(mut stream: TcpStream) -> Option<String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut buffer = [0u8; 8192];
    let read = stream.read(&mut buffer).ok()?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let first_line = request.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let (method, target) = (parts.next()?, parts.next()?);
    if method == "GET" && target.starts_with(CALLBACK_PATH) {
        let query = target
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or("")
            .to_string();
        respond(&mut stream, "200 OK", PAGE);
        Some(query)
    } else {
        respond(&mut stream, "404 Not Found", "");
        None
    }
}

impl LoginSession {
    /// Espera la devolución de Steam. Bloquea: ejecutar en un hilo aparte.
    pub fn wait(self, timeout: Duration, cancel: &AtomicBool) -> Result<SteamIdentity> {
        let deadline = Instant::now() + timeout;
        let query = loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(UglError::Invalid("Inicio de sesión cancelado.".into()));
            }
            if Instant::now() >= deadline {
                return Err(UglError::Invalid(
                    "Se agotó el tiempo de espera del inicio de sesión.".into(),
                ));
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(query) = read_callback(stream) {
                        break query;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(err.into()),
            }
        };

        let (steam_id, params) = parse_callback(&query, &self.return_to, &self.state)?;
        // Con curl disponible, Steam confirma la respuesta; sin él, se acepta pero se marca.
        let verified = match verify_with_steam(&params) {
            Ok(true) => true,
            Ok(false) => {
                return Err(UglError::Invalid(
                    "Steam no confirmó el inicio de sesión.".into(),
                ))
            }
            Err(err) => {
                crate::diagnostics::warn(&format!("No se pudo confirmar con Steam: {err}"));
                false
            }
        };
        Ok(SteamIdentity { steam_id, verified })
    }
}

// ---------------------------------------------------------------------------
// Identificación manual (alternativa sin navegador)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteamInput {
    Id(u64),
    /// Nombre personalizado del perfil (`steamcommunity.com/id/<nombre>`): hay que resolverlo con la clave.
    Vanity(String),
}

/// Acepta un SteamID64, la dirección `…/profiles/<id>` o `…/id/<nombre>`.
pub fn parse_account_input(text: &str) -> Option<SteamInput> {
    let text = text.trim().trim_end_matches('/');
    if text.len() == 17 && text.chars().all(|c| c.is_ascii_digit()) {
        return text
            .parse()
            .ok()
            .filter(|id| *id >= MIN_STEAM_ID)
            .map(SteamInput::Id);
    }
    if let Some((_, rest)) = text.split_once("/profiles/") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return digits
            .parse()
            .ok()
            .filter(|id| *id >= MIN_STEAM_ID)
            .map(SteamInput::Id);
    }
    if let Some((_, rest)) = text.split_once("/id/") {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if !name.is_empty() {
            return Some(SteamInput::Vanity(name));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// API web (con la clave del propio usuario)
// ---------------------------------------------------------------------------

/// Las claves de la Web API son 32 caracteres hexadecimales.
pub fn is_valid_api_key(key: &str) -> bool {
    key.len() == 32 && key.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn owned_games_url(key: &str, steam_id: u64) -> String {
    format!(
        "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={key}&steamid={steam_id}\
         &include_appinfo=true&include_played_free_games=true&format=json"
    )
}

pub fn player_summary_url(key: &str, steam_id: u64) -> String {
    format!("https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v2/?key={key}&steamids={steam_id}")
}

pub fn vanity_url(key: &str, vanity: &str) -> String {
    format!(
        "https://api.steampowered.com/ISteamUser/ResolveVanityURL/v1/?key={key}&vanityurl={}",
        url_encode(vanity)
    )
}

/// Un perfil con los detalles de juego privados devuelve `{"response":{}}`: lista vacía, no error.
pub fn parse_owned_games(text: &str) -> Result<Vec<OwnedGame>> {
    let root = json::parse(text).map_err(|err| UglError::Invalid(err.to_string()))?;
    let response = root
        .get("response")
        .ok_or_else(|| UglError::Invalid("Respuesta inesperada de la API de Steam.".into()))?;
    let games = response
        .get("games")
        .and_then(Json::as_array)
        .unwrap_or(&[]);
    let mut seen = std::collections::HashSet::new();
    Ok(games
        .iter()
        .filter_map(|game| {
            let app_id = game.get("appid").and_then(Json::as_u64)?;
            if !seen.insert(app_id) {
                return None;
            }
            let name = game
                .get("name")
                .and_then(Json::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Aplicación {app_id}"));
            let last_played = game
                .get("rtime_last_played")
                .and_then(Json::as_u64)
                .filter(|t| *t > 0)
                .map(|t| t as i64);
            Some(OwnedGame {
                app_id,
                name,
                playtime_minutes: game
                    .get("playtime_forever")
                    .and_then(Json::as_u64)
                    .unwrap_or(0),
                last_played_at: last_played,
            })
        })
        .collect())
}

pub fn parse_persona(text: &str) -> Option<String> {
    let root = json::parse(text).ok()?;
    root.get("response")?
        .get("players")?
        .as_array()?
        .first()?
        .get("personaname")?
        .as_str()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

pub fn parse_vanity(text: &str) -> Result<Option<u64>> {
    let root = json::parse(text).map_err(|err| UglError::Invalid(err.to_string()))?;
    let response = root
        .get("response")
        .ok_or_else(|| UglError::Invalid("Respuesta inesperada de la API de Steam.".into()))?;
    if response.get("success").and_then(Json::as_u64) != Some(1) {
        return Ok(None);
    }
    Ok(response.get("steamid").and_then(Json::as_u64))
}

fn fetch_text(url: &str, work_dir: &std::path::Path, tag: &str) -> Result<String> {
    std::fs::create_dir_all(work_dir)?;
    let dest = work_dir.join(format!("account-{tag}.json"));
    // La dirección lleva la clave: se descarga sin dejarla en la caché de internet de Windows.
    let downloaded = net::download_private(url, &dest);
    let bytes = std::fs::read(&dest);
    let _ = std::fs::remove_file(&dest);
    downloaded.map_err(|_| {
        UglError::Invalid(
            "No se pudo consultar la API de Steam (sin conexión, o la clave no es válida).".into(),
        )
    })?;
    Ok(String::from_utf8_lossy(&bytes?).into_owned())
}

pub fn fetch_owned_games(
    key: &str,
    steam_id: u64,
    work_dir: &std::path::Path,
) -> Result<Vec<OwnedGame>> {
    parse_owned_games(&fetch_text(
        &owned_games_url(key, steam_id),
        work_dir,
        "games",
    )?)
}

pub fn fetch_persona(
    key: &str,
    steam_id: u64,
    work_dir: &std::path::Path,
) -> Result<Option<String>> {
    Ok(parse_persona(&fetch_text(
        &player_summary_url(key, steam_id),
        work_dir,
        "persona",
    )?))
}

pub fn resolve_vanity(key: &str, vanity: &str, work_dir: &std::path::Path) -> Result<Option<u64>> {
    parse_vanity(&fetch_text(&vanity_url(key, vanity), work_dir, "vanity")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    const ID: u64 = 76_561_198_002_516_729;

    fn callback_query(return_to: &str, state: &str, id: u64) -> String {
        let claimed = format!("{CLAIMED_ID_PREFIX}{id}");
        format!(
            "state={state}&openid.ns={}&openid.mode=id_res&openid.op_endpoint={}&openid.claimed_id={}\
             &openid.identity={}&openid.return_to={}&openid.response_nonce=2024-01-01T00%3A00%3A00Zabc\
             &openid.assoc_handle=1234567890&openid.signed=signed%2Cop_endpoint%2Cclaimed_id%2Cidentity%2Creturn_to\
             &openid.sig=AAAA%2BBBB%3D",
            url_encode(OPENID_NS),
            url_encode(OPENID_ENDPOINT),
            url_encode(&claimed),
            url_encode(&claimed),
            url_encode(return_to),
        )
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("a%20b+c%2Fd%C3%B1"), "a b c/dñ");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn login_url_carries_the_required_openid_fields() {
        let url = build_login_url(
            "http://127.0.0.1:5000/steam-callback?state=abc",
            "http://127.0.0.1:5000/",
        );
        assert!(url.starts_with("https://steamcommunity.com/openid/login?"));
        assert!(url.contains("openid.mode=checkid_setup"));
        assert!(url.contains(
            "openid.return_to=http%3A%2F%2F127.0.0.1%3A5000%2Fsteam-callback%3Fstate%3Dabc"
        ));
        assert!(url.contains(
            "openid.claimed_id=http%3A%2F%2Fspecs.openid.net%2Fauth%2F2.0%2Fidentifier_select"
        ));
    }

    #[test]
    fn accepts_a_well_formed_steam_response() {
        let return_to = "http://127.0.0.1:5000/steam-callback?state=tok";
        let query = callback_query(return_to, "tok", ID);
        let (id, params) = parse_callback(&query, return_to, "tok").unwrap();
        assert_eq!(id, ID);
        assert!(params.iter().any(|(k, _)| k == "openid.sig"));
    }

    #[test]
    fn rejects_forged_or_foreign_responses() {
        let return_to = "http://127.0.0.1:5000/steam-callback?state=tok";
        let good = callback_query(return_to, "tok", ID);
        // Otro estado.
        assert!(parse_callback(&good, return_to, "otro").is_err());
        // return_to distinto del solicitado.
        assert!(parse_callback(
            &good,
            "http://127.0.0.1:5000/steam-callback?state=otro",
            "tok"
        )
        .is_err());
        // Identidad que no es de Steam.
        let fake = good.replace(
            "steamcommunity.com%2Fopenid%2Fid",
            "evil.example%2Fopenid%2Fid",
        );
        assert!(parse_callback(&fake, return_to, "tok").is_err());
        // Sin firma.
        let unsigned = good.replace("openid.sig=", "x.sig=");
        assert!(parse_callback(&unsigned, return_to, "tok").is_err());
        // SteamID absurdo.
        assert!(parse_callback(&callback_query(return_to, "tok", 5), return_to, "tok").is_err());
        // Cancelado por el usuario.
        let err = parse_callback("state=tok&openid.mode=cancel", return_to, "tok").unwrap_err();
        assert!(err.to_string().contains("Cancelaste"));
    }

    #[test]
    fn the_local_server_receives_the_redirect_and_ignores_other_requests() {
        let session = begin_login().unwrap();
        let port = session.listener.local_addr().unwrap().port();
        assert!(
            session
                .login_url
                .contains(&port.to_string().replace(':', ""))
                || session.login_url.contains("127.0.0.1")
        );
        let return_to = session.return_to.clone();
        let state = session.state.clone();
        let query = callback_query(&return_to, &state, ID);

        let client = std::thread::spawn(move || {
            // Una petición ajena (favicon) y después la devolución real.
            for target in [
                "/favicon.ico".to_string(),
                format!("{CALLBACK_PATH}?{query}"),
            ] {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                stream
                    .write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .unwrap();
                let mut answer = String::new();
                let _ = stream.read_to_string(&mut answer);
                assert!(answer.starts_with("HTTP/1.1"));
            }
        });

        // `wait` pediría confirmación a Steam por red; se prueba la recepción con la parte local.
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut received = None;
        while received.is_none() && Instant::now() < deadline {
            if let Ok((stream, _)) = session.listener.accept() {
                received = read_callback(stream);
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        client.join().unwrap();
        assert!(!cancel.load(Ordering::SeqCst));
        let query = received.expect("se esperaba la devolución");
        assert_eq!(parse_callback(&query, &return_to, &state).unwrap().0, ID);
    }

    #[test]
    fn a_cancelled_wait_stops_quickly() {
        let session = begin_login().unwrap();
        let cancel = AtomicBool::new(true);
        let err = session.wait(Duration::from_secs(30), &cancel).unwrap_err();
        assert!(err.to_string().contains("cancelado"));
    }

    #[test]
    fn understands_the_ways_a_user_may_identify_their_profile() {
        assert_eq!(
            parse_account_input("76561198002516729"),
            Some(SteamInput::Id(ID))
        );
        assert_eq!(
            parse_account_input("https://steamcommunity.com/profiles/76561198002516729/"),
            Some(SteamInput::Id(ID))
        );
        assert_eq!(
            parse_account_input("https://steamcommunity.com/id/mi_perfil-1/"),
            Some(SteamInput::Vanity("mi_perfil-1".into()))
        );
        assert_eq!(parse_account_input("12345"), None);
        assert_eq!(parse_account_input("hola mundo"), None);
    }

    #[test]
    fn api_keys_have_a_fixed_shape() {
        assert!(is_valid_api_key("0123456789ABCDEF0123456789abcdef"));
        assert!(!is_valid_api_key("0123"));
        assert!(!is_valid_api_key("ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ"));
    }

    #[test]
    fn parses_owned_games_and_private_profiles() {
        let games = parse_owned_games(
            r#"{"response":{"game_count":3,"games":[
                {"appid":220,"name":"Half-Life 2","playtime_forever":125,"rtime_last_played":1700000000},
                {"appid":440,"playtime_forever":0,"rtime_last_played":0},
                {"appid":220,"name":"Repetido"}]}}"#,
        )
        .unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].name, "Half-Life 2");
        assert_eq!(games[0].playtime_minutes, 125);
        assert_eq!(games[0].last_played_at, Some(1_700_000_000));
        assert_eq!(games[1].name, "Aplicación 440");
        assert_eq!(games[1].last_played_at, None);

        // Perfil privado: Steam responde con un objeto vacío.
        assert!(parse_owned_games(r#"{"response":{}}"#).unwrap().is_empty());
        assert!(parse_owned_games("<html>").is_err());
    }

    #[test]
    fn parses_persona_and_vanity_responses() {
        assert_eq!(
            parse_persona(r#"{"response":{"players":[{"steamid":"1","personaname":" Ana "}]}}"#),
            Some("Ana".to_string())
        );
        assert_eq!(parse_persona(r#"{"response":{"players":[]}}"#), None);
        assert_eq!(
            parse_vanity(r#"{"response":{"success":1,"steamid":"76561198002516729"}}"#).unwrap(),
            Some(ID)
        );
        assert_eq!(
            parse_vanity(r#"{"response":{"success":42}}"#).unwrap(),
            None
        );
    }

    #[test]
    fn urls_are_built_for_the_documented_endpoints() {
        let key = "0123456789ABCDEF0123456789ABCDEF";
        assert!(owned_games_url(key, ID).contains("IPlayerService/GetOwnedGames/v1/?key="));
        assert!(owned_games_url(key, ID).contains("include_appinfo=true"));
        assert!(vanity_url(key, "a b").contains("vanityurl=a%20b"));
    }
}
