// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Server-side attempt limits for the pre-session routes (ADR-0054 S4).
//!
//! Proof of work used to gate every anonymous auth route. It priced honest
//! phones without a JIT far above an attacker's hardware, so it now survives
//! only on `/signup/start`; every other route is limited by counters the
//! server keeps:
//!
//! - **Per name** ([`charge_name`]): a fixed-window counter keyed on what the
//!   request names — the login handle it tries to log into (ADR-0054 S5), the
//!   login handle a device request targets (ADR-0056). The counter is
//!   charged BEFORE the handler looks anything up, and it exists
//!   for a name with no account exactly as for one with an account, so a
//!   refusal says nothing about existence (same status, same body, same
//!   queries on both paths). Stored in `auth_name_limits` as a keyed BLAKE3
//!   of the name under a subkey of the login-handle pepper, so a database
//!   dump plus a username list does not show who was the target of which
//!   attempts; a store error refuses the request (fail closed, `503`).
//! - **Per source address** ([`ip_gate`]): an in-memory token bucket per
//!   (route category, address). Addresses never reach the database. The
//!   address is what the fronting nginx wrote (`X-Real-IP`, else the
//!   rightmost `X-Forwarded-For` entry), else the socket peer; a request with
//!   none of them is charged to one shared bucket rather than admitted
//!   unmetered. An IPv6 address is charged to its /64 (one subscriber
//!   usually holds a whole /64, so per address it would be 2^64 buckets);
//!   an IPv4 address, and an IPv4-mapped IPv6 one, to the address itself.
//!   The map holds at most [`MAX_ADDRESS_BUCKETS`] entries: a new source
//!   past the cap first evicts buckets that have refilled completely, then
//!   the least recently used ones (a batch at a time).
//!
//! A limit reached answers `429` with `{"error": <code>, "retry_after_secs":
//! N}` and a `Retry-After: N` header — never `401`, so no client reads it as
//! a bad credential or a lost session. The codes are one per route family so
//! the clients can map each to its copy (`docs/ux/anti-abuse.md` §2):
//! [`CODE_TOO_MANY_ATTEMPTS`], [`CODE_DEVICE_REQUEST_LIMITED`],
//! [`CODE_SIGNUP_RATE_LIMITED`], [`CODE_HANDLE_BIND_LIMITED`].
//!
//! - **The device-link guess budget** ([`DeviceLinkTier`]): the
//!   `wrong_code` outcomes an account's signed-in device records (ADR-0049
//!   section 2, security pass finding 6). Each device request gets one online
//!   guess at the six-digit code; across requests, `N` wrong codes in a
//!   window suspend the account's device requests for a while (defaults: 3
//!   in 24 h for 1 h, 6 in 24 h for 24 h). Enforced in
//!   [`crate::device_request`], where a suspended account's new requests are
//!   stored against its handle's stand-in, so the requester sees a request
//!   nobody answers - exactly what an unknown name sees.
//!
//! Every default below is **chosen, not measured**; each has an env knob of
//! the form `N/SECS` (at most `N` per `SECS` seconds), and a device-link tier
//! a second knob for its suspension in seconds.
//!
//! No limiter here fails open: the name counters fail closed on a store
//! error (the guessing routes are exactly where an
//! unmetered window would be spent by an attacker), and the address buckets
//! have no store to fail.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use lakko_crypto::hash::{blake3_derive_key_from_material, blake3_keyed};
use serde_json::json;
use sqlx::MySqlPool;

/// Body code for a guessing route (login, password reset, TOTP recovery,
/// device authorize) whose name counter or address bucket is empty.
pub(crate) const CODE_TOO_MANY_ATTEMPTS: &str = "too-many-attempts";

/// Body code for a refused `/device/request` (the "Is this you?" prompt).
pub(crate) const CODE_DEVICE_REQUEST_LIMITED: &str = "device-request-limited";

/// Body code for a refused `/signup/start` or `/pow/challenge`.
pub(crate) const CODE_SIGNUP_RATE_LIMITED: &str = "signup-rate-limited";

/// Body code for a refused `PUT /auth/login-handle` (the per-account bind
/// counter, the minimum interval between handle changes, or the address
/// bucket).
pub(crate) const CODE_HANDLE_BIND_LIMITED: &str = "handle-bind-limited";

/// Body code for a refused letter of the recovery email (ADR-0075): the
/// per-handle or per-account letter counter, or the address bucket of the
/// two routes that send one.
pub(crate) const CODE_EMAIL_RATE_LIMITED: &str = "email-rate-limited";

/// Body code when the name-counter store cannot be read (`503`).
pub(crate) const CODE_LIMITER_UNAVAILABLE: &str = "rate-limiter-unavailable";

/// Derive-key context of the limiter's subkey of the login-handle pepper.
/// The stored counter key is `keyed-BLAKE3(subkey, scope || name)`.
const NAME_KEY_DOMAIN: &str = "lakko/auth/name-limit-key/v1";

/// Most address buckets held in memory at once (every category together).
/// A bucket is a key of at most 17 bytes plus 16 bytes of state and the
/// map's overhead, so the cap bounds the map to a few MiB.
pub(crate) const MAX_ADDRESS_BUCKETS: usize = 65_536;

/// Default minimum interval between two binds of one account that change its
/// login handle (`LAKKO_AUTH_HANDLE_CHANGE_MIN_INTERVAL_SECS`). Chosen, not
/// measured.
const DEFAULT_HANDLE_CHANGE_MIN_INTERVAL_SECS: u32 = 60;

/// Env knob for the minimum interval between handle changes.
const HANDLE_CHANGE_INTERVAL_VAR: &str = "LAKKO_AUTH_HANDLE_CHANGE_MIN_INTERVAL_SECS";

/// Default recall window in days (ADR-0074 section 0.2, D4). Chosen, not
/// measured.
const DEFAULT_RECALL_WINDOW_DAYS: u32 = 30;

/// Env knob for the recall window, in whole days (positive).
const RECALL_WINDOW_VAR: &str = "LAKKO_RECALL_WINDOW_DAYS";

