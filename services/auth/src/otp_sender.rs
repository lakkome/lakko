// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! One-time-code delivery — recovery factor (d) (ADR-0018).
//!
//! Factor (d) recovers ACCESS by mailing or texting a 6-digit code to a
//! contact the user proved control of at enrollment. This module is the
//! delivery substrate: a single [`OtpSender`] abstraction with per-kind real
//! adapters, mirroring the `services/push-gw` provider posture (a trait +
//! a [`LogOnlyOtpSender`] default + real adapters armed by env config + a
//! resolver that wires them at startup).
//!
//! ## Invariant #1 (no plaintext PII on the server)
//!
//! The phone / email PLAINTEXT is supplied transiently to [`OtpSender::send`]
//! solely to address the outgoing message; it is **never stored** (the DB
//! holds only a keyed hash) and **never logged**. The [`LogOnlyOtpSender`]
//! logs the kind + code + account context but NOT the contact — even in dev,
//! where the developer enrolled the contact themselves. The real adapters
//! hand the plaintext to the SMTP / SMS transport and drop it.
//!
//! ## Trait mechanism
//!
//! [`OtpSender`] is an `#[async_trait]` trait — the exact same dispatch
//! mechanism `services/push-gw`'s `Provider` uses (`async-trait` 0.1). One
//! [`OtpDispatcher`] holds a sender per [`ContactKind`] and routes [`send`]
//! by kind; `AppState` stores it as `Arc<dyn OtpSender>`.
//!
//! [`send`]: OtpSender::send

use async_trait::async_trait;
use thiserror::Error;

/// Contact channel for a one-time code. The integer values match
/// `recovery_contacts.kind` / `otp_challenges.kind` and are wire-format
/// stable: never renumber.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum ContactKind {
    /// Email — code delivered over SMTP.
    Email,
    /// Phone — code delivered over SMS (Twilio-shaped HTTP API).
    Phone,
}

impl ContactKind {
    /// The `TINYINT UNSIGNED` DB discriminator.
    pub(crate) const fn as_db(self) -> u8 {
        match self {
            ContactKind::Email => 1,
            ContactKind::Phone => 2,
        }
    }

    /// Lowercase canonical name for log lines.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            ContactKind::Email => "email",
            ContactKind::Phone => "phone",
        }
    }
}

impl TryFrom<u8> for ContactKind {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(ContactKind::Email),
            2 => Ok(ContactKind::Phone),
            _ => Err(()),
        }
    }
}

/// Errors a sender can return. The handler maps every variant to an
/// `ApiError::Internal` (the caller never learns whether delivery itself
/// failed — that would be an oracle on the recover path), so the variants
/// exist mainly for operator-facing log detail.
#[derive(Error, Debug)]
#[allow(dead_code)] // some variants only constructed by the env-armed adapters
pub(crate) enum OtpSendError {
    /// No real adapter is configured for this kind — only happens if a
    /// dispatcher is built without a fallback, which [`build_otp_sender`]
    /// never does (it always wires a [`LogOnlyOtpSender`] fallback).
    #[error("no sender configured for kind {0}")]
    NoSenderForKind(&'static str),
    /// The transport (SMTP / SMS HTTP) rejected or failed to dispatch.
    #[error("otp delivery transport error: {0}")]
    Transport(String),
}

/// One OTP delivery channel. Implementations are constructed once at service
/// start and stored behind the [`OtpDispatcher`] in `AppState`.
#[async_trait]
pub(crate) trait OtpSender: Send + Sync {
    /// Deliver `code` to `contact` over the channel for `kind`.
    ///
    /// `contact` is the RAW phone / email plaintext, supplied transiently by
    /// the handler. Implementations MUST NOT persist or log it (invariant
    /// #1) — only use it to address the outgoing message.
    async fn send(&self, kind: ContactKind, contact: &str, code: &str) -> Result<(), OtpSendError>;
}

// ============================================================================
// LogOnlyOtpSender — dev / test / pre-credentials default
// ============================================================================

/// A sender that records the intent into `tracing` instead of contacting a
/// remote service, returning success. Used in dev, tests, and in production
/// for any kind whose real adapter is not configured.
///
/// Invariant #1: it logs the code + kind + a short account context but NEVER
/// the contact plaintext.
pub(crate) struct LogOnlyOtpSender;

#[async_trait]
impl OtpSender for LogOnlyOtpSender {
    async fn send(
        &self,
        kind: ContactKind,
        _contact: &str,
        code: &str,
    ) -> Result<(), OtpSendError> {
        // The contact (phone / email) is DELIBERATELY absent from this log
        // line — invariant #1 holds even in dev. The developer enrolled the
        // contact so they already know which one this code is for.
        tracing::info!(
            kind = kind.as_str(),
            code = %code,
            "[stub] would deliver recovery OTP (contact omitted per invariant #1)",
        );
        Ok(())
    }
}

// ============================================================================
// SmtpOtpSender — real email adapter (lettre, async tokio + rustls)
// ============================================================================

/// SMTP configuration, loaded from env. All five vars must be present for the
/// real email adapter to arm; otherwise email falls back to LogOnly.
pub(crate) struct SmtpConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    from: String,
}

