// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko vendored shim for `fluvio-wasm-timer` (G-25).
//!
//! The real `fluvio-wasm-timer 0.2.5` pulls `parking_lot 0.11 -> parking_lot_core
//! 0.8 -> instant` — the unmaintained `instant` crate (RUSTSEC-2024-0384, a
//! non-CVE unmaintained advisory). In the Lakko dependency graph the SOLE
//! consumer of `fluvio-wasm-timer` is `openmls 0.8.1`, which uses it in exactly
//! one place (`src/key_packages/lifetime.rs`, `#[cfg(target_arch = "wasm32")]`)
//! for `SystemTime` + `UNIX_EPOCH` — KeyPackage lifetime validation on the wasm
//! web client, where `std::time::SystemTime::now()` panics (no OS clock).
//!
//! This crate is **not** a fork of `fluvio-wasm-timer`'s source. It is a tiny
//! re-export of the maintained, `std`-compatible [`web-time`](https://docs.rs/web-time)
//! in its place — the same `SystemTime` / `UNIX_EPOCH` surface openmls imports —
//! wired in via `[patch.crates-io]` in the root `Cargo.toml`. The effect is that
//! `instant` leaves the dependency graph entirely (`cargo tree -i instant` is
//! empty on every target, incl. `wasm32`) and `cargo audit` is clean.
//!
//! Remove this shim (and the `[patch.crates-io]` entry) once `openmls` itself
//! moves off `fluvio-wasm-timer` (openmls 0.9+ swaps it for `web-time`) — that
//! is an MLS-core bump requiring its own ADR + property tests.

#![forbid(unsafe_code)]

pub use web_time::{SystemTime, UNIX_EPOCH};
