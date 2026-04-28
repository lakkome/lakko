// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Blinded presence tokens for Lakko.
//!
//! There is no global "online" / "last seen" indicator — that would leak the
//! social graph. Instead, each user publishes per-conversation, periodically
//! rotated presence tokens. The server cannot correlate that token X and
//! token Y belong to the same user; tokens are authenticated only inside
//! MLS-encrypted envelopes.

#![forbid(unsafe_code)]