/// Milliseconds in a day.
const MS_PER_DAY: i64 = 86_400_000;

/// Saturation ceiling for the stored hit count (the column is `INT UNSIGNED`).
const MAX_HITS: u32 = 1_000_000_000;

/// How often the pruner sweeps expired counters and idle buckets.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// At most `limit` events per `window_secs` seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    /// Events admitted per window (and the address bucket's capacity).
    pub(crate) limit: u32,
    /// Window length in seconds.
    pub(crate) window_secs: u32,
}

impl Window {
    pub(crate) const fn new(limit: u32, window_secs: u32) -> Self {
        Self { limit, window_secs }
    }

    /// Parse `N/SECS`, both positive.
    fn parse(raw: &str) -> Option<Self> {
        let (n, secs) = raw.trim().split_once('/')?;
        let limit: u32 = n.trim().parse().ok()?;
        let window_secs: u32 = secs.trim().parse().ok()?;
        (limit > 0 && window_secs > 0).then_some(Self { limit, window_secs })
    }
}

/// What a name counter counts. The discriminant is stored in
/// `auth_name_limits.scope`; never renumber. 6, 7 and 8 belonged to the
/// removed phone / email one-time-code factor, and 5 to the removed
/// server-side three-word verifier (`/vault/recover/finish`); they stay
/// unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Scope {
    /// `/login/start`, keyed on the login handle (ADR-0054 S5).
    LoginPassword = 1,
    /// `/recovery/login/start`, keyed on the login handle.
    LoginRecovery = 2,
    /// `/password/reset/start`, keyed on the login handle.
    PasswordReset = 3,
    /// `/totp/recover`, keyed on the login handle.
    TotpRecover = 4,
    /// `/device/request`, keyed on the target login handle (ADR-0056).
    DeviceRequestTarget = 9,
    /// `/device/authorize/start`, keyed on the signed-in account.
    DeviceGrantMint = 10,
    /// `PUT /auth/login-handle`, keyed on the signed-in account (charged on
    /// every bind, whatever it answers).
    HandleBind = 11,
    /// `POST /account/keys/reset/{start,finish}`, keyed on the signed-in
    /// account (one hit per call, so a whole reset costs two). The name is
    /// the recovery-credential replace's, which the reset folded in
    /// (ADR-0073 section 2).
    RecoveryReplace = 12,
    /// `POST /account/keys/reset/finish` only, keyed on the signed-in
    /// account: the reset itself, rarer and more destructive than the
    /// credential replace it carries (ADR-0073 open question 3).
    KeyReset = 13,
    /// `POST /account/devices/revoke`, keyed on the signed-in account
    /// (ADR-0074 section 2.1). Charged only after the fresh-auth grant and
    /// the account-key statement were accepted, so a caller holding neither
    /// spends none of the owner's budget.
    DeviceRevoke = 15,
    /// `POST /account/recall/start`, keyed on the account and the anchor
    /// (ADR-0074 section 1.2): the password attempts of a recall. Charged
    /// only after the account statement verified under the anchor's key, so
    /// a caller without that key spends none of it.
    Recall = 14,
    /// `POST /account/keys/rotate/finish`, keyed on the signed-in account
    /// (ADR-0074 section 3.4): the key rotation itself. Charged only after
    /// the fresh-auth grant and the supersession proof were accepted, so a
    /// caller holding a session and the password but not the account key
    /// spends none of it.
    KeyRotate = 16,
    /// A letter of the recovery email asked for by login handle (ADR-0075
    /// section 10): `POST /recovery-email/recover/start` and
    /// `POST /recovery-email/enrol/start`, keyed on the login handle and
    /// charged before anything is looked up, so an unknown name is counted
    /// and refused exactly like a known one.
    EmailLetterHandle = 17,
    /// A letter of the recovery email per account, keyed on the account the
    /// handle resolved to (or its decoy). On the anonymous recover route a
    /// spent counter sends no letter and answers as always, so it is no
    /// oracle; on the bearer enrol route it answers `429`.
    EmailLetterAccount = 18,
    /// `POST /recovery-email/recover/verify`, keyed on the login handle the
    /// challenge was opened for: wrong codes across challenges.
    EmailVerify = 19,
    /// `POST /recovery-email/recover/open`, keyed on the account: how often
    /// its email vault may leave the server.
    EmailVaultOpen = 20,
    /// `PUT /recovery-email/vault`, keyed on the signed-in account (ADR-0075,
    /// amended 2026-10-13). Charged before the typed address is compared
    /// with the stored key: without it a session could test addresses
    /// against the account as fast as it can send them.
    EmailVaultStore = 21,
    /// `POST /auth/login-handle/check`, keyed on the signed-in account
    /// (ADR-0076 section 2). Its own counter, not [`Self::HandleBind`]'s:
    /// every device asks at every session landing, and the bind's five a day
    /// would be spent by the question.
    HandleCheck = 22,
    /// `PUT /auth/anon-credential/commitment` and `POST /auth/anon-credential`,
    /// keyed on the signed-in account (ADR-0090 section 4.2). Charged after
    /// the body decodes and before the proof is verified.
    AnonCredential = 23,
}

impl Scope {
    const ALL: [Self; 19] = [
        Self::LoginPassword,
        Self::LoginRecovery,
        Self::PasswordReset,
        Self::TotpRecover,
        Self::DeviceRequestTarget,
        Self::DeviceGrantMint,
        Self::HandleBind,
        Self::RecoveryReplace,
        Self::KeyReset,
        Self::DeviceRevoke,
        Self::Recall,
        Self::KeyRotate,
        Self::EmailLetterHandle,
        Self::EmailLetterAccount,
        Self::EmailVerify,
        Self::EmailVaultOpen,
        Self::EmailVaultStore,
        Self::HandleCheck,
        Self::AnonCredential,
    ];

