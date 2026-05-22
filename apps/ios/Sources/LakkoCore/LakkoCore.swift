// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// `LakkoCore.swift` — umbrella source for the SPM target.
//
// The bulk of the public Swift surface lives in
// `lakko_ffi_uniffi.swift`, fan'd into this directory by
// `just build-ios` on every regen. That file is gitignored
// (build artefact; the source of truth is
// `crates/lakko-ffi-uniffi/`'s `#[uniffi::export]`
// annotations). SPM expects at least one source file in the
// target's directory to declare it a valid build target, so
// this committed placeholder fills the role + leaves the door
// open for hand-written Swift convenience extensions on top of
// the UniFFI-generated API (`Identity` ergonomic wrappers,
// `Group` async / await sugar, etc.) when the SwiftUI app
// asks for them in 1.14c+.
//
// Until that day: this file declares no public symbols. The
// `LakkoCore` library product still exports the full UniFFI
// surface because `lakko_ffi_uniffi.swift` is part of the same
// target's source set once `just build-ios` lays it down.

import Foundation

// Intentionally empty. The UniFFI-generated source
// (`lakko_ffi_uniffi.swift`, fan'd in by `just build-ios`)
// carries every public symbol and does its own
// `import lakko_ffi_uniffiFFI` against the C module name
// the XCFramework modulemap declares. This file just
// satisfies SPM's "target dir must have at least one
// committed source" requirement.
