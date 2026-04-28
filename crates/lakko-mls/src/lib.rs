// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS (RFC 9420) wrapper for Lakko.
//!
//! Wraps OpenMLS to fix the supported ciphersuites, isolate upstream API churn
//! and expose Lakko-specific abstractions (DeviceGroup, conversation groups,
//! channel PublisherGroup, exporter helpers for media keys).
//!
//! See `docs/adr/0001-mls-only.md`.

#![forbid(unsafe_code)]