    /// Position in [`Self::ALL`] (the stored discriminants have a gap, so
    /// this is not derived from them).
    const fn index(self) -> usize {
        match self {
            Self::LoginPassword => 0,
            Self::LoginRecovery => 1,
            Self::PasswordReset => 2,
            Self::TotpRecover => 3,
            Self::DeviceRequestTarget => 4,
            Self::DeviceGrantMint => 5,
            Self::HandleBind => 6,
            Self::RecoveryReplace => 7,
            Self::KeyReset => 8,
            Self::DeviceRevoke => 9,
            Self::Recall => 10,
            Self::KeyRotate => 11,
            Self::EmailLetterHandle => 12,
            Self::EmailLetterAccount => 13,
            Self::EmailVerify => 14,
            Self::EmailVaultOpen => 15,
            Self::EmailVaultStore => 16,
            Self::HandleCheck => 17,
            Self::AnonCredential => 18,
        }
    }

    const fn env_var(self) -> &'static str {
        match self {
            Self::LoginPassword => "LAKKO_AUTH_LIMIT_LOGIN",
            Self::LoginRecovery => "LAKKO_AUTH_LIMIT_RECOVERY_LOGIN",
            Self::PasswordReset => "LAKKO_AUTH_LIMIT_PASSWORD_RESET",
            Self::TotpRecover => "LAKKO_AUTH_LIMIT_TOTP_RECOVER",
            Self::DeviceRequestTarget => "LAKKO_AUTH_LIMIT_DEVICE_REQUEST",
            Self::DeviceGrantMint => "LAKKO_AUTH_LIMIT_DEVICE_GRANT",
            Self::HandleBind => "LAKKO_AUTH_LIMIT_HANDLE_BIND",
            Self::RecoveryReplace => "LAKKO_AUTH_LIMIT_RECOVERY_REPLACE",
            Self::KeyReset => "LAKKO_AUTH_LIMIT_KEY_RESET",
            Self::DeviceRevoke => "LAKKO_AUTH_LIMIT_DEVICE_REVOKE",
            Self::Recall => "LAKKO_AUTH_LIMIT_RECALL",
            Self::KeyRotate => "LAKKO_AUTH_LIMIT_KEY_ROTATE",
            Self::EmailLetterHandle => "LAKKO_AUTH_LIMIT_EMAIL_LETTER_HANDLE",
            Self::EmailLetterAccount => "LAKKO_AUTH_LIMIT_EMAIL_LETTER_ACCOUNT",
            Self::EmailVerify => "LAKKO_AUTH_LIMIT_EMAIL_VERIFY",
            Self::EmailVaultOpen => "LAKKO_AUTH_LIMIT_EMAIL_VAULT_OPEN",
            Self::EmailVaultStore => "LAKKO_AUTH_LIMIT_EMAIL_VAULT_STORE",
            Self::HandleCheck => "LAKKO_AUTH_LIMIT_HANDLE_CHECK",
            Self::AnonCredential => "LAKKO_AUTH_LIMIT_ANON_CREDENTIAL",
        }
    }

    /// Defaults — chosen, not measured. The TOTP value is the per-row
    /// lockout it replaces (5 in 15 minutes).
    const fn default_window(self) -> Window {
        match self {
            Self::LoginPassword
            | Self::LoginRecovery
            | Self::PasswordReset
            | Self::DeviceGrantMint => Window::new(10, 900),
            Self::TotpRecover | Self::Recall => Window::new(5, 900),
            Self::DeviceRequestTarget => Window::new(5, 600),
            // The email vault's five releases a day per account (ADR-0075
            // section 10) share the handle bind's figure.
            Self::HandleBind | Self::EmailVaultOpen => Window::new(5, 86_400),
            Self::RecoveryReplace => Window::new(10, 3600),
            Self::KeyReset | Self::KeyRotate => Window::new(3, 86_400),
            Self::DeviceRevoke => Window::new(20, 3600),
            // ADR-0075 section 10. Five letters an hour per name and ten a
            // day per account; fifteen code guesses an hour per name (three
            // challenges' worth).
            Self::EmailLetterHandle => Window::new(5, 3600),
            // Ten vault stores a day per account share the letter figure.
            Self::EmailLetterAccount | Self::EmailVaultStore => Window::new(10, 86_400),
            Self::EmailVerify => Window::new(15, 3600),
            // One question per device per landing, with room to spare.
            Self::HandleCheck => Window::new(120, 3600),
            // A device asks once a period, a few devices and retries a day.
            Self::AnonCredential => Window::new(30, 86_400),
        }
    }

    const fn refusal_code(self) -> &'static str {
        match self {
            Self::LoginPassword
            | Self::LoginRecovery
            | Self::PasswordReset
            | Self::TotpRecover
            | Self::DeviceGrantMint
            | Self::RecoveryReplace
            | Self::KeyReset
            | Self::DeviceRevoke
            | Self::Recall
            | Self::KeyRotate
            | Self::EmailVerify
            | Self::EmailVaultOpen
            | Self::EmailVaultStore
            | Self::HandleCheck
            | Self::AnonCredential => CODE_TOO_MANY_ATTEMPTS,
            Self::EmailLetterHandle | Self::EmailLetterAccount => CODE_EMAIL_RATE_LIMITED,
            Self::DeviceRequestTarget => CODE_DEVICE_REQUEST_LIMITED,
            Self::HandleBind => CODE_HANDLE_BIND_LIMITED,
        }
    }
}

/// A family of routes sharing one per-address bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Category {
    /// Login, recovery login, password reset, TOTP recovery, device
    /// authorize start / redeem.
    Attempt,
    /// `/device/request`.
    DeviceRequest,
    /// `/signup/start`.
    Signup,
    /// `/pow/challenge` (used only by signup).
    Challenge,
    /// `PUT /auth/login-handle`.
    HandleBind,
    /// The two routes that send a letter of the recovery email (ADR-0075):
    /// `/recovery-email/enrol/start` and `/recovery-email/recover/start`.
    Email,
}

