// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The app around the window (ADR-0085 section 4, `docs/ux/desktop.md`
//! sections 2, 3 and 5): window geometry, close is not quit, quitting during
//! a call, the Dock badge, the desktop-only settings, start at login, and the
//! macOS menu bar.
//!
//! The page talks to this module over the app scheme, under [`PATH`]:
//! - `POST state` `{callLive, unread}`: what the page knows; sets the badge;
//! - `GET settings` -> `{keepRunning, openAtLogin}`;
//! - `POST settings` `{keepRunning?, openAtLogin?}` -> the new settings;
//! - `POST quit`: the person confirmed quitting (after a call question).
//!
//! The host talks to the page with a DOM event, `lakko:host`, whose `detail`
//! is `{kind: "quit-requested"}` or `{kind: "open-settings"}`.

use std::path::PathBuf;
use std::sync::Mutex;

use http::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewWindow};
use tracing::{info, warn};

use crate::proxy::plain;

/// Path prefix of the app routes.
pub const PATH: &str = "/__host/app";
/// Label of the one main window.
pub const MAIN: &str = "main";
/// The argument a login item starts the app with: no window until asked.
pub const HIDDEN_ARG: &str = "--hidden";
/// Menu id of Settings.
const MENU_SETTINGS: &str = "lakko-settings";

/// Desktop-only settings, stored on this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Closing the window hides it instead of quitting (default on).
    pub keep_running: bool,
    /// Start hidden when the person signs in to the computer (default off).
    pub open_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { keep_running: true, open_at_login: false }
    }
}

impl Settings {
    fn to_json(self) -> Value {
        json!({ "keepRunning": self.keep_running, "openAtLogin": self.open_at_login })
    }

    fn from_json(v: &Value) -> Self {
        let d = Self::default();
        Self {
            keep_running: v.get("keepRunning").and_then(Value::as_bool).unwrap_or(d.keep_running),
            open_at_login: v.get("openAtLogin").and_then(Value::as_bool).unwrap_or(d.open_at_login),
        }
    }

    /// Apply a partial update from the page.
    fn merged(self, v: &Value) -> Self {
        Self {
            keep_running: v.get("keepRunning").and_then(Value::as_bool).unwrap_or(self.keep_running),
            open_at_login: v.get("openAtLogin").and_then(Value::as_bool).unwrap_or(self.open_at_login),
        }
    }
}

/// Saved window geometry, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// What the host knows and keeps.
pub struct Shell {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    settings: Settings,
    call_live: bool,
    unread: i64,
    /// Set once the person confirmed quitting; the next exit request passes.
    quitting: bool,
}

impl Shell {
    /// Load from `dir` (the app's config directory); missing or damaged files
    /// read as defaults.
    pub fn load(dir: PathBuf) -> Self {
        let settings = read_json(&dir.join("settings.json"))
            .map(|v| Settings::from_json(&v))
            .unwrap_or_default();
        Self {
            dir,
            inner: Mutex::new(Inner { settings, call_live: false, unread: 0, quitting: false }),
        }
    }

    pub fn settings(&self) -> Settings {
        self.inner.lock().map(|i| i.settings).unwrap_or_default()
    }

    fn call_live(&self) -> bool {
        self.inner.lock().map(|i| i.call_live).unwrap_or(false)
    }

    fn quitting(&self) -> bool {
        self.inner.lock().map(|i| i.quitting).unwrap_or(false)
    }

    fn set_quitting(&self) {
        if let Ok(mut i) = self.inner.lock() {
            i.quitting = true;
        }
    }

    /// The window geometry saved at the last close, if any.
    pub fn saved_geometry(&self) -> Option<Geometry> {
        let v = read_json(&self.dir.join("window.json"))?;
        let get = |k: &str| v.get(k).and_then(Value::as_i64);
        Some(Geometry {
            x: i32::try_from(get("x")?).ok()?,
            y: i32::try_from(get("y")?).ok()?,
            width: u32::try_from(get("width")?).ok()?,
            height: u32::try_from(get("height")?).ok()?,
        })
    }

    fn save_geometry(&self, g: Geometry) {
        let v = json!({ "x": g.x, "y": g.y, "width": g.width, "height": g.height });
        write_json(&self.dir.join("window.json"), &v);
    }
}

fn read_json(path: &std::path::Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_json(path: &std::path::Path, v: &Value) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(error = %e, "config dir");
            return;
        }
    }
    if let Err(e) = std::fs::write(path, v.to_string()) {
        warn!(error = %e, "config write");
    }
}

