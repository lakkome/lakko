// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The collapsed call as a window of its own (`docs/ux/calls.md` section
//! 4.5.4a, `docs/ux/desktop.md` section 2.1).
//!
//! In the desktop app a collapsed call is a small frameless window of the
//! operating system that stays above every other program's windows. Its
//! webview cannot receive the call's media: those live in the main window's
//! page. So the main page relays what the window shows, through the host:
//!
//! - `POST view` `{visible, view?}` (main window only): the window's state as
//!   the page computed it (the words, the lock, the mute button, what the
//!   picture is). `visible: false` hides the window and drops every frame.
//! - `PUT frame/picture`, `PUT frame/inset` (main window only): one JPEG or
//!   WebP image, the picture and the own-camera inset drawn at the window's
//!   size; an empty body clears the slot.
//!
//! and the window answers back:
//!
//! - `GET state` (call window only): `{view, picture, inset}`, the last view
//!   and the sequence numbers of the frames held, for a page that just loaded;
//! - `GET frame/picture?n=..`, `GET frame/inset?n=..` (call window only): the
//!   held image;
//! - `POST action` `{action: "mute" | "expand"}` (call window only): `mute`
//!   goes to the main page as `{kind: "call-window", action: "toggle-mute"}`;
//!   `expand` hides this window, brings the main window forward and sends the
//!   page `{kind: "call-window", action: "expand"}`;
//! - `POST drag` (call window only): the person is dragging the window; the
//!   OS moves it until the button is released.
//!
//! The host pushes to the window with a script call,
//! `window.__lakkoCallWindow.view(..)` / `.frame(slot, n)`, as it does to the
//! main page with the `lakko:host` event.
//!
//! What the call window can reach is fenced in `proxy.rs`: these routes and
//! the static files of its own page, nothing else (no `/api/*`, no device
//! key, no other host route, not the app's `index.html`). The images never
//! leave the process: they go from one webview to the other through this
//! module's memory and nowhere else, and no image or view is logged.

use std::sync::Mutex;

use http::{header, Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tauri::{
    AppHandle, LogicalSize, Manager, PhysicalPosition, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use tracing::warn;

use crate::app_shell::{self, MAIN};
use crate::proxy::plain;

/// Path prefix of the call window's routes.
pub const PATH: &str = "/__host/call-window";
/// Label of the call window.
pub const LABEL: &str = "call-window";
/// The page the call window loads: an entry of the bundled web build of its
/// own, which does not start the app.
pub const PAGE: &str = "/call-window.html";
/// Width of the window in points (section 4.5.2: a desktop window is 16:9,
/// 320 points wide).
pub const WIDTH: f64 = 320.0;
/// Height of the window in points.
pub const HEIGHT: f64 = 180.0;
/// Gap between the window and the edges of the work area, in points.
pub const EDGE: f64 = 16.0;
/// Largest view the page may post. A view is a few short strings; chosen,
/// not measured.
pub const MAX_VIEW_BYTES: usize = 16 * 1024;
/// Largest frame. A 640x360 JPEG at the page's quality is far below this;
/// chosen, not measured.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// One of the two images the window draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The person shown (their camera, their photo, or a screen share).
    Picture,
    /// Your own camera while it sends.
    Inset,
}

impl Slot {
    const fn index(self) -> usize {
        match self {
            Self::Picture => 0,
            Self::Inset => 1,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Picture => "picture",
            Self::Inset => "inset",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "picture" => Some(Self::Picture),
            "inset" => Some(Self::Inset),
            _ => None,
        }
    }
}

/// A route under [`PATH`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    View,
    PutFrame(Slot),
    State,
    GetFrame(Slot),
    Action,
    Drag,
}

/// Which route a request names.
pub fn route(method: &Method, op: &str) -> Option<Route> {
    let frame = op.strip_prefix("/frame/").and_then(Slot::parse);
    match (method, op) {
        (&Method::POST, "/view") => Some(Route::View),
        (&Method::GET, "/state") => Some(Route::State),
        (&Method::POST, "/action") => Some(Route::Action),
        (&Method::POST, "/drag") => Some(Route::Drag),
        (&Method::PUT | &Method::POST, _) => frame.map(Route::PutFrame),
        (&Method::GET, _) => frame.map(Route::GetFrame),
        _ => None,
    }
}

/// Whether the webview labelled `label` may use `route`: the main page feeds
/// the window, the window reads what it was fed and answers clicks.
pub fn allowed(route: Route, label: &str) -> bool {
    match route {
        Route::View | Route::PutFrame(_) => label == MAIN,
        Route::State | Route::GetFrame(_) | Route::Action | Route::Drag => label == LABEL,
    }
}

