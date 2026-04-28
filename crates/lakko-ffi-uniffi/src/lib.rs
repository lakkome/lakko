// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI bindings for Swift (iOS / iPadOS / macOS / watchOS) and Kotlin (Android).
//!
//! The `.udl` definition lives alongside this crate (added when the public
//! API surface stabilizes). Phase 0 leaves this crate empty so platform apps
//! can wire up the build pipeline (XCFramework / AAR generation) before the
//! API is ready.

#![forbid(unsafe_code)]
