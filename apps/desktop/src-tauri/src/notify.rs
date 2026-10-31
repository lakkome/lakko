// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! System notifications (ADR-0085 section 5, `docs/ux/desktop.md` sections 4
//! and 4.1).
//!
//! The page decides what to say and when; this module only shows it. A
//! notification carries a `target`, an app path the page opens when the
//! person clicks it, and may carry Accept and Decline for a ringing call. The
//! text arrives already decrypted and rendered in the interface language;
//! nothing here reaches a server.
//!
//! **Per OS:**
//! - macOS: `NSUserNotification` through `mac-notification-sys`, sent from its
//!   own thread so a click can be waited for. The notification is attributed
//!   to the app's bundle identifier, so it shows as Lakko only when the
//!   process runs from a registered `.app` bundle (`just dev-desktop` builds
//!   one). The library hands out no identifier, so a delivered notification
//!   is found again by its title and text and removed through
//!   `NSUserNotificationCenter`.
//! - Windows: a toast through the WinRT API, tagged so it can be replaced and
//!   withdrawn. Windows shows a toast only for an app id a Start menu shortcut
//!   carries, so it needs the installed app.
//! - Linux: the desktop notification service over D-Bus (`notify-rust`);
//!   withdrawing is `CloseNotification`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// Accept and Decline on a ringing call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallActions {
    pub accept: String,
    pub decline: String,
    /// Lowercase hex of the 16-byte call id, handed back to the page.
    pub call_id: String,
    /// The call was offered with video, handed back to the page.
    pub video: bool,
}

/// One notification, as the page asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Replaces an earlier notification with the same tag.
    pub tag: String,
    pub title: String,
    pub body: String,
    /// The app path a click opens; `None` only brings the window forward.
    pub target: Option<String>,
    pub call: Option<CallActions>,
}

/// Longest title and body kept, in characters; the page sends short text,
/// this only bounds what one request can make the host hold.
const MAX_TITLE: usize = 120;
const MAX_BODY: usize = 400;
const MAX_TAG: usize = 128;
const MAX_TARGET: usize = 512;
const MAX_ACTION_LABEL: usize = 40;
/// A call id is 16 bytes, written as hex.
const CALL_ID_HEX_LEN: usize = 32;

impl Request {
    /// Read `{tag, title, body, target?, call?}`; `None` when a field is
    /// missing or the target is not an app path.
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
        let call = match v.get("call") {
            None | Some(Value::Null) => None,
            Some(c) => Some(CallActions::from_json(c)?),
        };
        Some(Self {
            tag: tag.to_owned(),
            title: clip(title, MAX_TITLE),
            body: clip(body, MAX_BODY),
            target,
            call,
        })
    }
}