/// Whether the call window may load `path` as a static file: its own page
/// and the scripts, styles, fonts and images that page uses. Never another
/// page of the build (the app's `index.html` would start the app in this
/// window) and never the wasm core.
pub fn page_may_load(path: &str) -> bool {
    if path == PAGE {
        return true;
    }
    if path.contains("..") || path.contains('\\') || !path.starts_with('/') {
        return false;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    let Some((_, ext)) = last.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext,
        "js" | "css" | "woff2" | "woff" | "ttf" | "svg" | "png" | "ico"
    )
}

/// The image type of `bytes` from its first bytes, for the two formats the
/// page sends; `None` for anything else.
pub fn frame_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Check a posted view: a JSON object within [`MAX_VIEW_BYTES`]. Returns it
/// written out again, so what reaches the window's script is JSON this
/// module produced.
pub fn checked_view(v: &Value) -> Option<String> {
    if !v.is_object() {
        return None;
    }
    let text = v.to_string();
    (text.len() <= MAX_VIEW_BYTES).then_some(text)
}

/// A rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    fn contains(&self, x: i64, y: i64) -> bool {
        let (rx, ry) = (i64::from(self.x), i64::from(self.y));
        x >= rx && x < rx + i64::from(self.width) && y >= ry && y < ry + i64::from(self.height)
    }
}

/// Where the window opens: the top right corner of `work_area`, [`EDGE`]
/// points in, for a window `width` physical pixels wide.
pub fn default_position(work_area: Rect, scale: f64, width: u32) -> (i32, i32) {
    let edge = i64::from(points_to_px(EDGE, scale));
    let right = i64::from(work_area.x) + i64::from(work_area.width);
    let x = (right - edge - i64::from(width)).max(i64::from(work_area.x));
    let y = i64::from(work_area.y) + edge;
    (clamp_i32(x), clamp_i32(y))
}

/// `points` in physical pixels at `scale`; a scale that is not a positive
/// number reads as 1.
pub fn points_to_px(points: f64, scale: f64) -> u32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    // `as` saturates a float into the integer's range, and the window's
    // numbers are small and positive.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let px = (points * scale).round().max(0.0) as u32;
    px
}

fn clamp_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

/// Whether a window at `at` of `size` still has its centre on one of
/// `screens`, so a remembered place is used only while it is on screen.
pub fn on_a_screen(at: (i32, i32), size: (u32, u32), screens: &[Rect]) -> bool {
    let cx = i64::from(at.0) + i64::from(size.0) / 2;
    let cy = i64::from(at.1) + i64::from(size.1) / 2;
    screens.iter().any(|s| s.contains(cx, cy))
}

/// The script that hands `call` to the window's page, if it has loaded.
fn page_call(call: &str) -> String {
    format!("window.__lakkoCallWindow && window.__lakkoCallWindow.{call}")
}

struct Frame {
    bytes: Vec<u8>,
    mime: &'static str,
    seq: u64,
}

#[derive(Default)]
struct Inner {
    /// The page wants the window on screen.
    visible: bool,
    view: Option<String>,
    frames: [Option<Frame>; 2],
    seq: u64,
    /// Where the window was last left, while the app runs.
    position: Option<(i32, i32)>,
}

/// What the host keeps for the call window.
#[derive(Default)]
pub struct CallWindow {
    inner: Mutex<Inner>,
}

impl CallWindow {
    /// Record where the window is (it was moved or hidden).
    pub fn remember_position(&self, at: PhysicalPosition<i32>) {
        if let Ok(mut i) = self.inner.lock() {
            i.position = Some((at.x, at.y));
        }
    }

    fn visible(&self) -> bool {
        self.inner.lock().is_ok_and(|i| i.visible)
    }
}

/// Hide the window and drop what it was showing: the main page started a new
/// load (whatever call it relayed is gone with the old page).
pub fn reset<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<CallWindow>() else {
        return;
    };
    hide(app, &state);
}

fn hide<R: Runtime>(app: &AppHandle<R>, state: &CallWindow) {
    if let Ok(mut i) = state.inner.lock() {
        i.visible = false;
        i.view = None;
        i.frames = [None, None];
    }
    if let Some(w) = app.get_webview_window(LABEL) {
        if w.is_visible().unwrap_or(false) {
            if let Ok(at) = w.outer_position() {
                state.remember_position(at);
            }
        }
        let _ = w.hide();
        // The page drops its last picture too, so a later show never flashes
        // the previous call.
        let _ = w.eval(page_call("clear()"));
    }
}

