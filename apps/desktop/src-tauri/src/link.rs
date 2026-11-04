// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Links from the OS (ADR-0085 section 5, `docs/ux/desktop.md` section 7).
//!
//! A link arrives as `lakko://<kind>/<id>#<secret>`, from the invite page, or
//! as this deployment's own `https` link, where the OS hands those to the app
//! (a Universal Link on macOS, a packaged app's URI handler on Windows). On
//! macOS it is an event. On Windows and Linux it is a command line, which the
//! user's other processes can read: the link came through some other
//! messenger and sits in the browser's history, so nobody pastes it by hand to
//! keep it off one (maintainer, 2026-11-01).
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
pub struct Held {
    link: Mutex<Option<Value>>,
    /// This deployment's origin: the one `https` origin a link may name.
    origin: String,
}

impl Held {
    pub fn new(origin: String) -> Self {
        Self {
            link: Mutex::new(None),
            origin,
        }
    }

    fn put(&self, link: Value) {
        if let Ok(mut held) = self.link.lock() {
            *held = Some(link);
        }
    }

    /// Hand the link over, once.
    pub fn take(&self) -> Option<Value> {
        self.link.lock().ok().and_then(|mut held| held.take())
    }
}

/// The kind and the ledger id of `url`, by its form.
fn kind_and_id<'u>(url: &'u Url, origin: &str) -> Option<(&'u str, &'u str)> {
    if url.scheme() == "lakko" {
        return Some((url.host_str()?, url.path().strip_prefix('/')?));
    }
    if url.origin().ascii_serialization() != origin {
        return None;
    }
    url.path().strip_prefix('/')?.split_once('/')
}

/// What the page is told about `raw`, or `None` when it is no whole Lakko
/// link. Every link carries its secret: without it no screen can open.
fn parse(raw: &str, origin: &str) -> Option<Value> {
    let url = Url::parse(raw).ok()?;
    if url.query().is_some() {
        return None;
    }
    let (kind, id) = kind_and_id(&url, origin)?;
    if !matches!(kind, "i" | "c" | "g") || !is_link_text(id, ID_CHARS) {
        return None;
    }
    let secret = url.fragment().filter(|s| is_link_text(s, SECRET_CHARS))?;
    Some(json!({ "kind": kind, "id": id, "secret": secret }))
}

/// Keep `raw` for the page and bring the window up.
pub fn arrived<R: Runtime>(app: &AppHandle<R>, raw: &str) {
    let Some(held) = app.try_state::<Held>() else {
        warn!("no place to hold a link");
        return;
    };
    let Some(link) = parse(raw, &held.origin) else {
        warn!("refused a link that is not a Lakko link");
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

/// The first link among command-line arguments.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn link_arg<'a>(args: &'a [String], origin: &str) -> Option<&'a str> {
    let own = format!("{origin}/");
    args.iter()
        .map(String::as_str)
        .find(|a| a.starts_with("lakko://") || a.starts_with(&own))
}

/// Take a link off a command line.
#[cfg(not(target_os = "macos"))]
pub fn from_args<R: Runtime>(app: &AppHandle<R>, args: &[String]) {
    let Some(held) = app.try_state::<Held>() else {
        return;
    };
    if let Some(raw) = link_arg(args, &held.origin) {
        arrived(app, raw);
    }
}

#[cfg(test)]
mod tests {
    use super::{link_arg, parse, Held};
    use serde_json::json;

    const ID: &str = "AbCd_-12";
    const SECRET: &str = "0123456789abcdef";
    const ORIGIN: &str = "https://lakko.me";

    #[test]
    fn a_held_link_is_taken_once_and_the_newest_wins() {
        let held = Held::new(ORIGIN.to_owned());
        assert_eq!(held.take(), None);
        held.put(json!({ "id": "first" }));
        held.put(json!({ "id": "second" }));
        assert_eq!(held.take(), Some(json!({ "id": "second" })));
        assert_eq!(held.take(), None);
    }

    #[test]
    fn a_lakko_link_keeps_its_secret() {
        assert_eq!(
            parse(&format!("lakko://g/{ID}#{SECRET}"), ORIGIN),
            Some(json!({ "kind": "g", "id": ID, "secret": SECRET }))
        );
    }

    #[test]
    fn this_deployments_https_link_is_the_same_link() {
        assert_eq!(
            parse(&format!("{ORIGIN}/i/{ID}#{SECRET}"), ORIGIN),
            Some(json!({ "kind": "i", "id": ID, "secret": SECRET }))
        );
    }

    #[test]
    fn another_hosts_https_link_is_refused() {
        for raw in [
            format!("https://evil.example/i/{ID}#{SECRET}"),
            format!("http://lakko.me/i/{ID}#{SECRET}"),
            format!("https://lakko.me.evil.example/i/{ID}#{SECRET}"),
            format!("https://lakko.me@evil.example/i/{ID}#{SECRET}"),
        ] {
            assert_eq!(parse(&raw, ORIGIN), None, "{raw}");
        }
    }

    #[test]
    fn a_link_with_no_usable_secret_is_refused() {
        for raw in [
            format!("lakko://i/{ID}"),
            format!("lakko://c/{ID}#<script>"),
            format!("{ORIGIN}/g/{ID}"),
        ] {
            assert_eq!(parse(&raw, ORIGIN), None, "{raw}");
        }
    }

    #[test]
    fn a_command_line_gives_up_its_link_and_nothing_else() {
        let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        let lakko = format!("lakko://i/{ID}#{SECRET}");
        let https = format!("{ORIGIN}/g/{ID}#{SECRET}");
        assert_eq!(
            link_arg(&args(&["lakko-desktop", "--hidden", &lakko]), ORIGIN),
            Some(lakko.as_str())
        );
        assert_eq!(
            link_arg(&args(&["lakko-desktop", &https]), ORIGIN),
            Some(https.as_str())
        );
        assert_eq!(
            link_arg(
                &args(&["lakko-desktop", "https://lakko.me.evil.example/g/x"]),
                ORIGIN
            ),
            None
        );
        assert_eq!(link_arg(&args(&["lakko-desktop"]), ORIGIN), None);
    }

    #[test]
    fn anything_else_is_refused() {
        for raw in [
            "https://lakko.me/i/AbCd_-12",
            "https://lakko.me/settings",
            "https://lakko.me/i/AbCd_-12/more#0123456789abcdef",
            "lakko://x/AbCd_-12#0123456789abcdef",
            "lakko://i/short#0123456789abcdef",
            "lakko://i/AbCd_-12/more#0123456789abcdef",
            "lakko://i/AbCd_-12?next=https://evil.example#0123456789abcdef",
            "lakko://settings",
            "not a url",
        ] {
            assert_eq!(parse(raw, ORIGIN), None, "{raw}");
        }
    }
}
