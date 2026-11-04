// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! "Save" on an attachment (`docs/ux/desktop.md` section 6).
//!
//! The webview cannot download from the app scheme, so the page sends the
//! decrypted bytes here and the host asks where to put them with the system
//! save dialog, opened at Downloads. Nothing is kept: the bytes leave this
//! process only for the file the person chose.

use http::{Request, Response, StatusCode};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use tracing::warn;

use crate::app_shell::MAIN;
use crate::proxy::plain;

/// Path of the route.
pub const PATH: &str = "/__host/save-file";

/// What a name becomes when nothing of it can be used.
const FALLBACK_NAME: &str = "attachment";

/// A direction mark or override: it can show `cod.txt` for a file that ends
/// in `.exe`.
fn is_direction_mark(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// The suggested name, from a sender-chosen string: its last path part, with
/// no control character, no direction mark and no leading dot, so it can name
/// nothing but a plain file in the folder the dialog shows.
fn safe_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !is_direction_mark(*c))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        FALLBACK_NAME.to_owned()
    } else {
        cleaned.to_owned()
    }
}

fn suggested_name(query: Option<&str>) -> String {
    let name = url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .find(|(k, _)| k == "name")
        .map(|(_, v)| v.into_owned());
    safe_name(name.as_deref().unwrap_or(""))
}

/// Answer `POST` [`PATH`]`?name=...`: `200` written, `204` the person
/// cancelled.
pub async fn handle<R: Runtime>(
    app: &AppHandle<R>,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    if request.method() != http::Method::POST {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "method");
    }
    let name = suggested_name(request.uri().query());
    let mut dialog = app.dialog().file().set_file_name(name);
    if let Ok(downloads) = app.path().download_dir() {
        dialog = dialog.set_directory(downloads);
    }
    if let Some(window) = app.get_webview_window(MAIN) {
        dialog = dialog.set_parent(&window);
    }
    let (chosen, answer) = oneshot::channel();
    dialog.save_file(move |path| {
        let _ = chosen.send(path);
    });
    let Ok(Some(path)) = answer.await else {
        let mut r = Response::new(Vec::new());
        *r.status_mut() = StatusCode::NO_CONTENT;
        return r;
    };
    let Ok(path) = path.into_path() else {
        return plain(StatusCode::BAD_REQUEST, "path");
    };
    let body = request.into_body();
    let written = tokio::task::spawn_blocking(move || std::fs::write(path, body)).await;
    match written {
        Ok(Ok(())) => plain(StatusCode::OK, "saved"),
        Ok(Err(e)) => {
            warn!(kind = ?e.kind(), "save file failed");
            plain(StatusCode::INTERNAL_SERVER_ERROR, "write")
        },
        Err(_) => plain(StatusCode::INTERNAL_SERVER_ERROR, "write"),
    }
}

#[cfg(test)]
mod tests {
    use super::{safe_name, suggested_name};

    #[test]
    fn a_name_cannot_leave_the_folder_or_hide() {
        assert_eq!(safe_name("photo.jpg"), "photo.jpg");
        assert_eq!(safe_name("../../.ssh/authorized_keys"), "authorized_keys");
        assert_eq!(safe_name("C:\\Users\\me\\run.bat"), "run.bat");
        assert_eq!(safe_name(".bashrc"), "bashrc");
        assert_eq!(safe_name("a\u{202e}b\nc.txt"), "abc.txt");
        assert_eq!(safe_name("  "), "attachment");
        assert_eq!(safe_name(""), "attachment");
    }

    #[test]
    fn the_name_comes_from_the_query() {
        assert_eq!(suggested_name(Some("name=a%20b.pdf")), "a b.pdf");
        assert_eq!(suggested_name(None), "attachment");
    }
}