/// The call window, created the first time it is needed.
fn window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    if let Some(w) = app.get_webview_window(LABEL) {
        return Some(w);
    }
    let url = crate::app_url(PAGE).parse().ok()?;
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::CustomProtocol(url))
        .title("Lakko")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .closable(false)
        .decorations(false)
        // Transparent, so the page's rounded corners show (section 4.5.2:
        // `--radius-md`, 12 points); the shadow is the OS's own. macOS: a
        // clear, non-opaque window, and the system shadow follows the drawn
        // shape. Windows: the DWM shadow; on Windows 11 DWM also rounds the
        // window's corners (its own radius), on Windows 10 an undecorated
        // window with a shadow gets a 1 px border instead. Linux: no shadow
        // (unsupported), and the corners are transparent only under a
        // compositing window manager.
        .transparent(true)
        .shadow(true)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        // Showing it never takes the keyboard from the window the person is
        // typing in; a click still reaches it.
        .focused(false)
        .focusable(false)
        .accept_first_mouse(true)
        .disable_drag_drop_handler()
        .visible(false)
        .build();
    match built {
        Ok(w) => Some(w),
        Err(e) => {
            warn!(error = %e, "call window");
            None
        },
    }
}

/// Put the window where it was left, or at the top right of the screen the
/// main window is on.
fn place<R: Runtime>(app: &AppHandle<R>, state: &CallWindow, w: &WebviewWindow<R>) {
    let size = w.outer_size().map_or_else(
        |_| (points_to_px(WIDTH, 1.0), points_to_px(HEIGHT, 1.0)),
        |s| (s.width, s.height),
    );
    let screens: Vec<Rect> = w
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let a = m.work_area();
            Rect {
                x: a.position.x,
                y: a.position.y,
                width: a.size.width,
                height: a.size.height,
            }
        })
        .collect();
    let kept = state.inner.lock().ok().and_then(|i| i.position);
    if let Some(at) = kept {
        if on_a_screen(at, size, &screens) {
            let _ = w.set_position(PhysicalPosition::new(at.0, at.1));
            return;
        }
    }
    let monitor = app
        .get_webview_window(MAIN)
        .and_then(|m| m.current_monitor().ok().flatten())
        .or_else(|| w.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else { return };
    let a = monitor.work_area();
    let area = Rect {
        x: a.position.x,
        y: a.position.y,
        width: a.size.width,
        height: a.size.height,
    };
    let scale = monitor.scale_factor();
    // The window may have been made on a screen of another scale: size it in
    // points again for this one before reading its width.
    let _ = w.set_size(LogicalSize::new(WIDTH, HEIGHT));
    let width = points_to_px(WIDTH, scale);
    let (x, y) = default_position(area, scale, width);
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

/// Answer a request under [`PATH`] from the webview labelled `label`.
pub fn handle<R: Runtime>(
    app: &AppHandle<R>,
    state: &CallWindow,
    label: &str,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let op = request.uri().path().strip_prefix(PATH).unwrap_or("");
    let Some(route) = route(request.method(), op) else {
        return plain(StatusCode::NOT_FOUND, "not found");
    };
    if !allowed(route, label) {
        return plain(StatusCode::FORBIDDEN, "not for this window");
    }
    match route {
        Route::View => post_view(app, state, request.body()),
        Route::PutFrame(slot) => put_frame(app, state, slot, request.body()),
        Route::State => {
            let Ok(i) = state.inner.lock() else {
                return plain(StatusCode::INTERNAL_SERVER_ERROR, "state");
            };
            let view = i
                .view
                .as_deref()
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
                .unwrap_or(Value::Null);
            let seq = |s: Slot| i.frames[s.index()].as_ref().map(|f| f.seq);
            json_response(&json!({
                "visible": i.visible,
                "view": view,
                "picture": seq(Slot::Picture),
                "inset": seq(Slot::Inset),
            }))
        },
        Route::GetFrame(slot) => {
            let Ok(i) = state.inner.lock() else {
                return plain(StatusCode::INTERNAL_SERVER_ERROR, "state");
            };
            match &i.frames[slot.index()] {
                Some(f) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, f.mime)
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(f.bytes.clone())
                    .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "frame")),
                None => plain(StatusCode::NOT_FOUND, "no frame"),
            }
        },
        Route::Action => {
            let Ok(v) = serde_json::from_slice::<Value>(request.body()) else {
                return plain(StatusCode::BAD_REQUEST, "json");
            };
            match v.get("action").and_then(Value::as_str) {
                Some("mute") => {
                    app_shell::send_page_event(
                        app,
                        &json!({ "kind": "call-window", "action": "toggle-mute" }),
                    );
                    no_content()
                },
                Some("expand") => {
                    hide(app, state);
                    app_shell::show_main(app);
                    app_shell::send_page_event(
                        app,
                        &json!({ "kind": "call-window", "action": "expand" }),
                    );
                    no_content()
                },
                _ => plain(StatusCode::BAD_REQUEST, "action"),
            }
        },
        Route::Drag => {
            if let Some(w) = app.get_webview_window(LABEL) {
                if let Err(e) = w.start_dragging() {
                    warn!(error = %e, "call window drag");
                }
            }
            no_content()
        },
    }
}

