// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sending the one letter this service sends: a recovery-email code
//! ([ADR-0075] section 10, UX `docs/ux/identity-recovery.md` section 19.4).
//!
//! [`MailSender`] is the seam. Two implementations:
//!
//! - [`LogOnlySender`], the default when `LAKKO_AUTH_SMTP_HOST` is unset. It
//!   delivers nothing and logs only that a letter was queued. **It never logs
//!   the address or the code**: the address is not kept anywhere on this
//!   server (invariant #1), and a code in a log file is a code anyone who
//!   reads logs can use. A stack running on it cannot complete a recovery;
//!   it exists so the routes can be exercised without a relay.
//! - [`SmtpSender`], over `lettre` with rustls (never native-tls), configured
//!   by `LAKKO_AUTH_SMTP_*`.
//!
//! The letter is fixed text: subject "Your Lakko code"; the code; "It works
//! for 10 minutes."; "If you did not ask for this, ignore this letter. Your
//! password has not changed." No username, no link, no device or location,
//! no image. English only for now: [`render`] takes the requesting device's
//! language and is where per-language templates will be chosen.
//!
//! # Configuration
//!
//! - `LAKKO_AUTH_SMTP_HOST` — the relay's host name. Unset: log-only.
//! - `LAKKO_AUTH_SMTP_PORT` — default 587 with STARTTLS, 465 with implicit
//!   TLS.
//! - `LAKKO_AUTH_SMTP_TLS` — `starttls` (default) or `implicit`. There is no
//!   plaintext mode.
//! - `LAKKO_AUTH_SMTP_USERNAME` / `LAKKO_AUTH_SMTP_PASSWORD` — both or
//!   neither.
//! - `LAKKO_AUTH_SMTP_FROM` — the `From` mailbox, e.g.
//!   `Lakko <no-reply@lakko.me>`. Required with a host.
//! - `LAKKO_AUTH_SMTP_HELO` — the name sent in `EHLO`. Default: the domain
//!   of the `From` address.
//!
//! [ADR-0075]: ../../../docs/adr/0075-recovery-email.md

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// How long one SMTP exchange may take before the send is given up.
const SMTP_TIMEOUT: Duration = Duration::from_secs(20);

/// One letter to send. Deliberately neither `Debug` nor `Clone`: it holds the
/// address and the code, and nothing should be able to print it by accident.
pub(crate) struct Letter {
    /// The normalised address, exactly as the person typed it after
    /// normalisation.
    pub(crate) to: String,
    /// The six-digit code.
    pub(crate) code: String,
    /// The requesting device's language tag, if it sent one. Chooses the
    /// template in [`render`]; never logged.
    pub(crate) language: Option<String>,
}

/// Why a letter was not handed to the relay. Carries no address and no code.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MailError {
    /// The address is not one the SMTP library accepts as a mailbox.
    #[error("the address is not a deliverable mailbox")]
    BadAddress,
    /// The letter could not be built.
    #[error("the letter could not be built")]
    Build,
    /// The relay refused the letter or could not be reached.
    #[error("the relay did not accept the letter")]
    Transport,
    /// The relay did not answer in time.
    #[error("the relay timed out")]
    Timeout,
}

/// The future a [`MailSender`] returns.
pub(crate) type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<(), MailError>> + Send + 'a>>;

/// Something that can send a [`Letter`].
pub(crate) trait MailSender: Send + Sync {
    /// Hand the letter to the relay. `Ok` means the relay accepted it, not
    /// that it was delivered.
    fn send(&self, letter: Letter) -> SendFuture<'_>;

    /// A short name for the startup log.
    fn name(&self) -> &'static str;
}

/// The subject and plain-text body of a code letter.
///
/// **The seam for per-language templates.** `_language` is the requesting
/// device's language tag; every language gets the English text until the
/// templates exist (they are to come from the l10n catalog, ADR-0061).
pub(crate) fn render(_language: Option<&str>, code: &str) -> (String, String) {
    let subject = "Your Lakko code".to_string();
    let body = format!(
        "{code}\r\n\r\n\
         It works for 10 minutes.\r\n\r\n\
         If you did not ask for this, ignore this letter. Your password has not changed.\r\n"
    );
    (subject, body)
}

/// Delivers nothing; logs that a letter was queued and nothing about it.
pub(crate) struct LogOnlySender;

impl MailSender for LogOnlySender {
    fn send(&self, letter: Letter) -> SendFuture<'_> {
        Box::pin(async move {
            // The letter is dropped here on purpose. Neither field is logged.
            drop(letter);
            tracing::info!("recovery email: a letter was queued (log-only sender; not delivered)");
            Ok(())
        })
    }

    fn name(&self) -> &'static str {
        "log-only"
    }
}

/// Which TLS mode the relay is spoken to in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SmtpTls {
    /// Plain connection upgraded with `STARTTLS` (required, not opportunistic).
    StartTls,
    /// TLS from the first byte.
    Implicit,
}

/// Sends over SMTP with `lettre` (tokio, rustls with the bundled web PKI
/// roots).
pub(crate) struct SmtpSender {
    from: lettre::message::Mailbox,
    transport: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
}