/// Put the saved geometry back if it still lands on a connected display.
pub fn restore_geometry<R: Runtime>(window: &WebviewWindow<R>, shell: &Shell) {
    let Some(g) = shell.saved_geometry() else {
        let _ = window.center();
        return;
    };
    let on_a_display = window.available_monitors().unwrap_or_default().iter().any(|m| {
        let p = m.position();
        let s = m.size();
        let (mx, my) = (i64::from(p.x), i64::from(p.y));
        let (mw, mh) = (i64::from(s.width), i64::from(s.height));
        let (cx, cy) = (i64::from(g.x) + 40, i64::from(g.y) + 40);
        cx >= mx && cx < mx + mw && cy >= my && cy < my + mh
    });
    let _ = window.set_size(PhysicalSize::new(g.width, g.height));
    if on_a_display {
        let _ = window.set_position(PhysicalPosition::new(g.x, g.y));
    } else {
        let _ = window.center();
    }
}

/// Remember where the window is; called on close and on quit.
pub fn remember_geometry<R: Runtime>(window: &WebviewWindow<R>, shell: &Shell) {
    if window.is_minimized().unwrap_or(false) {
        return;
    }
    if let (Ok(p), Ok(s)) = (window.outer_position(), window.inner_size()) {
        shell.save_geometry(Geometry { x: p.x, y: p.y, width: s.width, height: s.height });
    }
}

/// Show and focus the main window.
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Send `{kind}` to the page as a `lakko:host` DOM event.
pub fn notify_page<R: Runtime>(app: &AppHandle<R>, kind: &str) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let detail = json!({ "kind": kind }).to_string();
        let js = format!("window.dispatchEvent(new CustomEvent('lakko:host', {{ detail: {detail} }}))");
        if let Err(e) = w.eval(&js) {
            warn!(error = %e, kind, "notify page");
        }
    }
}

/// What to do with a window close request.
pub enum CloseAction {
    /// Hide the window; the app keeps running.
    Hide,
    /// Treat as a quit request.
    Quit,
}

pub fn on_close_requested(shell: &Shell) -> CloseAction {
    if shell.settings().keep_running {
        CloseAction::Hide
    } else {
        CloseAction::Quit
    }
}

/// Whether a quit the person asked for (menu, Cmd+Q, last window) may go
/// ahead now. During a call it may not: the window is shown and the page asks
/// "End the call and quit?" (`desktop.md` section 3), then posts `quit`.
pub fn quit_may_proceed<R: Runtime>(app: &AppHandle<R>, shell: &Shell) -> bool {
    if shell.quitting() || !shell.call_live() {
        return true;
    }
    show_main(app);
    notify_page(app, "quit-requested");
    false
}

/// Answer a request under [`PATH`].
pub fn handle<R: Runtime>(app: &AppHandle<R>, shell: &Shell, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let op = request.uri().path().strip_prefix(PATH).unwrap_or("");
    match (request.method(), op) {
        (&Method::POST, "/state") => {
            let Ok(v) = serde_json::from_slice::<Value>(request.body()) else {
                return plain(StatusCode::BAD_REQUEST, "json");
            };
            let call_live = v.get("callLive").and_then(Value::as_bool).unwrap_or(false);
            let unread = v.get("unread").and_then(Value::as_i64).unwrap_or(0).max(0);
            let changed_unread = match shell.inner.lock() {
                Ok(mut i) => {
                    i.call_live = call_live;
                    let changed = i.unread != unread;
                    i.unread = unread;
                    changed
                }
                Err(_) => false,
            };
            if changed_unread {
                if let Some(w) = app.get_webview_window(MAIN) {
                    let badge = (unread > 0).then_some(unread);
                    if let Err(e) = w.set_badge_count(badge) {
                        warn!(error = %e, "badge");
                    }
                }
            }
            no_content()
        }
        (&Method::GET, "/settings") => json_response(&shell.settings().to_json()),
        (&Method::POST, "/settings") => {
            let Ok(v) = serde_json::from_slice::<Value>(request.body()) else {
                return plain(StatusCode::BAD_REQUEST, "json");
            };
            let before = shell.settings();
            let after = before.merged(&v);
            if after.open_at_login != before.open_at_login {
                if let Err(e) = login_item::set(app, after.open_at_login) {
                    warn!(error = %e, "login item");
                    return plain(StatusCode::INTERNAL_SERVER_ERROR, "login item");
                }
            }
            if let Ok(mut i) = shell.inner.lock() {
                i.settings = after;
            }
            write_json(&shell.dir.join("settings.json"), &after.to_json());
            json_response(&after.to_json())
        }
        (&Method::POST, "/quit") => {
            info!("quit confirmed by the page");
            shell.set_quitting();
            if let Some(w) = app.get_webview_window(MAIN) {
                remember_geometry(&w, shell);
            }
            app.exit(0);
            no_content()
        }
        _ => plain(StatusCode::NOT_FOUND, "not found"),
    }
}

