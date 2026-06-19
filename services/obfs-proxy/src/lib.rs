// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-svc-obfs-proxy` library surface (ADR-0026, Phase 1).
//!
//! The binary (`main.rs`) is a thin runner; the relay + config logic lives here
//! so integration tests (`tests/`) can drive the relay end-to-end against a real
//! obfs listener.

pub mod config;
pub mod relay;
