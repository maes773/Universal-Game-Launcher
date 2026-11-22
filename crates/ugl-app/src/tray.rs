//! Icono de la bandeja del sistema (Windows). Solo existe mientras UGL está minimizado.
//! Los eventos llegan por el bucle de mensajes de la interfaz; los manejadores solo avisan a
//! `main.rs` mediante funciones simples.

pub struct Tray {
    #[cfg(windows)]
    _icon: tray_icon::TrayIcon,
}

#[cfg(windows)]
pub fn create(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    on_open: fn(),
    on_quit: fn(),
) -> Result<Tray, String> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let icon = Icon::from_rgba(rgba, width, height).map_err(|err| err.to_string())?;

    let open_item = MenuItem::new("Abrir UGL", true, None);
    let quit_item = MenuItem::new("Salir de UGL", true, None);
    let menu = Menu::new();
    menu.append(&open_item).map_err(|err| err.to_string())?;
    menu.append(&quit_item).map_err(|err| err.to_string())?;
    let open_id = open_item.id().clone();
    let quit_id = quit_item.id().clone();

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == open_id {
            on_open();
        } else if event.id == quit_id {
            on_quit();
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // Clic (o doble clic) con el botón izquierdo: volver a abrir. El menú sale con el derecho.
        match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => on_open(),
            _ => {}
        }
    }));

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("UGL - Universal Game Launcher")
        .with_icon(icon)
        .build()
        .map_err(|err| err.to_string())?;
    Ok(Tray { _icon: tray })
}

#[cfg(not(windows))]
pub fn create(
    _rgba: Vec<u8>,
    _width: u32,
    _height: u32,
    _on_open: fn(),
    _on_quit: fn(),
) -> Result<Tray, String> {
    Err("la bandeja del sistema solo está disponible en Windows".to_string())
}