impl CallActions {
    fn from_json(v: &Value) -> Option<Self> {
        let label = |k: &str| {
            let t = v.get(k)?.as_str()?.trim();
            (!t.is_empty()).then(|| clip(t, MAX_ACTION_LABEL))
        };
        let call_id = v.get("callId")?.as_str()?;
        if !is_call_id(call_id) {
            return None;
        }
        Some(Self {
            accept: label("accept")?,
            decline: label("decline")?,
            call_id: call_id.to_owned(),
            video: v.get("video").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// Lowercase hex of 16 bytes.
fn is_call_id(id: &str) -> bool {
    id.len() == CALL_ID_HEX_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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
    let one_line: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
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
/// Delivered notifications remembered for a replace or a withdraw; past this
/// the older ones are simply left to the OS.
const MAX_TRACKED: usize = 256;

/// What a shown notification reports back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The person clicked it.
    Clicked,
    /// The person pressed Accept.
    Accepted,
    /// The person pressed Decline.
    Declined,
    /// Closed, dismissed or not waited for.
    Other,
}

/// The notifier: counts the threads waiting on a click and remembers what is
/// delivered, by tag.
pub struct Notifier {
    waiting: AtomicUsize,
    available: bool,
    app_id: String,
    delivered: Arc<Mutex<HashMap<String, platform::Delivered>>>,
}

impl Notifier {
    /// Set up for this process. `app_id` names the app the notifications are
    /// attributed to.
    pub fn new(app_id: &str) -> Self {
        Self {
            waiting: AtomicUsize::new(0),
            available: platform::init(app_id),
            app_id: app_id.to_owned(),
            delivered: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub const fn available(&self) -> bool {
        self.available
    }

    /// Show `req`; `on_done` runs on the notification's thread once the
    /// person answered it. Returns `false` when this OS cannot show it.
    pub fn show(
        self: &Arc<Self>,
        req: Request,
        on_done: impl FnOnce(Outcome, Request) + Send + 'static,
    ) -> bool {
        if !self.available {
            return false;
        }
        let replaced = self.take(&req.tag);
        let wait = self.waiting.fetch_add(1, Ordering::SeqCst) < MAX_WAITING;
        if !wait {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("lakko-notification".into())
            .spawn(move || {
                let tag = req.tag.clone();
                let remember = |d: platform::Delivered| me.remember(&tag, d);
                let outcome = platform::show(&me.app_id, &req, wait, replaced.as_ref(), remember);
                if wait {
                    me.waiting.fetch_sub(1, Ordering::SeqCst);
                }
                if outcome != Outcome::Other {
                    // An answered notification is gone from the screen.
                    me.take(&tag);
                    on_done(outcome, req);
                }
            });
        if spawned.is_err() && wait {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
        }
        spawned.is_ok()
    }

    /// Take back the notification with this tag, if one is delivered.
    pub fn withdraw(&self, tag: &str) {
        if let Some(d) = self.take(tag) {
            platform::withdraw(&self.app_id, tag, &d);
        }
    }

    fn take(&self, tag: &str) -> Option<platform::Delivered> {
        self.delivered.lock().ok()?.remove(tag)
    }

    fn remember(&self, tag: &str, delivered: platform::Delivered) {
        if let Ok(mut map) = self.delivered.lock() {
            if map.len() >= MAX_TRACKED {
                map.clear();
            }
            map.insert(tag.to_owned(), delivered);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use mac_notification_sys::{MainButton, Notification, NotificationResponse};
    #[allow(deprecated)] // The same API `mac-notification-sys` delivers through.
    use objc2_foundation::{NSString, NSUserNotificationCenter};
    use tracing::warn;

    use super::{Outcome, Request};

    /// What finds a delivered notification again: the library keeps its
    /// identifier to itself.
    pub struct Delivered {
        title: String,
        body: String,
    }

    pub fn init(app_id: &str) -> bool {
        match mac_notification_sys::set_application(app_id) {
            Ok(()) => true,
            Err(e) => {
                // Not a registered bundle (the bare binary of a plain
                // `cargo run`): notifications would show as another app.
                warn!(error = %e, "notifications unavailable: run the app from its .app bundle");
                false
            },
        }
    }

    pub fn show(
        app_id: &str,
        req: &Request,
        wait: bool,
        replaced: Option<&Delivered>,
        remember: impl FnOnce(Delivered),
    ) -> Outcome {
        if let Some(old) = replaced {
            withdraw(app_id, &req.tag, old);
        }
        remember(Delivered {
            title: req.title.clone(),
            body: req.body.clone(),
        });
        let mut n = Notification::new();
        n.title(&req.title)
            .message(&req.body)
            .default_sound()
            .wait_for_click(wait);
        if let Some(call) = &req.call {
            n.main_button(MainButton::SingleAction(&call.accept))
                .close_button(&call.decline);
        }
        if !wait {
            n.asynchronous(true);
        }
        match n.send() {
            Ok(NotificationResponse::Click) => Outcome::Clicked,
            Ok(NotificationResponse::ActionButton(_)) => Outcome::Accepted,
            Ok(NotificationResponse::CloseButton(_)) => Outcome::Declined,
            Ok(_) => Outcome::Other,
            Err(e) => {
                warn!(error = %e, "notification not shown");
                Outcome::Other
            },
        }
    }

    #[allow(deprecated)] // The same API `mac-notification-sys` delivers through.
    pub fn withdraw(_app_id: &str, _tag: &str, d: &Delivered) {
        let center = NSUserNotificationCenter::defaultUserNotificationCenter();
        let title = NSString::from_str(&d.title);
        let body = NSString::from_str(&d.body);
        for n in center.deliveredNotifications().iter() {
            let same = n.title().is_some_and(|t| t.isEqualToString(&title))
                && n.informativeText()
                    .is_some_and(|t| t.isEqualToString(&body));
            if same {
                center.removeDeliveredNotification(&n);
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::mpsc;

    use sha2::{Digest, Sha256};
    use tracing::warn;
    use windows::core::{IInspectable, Interface, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{
        ToastActivatedEventArgs, ToastDismissedEventArgs, ToastNotification,
        ToastNotificationManager,
    };

    use super::{Outcome, Request};

    const GROUP: &str = "lakko";
    const ACCEPT: &str = "accept";
    const DECLINE: &str = "decline";

    /// A toast is found again by its tag alone.
    pub struct Delivered;

    pub const fn init(_app_id: &str) -> bool {
        true
    }

    /// A toast tag is limited in length; a page tag is not.
    fn toast_tag(tag: &str) -> HSTRING {
        let digest = Sha256::digest(tag.as_bytes());
        let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
        HSTRING::from(hex)
    }

    fn toast(
        app_id: &str,
        req: &Request,
        tx: mpsc::Sender<Outcome>,
    ) -> windows::core::Result<ToastNotification> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(super::toast_xml(req, ACCEPT, DECLINE)))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        toast.SetTag(&toast_tag(&req.tag))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        let activated = tx.clone();
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, args| {
                let action = args
                    .as_ref()
                    .and_then(|a| a.cast::<ToastActivatedEventArgs>().ok())
                    .and_then(|a| a.Arguments().ok())
                    .map(|a| a.to_string_lossy());
                let outcome = match action.as_deref() {
                    Some(ACCEPT) => Outcome::Accepted,
                    Some(DECLINE) => Outcome::Declined,
                    _ => Outcome::Clicked,
                };
                let _ = activated.send(outcome);
                Ok(())
            },
        ))?;
        toast.Dismissed(&TypedEventHandler::<
            ToastNotification,
            ToastDismissedEventArgs,
        >::new(move |_, _| {
            let _ = tx.send(Outcome::Other);
            Ok(())
        }))?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))?
            .Show(&toast)?;
        Ok(toast)
    }

    pub fn show(
        app_id: &str,
        req: &Request,
        wait: bool,
        _replaced: Option<&Delivered>,
        remember: impl FnOnce(Delivered),
    ) -> Outcome {
        remember(Delivered);
        let (tx, rx) = mpsc::channel();
        // Held while waiting: the events belong to this object.
        let toast = match toast(app_id, req, tx) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "notification not shown");
                return Outcome::Other;
            },
        };
        let outcome = if wait {
            rx.recv().unwrap_or(Outcome::Other)
        } else {
            Outcome::Other
        };
        drop(toast);
        outcome
    }

    pub fn withdraw(app_id: &str, tag: &str, _d: &Delivered) {
        let removed = ToastNotificationManager::History().and_then(|h| {
            h.RemoveGroupedTagWithId(
                &toast_tag(tag),
                &HSTRING::from(GROUP),
                &HSTRING::from(app_id),
            )
        });
        if let Err(e) = removed {
            warn!(error = %e, "notification not withdrawn");
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use notify_rust::{Notification, Urgency};
    use tracing::warn;

    use super::{Outcome, Request};

    const OPEN: &str = "default";
    const ACCEPT: &str = "accept";
    const DECLINE: &str = "decline";

    /// The id the notification service gave it.
    pub struct Delivered(u32);

    pub const fn init(_app_id: &str) -> bool {
        true
    }

    pub fn show(
        _app_id: &str,
        req: &Request,
        wait: bool,
        replaced: Option<&Delivered>,
        remember: impl FnOnce(Delivered),
    ) -> Outcome {
        let mut n = Notification::new();
        // A notification service may read the body as markup.
        n.appname("Lakko")
            .summary(&req.title)
            .body(&super::xml_escape(&req.body))
            .action(OPEN, "Open");
        if let Some(call) = &req.call {
            n.action(ACCEPT, &call.accept)
                .action(DECLINE, &call.decline)
                .urgency(Urgency::Critical);
        }
        if let Some(old) = replaced {
            n.id(old.0);
        }
        let handle = match n.show() {
            Ok(h) => h,
            Err(e) => {
                warn!(error = %e, "notification not shown");
                return Outcome::Other;
            },
        };
        remember(Delivered(handle.id()));
        if !wait {
            return Outcome::Other;
        }
        let mut outcome = Outcome::Other;
        handle.wait_for_action(|action| {
            outcome = match action {
                ACCEPT => Outcome::Accepted,
                DECLINE => Outcome::Declined,
                OPEN => Outcome::Clicked,
                _ => Outcome::Other,
            };
        });
        outcome
    }

    pub fn withdraw(_app_id: &str, _tag: &str, d: &Delivered) {
        let closed = zbus::blocking::Connection::session().and_then(|c| {
            c.call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "CloseNotification",
                &(d.0,),
            )
        });
        if let Err(e) = closed {
            warn!(error = %e, "notification not withdrawn");
        }
    }
}

/// `<`, `>`, `&` and quotes as entities.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The toast of `req`; a call rings as a call and carries its two buttons.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn toast_xml(req: &Request, accept: &str, decline: &str) -> String {
    let (scenario, actions) = req.call.as_ref().map_or((String::new(), String::new()), |c| {
        (
            " scenario=\"incomingCall\"".to_owned(),
            format!(
                "<actions><action content=\"{}\" arguments=\"{accept}\"/><action content=\"{}\" arguments=\"{decline}\"/></actions>",
                xml_escape(&c.accept),
                xml_escape(&c.decline),
            ),
        )
    });
    format!(
        "<toast{scenario}><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{actions}</toast>",
        xml_escape(&req.title),
        xml_escape(&req.body),
    )
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
        let r = Request::from_json(
            &json!({ "tag": "t", "title": "Ann\nBob", "body": long, "target": "/chat/a" }),
        )
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
        assert!(
            Request::from_json(&json!({ "tag": "t", "title": "x", "target": "https://a" }))
                .is_none()
        );
        assert!(Request::from_json(&json!({ "tag": "t", "title": "x", "target": 3 })).is_none());
        let none = Request::from_json(&json!({ "tag": "t", "title": "x", "target": null }))
            .expect("valid");
        assert!(none.target.is_none());
    }

    #[test]
    fn call_actions_are_read_and_checked() {
        let id = "0123456789abcdef0123456789abcdef";
        let call = json!({ "accept": "Accept", "decline": "Decline", "callId": id, "video": true });
        let r =
            Request::from_json(&json!({ "tag": "t", "title": "x", "call": call })).expect("valid");
        let c = r.call.expect("call");
        assert_eq!(
            (
                c.accept.as_str(),
                c.decline.as_str(),
                c.call_id.as_str(),
                c.video
            ),
            ("Accept", "Decline", id, true)
        );

        let bad_id = json!({ "accept": "A", "decline": "D", "callId": "ABC" });
        assert!(Request::from_json(&json!({ "tag": "t", "title": "x", "call": bad_id })).is_none());
        let no_label = json!({ "accept": " ", "decline": "D", "callId": id });
        assert!(
            Request::from_json(&json!({ "tag": "t", "title": "x", "call": no_label })).is_none()
        );
    }

    #[test]
    fn toast_text_cannot_break_out_of_the_xml() {
        let r = Request {
            tag: "t".into(),
            title: "<a&b>".into(),
            body: "\"quoted\"".into(),
            target: None,
            call: Some(CallActions {
                accept: "Yes".into(),
                decline: "<No>".into(),
                call_id: "0".repeat(32),
                video: false,
            }),
        };
        let xml = toast_xml(&r, "accept", "decline");
        assert!(xml.contains("<text>&lt;a&amp;b&gt;</text>"));
        assert!(xml.contains("<text>&quot;quoted&quot;</text>"));
        assert!(xml.contains("content=\"&lt;No&gt;\" arguments=\"decline\""));
        assert!(xml.starts_with("<toast scenario=\"incomingCall\">"));
    }
}
