// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The canary's error type.
//!
//! Every variant reduces to a short class ([`Error::class`]) that is safe to
//! log: it never carries a URL, an account, a group, a blob, a room or a
//! peer, only a status, a server error code from a closed set, or a fixed
//! word.

use thiserror::Error;

/// Anything that can stop a step.
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// The command line is wrong.
    #[error("usage: {0}")]
    Usage(String),
    /// The state file is missing, unreadable, malformed or already exists.
    #[error("state: {0}")]
    State(String),
    /// A server answered with an unexpected status.
    #[error("http status {status} ({code})")]
    Http {
        /// The status code.
        status: u16,
        /// The server's error code when it is a short kebab word, else `-`.
        code: String,
    },
    /// The request did not complete (connect, TLS, timeout, body).
    #[error("network: {0}")]
    Network(&'static str),
    /// A server answer did not have the expected shape.
    #[error("protocol: {0}")]
    Protocol(&'static str),
    /// An MLS operation failed.
    #[error("mls: {0}")]
    Mls(&'static str),
    /// A cryptographic operation (OPAQUE, token, AEAD, SFrame) failed.
    #[error("crypto: {0}")]
    Crypto(&'static str),
    /// The step did not finish before its deadline.
    #[error("timeout: {0}")]
    Timeout(&'static str),
    /// The step finished but the result was wrong (e.g. bytes differ).
    #[error("mismatch: {0}")]
    Mismatch(&'static str),
}

impl Error {
    /// A short class for the one WARN line a failed step logs.
    #[must_use]
    pub(crate) fn class(&self) -> String {
        match self {
            Self::Usage(_) => "usage".to_owned(),
            Self::State(_) => "state".to_owned(),
            Self::Http { status, code } if code == "-" => format!("http-{status}"),
            Self::Http { status, code } => format!("http-{status}-{code}"),
            Self::Network(w) => format!("network-{w}"),
            Self::Protocol(w) => format!("protocol-{w}"),
            Self::Mls(w) => format!("mls-{w}"),
            Self::Crypto(w) => format!("crypto-{w}"),
            Self::Timeout(w) => format!("timeout-{w}"),
            Self::Mismatch(w) => format!("mismatch-{w}"),
        }
    }
}

/// Keep a server error code only when it is a short kebab-case word, so a
/// body that is not one of the services' fixed codes never reaches a log.
#[must_use]
pub(crate) fn safe_code(raw: Option<&str>) -> String {
    match raw {
        Some(c)
            if !c.is_empty()
                && c.len() <= 48
                && c.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'-' || b.is_ascii_digit()) =>
        {
            c.to_owned()
        },
        _ => "-".to_owned(),
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Network("timeout")
        } else if e.is_connect() {
            Self::Network("connect")
        } else if e.is_decode() {
            Self::Protocol("body")
        } else {
            Self::Network("request")
        }
    }
}

/// Result alias.
pub(crate) type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_outside_the_kebab_shape_are_dropped() {
        assert_eq!(safe_code(Some("too-many-attempts")), "too-many-attempts");
        assert_eq!(safe_code(Some("Bad Thing")), "-");
        assert_eq!(safe_code(Some(&"a".repeat(49))), "-");
        assert_eq!(safe_code(None), "-");
        assert_eq!(
            Error::Http {
                status: 401,
                code: "-".into()
            }
            .class(),
            "http-401"
        );
    }
}