impl Category {
    const ALL: [Self; 6] = [
        Self::Attempt,
        Self::DeviceRequest,
        Self::Signup,
        Self::Challenge,
        Self::HandleBind,
        Self::Email,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    const fn env_var(self) -> &'static str {
        match self {
            Self::Attempt => "LAKKO_AUTH_IP_LIMIT_ATTEMPT",
            Self::DeviceRequest => "LAKKO_AUTH_IP_LIMIT_DEVICE_REQUEST",
            Self::Signup => "LAKKO_AUTH_IP_LIMIT_SIGNUP",
            Self::Challenge => "LAKKO_AUTH_IP_LIMIT_POW_CHALLENGE",
            Self::HandleBind => "LAKKO_AUTH_IP_LIMIT_HANDLE_BIND",
            Self::Email => "LAKKO_AUTH_IP_LIMIT_EMAIL",
        }
    }

    /// Defaults — chosen, not measured. Generous enough for a household or
    /// an office behind one address; an attacker rotating addresses is what
    /// the name counters are for.
    const fn default_window(self) -> Window {
        match self {
            Self::Attempt => Window::new(30, 60),
            Self::DeviceRequest | Self::Signup => Window::new(10, 3600),
            Self::Challenge => Window::new(30, 3600),
            Self::HandleBind | Self::Email => Window::new(20, 3600),
        }
    }

    const fn refusal_code(self) -> &'static str {
        match self {
            Self::Attempt => CODE_TOO_MANY_ATTEMPTS,
            Self::DeviceRequest => CODE_DEVICE_REQUEST_LIMITED,
            Self::Signup | Self::Challenge => CODE_SIGNUP_RATE_LIMITED,
            Self::HandleBind => CODE_HANDLE_BIND_LIMITED,
            Self::Email => CODE_EMAIL_RATE_LIMITED,
        }
    }
}

/// One tier of the device-link guess budget: `wrong_codes.limit` wrong codes
/// within `wrong_codes.window_secs` suspend the account's device requests for
/// `suspend_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceLinkTier {
    /// How many wrong codes in how long trip this tier.
    pub(crate) wrong_codes: Window,
    /// How long a tripped tier suspends the account's device requests.
    pub(crate) suspend_secs: u32,
}

/// The device-link tiers, each with its `(N/SECS knob, suspension knob,
/// default)`. Defaults chosen, not measured.
const DEVICE_LINK_TIERS: [(&str, &str, DeviceLinkTier); 2] = [
    (
        "LAKKO_AUTH_LIMIT_DEVICE_LINK_WRONG_CODE_SHORT",
        "LAKKO_AUTH_DEVICE_LINK_SUSPEND_SHORT_SECS",
        DeviceLinkTier {
            wrong_codes: Window::new(3, 86_400),
            suspend_secs: 3600,
        },
    ),
    (
        "LAKKO_AUTH_LIMIT_DEVICE_LINK_WRONG_CODE_LONG",
        "LAKKO_AUTH_DEVICE_LINK_SUSPEND_LONG_SECS",
        DeviceLinkTier {
            wrong_codes: Window::new(6, 86_400),
            suspend_secs: 86_400,
        },
    ),
];

/// Every limit the service enforces, plus the in-memory address buckets.
#[derive(Debug)]
pub(crate) struct Limits {
    names: [Window; Scope::ALL.len()],
    ips: [Window; Category::ALL.len()],
    /// Key of the stored name-counter hashes (a subkey of the login-handle
    /// pepper, or a per-process random key when the pepper is unset).
    name_key: [u8; 32],
    /// Minimum seconds between two handle-changing binds of one account
    /// (0 = no minimum).
    handle_change_min_interval_secs: u32,
    /// The device-link guess budget, one entry per tier.
    device_link: [DeviceLinkTier; DEVICE_LINK_TIERS.len()],
    /// How long a recall anchor lives (ADR-0074 section 0.2), milliseconds.
    recall_window_ms: i64,
    /// Cap on the address-bucket map ([`MAX_ADDRESS_BUCKETS`]).
    max_buckets: usize,
    buckets: Mutex<HashMap<(Category, IpAddr), Bucket>>,
}

impl Limits {
    /// The documented defaults, keyed as under the test pepper (tests use
    /// this directly).
    #[cfg(test)]
    pub(crate) fn defaults() -> Self {
        Self::with_windows(
            Scope::ALL.map(Scope::default_window),
            Category::ALL.map(Category::default_window),
            name_subkey(&crate::login_handle::TEST_PEPPER),
            DEFAULT_HANDLE_CHANGE_MIN_INTERVAL_SECS,
        )
    }

