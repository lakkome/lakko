// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The capture layer service integration tests install (ADR-0078 section
//! 11, decision 5 half b).
//!
//! [`Capture::install`] sets a thread-local `tracing` subscriber that writes
//! every event at info level and above, formatted the way the services'
//! `fmt` subscriber formats it, into a buffer. A test drives real routes
//! (handlers run on the test's current-thread runtime, so on the same
//! thread) and then calls [`Capture::assert_no_identifiers`], which fails on
//! any value shaped like a 16- or 32-byte identifier ([`crate::scan`]) not
//! on the allow-list it is given.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use tracing::subscriber::DefaultGuard;
use tracing_subscriber::fmt::MakeWriter;

use crate::scan::{find_identifiers, Finding};

/// A buffer the capture subscriber writes formatted log lines into.
#[derive(Clone, Default)]
pub struct Capture {
    buf: Arc<Mutex<Vec<u8>>>,
}

/// One write handle into a [`Capture`].
pub struct CaptureWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for CaptureWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter {
            buf: Arc::clone(&self.buf),
        }
    }
}

impl Capture {
    /// Install a capture subscriber as this thread's default until the
    /// returned guard drops. Info level and above, no ANSI colour, every
    /// field of the event and of its enclosing spans included.
    #[must_use]
    pub fn install() -> (Self, DefaultGuard) {
        let capture = Self::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (capture, guard)
    }

    /// Everything captured so far, as text.
    #[must_use]
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }

    /// Every identifier-shaped value captured so far that is not in
    /// `allow`.
    #[must_use]
    pub fn identifiers(&self, allow: &[&str]) -> Vec<Finding> {
        find_identifiers(&self.output(), allow)
    }

    /// Fail the test if anything identifier-shaped was logged.
    ///
    /// # Panics
    ///
    /// When [`Self::identifiers`] is not empty; the message lists every
    /// finding and the captured output.
    pub fn assert_no_identifiers(&self, allow: &[&str]) {
        let found = self.identifiers(allow);
        assert!(
            found.is_empty(),
            "ADR-0078 section 11: identifier-shaped values in service logs: {found:?}\n--- captured output ---\n{}",
            self.output()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_info_line_with_an_id_is_caught_and_debug_is_not_captured() {
        let (capture, _guard) = Capture::install();
        tracing::info!(rows = 3, "pruned");
        tracing::debug!(id = "00112233445566778899aabbccddeeff", "debug only");
        capture.assert_no_identifiers(&[]);
        tracing::warn!(id = "00112233445566778899aabbccddeeff", "leak");
        assert_eq!(capture.identifiers(&[]).len(), 1);
        assert!(capture.output().contains("leak"));
    }
}