impl SmtpConfig {
    /// Read `LAKKO_SMTP_{HOST,PORT,USERNAME,PASSWORD,FROM}`. Returns `None`
    /// when `LAKKO_SMTP_HOST` is unset (email then falls back to LogOnly);
    /// returns `None` with a warning when HOST is set but a required
    /// companion var is missing or malformed (a half-configured adapter is a
    /// config error, but it must not crash the service — the LogOnly
    /// fallback keeps the rest of auth running).
    fn from_env() -> Option<Self> {
        let host = non_empty_env("LAKKO_SMTP_HOST")?;
        // Default to submission-over-implicit-TLS (465) when unset; a SET but
        // unparseable port is a config error -> LogOnly fallback.
        let port = match non_empty_env("LAKKO_SMTP_PORT") {
            None => 465,
            Some(raw) => {
                let Ok(p) = raw.parse::<u16>() else {
                    tracing::warn!("LAKKO_SMTP_PORT is not a valid port; email -> LogOnly");
                    return None;
                };
                p
            },
        };
        let Some(username) = non_empty_env("LAKKO_SMTP_USERNAME") else {
            tracing::warn!("LAKKO_SMTP_HOST set but LAKKO_SMTP_USERNAME missing; email -> LogOnly");
            return None;
        };
        let Some(password) = non_empty_env("LAKKO_SMTP_PASSWORD") else {
            tracing::warn!("LAKKO_SMTP_HOST set but LAKKO_SMTP_PASSWORD missing; email -> LogOnly");
            return None;
        };
        let Some(from) = non_empty_env("LAKKO_SMTP_FROM") else {
            tracing::warn!("LAKKO_SMTP_HOST set but LAKKO_SMTP_FROM missing; email -> LogOnly");
            return None;
        };
        Some(Self {
            host,
            port,
            username,
            password,
            from,
        })
    }
}

/// Real email sender over SMTP (lettre, async tokio transport + rustls TLS,
/// no native-tls). The transport is built once at startup and reused.
pub(crate) struct SmtpOtpSender {
    from: lettre::message::Mailbox,
    transport: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
}

impl SmtpOtpSender {
    /// Build the sender from [`SmtpConfig`].
    ///
    /// # Errors
    ///
    /// Returns an error when the `from` address does not parse as a mailbox
    /// or the SMTP relay handshake cannot be configured for `host`.
    fn new(cfg: SmtpConfig) -> anyhow::Result<Self> {
        use lettre::transport::smtp::authentication::Credentials;
        let from: lettre::message::Mailbox = cfg
            .from
            .parse()
            .map_err(|e| anyhow::anyhow!("LAKKO_SMTP_FROM is not a valid mailbox: {e}"))?;
        let creds = Credentials::new(cfg.username, cfg.password);
        let transport = lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&cfg.host)
            .map_err(|e| anyhow::anyhow!("configure SMTP relay {}: {e}", cfg.host))?
            .port(cfg.port)
            .credentials(creds)
            .build();
        Ok(Self { from, transport })
    }
}

