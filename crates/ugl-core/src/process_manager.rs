//! process_manager: inicia juegos. Tres métodos: ejecutable directo (con argumentos y directorio
//! de trabajo), Steam (`steam://rungameid/…`) y otras plataformas (URI de una lista permitida).
//! Nunca se inicia un juego a través de un acceso directo (.lnk/.url).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diagnostics;
use crate::domain::{Game, GameStatus, LaunchKind};
use crate::error::{Result, UglError};

/// Esquemas de URI que UGL acepta como método de inicio «otra plataforma».
pub const ALLOWED_URI_SCHEMES: &[&str] = &[
    "steam",
    "com.epicgames.launcher",
    "goggalaxy",
    "itch",
    "uplay",
    "origin2",
    "battlenet",
];

/// Comprobación barata (solo consulta si existen carpeta y ejecutable). No se hace en bloque al
/// arrancar: se usa al abrir la ficha de un juego y al iniciarlo.
pub fn check_status(game: &Game) -> GameStatus {
    if game.launch_kind == LaunchKind::Uri {
        return GameStatus::Ready;
    }
    if !game.install_path.is_dir() {
        return GameStatus::MissingFolder;
    }
    if game.launch_kind == LaunchKind::Executable
        && !game.install_path.join(&game.launch_target).is_file()
    {
        return GameStatus::MissingExecutable;
    }
    GameStatus::Ready
}

pub fn is_shortcut_file(path: &Path) -> bool {
    path.extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            e == "lnk" || e == "url"
        })
        .unwrap_or(false)
}

/// Una URI es válida si su esquema está en la lista permitida y solo lleva caracteres seguros.
pub fn validate_uri(uri: &str) -> Result<()> {
    let uri = uri.trim();
    let (scheme, rest) = uri
        .split_once("://")
        .ok_or_else(|| UglError::Invalid("La dirección debe tener la forma esquema://…".into()))?;
    if !ALLOWED_URI_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
        return Err(UglError::Invalid(format!(
            "El esquema «{scheme}» no está permitido. Esquemas admitidos: {}.",
            ALLOWED_URI_SCHEMES.join(", ")
        )));
    }
    if rest.is_empty() || uri.len() > 512 {
        return Err(UglError::Invalid(
            "La dirección está vacía o es demasiado larga.".into(),
        ));
    }
    let safe = uri
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ":/?=&._~%+#@!-,".contains(c));
    if !safe {
        return Err(UglError::Invalid(
            "La dirección contiene caracteres no permitidos.".into(),
        ));
    }
    Ok(())
}

/// Direcciones web que UGL abre en el navegador: solo sitios de Steam y sin caracteres especiales.
pub fn validate_web_url(url: &str) -> Result<()> {
    const ALLOWED_PREFIXES: &[&str] = &[
        "https://steamcommunity.com/",
        "https://store.steampowered.com/",
    ];
    let safe = url
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ":/?=&._~%+#@!-,".contains(c));
    if !ALLOWED_PREFIXES
        .iter()
        .any(|prefix| url.starts_with(prefix))
        || !safe
        || url.len() > 1024
    {
        return Err(UglError::Invalid("Dirección web no permitida.".into()));
    }
    Ok(())
}

pub fn open_web_url(url: &str) -> Result<()> {
    validate_web_url(url)?;
    open_platform_uri(url)
}

/// Separa una línea de argumentos respetando las comillas (se usa fuera de Windows; en Windows la
/// línea se pasa tal cual al sistema).
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(current);
    }
    args
}

/// Inicia el juego. Devuelve el PID del proceso lanzado cuando UGL lo inicia directamente.
pub fn launch(game: &Game) -> Result<Option<u32>> {
    match game.launch_kind {
        LaunchKind::Executable => launch_executable(game),
        LaunchKind::SteamUri => {
            // Solo dígitos: evita que un valor corrupto llegue al intérprete de comandos.
            let app_id: u64 = game.launch_target.trim().parse().map_err(|_| {
                UglError::Invalid(format!(
                    "Identificador de Steam no válido: {}",
                    game.launch_target
                ))
            })?;
            diagnostics::info(&format!("Iniciando juego de Steam (AppID {app_id})"));
            open_steam_uri(&format!("steam://rungameid/{app_id}")).map(|()| None)
        }
        LaunchKind::Uri => {
            validate_uri(&game.launch_target)?;
            diagnostics::info(&format!("Iniciando por plataforma: {}", game.launch_target));
            open_platform_uri(game.launch_target.trim()).map(|()| None)
        }
    }
}

