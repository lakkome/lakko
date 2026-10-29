// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! System notifications (ADR-0085 section 5, `docs/ux/desktop.md` section 4).
//!
//! The page decides what to say and when; this module only shows it. A
//! notification carries a `target`, an app path the page opens when the
//! person clicks it. The text arrives already decrypted and rendered in the
//! interface language; nothing here reaches a server.
//!
//! macOS: `NSUserNotification` through `mac-notification-sys`, sent from its
//! own thread so a click can be waited for. The notification is attributed to
//! the app's bundle identifier, so it shows as Lakko only when the process
//! runs from a registered `.app` bundle (`just dev-desktop` builds one). A
//! delivered notification cannot be withdrawn through this library; a
//! withdraw is answered and does nothing on macOS. Windows and Linux are not
//! built yet: the routes answer `notifications-unsupported`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;

/// One notification, as the page asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Replaces an earlier notification with the same tag where the OS can.
    pub tag: String,
    pub title: String,
    pub body: String,
    /// The app path a click opens; `None` only brings the window forward.
    pub target: Option<String>,
}

/// Longest title and body kept, in characters; the page sends short text,
/// this only bounds what one request can make the host hold.
const MAX_TITLE: usize = 120;
const MAX_BODY: usize = 400;
const MAX_TAG: usize = 128;
const MAX_TARGET: usize = 512;

impl Request {
    /// Read `{tag, title, body, target?}`; `None` when a field is missing or
    /// the target is not an app path.
    pub fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        let tag = s("tag")?;
        let title = s("title")?;
        let body = s("body").unwrap_or("");
        if tag.is_empty() || tag.chars().count() > MAX_TAG || title.trim().is_empty() {
            return None;
        }
        let target = match v.get("target") {
            None | Some(Value::Null) => None,
            Some(Value::String(t)) if is_app_path(t) => Some(t.clone()),
            Some(_) => return None,
        };
        Some(Self {
            tag: tag.to_owned(),
            title: clip(title, MAX_TITLE),
            body: clip(body, MAX_BODY),
            target,
        })
    }
}

/// A path inside the app: absolute, not protocol-relative, no scheme, no
/// control characters. A click navigates the page to it, so nothing else may
/// pass.
pub fn is_app_path(p: &str) -> bool {
    p.starts_with('/')
        && !p.starts_with("//")
        && !p.contains('\\')
        && !p.contains("://")
        && p.chars().count() <= MAX_TARGET
        && !p.chars().any(char::is_control)
}

fn clip(s: &str, max: usize) -> String {
    let one_line: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let trimmed = one_line.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_owned();
    }
    let mut out: String = trimmed.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// At most this many notifications wait for a click at once; later ones are
/// shown without waiting (a click on them only brings the app forward).
const MAX_WAITING: usize = 16;

/// What a shown notification reports back.
pub enum Outcome {
    /// The person clicked it.
    Clicked,
    /// Closed, dismissed or not waited for.
    Other,
}

/// The notifier: counts the threads waiting on a click.
pub struct Notifier {
    waiting: AtomicUsize,
    available: bool,
}

impl Notifier {
    /// Set up for this process. `bundle_id` names the app the notifications
    /// are attributed to.
    pub fn new(bundle_id: &str) -> Self {
        Self { waiting: AtomicUsize::new(0), available: platform::init(bundle_id) }
    }

    pub const fn available(&self) -> bool {
        self.available
    }

    /// Show `req`; `on_click` runs on the notification's thread when the
    /// person clicks it. Returns `false` when this OS cannot show it.
    pub fn show(self: &Arc<Self>, req: Request, on_click: impl FnOnce(Option<String>) + Send + 'static) -> bool {
        if !self.available {
            return false;
        }
        let wait = self.waiting.fetch_add(1, Ordering::SeqCst) < MAX_WAITING;
        if !wait {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("lakko-notification".into()).spawn(move || {
            let outcome = platform::show(&req, wait);
            if wait {
                me.waiting.fetch_sub(1, Ordering::SeqCst);
            }
            if matches!(outcome, Outcome::Clicked) {
                on_click(req.target);
            }
        });
        if spawned.is_err() && wait {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
        }
        spawned.is_ok()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use mac_notification_sys::{Notification, NotificationResponse};
    use tracing::warn;

    use super::{Outcome, Request};

    pub fn init(bundle_id: &str) -> bool {
        match mac_notification_sys::set_application(bundle_id) {
            Ok(()) => true,
            Err(e) => {
                // Not a registered bundle (the bare binary of a plain
                // `cargo run`): notifications would show as another app.
                warn!(error = %e, "notifications unavailable: run the app from its .app bundle");
                false
            }
        }
    }

    pub fn show(req: &Request, wait: bool) -> Outcome {
        let mut n = Notification::new();
        n.title(&req.title).message(&req.body).default_sound().wait_for_click(wait);
        if !wait {
            n.asynchronous(true);
        }
        match n.send() {
            Ok(NotificationResponse::Click) => Outcome::Clicked,
            Ok(_) => Outcome::Other,
            Err(e) => {
                warn!(error = %e, "notification not shown");
                Outcome::Other
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{Outcome, Request};

    pub const fn init(_bundle_id: &str) -> bool {
        false
    }

    pub const fn show(_req: &Request, _wait: bool) -> Outcome {
        Outcome::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn app_paths_only() {
        assert!(is_app_path("/chat/abc"));
        assert!(is_app_path("/settings/devices"));
        assert!(!is_app_path("//evil.example/x"));
        assert!(!is_app_path("https://evil.example"));
        assert!(!is_app_path("javascript:alert(1)"));
        assert!(!is_app_path("/a\\b"));
        assert!(!is_app_path("/a\nb"));
        assert!(!is_app_path("relative"));
    }

    #[test]
    fn request_is_bounded_and_one_line() {
        let long = "x".repeat(1000);
        let r = Request::from_json(&json!({ "tag": "t", "title": "Ann\nBob", "body": long, "target": "/chat/a" }))
            .expect("valid");
        assert_eq!(r.title, "Ann Bob");
        assert_eq!(r.body.chars().count(), MAX_BODY);
        assert!(r.body.ends_with('\u{2026}'));
        assert_eq!(r.target.as_deref(), Some("/chat/a"));
    }

    #[test]
    fn request_refuses_bad_fields() {
        assert!(Request::from_json(&json!({ "title": "t" })).is_none());
        assert!(Request::from_json(&json!({ "tag": "", "title": "t" })).is_none());
        assert!(Request::from_json(&json!({ "tag": "t", "title": "  " })).is_none());
        assert!(Request::from_json(&json!({ "tag": "t", "title": "x", "target": "https://a" })).is_none());
        assert!(Request::from_json(&json!({ "tag": "t", "title": "x", "target": 3 })).is_none());
        let none = Request::from_json(&json!({ "tag": "t", "title": "x", "target": null })).expect("valid");
        assert!(none.target.is_none());
    }
}
