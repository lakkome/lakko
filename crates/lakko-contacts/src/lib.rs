// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Contacts, discovery, and invitations for Lakko.
//!
//! Implements:
//! - Username discovery via OPRF blinded lookups (RFC 9497)
//! - Capability-based invite links (revocable, rate-limited)
//! - Six invitation scenarios from `docs/adr/0005-contact-invitations.md`
//! - Pseudonym lifecycle (identity / per-relationship / stranger-inbox / per-invite)
//! - QR safety numbers for in-person verification
//! - Six discoverability scopes (Open / ContactsOfContacts / Contacts /
//!   InviteOnly / Allowlist / VerifiedNetwork)
//!
//! The social graph is never materialized server-side; all routing happens
//! through rotating per-relationship pseudonyms.

#![forbid(unsafe_code)]
