// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Crate-local `uniffi-bindgen` binary (ADR-0041 Phase 2).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/bin/uniffi-bindgen.rs`. UniFFI's
//! `cli` feature exposes a stock `uniffi_bindgen_main()` entry point that
//! handles the `generate` / `print-repr` / `scaffolding` /
//! `swift-bridging-header` subcommands. Wrapping it in a crate-local bin
//! (instead of a globally installed `uniffi-bindgen`) guarantees the bindgen
//! and the `lakko-ffi-extension` cdylib were built from the **same** UniFFI
//! version — pinned via the workspace's `Cargo.lock`.
//!
//! Invocation (driven by `just build-ios-extension`):
//!
//! ```text
//! cargo run -p lakko-ffi-extension --bin uniffi-bindgen --release -- \
//!     generate \
//!     --library target/release/liblakko_ffi_extension.dylib \
//!     --language swift \
//!     --out-dir target/bindings-extension/swift
//! ```

fn main() {
    uniffi::uniffi_bindgen_main();
}
