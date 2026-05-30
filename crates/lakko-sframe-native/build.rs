// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Build script — runs cbindgen to emit a C header alongside the
//! compiled library. The per-platform native shims (Obj-C++ for iOS,
//! JNI for Android) `#include "lakko_sframe_native.h"` and link
//! `liblakko_sframe_native.a` / `.so`.

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir =
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR must be set by cargo");

    // The header lands next to the compiled artefacts so a
    // downstream `cargo build -p lakko-sframe-native` produces both
    // `liblakko_sframe_native.{a,so,dylib}` and
    // `lakko_sframe_native.h` in known paths. The platform shims
    // pass `--include-path <OUT_DIR>` to their respective compilers.
    let header_path = PathBuf::from(&out_dir).join("lakko_sframe_native.h");

    // `from_crate` picks up `cbindgen.toml` alongside `Cargo.toml`.
    // Failures during header generation are loud — a broken header
    // would mask itself as a "missing symbol" link error later.
    match cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(
            cbindgen::Config::from_file(PathBuf::from(&crate_dir).join("cbindgen.toml"))
                .expect("cbindgen.toml present alongside Cargo.toml"),
        )
        .generate()
    {
        Ok(bindings) => {
            bindings.write_to_file(&header_path);
            println!("cargo:rerun-if-changed=src/lib.rs");
            println!("cargo:rerun-if-changed=cbindgen.toml");
            println!(
                "cargo:warning=lakko-sframe-native: emitted {}",
                header_path.display()
            );
        },
        Err(e) => {
            // Don't `panic!` here — a header-generation failure
            // should not block the Rust unit + integration tests
            // for the C ABI (those exercise the symbols through
            // the workspace-local rlib without ever touching the
            // .h). Emit a `cargo:warning` so the build log
            // surfaces it loud.
            println!("cargo:warning=lakko-sframe-native: cbindgen failed: {e:?}");
        },
    }
}