/// Carpeta de trabajo: la elegida por el usuario (si existe) o la carpeta del ejecutable.
fn resolve_working_dir(game: &Game, exe: &Path) -> PathBuf {
    let fallback = || {
        exe.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| game.install_path.clone())
    };
    match game
        .working_dir
        .as_deref()
        .filter(|dir| !dir.trim().is_empty())
    {
        Some(dir) => {
            let candidate = game.install_path.join(dir);
            if candidate.is_dir() {
                candidate
            } else {
                fallback()
            }
        }
        None => fallback(),
    }
}

fn launch_executable(game: &Game) -> Result<Option<u32>> {
    // Si el destino es absoluto, `join` lo respeta tal cual.
    let exe = game.install_path.join(&game.launch_target);
    if is_shortcut_file(&exe) {
        return Err(UglError::Invalid(
            "El inicio apunta a un acceso directo (.lnk/.url). Edita el método de inicio del juego \
             y elige su ejecutable (.exe)."
                .into(),
        ));
    }
    if !exe.is_file() {
        return Err(UglError::NotFound(format!(
            "No se encontró el ejecutable en {}. Puedes elegir otro desde la ficha del juego.",
            exe.display()
        )));
    }
    let working_dir = resolve_working_dir(game, &exe);
    diagnostics::info(&format!(
        "Iniciando {} (carpeta de trabajo: {}{})",
        exe.display(),
        working_dir.display(),
        if game.launch_args.trim().is_empty() {
            ""
        } else {
            "; con argumentos"
        }
    ));

    let mut command = Command::new(&exe);
    apply_args(&mut command, &game.launch_args);
    command.current_dir(&working_dir);
    command
        .spawn()
        .map(|child| Some(child.id()))
        .map_err(|err| describe_spawn_error(&exe, err))
}

#[cfg(windows)]
fn apply_args(command: &mut Command, args: &str) {
    use std::os::windows::process::CommandExt;
    let args = args.trim();
    if !args.is_empty() {
        // La línea se entrega tal cual, como la escribió el usuario (sin volver a entrecomillar).
        command.raw_arg(args);
    }
}

#[cfg(not(windows))]
fn apply_args(command: &mut Command, args: &str) {
    for arg in split_args(args) {
        command.arg(arg);
    }
}

fn describe_spawn_error(exe: &Path, err: io::Error) -> UglError {
    // 740 = ERROR_ELEVATION_REQUIRED en Windows.
    if err.raw_os_error() == Some(740) {
        UglError::Invalid(format!(
            "{} requiere permisos de administrador. Ejecuta UGL como administrador o inicia el juego manualmente.",
            exe.display()
        ))
    } else {
        UglError::Invalid(format!("No se pudo iniciar {}: {err}", exe.display()))
    }
}

#[cfg(windows)]
fn open_steam_uri(uri: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("cmd")
        .args(["/C", "start", "", uri])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_child| ())
        .map_err(|err| UglError::Invalid(format!("No se pudo abrir {uri}: {err}")))
}

#[cfg(not(windows))]
fn open_steam_uri(uri: &str) -> Result<()> {
    open_platform_uri(uri)
}