#[async_trait]
impl OtpSender for SmtpOtpSender {
    async fn send(
        &self,
        _kind: ContactKind,
        contact: &str,
        code: &str,
    ) -> Result<(), OtpSendError> {
        use lettre::AsyncTransport as _;
        let to: lettre::message::Mailbox = contact
            .parse()
            .map_err(|e| OtpSendError::Transport(format!("recipient address invalid: {e}")))?;
        let body = format!("{code} is your Lakko recovery code. It expires in 10 minutes.");
        let email = lettre::Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject("Your Lakko recovery code")
            .header(lettre::message::header::ContentType::TEXT_PLAIN)
            .body(body)
            .map_err(|e| OtpSendError::Transport(format!("build email: {e}")))?;
        self.transport
            .send(email)
            .await
            .map_err(|e| OtpSendError::Transport(format!("smtp send: {e}")))?;
        Ok(())
    }
}

// ============================================================================
// SmsOtpSender — real SMS adapter (Twilio-shaped HTTP, reqwest + rustls)
// ============================================================================

/// SMS configuration, loaded from env. Twilio-shaped: the API URL already
/// embeds the account SID (`.../Accounts/<SID>/Messages.json`) and the SID +
/// auth token go in HTTP Basic auth.
pub(crate) struct SmsConfig {
    api_url: String,
    account_sid: String,
    auth_token: String,
    from: String,
}

impl SmsConfig {
    /// Read `LAKKO_SMS_{API_URL,ACCOUNT_SID,AUTH_TOKEN,FROM}`. Returns `None`
    /// when `LAKKO_SMS_API_URL` is unset (phone then falls back to LogOnly);
    /// returns `None` with a warning when the URL is set but a required
    /// companion var is missing.
    fn from_env() -> Option<Self> {
        let api_url = non_empty_env("LAKKO_SMS_API_URL")?;
        let Some(account_sid) = non_empty_env("LAKKO_SMS_ACCOUNT_SID") else {
            tracing::warn!(
                "LAKKO_SMS_API_URL set but LAKKO_SMS_ACCOUNT_SID missing; phone -> LogOnly"
            );
            return None;
        };
        let Some(auth_token) = non_empty_env("LAKKO_SMS_AUTH_TOKEN") else {
            tracing::warn!(
                "LAKKO_SMS_API_URL set but LAKKO_SMS_AUTH_TOKEN missing; phone -> LogOnly"
            );
            return None;
        };
        let Some(from) = non_empty_env("LAKKO_SMS_FROM") else {
            tracing::warn!("LAKKO_SMS_API_URL set but LAKKO_SMS_FROM missing; phone -> LogOnly");
            return None;
        };
        Some(Self {
            api_url,
            account_sid,
            auth_token,
            from,
        })
    }
}

/// Real SMS sender over a Twilio-shaped HTTP API (reqwest, rustls, no
/// native-tls). The client is built once at startup and reused.
pub(crate) struct SmsOtpSender {
    http: reqwest::Client,
    api_url: String,
    account_sid: String,
    auth_token: String,
    from: String,
}

impl SmsOtpSender {
    /// Build the sender from [`SmsConfig`].
    ///
    /// # Errors
    ///
    /// Returns an error when the reqwest client cannot be constructed.
    fn new(cfg: SmsConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| anyhow::anyhow!("build SMS HTTP client: {e}"))?;
        Ok(Self {
            http,
            api_url: cfg.api_url,
            account_sid: cfg.account_sid,
            auth_token: cfg.auth_token,
            from: cfg.from,
        })
    }
}

#[async_trait]
impl OtpSender for SmsOtpSender {
    async fn send(
        &self,
        _kind: ContactKind,
        contact: &str,
        code: &str,
    ) -> Result<(), OtpSendError> {
        let body = format!("{code} is your Lakko code");
        let form = [
            ("From", self.from.as_str()),
            ("To", contact),
            ("Body", body.as_str()),
        ];
        let resp = self
            .http
            .post(&self.api_url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&form)
            .send()
            .await
            .map_err(|e| OtpSendError::Transport(format!("sms http send: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(OtpSendError::Transport(format!(
                "sms provider returned HTTP {status}"
            )));
        }
        Ok(())
    }
}

