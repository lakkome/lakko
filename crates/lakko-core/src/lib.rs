// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko umbrella crate.
//!
//! Re-exports the public API surface of Lakko core. Application code (clients,
//! services, FFI shims) should depend on `lakko-core` rather than reaching into
//! individual sub-crates.
//!
//! See `docs/adr/0002-rust-core.md` for the motivation behind this layering.

#![forbid(unsafe_code)]