    fn with_windows(
        names: [Window; Scope::ALL.len()],
        ips: [Window; Category::ALL.len()],
        name_key: [u8; 32],
        handle_change_min_interval_secs: u32,
    ) -> Self {
        Self {
            names,
            ips,
            name_key,
            handle_change_min_interval_secs,
            device_link: DEVICE_LINK_TIERS.map(|(_, _, tier)| tier),
            recall_window_ms: i64::from(DEFAULT_RECALL_WINDOW_DAYS) * MS_PER_DAY,
            max_buckets: MAX_ADDRESS_BUCKETS,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Read every knob; an unset knob takes its default, a malformed one
    /// fails startup. `handle_pepper` keys the stored name counters; without
    /// it they are keyed by a per-process random key (logged), so a restart
    /// starts every name window afresh. That posture is dormant anyway: the
    /// name routes answer `503` without the pepper.
    ///
    /// # Errors
    ///
    /// Returns an error naming the first knob that is set but is not `N/SECS`
    /// with both numbers positive, or a minimum interval that is not an
    /// unsigned integer.
    pub(crate) fn from_env(handle_pepper: Option<&[u8; 32]>) -> anyhow::Result<Self> {
        fn read(var: &str, default: Window) -> anyhow::Result<Window> {
            match std::env::var(var) {
                Ok(v) if !v.trim().is_empty() => Window::parse(&v)
                    .ok_or_else(|| anyhow::anyhow!("{var} must be N/SECS with both positive")),
                _ => Ok(default),
            }
        }
        let mut names = Scope::ALL.map(Scope::default_window);
        for scope in Scope::ALL {
            names[scope.index()] = read(scope.env_var(), scope.default_window())?;
        }
        let mut ips = Category::ALL.map(Category::default_window);
        for cat in Category::ALL {
            ips[cat.index()] = read(cat.env_var(), cat.default_window())?;
        }
        let interval = match std::env::var(HANDLE_CHANGE_INTERVAL_VAR) {
            Ok(v) if !v.trim().is_empty() => v.trim().parse::<u32>().map_err(|_| {
                anyhow::anyhow!("{HANDLE_CHANGE_INTERVAL_VAR} must be an unsigned integer")
            })?,
            _ => DEFAULT_HANDLE_CHANGE_MIN_INTERVAL_SECS,
        };
        let mut device_link = DEVICE_LINK_TIERS.map(|(_, _, tier)| tier);
        for (i, (window_var, suspend_var, default)) in DEVICE_LINK_TIERS.iter().enumerate() {
            let suspend_secs = match std::env::var(suspend_var) {
                Ok(v) if !v.trim().is_empty() => v
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| anyhow::anyhow!("{suspend_var} must be a positive integer"))?,
                _ => default.suspend_secs,
            };
            device_link[i] = DeviceLinkTier {
                wrong_codes: read(window_var, default.wrong_codes)?,
                suspend_secs,
            };
        }
        let name_key = if let Some(p) = handle_pepper {
            name_subkey(p)
        } else {
            use rand::RngCore as _;
            tracing::warn!(
                "auth name counters keyed by a per-process random key: \
                 LAKKO_AUTH_HANDLE_PEPPER is unset, so a restart resets every name window"
            );
            let mut k = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut k);
            k
        };
        let recall_window_days = match std::env::var(RECALL_WINDOW_VAR) {
            Ok(v) if !v.trim().is_empty() => v
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|d| *d > 0)
                .ok_or_else(|| anyhow::anyhow!("{RECALL_WINDOW_VAR} must be a positive integer"))?,
            _ => DEFAULT_RECALL_WINDOW_DAYS,
        };
        let mut limits = Self::with_windows(names, ips, name_key, interval);
        limits.device_link = device_link;
        limits.recall_window_ms = i64::from(recall_window_days) * MS_PER_DAY;
        Ok(limits)
    }

    /// Override one name window (tests).
    #[cfg(test)]
    pub(crate) fn with_name_window(mut self, scope: Scope, window: Window) -> Self {
        self.names[scope.index()] = window;
        self
    }

    /// Override one address window (tests).
    #[cfg(test)]
    pub(crate) fn with_ip_window(mut self, cat: Category, window: Window) -> Self {
        self.ips[cat.index()] = window;
        self
    }

    /// Override the minimum interval between handle changes (tests).
    #[cfg(test)]
    pub(crate) const fn with_handle_change_interval(mut self, secs: u32) -> Self {
        self.handle_change_min_interval_secs = secs;
        self
    }

    /// How long a recall anchor lives, in milliseconds
    /// (`LAKKO_RECALL_WINDOW_DAYS`, default 30).
    pub(crate) const fn recall_window_ms(&self) -> i64 {
        self.recall_window_ms
    }

    /// The device-link guess budget, one entry per tier.
    pub(crate) const fn device_link(&self) -> &[DeviceLinkTier] {
        &self.device_link
    }

    /// The longest device-link window, for the pruner.
    fn max_device_link_window_secs(&self) -> u32 {
        self.device_link
            .iter()
            .map(|t| t.wrong_codes.window_secs)
            .max()
            .unwrap_or(1)
    }

    /// Override the address-bucket cap (tests).
    #[cfg(test)]
    const fn with_max_buckets(mut self, max: usize) -> Self {
        self.max_buckets = max;
        self
    }

    /// The window for a name scope.
    pub(crate) const fn name_window(&self, scope: Scope) -> Window {
        self.names[scope.index()]
    }

    /// Minimum seconds between two handle-changing binds of one account.
    pub(crate) const fn handle_change_min_interval_secs(&self) -> u32 {
        self.handle_change_min_interval_secs
    }

    /// The stored counter key for `name` in `scope`.
    fn name_key(&self, scope: Scope, name: &[u8]) -> [u8; 32] {
        let mut material = Vec::with_capacity(1 + name.len());
        material.push(scope as u8);
        material.extend_from_slice(name);
        blake3_keyed(&self.name_key, &material)
    }

    /// Log the effective limits once at startup.
    pub(crate) fn log_startup(&self) {
        for scope in Scope::ALL {
            let w = self.name_window(scope);
            tracing::info!(
                knob = scope.env_var(),
                limit = w.limit,
                window_secs = w.window_secs,
                "auth name limit"
            );
        }
        for cat in Category::ALL {
            let w = self.ips[cat.index()];
            tracing::info!(
                knob = cat.env_var(),
                limit = w.limit,
                window_secs = w.window_secs,
                "auth address limit"
            );
        }
        tracing::info!(
            knob = HANDLE_CHANGE_INTERVAL_VAR,
            secs = self.handle_change_min_interval_secs,
            "auth handle change minimum interval"
        );
        for ((window_var, suspend_var, _), tier) in DEVICE_LINK_TIERS.iter().zip(&self.device_link)
        {
            tracing::info!(
                knob = window_var,
                suspend_knob = suspend_var,
                wrong_codes = tier.wrong_codes.limit,
                window_secs = tier.wrong_codes.window_secs,
                suspend_secs = tier.suspend_secs,
                "auth device-link guess budget"
            );
        }
    }

    /// Charge one request from `ip` (grouped by [`bucket_address`]) to
    /// `cat`'s bucket.
    fn charge_ip_at(&self, cat: Category, ip: IpAddr, now: Instant) -> Result<(), u64> {
        let w = self.ips[cat.index()];
        let capacity = f64::from(w.limit);
        let rate = capacity / f64::from(w.window_secs);
        let key = (cat, bucket_address(ip));
        let mut buckets = match self.buckets.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if !buckets.contains_key(&key) && buckets.len() >= self.max_buckets {
            self.make_room(&mut buckets, now);
        }
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: capacity,
            last: now,
        });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = rate.mul_add(elapsed, bucket.tokens).min(capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = ((1.0 - bucket.tokens) / rate).ceil();
            // `wait` is bounded by one window (u32 seconds), so the cast
            // cannot truncate a meaningful value.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let secs = wait.max(1.0) as u64;
            Err(secs)
        }
    }

    /// Drop buckets that have refilled completely; returns how many.
    fn prune_idle(&self, now: Instant) -> usize {
        let mut buckets = match self.buckets.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        self.retain_busy(&mut buckets, now)
    }

    fn retain_busy(
        &self,
        buckets: &mut HashMap<(Category, IpAddr), Bucket>,
        now: Instant,
    ) -> usize {
        let before = buckets.len();
        let ips = self.ips;
        buckets.retain(|(cat, _), b| {
            let full_after = Duration::from_secs(u64::from(ips[cat.index()].window_secs));
            now.saturating_duration_since(b.last) < full_after
        });
        before - buckets.len()
    }

    /// Bring the map below its cap: drop refilled buckets first; if that is
    /// not enough, drop the least recently charged eighth of the cap (at
    /// least one), so a flood of new sources pays one scan per batch rather
    /// than one per request. An evicted bucket that was not full is
    /// forgotten, i.e. refilled: past the cap, the per-name counters are what
    /// still bounds guessing.
    fn make_room(&self, buckets: &mut HashMap<(Category, IpAddr), Bucket>, now: Instant) {
        self.retain_busy(buckets, now);
        if buckets.len() < self.max_buckets {
            return;
        }
        let batch = (self.max_buckets / 8).max(1).min(buckets.len());
        let mut by_age: Vec<(Instant, (Category, IpAddr))> =
            buckets.iter().map(|(k, b)| (b.last, *k)).collect();
        by_age.select_nth_unstable_by_key(batch - 1, |(last, _)| *last);
        for (_, k) in &by_age[..batch] {
            buckets.remove(k);
        }
        tracing::debug!(
            evicted = batch,
            "auth address buckets at cap; evicted least recent"
        );
    }

    /// The longest name window, for the pruner.
    fn max_name_window_secs(&self) -> u32 {
        self.names.iter().map(|w| w.window_secs).max().unwrap_or(1)
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// A request refused by a limiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LimitError {
    /// A limit is reached: `429`, body code + `Retry-After`.
    Limited {
        /// Stable body code (one per route family).
        code: &'static str,
        /// Seconds until the limit admits a request again (>= 1).
        retry_after_secs: u64,
    },
    /// The counter store failed: `503`, the request is refused.
    Unavailable,
}

