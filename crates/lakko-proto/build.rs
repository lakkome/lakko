// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Compiles `schemas/*.proto` to Rust types via `prost-build`.
//!
//! Uses [`protox`] (pure-Rust protobuf parser) so no `protoc` binary is
//! required on the build host. Generated code lands in `OUT_DIR` and is
//! included from `src/lib.rs`.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schemas_root: PathBuf = manifest_dir().join("../../schemas");

    let proto_files: Vec<PathBuf> = [
        "wire/v1.proto",
        "wire/device_pairing_v1.proto",
        "mls-app/v1.proto",
        "mls-app/devicegroup_v1.proto",
        "mls-app/history_bundle_v1.proto",
        "media/v1.proto",
    ]
    .iter()
    .map(|p| schemas_root.join(p))
    .collect();

    // Re-run when any of our protos changes.
    println!("cargo:rerun-if-changed={}", schemas_root.display());
    for p in &proto_files {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let file_descriptors = protox::compile(&proto_files, [&schemas_root])?;

    prost_build::Config::new().compile_fds(file_descriptors)?;

    Ok(())
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    )
}