// ============================================================================
// OtpDispatcher — routes send() to the per-kind sender
// ============================================================================

/// One dispatcher holding a sender per [`ContactKind`]. Stored in `AppState`
/// as `Arc<dyn OtpSender>`; the handler calls [`OtpSender::send`] and the
/// dispatcher forwards to the right adapter — mirroring how `services/push-gw`
/// routes through a `HashMap<ProviderKind, Box<dyn Provider>>`.
pub(crate) struct OtpDispatcher {
    email: Box<dyn OtpSender>,
    phone: Box<dyn OtpSender>,
}

#[async_trait]
impl OtpSender for OtpDispatcher {
    async fn send(&self, kind: ContactKind, contact: &str, code: &str) -> Result<(), OtpSendError> {
        match kind {
            ContactKind::Email => self.email.send(kind, contact, code).await,
            ContactKind::Phone => self.phone.send(kind, contact, code).await,
        }
    }
}

/// `Some(v)` when env var `name` is set and non-empty (trimmed); `None`
/// otherwise.
fn non_empty_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

/// Build the OTP dispatcher, choosing a real adapter per kind when its env is
/// configured and falling back to [`LogOnlyOtpSender`] otherwise — exactly
/// the `services/push-gw::resolve_providers` posture. Logs which real senders
/// armed at startup.
///
/// A malformed real-adapter config (e.g. an unparseable `from` mailbox) is
/// logged and degrades that kind to LogOnly rather than crashing the service;
/// the rest of auth stays up.
pub(crate) fn build_otp_sender() -> OtpDispatcher {
    // Explicit `let p: Box<dyn OtpSender>` so each branch can return a
    // concrete `Box<SmtpOtpSender>` / `Box<LogOnlyOtpSender>` without an `as`
    // coercion — the let-type drives unification (the `services/push-gw`
    // `resolve_providers` posture).
    let email: Box<dyn OtpSender> = if let Some(cfg) = SmtpConfig::from_env() {
        match SmtpOtpSender::new(cfg) {
            Ok(s) => {
                tracing::info!("otp email sender: SMTP armed from env");
                Box::new(s)
            },
            Err(e) => {
                tracing::warn!(error = %e, "SMTP config present but unusable; email -> LogOnly");
                Box::new(LogOnlyOtpSender)
            },
        }
    } else {
        tracing::info!("otp email sender: LogOnly (LAKKO_SMTP_HOST not configured)");
        Box::new(LogOnlyOtpSender)
    };

    let phone: Box<dyn OtpSender> = if let Some(cfg) = SmsConfig::from_env() {
        match SmsOtpSender::new(cfg) {
            Ok(s) => {
                tracing::info!("otp phone sender: SMS (Twilio-shaped) armed from env");
                Box::new(s)
            },
            Err(e) => {
                tracing::warn!(error = %e, "SMS config present but unusable; phone -> LogOnly");
                Box::new(LogOnlyOtpSender)
            },
        }
    } else {
        tracing::info!("otp phone sender: LogOnly (LAKKO_SMS_API_URL not configured)");
        Box::new(LogOnlyOtpSender)
    };

    OtpDispatcher { email, phone }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_kind_db_values_are_stable() {
        assert_eq!(ContactKind::Email.as_db(), 1);
        assert_eq!(ContactKind::Phone.as_db(), 2);
        assert_eq!(ContactKind::try_from(1u8), Ok(ContactKind::Email));
        assert_eq!(ContactKind::try_from(2u8), Ok(ContactKind::Phone));
        assert_eq!(ContactKind::try_from(0u8), Err(()));
        assert_eq!(ContactKind::try_from(3u8), Err(()));
    }

    #[tokio::test]
    async fn log_only_sender_always_succeeds() {
        let s = LogOnlyOtpSender;
        s.send(ContactKind::Email, "alice@example.com", "123456")
            .await
            .expect("LogOnly always Ok");
        s.send(ContactKind::Phone, "+15551234567", "654321")
            .await
            .expect("LogOnly always Ok");
    }
}