impl IntoResponse for LimitError {
    fn into_response(self) -> Response {
        match self {
            Self::Limited {
                code,
                retry_after_secs,
            } => {
                let mut resp = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({ "error": code, "retry_after_secs": retry_after_secs })),
                )
                    .into_response();
                if let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                    resp.headers_mut().insert(header::RETRY_AFTER, v);
                }
                resp
            },
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": CODE_LIMITER_UNAVAILABLE })),
            )
                .into_response(),
        }
    }
}

// ---------------------------------------------------------------------------
// Name counters
// ---------------------------------------------------------------------------

/// The limiter's subkey of the login-handle pepper (domain-separated from
/// the handle keys the pepper itself produces).
fn name_subkey(handle_pepper: &[u8; 32]) -> [u8; 32] {
    blake3_derive_key_from_material(NAME_KEY_DOMAIN, handle_pepper)
}

/// Charge one attempt against `scope` for `name` and refuse once the window's
/// limit is exceeded.
///
/// Call it before the handler looks `name` up, on every path: the counter
/// is keyed on the name alone, so an unknown name is counted and refused
/// exactly like a known one.
///
/// # Errors
///
/// [`LimitError::Limited`] once the window is spent; [`LimitError::Unavailable`]
/// when the store fails (fail closed).
pub(crate) async fn charge_name(
    pool: &MySqlPool,
    limits: &Limits,
    scope: Scope,
    name: &[u8],
) -> Result<(), LimitError> {
    let w = limits.name_window(scope);
    match bump(pool, scope, &limits.name_key(scope, name), w.window_secs).await {
        Ok((hits, _)) if hits <= w.limit => Ok(()),
        Ok((_, retry_after_secs)) => {
            tracing::debug!(scope = ?scope, "auth name limit reached");
            Err(LimitError::Limited {
                code: scope.refusal_code(),
                retry_after_secs,
            })
        },
        Err(e) => {
            tracing::error!(error = %e, scope = ?scope, "auth name limit store failed; refusing");
            Err(LimitError::Unavailable)
        },
    }
}

/// Atomically bump the fixed-window counter; returns `(hits in the current
/// window, seconds until it resets)`.
async fn bump(
    pool: &MySqlPool,
    scope: Scope,
    key: &[u8; 32],
    window_secs: u32,
) -> Result<(u32, u64), sqlx::Error> {
    let window = i64::from(window_secs);
    let mut tx = pool.begin().await?;
    // The `ON DUPLICATE KEY UPDATE` reads the pre-update `window_start` for
    // both conditions (assignments run left to right and `hits` comes first).
    sqlx::query(
        "INSERT INTO auth_name_limits (scope, key_hash, window_start, hits) \
         VALUES (?, ?, NOW(3), 1) \
         ON DUPLICATE KEY UPDATE \
           hits = IF(window_start <= NOW(3) - INTERVAL ? SECOND, 1, LEAST(hits + 1, ?)), \
           window_start = IF(window_start <= NOW(3) - INTERVAL ? SECOND, NOW(3), window_start)",
    )
    .bind(scope as u8)
    .bind(key.as_slice())
    .bind(window)
    .bind(MAX_HITS)
    .bind(window)
    .execute(&mut *tx)
    .await?;
    let (hits, remaining): (u32, i64) = sqlx::query_as(
        "SELECT hits, CAST(? - TIMESTAMPDIFF(SECOND, window_start, NOW(3)) AS SIGNED) \
         FROM auth_name_limits WHERE scope = ? AND key_hash = ?",
    )
    .bind(window)
    .bind(scope as u8)
    .bind(key.as_slice())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((hits, u64::try_from(remaining.max(1)).unwrap_or(1)))
}