impl SmtpSender {
    /// Build the sender from the `LAKKO_AUTH_SMTP_*` variables. `Ok(None)`
    /// when no host is set.
    ///
    /// # Errors
    ///
    /// A host with a missing or unparsable `From`, a malformed port or TLS
    /// mode, a username without a password (or the reverse), or a relay name
    /// the library refuses.
    pub(crate) fn from_env() -> anyhow::Result<Option<Self>> {
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::transport::smtp::extension::ClientId;

        let Some(host) = env("LAKKO_AUTH_SMTP_HOST") else {
            return Ok(None);
        };
        let tls = match env("LAKKO_AUTH_SMTP_TLS").as_deref() {
            None | Some("starttls") => SmtpTls::StartTls,
            Some("implicit") => SmtpTls::Implicit,
            Some(_) => anyhow::bail!("LAKKO_AUTH_SMTP_TLS must be starttls or implicit"),
        };
        let from: lettre::message::Mailbox = env("LAKKO_AUTH_SMTP_FROM")
            .ok_or_else(|| anyhow::anyhow!("LAKKO_AUTH_SMTP_FROM must be set with a host"))?
            .parse()
            .map_err(|_| anyhow::anyhow!("LAKKO_AUTH_SMTP_FROM is not a mailbox"))?;
        let builder = match tls {
            SmtpTls::StartTls => {
                lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::starttls_relay(&host)
            },
            SmtpTls::Implicit => lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&host),
        }
        .map_err(|e| anyhow::anyhow!("LAKKO_AUTH_SMTP_HOST is not usable: {e}"))?;
        let mut builder = builder.timeout(Some(SMTP_TIMEOUT));
        if let Some(port) = env("LAKKO_AUTH_SMTP_PORT") {
            let port: u16 = port
                .parse()
                .map_err(|_| anyhow::anyhow!("LAKKO_AUTH_SMTP_PORT must be a port number"))?;
            builder = builder.port(port);
        }
        match (
            env("LAKKO_AUTH_SMTP_USERNAME"),
            env("LAKKO_AUTH_SMTP_PASSWORD"),
        ) {
            (Some(user), Some(pass)) => builder = builder.credentials(Credentials::new(user, pass)),
            (None, None) => {},
            _ => anyhow::bail!(
                "LAKKO_AUTH_SMTP_USERNAME and LAKKO_AUTH_SMTP_PASSWORD must be set together"
            ),
        }
        let helo = env("LAKKO_AUTH_SMTP_HELO").unwrap_or_else(|| from.email.domain().to_string());
        let transport = builder.hello_name(ClientId::Domain(helo)).build();
        Ok(Some(Self { from, transport }))
    }
}

impl MailSender for SmtpSender {
    fn send(&self, letter: Letter) -> SendFuture<'_> {
        Box::pin(async move {
            use lettre::AsyncTransport as _;
            let to: lettre::message::Mailbox =
                letter.to.parse().map_err(|_| MailError::BadAddress)?;
            let (subject, body) = render(letter.language.as_deref(), &letter.code);
            let message = lettre::Message::builder()
                .from(self.from.clone())
                .to(to)
                .subject(subject)
                .header(lettre::message::header::ContentType::TEXT_PLAIN)
                .body(body)
                .map_err(|_| MailError::Build)?;
            // The library's own error can quote the relay's answer, which can
            // quote the recipient: only its kind is kept.
            match tokio::time::timeout(SMTP_TIMEOUT, self.transport.send(message)).await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => {
                    tracing::warn!(
                        permanent = e.is_permanent(),
                        transient = e.is_transient(),
                        "recovery email: the relay did not accept a letter"
                    );
                    Err(MailError::Transport)
                },
                Err(_) => Err(MailError::Timeout),
            }
        })
    }

    fn name(&self) -> &'static str {
        "smtp"
    }
}

/// The sender the environment configures: SMTP with a host, else log-only.
///
/// # Errors
///
/// A malformed `LAKKO_AUTH_SMTP_*` value (see [`SmtpSender::from_env`]).
pub(crate) fn sender_from_env() -> anyhow::Result<Arc<dyn MailSender>> {
    Ok(match SmtpSender::from_env()? {
        Some(smtp) => Arc::new(smtp),
        None => Arc::new(LogOnlySender),
    })
}

/// A trimmed, non-empty environment variable.
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Keeps every letter it is handed, so a test can read the code a person
/// would read in their mailbox.
#[cfg(test)]
pub(crate) struct RecordingSender {
    letters: std::sync::Mutex<Vec<(String, String)>>,
    fail: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl RecordingSender {
    pub(crate) fn new() -> Self {
        Self {
            letters: std::sync::Mutex::new(Vec::new()),
            fail: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Make every later send fail, as an unreachable relay does.
    pub(crate) fn fail_from_now(&self) {
        self.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The `(address, code)` pairs sent so far, oldest first.
    pub(crate) fn sent(&self) -> Vec<(String, String)> {
        self.letters.lock().expect("letters lock").clone()
    }
}

#[cfg(test)]
impl MailSender for RecordingSender {
    fn send(&self, letter: Letter) -> SendFuture<'_> {
        Box::pin(async move {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(MailError::Transport);
            }
            self.letters
                .lock()
                .expect("letters lock")
                .push((letter.to, letter.code));
            Ok(())
        })
    }

    fn name(&self) -> &'static str {
        "recording"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_letter_is_the_fixed_text_with_the_code_and_nothing_else() {
        let (subject, body) = render(Some("ru"), "042917");
        assert_eq!(subject, "Your Lakko code");
        assert!(body.starts_with("042917\r\n"));
        assert!(body.contains("It works for 10 minutes."));
        assert!(body.contains(
            "If you did not ask for this, ignore this letter. Your password has not changed."
        ));
        assert!(!body.contains("http"), "the letter carries no link");
        // The language does not change the text yet.
        assert_eq!(render(None, "042917").1, body);
    }

    #[tokio::test]
    async fn the_log_only_sender_accepts_and_delivers_nothing() {
        let sent = LogOnlySender
            .send(Letter {
                to: "person@example.com".to_string(),
                code: "123456".to_string(),
                language: None,
            })
            .await;
        assert!(sent.is_ok());
        assert_eq!(LogOnlySender.name(), "log-only");
    }
}
