// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The tray icon of Windows and Linux (`docs/ux/desktop.md` section 3): click
//! opens the window, the menu offers "Open Lakko" and "Quit Lakko". macOS has
//! the Dock and no tray icon.
//!
//! The host holds no catalog: the labels start in English and the page sends
//! them in the interface language (`POST /__host/app/labels`).

use tauri::menu::{Menu, MenuEvent, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};
use tracing::warn;

use crate::app_shell::{self, Shell};

const OPEN: &str = "lakko-tray-open";
const QUIT: &str = "lakko-tray-quit";

/// The two menu items, kept so their labels can follow the language.
pub struct Tray<R: Runtime> {
    open: MenuItem<R>,
    quit: MenuItem<R>,
}

impl<R: Runtime> Tray<R> {
    pub fn set_labels(&self, open: &str, quit: &str) {
        for (item, text) in [(&self.open, open), (&self.quit, quit)] {
            if let Err(e) = item.set_text(text) {
                warn!(error = %e, "tray label");
            }
        }
    }
}

/// Whether this desktop shows tray icons. Windows always does. On Linux a
/// tray exists when something owns the StatusNotifier watcher; stock GNOME has
/// none, and the tray library would have nowhere to draw.
fn tray_exists() -> bool {
    #[cfg(target_os = "linux")]
    {
        let owner = zbus::blocking::Connection::session().and_then(|c| {
            c.call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &("org.kde.StatusNotifierWatcher",),
            )?
            .body()
            .deserialize::<bool>()
        });
        owner.unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn on_menu<R: Runtime>(app: &AppHandle<R>, event: &MenuEvent) {
    match event.id().as_ref() {
        OPEN => app_shell::show_main(app),
        QUIT => {
            let Some(shell) = app.try_state::<Shell>() else {
                return;
            };
            if app_shell::quit_may_proceed(app, &shell) {
                if let Some(w) = app.get_webview_window(app_shell::MAIN) {
                    app_shell::remember_geometry(&w, &shell);
                }
                shell.set_quitting();
                app.exit(0);
            }
        },
        _ => {},
    }
}

/// Add the tray icon. Returns whether there is one.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<bool> {
    if !tray_exists() {
        return Ok(false);
    }
    let open = MenuItem::with_id(app, OPEN, "Open Lakko", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit Lakko", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Lakko")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, &event))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                app_shell::show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    app.manage(Tray { open, quit });
    Ok(true)
}
