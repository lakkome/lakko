// swift-tools-version: 5.10
// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors


// `apps/ios/Package.swift` — Swift Package wrapping the
// `LakkoCore.xcframework` (Phase 1.14a) into a normal SPM
// library product the SwiftUI app (Phase 1.14c, separate
// `.xcodeproj`) imports as a single Package.swift dependency.
//
// Two targets:
//
//   * `LakkoCoreFFI` (binaryTarget) — the C-ABI symbols
//     `lakko-ffi-uniffi` exposes, packaged in
//     `../../target/ios/LakkoCore.xcframework`. Built by
//     `just build-ios`; gitignored (the workspace-root
//     `target/` rule covers it).
//
//   * `LakkoCore` (target) — the Swift source UniFFI emits
//     into `target/bindings/swift/lakko_ffi_uniffi.swift`,
//     fan'd into `apps/ios/Sources/LakkoCore/` by the
//     `just build-ios` recipe. Depends on `LakkoCoreFFI` for
//     the underlying C symbols.
//
// One product (`LakkoCore` library) re-exports both. The
// SwiftUI app adds this package as a local dependency
// (`File → Add Packages → Add Local…` in Xcode) and then
// imports the public Swift API via `import LakkoCore`.
//
// **Versioning**: this Package.swift lives in-tree alongside
// the iOS app source rather than as a separate repo. Pinning
// is implicit — the app's `Package.swift` references the
// local path; any commit to the workspace updates everything
// atomically.

import PackageDescription

let package = Package(
    name: "LakkoCore",
    platforms: [
        // iOS 17 covers every device the App Store still
        // accepts in 2026 (iPhone XS+, iPad 6th-gen+). The
        // Apple-platform tier raises the floor compared to the
        // Android minSdk 26 (~2017 Android 8.0) on the
        // assumption that the iOS user base lives on more
        // recent hardware on average — well-supported by every
        // measurement of the install-base distribution since
        // 2024.
        .iOS(.v17),
        // macCatalyst is the canonical path to a Mac build
        // from the same SwiftUI source; Phase 1.14 ships the
        // iPhone / iPad surface, but pre-declaring the
        // platform here means the future Mac target (Phase
        // 1.14-mac, deferred) can add UI without churn here.
        .macCatalyst(.v17),
    ],
    products: [
        // Single library product re-exporting the Swift
        // surface. The binary target is wired as an internal
        // dependency; consumers only see `LakkoCore`.
        .library(
            name: "LakkoCore",
            targets: ["LakkoCore"]
        ),
    ],
    targets: [
        // Binary target pointing at the XCFramework `just
        // build-ios` produces. The relative path resolves from
        // `apps/ios/` to the workspace-root `target/`
        // directory; same depth as `apps/android/lakko-core/`
        // resolving to `target/bindings/kotlin/` on the
        // Android side.
        // SPM target name must match the **C module name**
        // declared in the XCFramework's `module.modulemap`
        // (`lakko_ffi_uniffiFFI`) — the Swift compiler ties
        // the binary-target name to the import lookup. The
        // UniFFI-generated bindings (`lakko_ffi_uniffi.swift`)
        // hard-code `import lakko_ffi_uniffiFFI`; aligning the
        // SPM name removes the indirection that otherwise
        // requires either an aliased modulemap inside the
        // XCFramework or a hand-written shim re-import.
        .binaryTarget(
            name: "lakko_ffi_uniffiFFI",
            path: "../../target/ios/LakkoCore.xcframework"
        ),
        // The Swift wrapper. Source lives under
        // `Sources/LakkoCore/`; the `just build-ios` recipe
        // fans `target/bindings/swift/lakko_ffi_uniffi.swift`
        // there on every regen so SPM picks it up
        // automatically as part of the target's source set.
        // Same posture as Android's `regen-bindings` copying
        // the Kotlin tree into
        // `apps/android/lakko-core/src/main/kotlin/uniffi/`.
        .target(
            name: "LakkoCore",
            dependencies: ["lakko_ffi_uniffiFFI"]
        ),
    ]
)
