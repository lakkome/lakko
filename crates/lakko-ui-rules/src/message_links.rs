// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which parts of a message's text are links (`docs/ux/message-links.md`).
//!
//! The rule is defined once, in `lakko_attachments::message_links`, so the
//! ADR-0090 article codec checks link URLs with the same rule. Re-exported
//! here so every caller of this crate keeps its path.

pub use lakko_attachments::message_links::{
    is_whole_link, split_message_links, MessageTextPiece, MAX_LINK_BYTES,
};
