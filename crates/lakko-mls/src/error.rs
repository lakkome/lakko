// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-mls`.

use thiserror::Error;

use crate::refusal::RefusalReason;

/// Errors produced by Lakko's MLS wrapper.
///
/// Variants are deliberately coarse: callers should not branch on internal
/// OpenMLS failure detail. Where additional context is useful for logs, a
/// short string is captured.
///
/// The one exception is [`Error::MembershipRefused`], which carries a typed
/// [`RefusalReason`] **in addition to** its message, because a client has to
/// select fixed user-facing copy from it. Read it with
/// [`Error::refusal_reason`] — never by inspecting the message text.
#[derive(Debug, Error)]
pub enum Error {
    /// Identity creation, key generation, or credential setup failed.
    #[error("identity / key generation failed: {0}")]
    Identity(String),

    /// KeyPackage creation or validation failed.
    #[error("key package operation failed: {0}")]
    KeyPackage(String),

    /// Group creation, joining, or commit processing failed.
    #[error("group operation failed: {0}")]
    Group(String),

    /// Application-message creation or processing failed.
    #[error("application message failed: {0}")]
    Message(String),

    /// MLS exporter failed.
    #[error("exporter failed: {0}")]
    Exporter(String),

    /// Wire-format (de)serialization failed.
    #[error("tls codec error: {0}")]
    Codec(String),

    /// Received an unsupported / unexpected message kind.
    #[error("unsupported message: {0}")]
    Unsupported(&'static str),

    /// A group-membership operation was refused by the ADR-0022 role gate or
    /// one of ADR-0045's proposal gates.
    ///
    /// Carved out of [`Error::Group`] for one reason: a client has to turn this
    /// into fixed user-facing copy, and the only way to do that from
    /// `Group(String)` was to substring-match the sentence — the drift class
    /// ADR-0044 was written about, and one that had already produced three
    /// different behaviours for one intended string across three clients.
    ///
    /// `reason` is the contract. `detail` is the same sentence the refusal
    /// always carried and is **only** for logs; it is the whole `Display`
    /// output, so nothing about a log line changes.
    #[error("{detail}")]
    MembershipRefused {
        /// Why the operation was refused, machine-readably.
        reason: RefusalReason,
        /// The human-readable sentence, for logs and stacks.
        detail: String,
    },

    /// This device still holds `group_id` as active: a Welcome into it was
    /// refused, or its local state was not deleted. Nothing changed. The group
    /// id is read from the Welcome's verified `GroupContext`, so the caller
    /// knows which journal to read before it tries again
    /// ([`RefusalReason::GroupStillActive`], C-88).
    #[error("this device still holds group {} as active", group_id_hex(.group_id))]
    GroupStillActive {
        /// The group the refusal is about.
        group_id: Vec<u8>,
    },

    /// A DeviceGroup `FolderChanges` or `StateSnapshot` payload broke an
    /// ADR-0058 section 4 bound and was refused whole (never truncated).
    #[error("device-group payload out of bounds: {0}")]
    PayloadBounds(lakko_sync::synced::BoundsError),
}

impl Error {
    /// Build a [`Error::MembershipRefused`].
    ///
    /// `pub(crate)` on purpose: a refusal reason is a statement this crate's
    /// gates make about a group's own rules, not something a caller may mint.
    pub(crate) fn refused(reason: RefusalReason, detail: impl Into<String>) -> Self {
        Self::MembershipRefused {
            reason,
            detail: detail.into(),
        }
    }

    /// Mint a refusal directly, for tests **outside** this crate.
    ///
    /// The FFI crates have to pin that every [`RefusalReason`] survives their
    /// boundary, and reconstructing sixteen group states to reach sixteen gates
    /// would test the gates over again rather than the projection. This exists
    /// so that assertion can be written against the mapping itself.
    ///
    /// Not part of the supported API — same posture as
    /// [`crate::fuzz_api`]. Production code inside this crate uses the private
    /// `refused`, because a refusal reason is a statement a gate makes about a
    /// group's own rules, not something a caller may assert.
    #[doc(hidden)]
    #[must_use]
    pub fn refused_for_tests(reason: RefusalReason, detail: &str) -> Self {
        Self::refused(reason, detail)
    }

    /// The machine-readable reason this error refused a membership operation,
    /// or `None` when it is not a membership refusal at all.
    ///
    /// **This is the only supported way to discriminate a refusal.** Do not
    /// match on [`Display`](core::fmt::Display) output: the sentences here are
    /// written for a developer reading a log and are reworded freely, whereas
    /// the reason is stable. A `None` is meaningful rather than a gap — it says
    /// "this failure is not a permission answer" (a codec failure, an OpenMLS
    /// validation failure, an argument that was the wrong length), which a
    /// client renders with its fail-closed fallback string.
    #[must_use]
    pub const fn refusal_reason(&self) -> Option<RefusalReason> {
        match self {
            Self::MembershipRefused { reason, .. } => Some(*reason),
            Self::GroupStillActive { .. } => Some(RefusalReason::GroupStillActive),
            _ => None,
        }
    }
}

/// Lower-case hex, for the one sentence that names a group.
#[must_use]
pub fn group_id_hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            // Writing to a `String` cannot fail.
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
