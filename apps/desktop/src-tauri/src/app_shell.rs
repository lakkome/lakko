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
//! - `POST quit`: the person confirmed quitting (after a call question);
//! - `POST notify` `{tag, title, body, target?}`: show a system notification
//!   (`notify.rs`), with `call: {accept, decline, callId, video}` for a ringing
//!   call; `503 notifications-unsupported` where none can be shown;
//! - `POST withdraw` `{tag}`: take a delivered notification back;
//! - `POST labels` `{open, quit}`: the tray menu in the interface language;
//! - `GET capabilities` -> `{privacySettings}`: whether this OS has a link to
//!   its microphone and camera settings;
//! - `POST open-privacy` `{what: "microphone" | "camera"}`: open that page;
//! - `POST attention`: bounce the Dock icon once / flash the taskbar (an
//!   incoming call with the window open, `desktop.md` section 4.1).
//!
//! The host talks to the page with a DOM event, `lakko:host`, whose `detail`
//! is `{kind: "quit-requested"}`, `{kind: "open-settings"}`,
//! `{kind: "open-target", target}` (a notification was clicked),
//! `{kind: "call-action", action, callId, video}` (Accept or Decline on one)
//! or `{kind: "still-running"}` (the window was hidden for the first time, on
//! the OSes that say so, `desktop.md` section 3).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use http::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};

use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime, UserAttentionType, WebviewWindow,
};
use tracing::{info, warn};

use crate::notify::{self, Notifier, Outcome};
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
        Self {
            keep_running: true,
            open_at_login: false,
        }
    }
}

impl Settings {
    fn to_json(self) -> Value {
        json!({ "keepRunning": self.keep_running, "openAtLogin": self.open_at_login })
    }

    fn from_json(v: &Value) -> Self {
        let d = Self::default();
        Self {
            keep_running: v
                .get("keepRunning")
                .and_then(Value::as_bool)
                .unwrap_or(d.keep_running),
            open_at_login: v
                .get("openAtLogin")
                .and_then(Value::as_bool)
                .unwrap_or(d.open_at_login),
        }
    }