// ---------------------------------------------------------------------------
// Address buckets (middleware)
// ---------------------------------------------------------------------------

/// The key requests without any identifiable source share.
const UNIDENTIFIED: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

/// The address a bucket is keyed on: an IPv4 address as is (an IPv4-mapped
/// IPv6 address as its IPv4 form), an IPv6 address as its /64 prefix.
/// Every address bucket goes through this, so rotating through one /64
/// buys nothing.
pub(crate) fn bucket_address(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(
            || {
                let prefix = u128::from(v6) & !u128::from(u64::MAX);
                IpAddr::V6(Ipv6Addr::from(prefix))
            },
            IpAddr::V4,
        ),
    }
}

/// The source address, trusting only what nginx wrote.
///
/// `X-Real-IP` is set by nginx to `$remote_addr`; of `X-Forwarded-For` only
/// the rightmost entry is nginx's (the rest is whatever the client claimed).
/// The service binds loopback in production, so nobody reaches it without
/// passing nginx.
fn source_ip(req: &Request<Body>) -> Option<IpAddr> {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
    if let Some(ip) = header("x-real-ip").and_then(|s| s.trim().parse::<IpAddr>().ok()) {
        return Some(ip);
    }
    if let Some(ip) = header("x-forwarded-for")
        .and_then(|s| s.rsplit(',').next())
        .and_then(|last| last.trim().parse::<IpAddr>().ok())
    {
        return Some(ip);
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

/// Route middleware: charge the request's source address to `cat`'s bucket,
/// `429` when it is empty. Wire with
/// `post(handler).route_layer(from_fn_with_state((limits, cat), ip_gate))`.
pub(crate) async fn ip_gate(
    State((limits, cat)): State<(Arc<Limits>, Category)>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = source_ip(&req).unwrap_or(UNIDENTIFIED);
    match limits.charge_ip_at(cat, ip, Instant::now()) {
        Ok(()) => next.run(req).await,
        Err(retry_after_secs) => {
            tracing::debug!(category = ?cat, "auth address limit reached");
            LimitError::Limited {
                code: cat.refusal_code(),
                retry_after_secs,
            }
            .into_response()
        },
    }
}

// ---------------------------------------------------------------------------
// Pruner
// ---------------------------------------------------------------------------

/// Periodically drop name counters whose window has long passed, address
/// buckets that have refilled, and device-link wrong codes and suspensions
/// that no longer count.
pub(crate) fn spawn_pruner(pool: MySqlPool, limits: Arc<Limits>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let dropped = limits.prune_idle(Instant::now());
            if dropped > 0 {
                tracing::debug!(dropped, "auth address buckets pruned");
            }
            let result = sqlx::query(
                "DELETE FROM auth_name_limits WHERE window_start < NOW(3) - INTERVAL ? SECOND",
            )
            .bind(i64::from(limits.max_name_window_secs()))
            .execute(&pool)
            .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "auth_name_limits prune failed");
            }
            let result = sqlx::query(
                "DELETE FROM device_link_wrong_codes WHERE at < NOW(3) - INTERVAL ? SECOND",
            )
            .bind(i64::from(limits.max_device_link_window_secs()))
            .execute(&pool)
            .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "device_link_wrong_codes prune failed");
            }
            let result =
                sqlx::query("DELETE FROM device_link_suspensions WHERE suspended_until < NOW(3)")
                    .execute(&pool)
                    .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "device_link_suspensions prune failed");
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, last))
    }

    #[test]
    fn window_parse() {
        assert_eq!(Window::parse("10/900"), Some(Window::new(10, 900)));
        assert_eq!(Window::parse(" 3 / 60 "), Some(Window::new(3, 60)));
        assert_eq!(Window::parse("0/60"), None);
        assert_eq!(Window::parse("5/0"), None);
        assert_eq!(Window::parse("5"), None);
        assert_eq!(Window::parse("a/b"), None);
    }

    #[test]
    fn scope_discriminants_are_pinned() {
        // Stored in the database; a renumbering would merge two counters.
        let pinned: Vec<u8> = Scope::ALL.iter().map(|s| *s as u8).collect();
        // 5 was the removed server-side three-word verifier's; 6, 7 and 8
        // were the removed one-time-code factor's. None is reused. 14 is the
        // ADR-0074 recall (listed after 15: it was added later); 16 is the
        // key rotation.
        assert_eq!(
            pinned,
            vec![1, 2, 3, 4, 9, 10, 11, 12, 13, 15, 14, 16, 17, 18, 19, 20, 21, 22, 23]
        );
        for (i, s) in Scope::ALL.iter().enumerate() {
            assert_eq!(s.index(), i);
        }
        for (i, c) in Category::ALL.iter().enumerate() {
            assert_eq!(c.index(), i);
        }
    }

    #[test]
    fn name_key_separates_scopes_and_names() {
        let limits = Limits::defaults();
        let a = limits.name_key(Scope::LoginPassword, b"acct");
        assert_eq!(a, limits.name_key(Scope::LoginPassword, b"acct"));
        assert_ne!(a, limits.name_key(Scope::LoginRecovery, b"acct"));
        assert_ne!(a, limits.name_key(Scope::LoginPassword, b"acct2"));
    }

    #[test]
    fn name_key_is_keyed_by_the_pepper() {
        // Without the pepper, a dump of `auth_name_limits` plus a list of
        // candidate names must not show which name a counter belongs to: the
        // stored key is not computable from (scope, name) alone.
        let limits = Limits::defaults();
        let mut material = vec![Scope::LoginPassword as u8];
        material.extend_from_slice(b"acct");
        let unkeyed = blake3_derive_key_from_material("lakko/auth/name-limit/v1", &material);
        assert_ne!(limits.name_key(Scope::LoginPassword, b"acct"), unkeyed);
        // Another pepper gives another key for the same name.
        let other = Limits::with_windows(
            Scope::ALL.map(Scope::default_window),
            Category::ALL.map(Category::default_window),
            name_subkey(&[0x11; 32]),
            0,
        );
        assert_ne!(
            limits.name_key(Scope::LoginPassword, b"acct"),
            other.name_key(Scope::LoginPassword, b"acct")
        );
        // The subkey is not the pepper itself (domain separation from the
        // login-handle keys the pepper produces).
        assert_ne!(
            name_subkey(&crate::login_handle::TEST_PEPPER),
            crate::login_handle::TEST_PEPPER
        );
    }

    #[test]
    fn ipv6_is_bucketed_per_64_and_ipv4_per_address() {
        let a: IpAddr = "2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:1:2:3:4".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(bucket_address(a), bucket_address(b));
        assert_eq!(
            bucket_address(a),
            "2001:db8:1:2::".parse::<IpAddr>().unwrap()
        );
        assert_ne!(bucket_address(a), bucket_address(c));
        assert_eq!(bucket_address(ip(7)), ip(7));
        assert_ne!(bucket_address(ip(7)), bucket_address(ip(8)));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(bucket_address(mapped), ip(7));

        // Rotating through one /64 draws on one bucket.
        let limits = Limits::defaults().with_ip_window(Category::Signup, Window::new(2, 3600));
        let t0 = Instant::now();
        assert!(limits.charge_ip_at(Category::Signup, a, t0).is_ok());
        assert!(limits.charge_ip_at(Category::Signup, b, t0).is_ok());
        assert!(limits
            .charge_ip_at(Category::Signup, "2001:db8:1:2::99".parse().unwrap(), t0)
            .is_err());
        assert!(limits.charge_ip_at(Category::Signup, c, t0).is_ok());
    }

    #[test]
    fn bucket_map_is_capped_idle_first_then_least_recent() {
        let limits = Limits::defaults()
            .with_ip_window(Category::Attempt, Window::new(2, 60))
            .with_max_buckets(8);
        let t0 = Instant::now();
        // Eight sources fill the map; the first four were charged earliest.
        for i in 0..8u8 {
            let t = t0 + Duration::from_secs(u64::from(i));
            assert!(limits.charge_ip_at(Category::Attempt, ip(i), t).is_ok());
        }
        let len = || limits.buckets.lock().unwrap().len();
        assert_eq!(len(), 8);
        // A ninth source, with nothing idle: the least recently charged
        // eighth of the cap (one bucket, source 0) goes.
        let t9 = t0 + Duration::from_secs(10);
        assert!(limits.charge_ip_at(Category::Attempt, ip(100), t9).is_ok());
        assert_eq!(len(), 8);
        {
            let b = limits.buckets.lock().unwrap();
            assert!(!b.contains_key(&(Category::Attempt, ip(0))));
            assert!(b.contains_key(&(Category::Attempt, ip(1))));
            assert!(b.contains_key(&(Category::Attempt, ip(100))));
        }
        // Once every bucket has refilled, a new source clears the idle ones
        // first rather than evicting by age.
        let later = t0 + Duration::from_secs(200);
        assert!(limits
            .charge_ip_at(Category::Attempt, ip(200), later)
            .is_ok());
        assert_eq!(len(), 1);
        // A known source never evicts.
        for _ in 0..3 {
            let _ = limits.charge_ip_at(Category::Attempt, ip(200), later);
        }
        assert_eq!(len(), 1);
    }

    #[test]
    fn bucket_admits_capacity_then_refuses_with_retry_after() {
        let limits = Limits::defaults().with_ip_window(Category::Signup, Window::new(3, 3600));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(limits.charge_ip_at(Category::Signup, ip(1), t0).is_ok());
        }
        // One token refills every 1200 s.
        assert_eq!(limits.charge_ip_at(Category::Signup, ip(1), t0), Err(1200));
        // Another address and another category are untouched.
        assert!(limits.charge_ip_at(Category::Signup, ip(2), t0).is_ok());
        assert!(limits.charge_ip_at(Category::Attempt, ip(1), t0).is_ok());
        // After one refill period exactly one more is admitted.
        let t1 = t0 + Duration::from_secs(1200);
        assert!(limits.charge_ip_at(Category::Signup, ip(1), t1).is_ok());
        assert!(limits.charge_ip_at(Category::Signup, ip(1), t1).is_err());
    }

    #[test]
    fn prune_drops_only_refilled_buckets() {
        let limits = Limits::defaults().with_ip_window(Category::Attempt, Window::new(2, 60));
        let t0 = Instant::now();
        assert!(limits.charge_ip_at(Category::Attempt, ip(1), t0).is_ok());
        assert_eq!(limits.prune_idle(t0 + Duration::from_secs(30)), 0);
        assert_eq!(limits.prune_idle(t0 + Duration::from_secs(60)), 1);
    }

    #[test]
    fn source_ip_trusts_real_ip_then_rightmost_forwarded() {
        let req = Request::builder()
            .header("x-forwarded-for", "203.0.113.9, 198.51.100.7")
            .body(Body::empty())
            .unwrap();
        assert_eq!(source_ip(&req), "198.51.100.7".parse().ok());
        let req = Request::builder()
            .header("x-real-ip", "198.51.100.8")
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(source_ip(&req), "198.51.100.8".parse().ok());
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(source_ip(&req), None);
    }

    #[tokio::test]
    async fn refusal_is_429_with_retry_after_and_code() {
        use http_body_util::BodyExt as _;
        let resp = LimitError::Limited {
            code: CODE_TOO_MANY_ATTEMPTS,
            retry_after_secs: 42,
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "42");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], CODE_TOO_MANY_ATTEMPTS);
        assert_eq!(v["retry_after_secs"], 42);

        let resp = LimitError::Unavailable.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