fn post_view<R: Runtime>(app: &AppHandle<R>, state: &CallWindow, body: &[u8]) -> Response<Vec<u8>> {
    if body.len() > MAX_VIEW_BYTES + 64 {
        return plain(StatusCode::PAYLOAD_TOO_LARGE, "view");
    }
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return plain(StatusCode::BAD_REQUEST, "json");
    };
    if v.get("visible").and_then(Value::as_bool) != Some(true) {
        hide(app, state);
        return no_content();
    }
    let Some(view) = v.get("view").and_then(checked_view) else {
        return plain(StatusCode::BAD_REQUEST, "view");
    };
    let was_visible = state.visible();
    if let Ok(mut i) = state.inner.lock() {
        i.visible = true;
        i.view = Some(view.clone());
    }
    let Some(w) = window(app) else {
        return plain(StatusCode::SERVICE_UNAVAILABLE, "call window");
    };
    if let Err(e) = w.eval(page_call(&format!("view({view})"))) {
        warn!(error = %e, "call window view");
    }
    if !was_visible || !w.is_visible().unwrap_or(false) {
        place(app, state, &w);
        if let Err(e) = w.show() {
            warn!(error = %e, "call window show");
        }
    }
    no_content()
}

fn put_frame<R: Runtime>(
    app: &AppHandle<R>,
    state: &CallWindow,
    slot: Slot,
    body: &[u8],
) -> Response<Vec<u8>> {
    if body.len() > MAX_FRAME_BYTES {
        return plain(StatusCode::PAYLOAD_TOO_LARGE, "frame");
    }
    let frame = if body.is_empty() {
        None
    } else {
        let Some(mime) = frame_mime(body) else {
            return plain(StatusCode::UNSUPPORTED_MEDIA_TYPE, "frame");
        };
        Some((mime, body.to_vec()))
    };
    let seq = {
        let Ok(mut i) = state.inner.lock() else {
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "state");
        };
        if !i.visible {
            // A frame that raced the hide: nothing to show it in.
            return plain(StatusCode::CONFLICT, "hidden");
        }
        i.seq += 1;
        let seq = i.seq;
        i.frames[slot.index()] = frame.map(|(mime, bytes)| Frame { bytes, mime, seq });
        i.frames[slot.index()].as_ref().map(|f| f.seq)
    };
    if let Some(w) = app.get_webview_window(LABEL) {
        let arg = seq.map_or_else(|| "null".to_owned(), |n| n.to_string());
        let _ = w.eval(page_call(&format!("frame(\"{}\", {arg})", slot.name())));
    }
    no_content()
}

fn no_content() -> Response<Vec<u8>> {
    let mut r = Response::new(Vec::new());
    *r.status_mut() = StatusCode::NO_CONTENT;
    r
}