/// Otras plataformas y sitios web: se entrega la dirección (ya validada) al sistema con la función
/// oficial de Windows para abrir enlaces en su programa predeterminado. No se usa `explorer.exe`:
/// cuando no entiende el argumento abre la carpeta de Documentos.
#[cfg(windows)]
fn open_platform_uri(uri: &str) -> Result<()> {
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            window: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }
    const SW_SHOWNORMAL: i32 = 1;

    let operation: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: ambas cadenas terminan en cero y viven durante la llamada.
    let result = unsafe {
        ShellExecuteW(
            0,
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Un valor mayor que 32 indica éxito.
    if result > 32 {
        Ok(())
    } else {
        Err(UglError::Invalid(format!(
            "Windows no pudo abrir la dirección (código {result}). ¿Hay un navegador predeterminado configurado?"
        )))
    }
}

#[cfg(not(windows))]
fn open_platform_uri(uri: &str) -> Result<()> {
    Command::new("xdg-open")
        .arg(uri)
        .spawn()
        .map(|_child| ())
        .map_err(|err| UglError::Invalid(format!("No se pudo abrir {uri}: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(kind: LaunchKind, target: &str) -> Game {
        Game {
            id: 1,
            title: "T".into(),
            platform: "pc".into(),
            source: "manual".into(),
            install_path: PathBuf::from("juego"),
            launch_kind: kind,
            launch_target: target.into(),
            added_at: 0,
            last_played_at: None,
            achievements_total: 0,
            achievements_unlocked: 0,
            icon_path: None,
            exe_path: None,
            launch_args: String::new(),
            working_dir: None,
            steam_app_id: None,
        }
    }

    #[test]
    fn splits_arguments_respecting_quotes() {
        assert_eq!(
            split_args(r#"-windowed -config "C:\Mis Juegos\a.cfg" +map "" x"#),
            vec![
                "-windowed",
                "-config",
                "C:\\Mis Juegos\\a.cfg",
                "+map",
                "",
                "x"
            ]
        );
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn only_known_platform_uris_are_accepted() {
        assert!(validate_uri("goggalaxy://openGameView/123").is_ok());
        assert!(
            validate_uri("com.epicgames.launcher://apps/Fortnite?action=launch&silent=true")
                .is_ok()
        );
        assert!(validate_uri("steam://rungameid/220").is_ok());
        assert!(validate_uri("file:///C:/Windows/System32/cmd.exe").is_err());
        assert!(validate_uri("http://example.com").is_err());
        assert!(validate_uri("steam://x y").is_err());
        assert!(validate_uri("steam://x|calc").is_err());
        assert!(validate_uri("steam://").is_err());
        assert!(validate_uri("no es una uri").is_err());
    }

    #[test]
    fn only_steam_sites_open_in_the_browser() {
        assert!(validate_web_url("https://store.steampowered.com/app/220").is_ok());
        assert!(validate_web_url("https://steamcommunity.com/dev/apikey").is_ok());
        assert!(validate_web_url(
            "https://steamcommunity.com/openid/login?openid.mode=checkid_setup&a=b%3A"
        )
        .is_ok());
        assert!(validate_web_url("https://evil.example/").is_err());
        assert!(validate_web_url("https://steamcommunity.com.evil.example/").is_err());
        assert!(validate_web_url("https://store.steampowered.com/a b").is_err());
        assert!(validate_web_url("https://store.steampowered.com/a|calc").is_err());
        assert!(validate_web_url("file:///C:/x").is_err());
    }

    #[test]
    fn shortcuts_are_never_launched() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("juego.lnk"), b"x").unwrap();
        let mut g = game(LaunchKind::Executable, "juego.lnk");
        g.install_path = tmp.path().to_path_buf();
        let err = launch(&g).unwrap_err().to_string();
        assert!(err.contains("acceso directo"));
        assert!(is_shortcut_file(Path::new("a.LNK")));
        assert!(is_shortcut_file(Path::new("a.url")));
        assert!(!is_shortcut_file(Path::new("a.exe")));
    }

    #[test]
    fn working_dir_defaults_to_the_executable_folder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
        std::fs::create_dir_all(tmp.path().join("datos")).unwrap();
        let exe = tmp.path().join("bin").join("g.exe");

        let mut g = game(LaunchKind::Executable, "bin/g.exe");
        g.install_path = tmp.path().to_path_buf();
        assert_eq!(resolve_working_dir(&g, &exe), tmp.path().join("bin"));

        g.working_dir = Some("datos".to_string()); // relativa a la carpeta del juego
        assert_eq!(resolve_working_dir(&g, &exe), tmp.path().join("datos"));

        g.working_dir = Some("no_existe".to_string()); // inexistente: se vuelve a la del ejecutable
        assert_eq!(resolve_working_dir(&g, &exe), tmp.path().join("bin"));
    }

    #[test]
    fn uri_games_do_not_need_an_install_folder() {
        let g = game(LaunchKind::Uri, "goggalaxy://openGameView/1");
        assert_eq!(check_status(&g), GameStatus::Ready);
        let missing = game(LaunchKind::Executable, "x.exe");
        assert_eq!(check_status(&missing), GameStatus::MissingFolder);
    }
}
