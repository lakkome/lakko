// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko://` links from the OS (ADR-0085 section 5).
//!
//! macOS delivers a link as an event, so it may carry the secret after `#`.
//! Windows and Linux start the app with it as an argument other processes can
//! read, so a link from there is taken as the kind and the ledger id alone,
//! whatever it held, and the person pastes the whole link into the app.
//!
//! The link waits in [`Held`] until the page takes it: at a cold start the
//! page does not exist yet, and the secret stays out of a script string.
//! [`Held`] is registered on the builder, so a link is kept whenever the OS
//! delivers it, before the app's setup has run or after.

use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};
use tracing::{info, warn};
use url::Url;

use crate::app_shell::{notify_page, show_main};

const ID_CHARS: usize = 8;
const SECRET_CHARS: usize = 16;

fn is_link_text(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The newest link no page has taken yet.
#[derive(Default)]
pub struct Held(Mutex<Option<Value>>);

impl Held {
    fn put(&self, link: Value) {
        if let Ok(mut held) = self.0.lock() {
            *held = Some(link);
        }
    }

    /// Hand the link over, once.
    pub fn take(&self) -> Option<Value> {
        self.0.lock().ok().and_then(|mut held| held.take())
    }
}

/// How a link reached this process.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// As an OS event, which no other process can read.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Private,
    /// On a command line.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    CommandLine,
}

/// What the page is told about `raw`, or `None` when it is no Lakko link.
fn parse(raw: &str, delivery: Delivery) -> Option<Value> {
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "lakko" || url.query().is_some() {
        return None;
    }
    let kind = url.host_str().filter(|k| matches!(*k, "i" | "c" | "g"))?;
    let id = url
        .path()
        .strip_prefix('/')
        .filter(|id| is_link_text(id, ID_CHARS))?;
    let secret = match delivery {
        Delivery::Private => url.fragment().filter(|s| is_link_text(s, SECRET_CHARS)),
        Delivery::CommandLine => None,
    };
    Some(json!({ "kind": kind, "id": id, "secret": secret }))
}

/// Keep `raw` for the page and bring the window up.
pub fn arrived<R: Runtime>(app: &AppHandle<R>, raw: &str, delivery: Delivery) {
    let Some(link) = parse(raw, delivery) else {
        warn!("refused a link that is not a Lakko link");
        return;
    };
    let Some(held) = app.try_state::<Held>() else {
        warn!("no place to hold a link");
        return;
    };
    info!(
        kind = link["kind"].as_str().unwrap_or(""),
        "link held for the page"
    );
    held.put(link);
    show_main(app);
    notify_page(app, "link-arrived");
}

/// The first `lakko://` argument of a command line.
#[cfg(not(target_os = "macos"))]
pub fn from_args<R: Runtime>(app: &AppHandle<R>, args: &[String]) {
    if let Some(raw) = args.iter().find(|a| a.starts_with("lakko://")) {
        arrived(app, raw, Delivery::CommandLine);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Delivery, Held};
    use serde_json::json;

    const ID: &str = "AbCd_-12";
    const SECRET: &str = "0123456789abcdef";

    #[test]
    fn a_held_link_is_taken_once_and_the_newest_wins() {
        let held = Held::default();
        assert_eq!(held.take(), None);
        held.put(json!({ "id": "first" }));
        held.put(json!({ "id": "second" }));
        assert_eq!(held.take(), Some(json!({ "id": "second" })));
        assert_eq!(held.take(), None);
    }

    #[test]
    fn a_private_delivery_keeps_the_secret() {
        let raw = format!("lakko://g/{ID}#{SECRET}");
        assert_eq!(
            parse(&raw, Delivery::Private),
            Some(json!({ "kind": "g", "id": ID, "secret": SECRET }))
        );
    }

    #[test]
    fn a_command_line_never_carries_the_secret_onward() {
        let raw = format!("lakko://i/{ID}#{SECRET}");
        assert_eq!(
            parse(&raw, Delivery::CommandLine),
            Some(json!({ "kind": "i", "id": ID, "secret": null }))
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for raw in [
            "https://lakko.me/i/AbCd_-12",
            "lakko://x/AbCd_-12",
            "lakko://i/short",
            "lakko://i/AbCd_-12/more",
            "lakko://i/AbCd_-12?next=https://evil.example",
            "lakko://settings",
            "not a url",
        ] {
            assert_eq!(parse(raw, Delivery::Private), None, "{raw}");
        }
    }

    #[test]
    fn a_malformed_secret_is_dropped_not_passed_on() {
        let raw = format!("lakko://c/{ID}#<script>");
        assert_eq!(
            parse(&raw, Delivery::Private),
            Some(json!({ "kind": "c", "id": ID, "secret": null }))
        );
    }
}
