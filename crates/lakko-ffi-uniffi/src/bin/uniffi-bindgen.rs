// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Crate-local `uniffi-bindgen` binary — Phase 1.15a.
//!
//! UniFFI's `cli` feature exposes a stock `uniffi_bindgen_main()`
//! entry point that handles the `generate` / `print-repr` /
//! `scaffolding` / `swift-bridging-header` subcommands. Wrapping
//! it in a crate-local bin (instead of relying on a globally
//! installed `uniffi-bindgen`) guarantees the bindgen and the
//! `lakko-ffi-uniffi` cdylib were built from the **same**
//! UniFFI version — pinned via the workspace's
//! `Cargo.lock`. A floating `cargo install uniffi-bindgen`
//! couldn't promise that and would silently drift on the day a
//! UniFFI minor release lands.
//!
//! Invocation (driven by `just regen-bindings`):
//!
//! ```text
//! cargo run -p lakko-ffi-uniffi --bin uniffi-bindgen --release -- \
//!     generate \
//!     --library target/release/liblakko_ffi_uniffi.{so,dylib,dll} \
//!     --language kotlin \
//!     --out-dir target/bindings/kotlin
//! ```

fn main() {
    uniffi::uniffi_bindgen_main();
}