fn no_content() -> Response<Vec<u8>> {
    let mut r = Response::new(Vec::new());
    *r.status_mut() = StatusCode::NO_CONTENT;
    r
}

fn json_response(v: &Value) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(v.to_string().into_bytes())
        .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response"))
}

/// The macOS menu bar (`desktop.md` section 5). Windows and Linux get no
/// menu bar.
pub fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Option<Menu<R>>> {
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    let settings = MenuItem::with_id(app, MENU_SETTINGS, "Settings…", true, Some("CmdOrCtrl+,"))?;
    let app_menu = Submenu::with_items(
        app,
        "Lakko",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("About Lakko"), None)?,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some("Hide Lakko"))?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some("Quit Lakko"))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, Some("Zoom"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::bring_all_to_front(app, None)?,
        ],
    )?;
    Ok(Some(Menu::with_items(app, &[&app_menu, &edit, &window])?))
}

/// A menu item was chosen.
pub fn on_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if id == MENU_SETTINGS {
        show_main(app);
        notify_page(app, "open-settings");
    }
}

/// Start at login. macOS: a per-user LaunchAgent that starts this executable
/// with [`HIDDEN_ARG`]. A signed bundle should use `SMAppService` instead;
/// that waits for S5.
mod login_item {
    use tauri::{AppHandle, Manager, Runtime};

    #[cfg(target_os = "macos")]
    pub fn set<R: Runtime>(app: &AppHandle<R>, on: bool) -> Result<(), String> {
        let id = app.config().identifier.clone();
        let home = app.path().home_dir().map_err(|e| e.to_string())?;
        let path = home.join("Library/LaunchAgents").join(format!("{id}.plist"));
        if !on {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            };
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.to_string_lossy();
        if exe.contains(['<', '>', '&']) {
            return Err("executable path needs XML escaping".into());
        }
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
<key>Label</key><string>{id}</string>\n\
<key>ProgramArguments</key><array><string>{exe}</string><string>{arg}</string></array>\n\
<key>RunAtLoad</key><true/>\n\
</dict></plist>\n",
            arg = super::HIDDEN_ARG,
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, plist).map_err(|e| e.to_string())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn set<R: Runtime>(_app: &AppHandle<R>, _on: bool) -> Result<(), String> {
        Err("start at login is not built for this OS yet".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_and_merge() {
        let d = Settings::default();
        assert!(d.keep_running);
        assert!(!d.open_at_login);
        let m = d.merged(&json!({ "openAtLogin": true }));
        assert!(m.keep_running && m.open_at_login);
        let damaged = Settings::from_json(&json!({ "keepRunning": "yes" }));
        assert_eq!(damaged, Settings::default());
    }

    #[test]
    fn geometry_round_trips_through_the_config_dir() {
        let dir = std::env::temp_dir().join(format!("lakko-shell-test-{}", std::process::id()));
        let shell = Shell::load(dir.clone());
        assert!(shell.saved_geometry().is_none());
        shell.save_geometry(Geometry { x: -100, y: 40, width: 1100, height: 760 });
        let again = Shell::load(dir.clone());
        assert_eq!(again.saved_geometry(), Some(Geometry { x: -100, y: 40, width: 1100, height: 760 }));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn close_hides_unless_keep_running_is_off() {
        let dir = std::env::temp_dir().join(format!("lakko-shell-close-{}", std::process::id()));
        let shell = Shell::load(dir.clone());
        assert!(matches!(on_close_requested(&shell), CloseAction::Hide));
        if let Ok(mut i) = shell.inner.lock() {
            i.settings.keep_running = false;
        }
        assert!(matches!(on_close_requested(&shell), CloseAction::Quit));
        let _ = std::fs::remove_dir_all(dir);
    }
}