    /// Apply a partial update from the page.
    fn merged(self, v: &Value) -> Self {
        Self {
            keep_running: v
                .get("keepRunning")
                .and_then(Value::as_bool)
                .unwrap_or(self.keep_running),
            open_at_login: v
                .get("openAtLogin")
                .and_then(Value::as_bool)
                .unwrap_or(self.open_at_login),
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
    notifier: Arc<Notifier>,
    /// A tray icon is on screen, so a hidden window has a way back.
    has_tray: AtomicBool,
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
    pub fn load(dir: PathBuf, notifier: Notifier) -> Self {
        let settings = read_json(&dir.join("settings.json"))
            .map(|v| Settings::from_json(&v))
            .unwrap_or_default();
        Self {
            dir,
            inner: Mutex::new(Inner {
                settings,
                call_live: false,
                unread: 0,
                quitting: false,
            }),
            notifier: Arc::new(notifier),
            has_tray: AtomicBool::new(false),
        }
    }

    /// Record whether the tray icon went up.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn set_has_tray(&self, on: bool) {
        self.has_tray.store(on, Ordering::SeqCst);
    }

    /// True once, the first time on this computer: the page says that Lakko
    /// is still running. macOS says nothing (`desktop.md` section 3).
    fn first_hide(&self) -> bool {
        if cfg!(target_os = "macos") {
            return false;
        }
        let marker = self.dir.join("still-running-told");
        if marker.exists() {
            return false;
        }
        write_json(&marker, &json!(true));
        true
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

    pub fn set_quitting(&self) {
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
    let on_a_display = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .any(|m| {
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
        shell.save_geometry(Geometry {
            x: p.x,
            y: p.y,
            width: s.width,
            height: s.height,
        });
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
    send_page_event(app, &json!({ "kind": kind }));
}

/// Send `detail` to the page as a `lakko:host` DOM event. `detail` is JSON
/// built here, so it is a valid script expression.
fn send_page_event<R: Runtime>(app: &AppHandle<R>, detail: &Value) {
    let kind = detail.get("kind").and_then(Value::as_str).unwrap_or("");
    if let Some(w) = app.get_webview_window(MAIN) {
        let detail = detail.to_string();
        let js =
            format!("window.dispatchEvent(new CustomEvent('lakko:host', {{ detail: {detail} }}))");
        if let Err(e) = w.eval(&js) {
            warn!(error = %e, kind, "notify page");
        }
    }
}

/// What to do with a window close request.
pub enum CloseAction {
    /// Hide the window; the app keeps running.
    Hide,
    /// Minimise it: Linux with no tray has no other way back.
    Minimize,
    /// Treat as a quit request.
    Quit,
}

pub fn on_close_requested(shell: &Shell) -> CloseAction {
    let has_way_back = cfg!(target_os = "macos") || shell.has_tray.load(Ordering::SeqCst);
    match (shell.settings().keep_running, has_way_back) {
        (false, _) => CloseAction::Quit,
        (true, true) => CloseAction::Hide,
        (true, false) => CloseAction::Minimize,
    }
}

/// The window was hidden; tell the page the first time.
pub fn after_hide<R: Runtime>(app: &AppHandle<R>, shell: &Shell) {
    if shell.first_hide() {
        notify_page(app, "still-running");
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
pub fn handle<R: Runtime>(
    app: &AppHandle<R>,
    shell: &Shell,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
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
                },
                Err(_) => false,
            };
            if changed_unread {
                if let Some(w) = app.get_webview_window(MAIN) {
                    set_unread_badge(&w, unread);
                }
            }
            no_content()
        },
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
        },
        (&Method::POST, "/notify") => {
            let Ok(v) = serde_json::from_slice::<Value>(request.body()) else {
                return plain(StatusCode::BAD_REQUEST, "json");
            };
            let Some(req) = notify::Request::from_json(&v) else {
                return plain(StatusCode::BAD_REQUEST, "notification");
            };
            if !shell.notifier.available() {
                return plain(StatusCode::SERVICE_UNAVAILABLE, "notifications-unsupported");
            }
            let app = app.clone();
            let shown = shell
                .notifier
                .show(req, move |outcome, req| on_notification(&app, outcome, req));
            if shown {
                no_content()
            } else {
                plain(StatusCode::SERVICE_UNAVAILABLE, "notifications-unsupported")
            }
        },
        (&Method::POST, "/withdraw") => {
            let Ok(v) = serde_json::from_slice::<Value>(request.body()) else {
                return plain(StatusCode::BAD_REQUEST, "json");
            };
            let Some(tag) = v.get("tag").and_then(Value::as_str) else {
                return plain(StatusCode::BAD_REQUEST, "tag");
            };
            // The OS call may block; the scheme handler must not.
            let notifier = Arc::clone(&shell.notifier);
            let tag = tag.to_owned();
            std::thread::spawn(move || notifier.withdraw(&tag));
            no_content()
        },
        (&Method::POST, "/labels") => {
            let Ok(v) = serde_json::from_slice::<Value>(request.body()) else {
                return plain(StatusCode::BAD_REQUEST, "json");
            };
            let label = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
            set_tray_labels(app, label("open"), label("quit"));
            no_content()
        },
        (&Method::POST, "/take-link") => match app
            .try_state::<crate::link::Held>()
            .and_then(|held| held.take())
        {
            Some(link) => json_response(&link),
            None => no_content(),
        },
        (&Method::GET, "/capabilities") => {
            json_response(&json!({ "privacySettings": privacy_link("microphone").is_some() }))
        },
        (&Method::POST, "/open-privacy") => {
            let Ok(v) = serde_json::from_slice::<Value>(request.body()) else {
                return plain(StatusCode::BAD_REQUEST, "json");
            };
            let Some(link) = v.get("what").and_then(Value::as_str).and_then(privacy_link) else {
                return plain(StatusCode::NOT_FOUND, "no link");
            };
            if let Err(e) = std::process::Command::new(OPENER).arg(link).spawn() {
                warn!(error = %e, "open privacy settings");
                return plain(StatusCode::INTERNAL_SERVER_ERROR, "open");
            }
            no_content()
        },
        (&Method::POST, "/attention") => {
            if let Some(w) = app.get_webview_window(MAIN) {
                if let Err(e) = w.request_user_attention(Some(UserAttentionType::Informational)) {
                    warn!(error = %e, "attention");
                }
            }
            no_content()
        },
        (&Method::POST, "/quit") => {
            info!("quit confirmed by the page");
            shell.set_quitting();
            if let Some(w) = app.get_webview_window(MAIN) {
                remember_geometry(&w, shell);
            }
            app.exit(0);
            no_content()
        },
        _ => plain(StatusCode::NOT_FOUND, "not found"),
    }
}

/// A notification was answered.
fn on_notification<R: Runtime>(app: &AppHandle<R>, outcome: Outcome, req: notify::Request) {
    if let (Some(call), Outcome::Accepted | Outcome::Declined) = (&req.call, outcome) {
        let action = if outcome == Outcome::Accepted {
            "accept"
        } else {
            "decline"
        };
        // Accept opens the window straight into the call; Decline leaves it
        // where it is (`desktop.md` section 4.1).
        if outcome == Outcome::Accepted {
            show_main(app);
        }
        send_page_event(
            app,
            &json!({ "kind": "call-action", "action": action, "callId": call.call_id, "video": call.video }),
        );
        return;
    }
    show_main(app);
    if let Some(target) = req.target {
        send_page_event(app, &json!({ "kind": "open-target", "target": target }));
    }
}

/// The command that opens a settings link.
#[cfg(windows)]
const OPENER: &str = "explorer.exe";
#[cfg(not(windows))]
const OPENER: &str = "open";

/// The OS page for `what`, from a fixed table: nothing the page sends reaches
/// the command. Linux has no such page.
fn privacy_link(what: &str) -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        match what {
            "microphone" => {
                Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            },
            "camera" => {
                Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Camera")
            },
            _ => None,
        }
    } else if cfg!(windows) {
        match what {
            "microphone" => Some("ms-settings:privacy-microphone"),
            "camera" => Some("ms-settings:privacy-webcam"),
            _ => None,
        }
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn set_tray_labels<R: Runtime>(app: &AppHandle<R>, open: &str, quit: &str) {
    if open.trim().is_empty() || quit.trim().is_empty() {
        return;
    }
    if let Some(tray) = app.try_state::<crate::tray::Tray<R>>() {
        tray.set_labels(open, quit);
    }
}

#[cfg(target_os = "macos")]
const fn set_tray_labels<R: Runtime>(_app: &AppHandle<R>, _open: &str, _quit: &str) {}

/// The unread count on the app icon. Windows has no number on a taskbar
/// button, only an overlay image: a dot says there is something unread.
#[cfg(not(windows))]
fn set_unread_badge<R: Runtime>(w: &WebviewWindow<R>, unread: i64) {
    if let Err(e) = w.set_badge_count((unread > 0).then_some(unread)) {
        warn!(error = %e, "badge");
    }
}

#[cfg(windows)]
fn set_unread_badge<R: Runtime>(w: &WebviewWindow<R>, unread: i64) {
    const SIZE: u32 = 16;
    let overlay = (unread > 0).then(|| {
        let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let (dx, dy) = (
                    2 * i64::from(x) + 1 - i64::from(SIZE),
                    2 * i64::from(y) + 1 - i64::from(SIZE),
                );
                let inside = dx * dx + dy * dy <= i64::from(SIZE) * i64::from(SIZE);
                rgba.extend_from_slice(&[0xE0, 0x3A, 0x3A, if inside { 0xFF } else { 0x00 }]);
            }
        }
        tauri::image::Image::new_owned(rgba, SIZE, SIZE)
    });
    if let Err(e) = w.set_overlay_icon(overlay) {
        warn!(error = %e, "badge");
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

/// Start at login, with [`HIDDEN_ARG`]. macOS: a per-user LaunchAgent (a
/// signed bundle should use `SMAppService` instead; that waits for S5).
/// Windows: the per-user Run key. Linux: an XDG autostart entry.
mod login_item {
    #[cfg(not(windows))]
    use tauri::Manager;
    use tauri::{AppHandle, Runtime};

    #[cfg(target_os = "macos")]
    pub fn set<R: Runtime>(app: &AppHandle<R>, on: bool) -> Result<(), String> {
        let id = app.config().identifier.clone();
        let home = app.path().home_dir().map_err(|e| e.to_string())?;
        let path = home
            .join("Library/LaunchAgents")
            .join(format!("{id}.plist"));
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

    #[cfg(windows)]
    pub fn set<R: Runtime>(_app: &AppHandle<R>, on: bool) -> Result<(), String> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        use winreg::RegKey;

        const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
        const VALUE: &str = "Lakko";
        let run = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN, KEY_SET_VALUE)
            .map_err(|e| e.to_string())?;
        if !on {
            return match run.delete_value(VALUE) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            };
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" {}", exe.display(), super::HIDDEN_ARG);
        run.set_value(VALUE, &command).map_err(|e| e.to_string())
    }