fn json_response(v: &Value) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(v.to_string().into_bytes())
        .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_parsed_by_method_and_path() {
        assert_eq!(route(&Method::POST, "/view"), Some(Route::View));
        assert_eq!(route(&Method::GET, "/state"), Some(Route::State));
        assert_eq!(
            route(&Method::PUT, "/frame/picture"),
            Some(Route::PutFrame(Slot::Picture))
        );
        assert_eq!(
            route(&Method::POST, "/frame/inset"),
            Some(Route::PutFrame(Slot::Inset))
        );
        assert_eq!(
            route(&Method::GET, "/frame/inset"),
            Some(Route::GetFrame(Slot::Inset))
        );
        assert_eq!(route(&Method::POST, "/action"), Some(Route::Action));
        assert_eq!(route(&Method::POST, "/drag"), Some(Route::Drag));
        assert_eq!(route(&Method::GET, "/view"), None);
        assert_eq!(route(&Method::GET, "/frame/other"), None);
        assert_eq!(route(&Method::DELETE, "/frame/picture"), None);
        assert_eq!(route(&Method::POST, "/frame/"), None);
    }

    #[test]
    fn the_main_page_feeds_and_the_call_window_reads() {
        for r in [
            Route::View,
            Route::PutFrame(Slot::Picture),
            Route::PutFrame(Slot::Inset),
        ] {
            assert!(allowed(r, MAIN));
            assert!(!allowed(r, LABEL));
            assert!(!allowed(r, "other"));
        }
        for r in [
            Route::State,
            Route::GetFrame(Slot::Picture),
            Route::Action,
            Route::Drag,
        ] {
            assert!(allowed(r, LABEL));
            assert!(!allowed(r, MAIN));
            assert!(!allowed(r, "other"));
        }
    }

    #[test]
    fn the_call_window_loads_only_its_own_page_and_static_files() {
        assert!(page_may_load("/call-window.html"));
        assert!(page_may_load("/assets/call-window-abc123.js"));
        assert!(page_may_load("/assets/styles-abc.css"));
        assert!(page_may_load("/fonts/inter.woff2"));
        assert!(!page_may_load("/"));
        assert!(!page_may_load("/index.html"));
        assert!(!page_may_load("/chats"));
        assert!(!page_may_load("/assets/lakko_ffi_wasm_bg-abc.wasm"));
        assert!(!page_may_load("/assets/../index.html"));
        assert!(!page_may_load("/link-preview/i.html"));
        assert!(!page_may_load("assets/x.js"));
    }

    #[test]
    fn only_jpeg_and_webp_frames_are_taken() {
        assert_eq!(frame_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0]), Some("image/jpeg"));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        assert_eq!(frame_mime(&webp), Some("image/webp"));
        webp[9] = b'X';
        assert_eq!(frame_mime(&webp), None);
        assert_eq!(frame_mime(b"<svg"), None);
        assert_eq!(frame_mime(&[0xFF, 0xD8]), None);
        assert_eq!(frame_mime(&[]), None);
    }

    #[test]
    fn a_view_is_an_object_within_the_bound() {
        assert!(checked_view(&json!({ "status": "12:04" })).is_some());
        assert!(checked_view(&json!("text")).is_none());
        assert!(checked_view(&json!([1, 2])).is_none());
        let big = "x".repeat(MAX_VIEW_BYTES);
        assert!(checked_view(&json!({ "name": big })).is_none());
        // What reaches the script is JSON written here, so a closing quote
        // inside a string stays inside it.
        let text = checked_view(&json!({ "name": "a\"); evil(\"" })).unwrap_or_default();
        assert_eq!(
            serde_json::from_str::<Value>(&text).ok(),
            Some(json!({ "name": "a\"); evil(\"" }))
        );
    }

    #[test]
    fn the_window_opens_at_the_top_right_of_the_work_area() {
        let area = Rect {
            x: 0,
            y: 50,
            width: 2880,
            height: 1700,
        };
        // 2x screen: 320 points are 640 pixels, 16 points are 32.
        assert_eq!(default_position(area, 2.0, 640), (2880 - 32 - 640, 50 + 32));
        let left_screen = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1040,
        };
        assert_eq!(default_position(left_screen, 1.0, 320), (-16 - 320, 16));
        // A bad scale reads as 1; a work area narrower than the window keeps
        // the window's left edge on it.
        assert_eq!(default_position(area, f64::NAN, 320), (2880 - 16 - 320, 66));
        let tiny = Rect {
            x: 100,
            y: 0,
            width: 200,
            height: 200,
        };
        assert_eq!(default_position(tiny, 1.0, 320).0, 100);
    }

    #[test]
    fn a_remembered_place_counts_only_while_on_a_screen() {
        let screens = [
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            Rect {
                x: 1920,
                y: 0,
                width: 1280,
                height: 800,
            },
        ];
        assert!(on_a_screen((100, 100), (320, 180), &screens));
        assert!(on_a_screen((2000, 500), (320, 180), &screens));
        assert!(!on_a_screen((5000, 100), (320, 180), &screens));
        // The second screen is shorter: a place below it is off screen.
        assert!(!on_a_screen((2000, 900), (320, 180), &screens));
        assert!(!on_a_screen((100, 100), (320, 180), &[]));
    }
}