    /// An XDG autostart entry. An AppImage runs from a temporary mount, so its
    /// own path is what has to start.
    #[cfg(target_os = "linux")]
    pub fn set<R: Runtime>(app: &AppHandle<R>, on: bool) -> Result<(), String> {
        let id = app.config().identifier.clone();
        let dir = app
            .path()
            .config_dir()
            .map_err(|e| e.to_string())?
            .join("autostart");
        let path = dir.join(format!("{id}.desktop"));
        if !on {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            };
        }
        let exe = match std::env::var_os("APPIMAGE") {
            Some(path) => std::path::PathBuf::from(path),
            None => std::env::current_exe().map_err(|e| e.to_string())?,
        };
        let entry = super::autostart_entry(&exe.to_string_lossy());
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, entry).map_err(|e| e.to_string())
    }
}

/// An autostart `.desktop` file; the path is quoted as its `Exec` key asks.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn autostart_entry(exe: &str) -> String {
    let mut quoted = String::with_capacity(exe.len() + 2);
    for c in exe.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    format!("[Desktop Entry]\nType=Application\nName=Lakko\nExec=\"{quoted}\" {HIDDEN_ARG}\nX-GNOME-Autostart-enabled=true\n")
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
        let shell = Shell::load(dir.clone(), Notifier::new("test"));
        assert!(shell.saved_geometry().is_none());
        shell.save_geometry(Geometry {
            x: -100,
            y: 40,
            width: 1100,
            height: 760,
        });
        let again = Shell::load(dir.clone(), Notifier::new("test"));
        assert_eq!(
            again.saved_geometry(),
            Some(Geometry {
                x: -100,
                y: 40,
                width: 1100,
                height: 760
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn autostart_entry_quotes_the_path() {
        let e = autostart_entry("/opt/My \"Apps\"/$lakko");
        assert!(e.contains("Exec=\"/opt/My \\\"Apps\\\"/\\$lakko\" --hidden\n"));
    }

    #[test]
    fn privacy_links_come_from_a_fixed_table() {
        assert_eq!(privacy_link("anything else"), None);
        assert_eq!(
            privacy_link("microphone").is_some(),
            cfg!(any(target_os = "macos", windows))
        );
    }

    #[test]
    fn close_hides_unless_keep_running_is_off() {
        let dir = std::env::temp_dir().join(format!("lakko-shell-close-{}", std::process::id()));
        let shell = Shell::load(dir.clone(), Notifier::new("test"));
        let expected_way_back = cfg!(target_os = "macos");
        assert_eq!(
            matches!(on_close_requested(&shell), CloseAction::Hide),
            expected_way_back
        );
        shell.set_has_tray(true);
        assert!(matches!(on_close_requested(&shell), CloseAction::Hide));
        if let Ok(mut i) = shell.inner.lock() {
            i.settings.keep_running = false;
        }
        assert!(matches!(on_close_requested(&shell), CloseAction::Quit));
        let _ = std::fs::remove_dir_all(dir);
    }
}
