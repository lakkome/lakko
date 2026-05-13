<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `apps/ios` — Lakko for iOS / iPadOS

**Phase 1.14.** SwiftUI on top of `lakko-ffi-uniffi` Swift
bindings. Mirror of `apps/android` (`auth → contacts → 1×1 chat
→ invites`) on Apple platforms with the same Rust core.

## Status

| Sub-sprint | Scope | Status |
|---|---|---|
| 1.14a | iOS cross-compile + XCFramework build (`just build-ios`) | ✅ |
| 1.14b | `apps/ios/Package.swift` Swift Package wrapping the XCFramework | ✅ |
| 1.14c | SwiftUI app + `apps/ios/project.yml` xcodegen spec + "Hello core" smoke | ✅ |
| 1.14d | OPAQUE-PAKE auth (signup / login / recover) | TODO |
| 1.14e | Contacts (search / send / accept / invite-link) | TODO |
| 1.14f | 1×1 chat UI (timeline + composer + ack indicators) | TODO |

`1.14a` produces the `LakkoCore.xcframework`; `1.14b` wraps it
in a Swift Package; `1.14c` lands the SwiftUI app source +
the xcodegen spec that generates the `Lakko.xcodeproj/` on
first build. The `.xcodeproj/` is **gitignored** as a build
artefact — `project.yml` is the reviewable source of truth.

The dependency arrow is: `lakko-ffi-uniffi` (Rust) →
`just regen-bindings` (Swift source) + `just build-ios` (the
three static-lib slices fused into one XCFramework) → Xcode
project → SwiftUI app.

## What 1.14a ships

`just build-ios` produces, alongside the existing Kotlin output
that `just regen-bindings` already emits:

```
target/ios/
├── headers/
│   ├── lakko_ffi_uniffiFFI.h    — C-ABI symbol surface
│   └── module.modulemap          — exposes the C symbols to Swift
├── liblakko_ffi_uniffi-simulator.a  — fat lipo of arm64 + x86_64 sim slices
└── LakkoCore.xcframework/
    ├── Info.plist
    ├── ios-arm64/                — physical-device slice
    │   ├── headers/
    │   └── liblakko_ffi_uniffi.a
    └── ios-arm64_x86_64-simulator/  — combined-simulator slice
        ├── headers/
        └── liblakko_ffi_uniffi.a
```

The `LakkoCore.xcframework/` directory is the only thing the
Xcode project from 1.14b needs to import — Xcode handles slice
selection automatically based on the active build destination
(physical iPhone vs Simulator).

### Three iOS targets in the matrix

* **`aarch64-apple-ios`** — modern physical iPhones + iPads
  (every device since A7, 2013). Built standalone.
* **`aarch64-apple-ios-sim`** — Apple-Silicon Mac simulator.
* **`x86_64-apple-ios-sim`** — Intel Mac simulator (kept until
  the Intel-Mac install base drops below ~5% — cheap to keep).

32-bit iOS (armv7 / armv7s) is intentionally NOT in the matrix
— Apple dropped support with iOS 11 (2017); every device the
App Store still accepts is 64-bit.

### Why `staticlib`, not `cdylib`

iOS embeds frameworks differently from Android. The codesign +
entitlement story for dynamic libraries on iOS pays off only at
app-update scale (the dynamic lib can be re-shipped without
re-shipping the host app). For a single-app `.xcframework`
consumed by one Xcode target, static linking is the canonical
shape; App Store reviewer friction is zero.

`crates/lakko-ffi-uniffi/Cargo.toml` already declares
`crate-type = ["staticlib", "cdylib", "rlib"]` from 1.15b —
cdylib for Android JNI, **staticlib for the iOS XCFramework**,
rlib for in-workspace consumers.

## Pre-reqs

* **macOS host** — `xcodebuild` doesn't run on Linux/Windows.
* **Xcode Command Line Tools**: `xcode-select --install`. The
  full Xcode app is not required to BUILD the XCFramework —
  only to ship the eventual SwiftUI app via the archive / export
  flows in 1.14b/c.
* **Rust iOS targets**:
  ```bash
  rustup target add aarch64-apple-ios \
                    aarch64-apple-ios-sim \
                    x86_64-apple-ios-sim
  ```

## What 1.14b ships

`apps/ios/Package.swift` declares a Swift Package with a single
library product (`LakkoCore`) backed by two targets:

```
apps/ios/
├── Package.swift                 — SPM manifest
├── .gitignore                    — Xcode + SPM build artefacts
└── Sources/
    └── LakkoCore/
        ├── LakkoCore.swift                 — committed umbrella placeholder
        └── lakko_ffi_uniffi.swift          — fan'd in by `just build-ios`, gitignored
```

The package wires two targets:

* **`LakkoCoreFFI`** — `.binaryTarget` pointing at
  `../../target/ios/LakkoCore.xcframework`. Resolves from
  `apps/ios/` to the workspace-root `target/` (same depth as
  the Android side's `apps/android/lakko-core/` resolving to
  `target/bindings/kotlin/`).
* **`LakkoCore`** — regular Swift `.target` depending on the
  binary target; its source dir is the UniFFI-emitted
  `lakko_ffi_uniffi.swift` plus the committed
  `LakkoCore.swift` umbrella (the latter exists so SPM has a
  source file to find before the first `just build-ios` runs;
  it declares no public symbols itself).

The SwiftUI app from 1.14c adds this package as a local
dependency (`File → Add Packages → Add Local…` in Xcode, point
at `apps/ios/`) and then `import LakkoCore` to access the full
public surface.

### Platforms pinned

```swift
platforms: [
    .iOS(.v17),
    .macCatalyst(.v17),
]
```

iOS 17 covers every device the App Store still accepts in
2026 (iPhone XS+, iPad 6th-gen+). The Apple-platform tier
raises the floor compared to Android `minSdk = 26` (Android
8.0, ~2017) on the assumption that the iOS user base lives on
more recent hardware on average. `macCatalyst` pre-declared so
the future Mac target (Phase 1.14-mac, deferred) can add UI
without churn here.

## What 1.14c ships

`apps/ios/project.yml` is the xcodegen spec the
`just generate-ios-project` recipe drives to produce
`apps/ios/Lakko.xcodeproj/`. The generated `.xcodeproj` is
gitignored (Xcode's project format is a plist-like binary blob
with per-developer path drift; reviewing diffs on it is a
permanent papercut). The YAML is the reviewable source of
truth + regenerates the binary blob deterministically on every
contributor's machine.

```
apps/ios/
├── Package.swift                 — SPM library wrapper (1.14b)
├── project.yml                   — xcodegen spec  ← regenerates .xcodeproj
├── Lakko/                        — committed SwiftUI source
│   ├── LakkoApp.swift            — @main App struct
│   └── HelloCoreView.swift       — Hello-core smoke screen
├── Sources/LakkoCore/            — SPM target source (placeholder + UniFFI fan-out)
├── .gitignore
└── Lakko.xcodeproj/              — gitignored; produced by xcodegen
```

The `Lakko` app target declared in `project.yml`:

* `type: application` / `platform: iOS` / `deploymentTarget: 17.0`
* `dependencies: - package: LakkoCore` — local SPM dependency
  on the `apps/ios/Package.swift` from 1.14b. xcodegen forwards
  this to Xcode as a "local SPM dependency" (same shape Xcode's
  `File → Add Packages → Add Local…` wizard produces; xcodegen
  just generates it from YAML).
* `PRODUCT_BUNDLE_IDENTIFIER: me.lakko.dev` — mirror of the
  Android `applicationId` from 1.15c; keeps debug installs
  separate from a future production `me.lakko` bundle.
* `SWIFT_VERSION: "5.10"` — matches the
  `Package.swift swift-tools-version` from 1.14b.
* Auto-synthesized `Info.plist` from `info.properties` in the
  spec — `CFBundleDisplayName: Lakko`, blank launch screen,
  portrait orientation. No asset catalog in 1.14c — default
  Xcode launcher icon is fine for the smoke build; real
  branding lands when 1.14d's signup screen needs a place to
  surface it.
* Single `Lakko` scheme covers Debug build / Run / Release
  archive.

## Pre-reqs (Phase 1.14c)

In addition to the 1.14a pre-reqs (macOS host + Xcode CLI
tools + Rust iOS targets):

* **xcodegen**:
  ```bash
  brew install xcodegen
  # or:
  mint install yonaskolb/XcodeGen
  ```

## First-run instructions (Phase 1.14c)

```bash
# 1. Regenerate Swift bindings + the XCFramework (1.14a).
#    Fans `target/bindings/swift/lakko_ffi_uniffi.swift` into
#    `apps/ios/Sources/LakkoCore/` so the SPM target picks it
#    up; produces `target/ios/LakkoCore.xcframework`.
just regen-bindings
just build-ios

# 2. Generate the Xcode project from the YAML spec.
just generate-ios-project
# → apps/ios/Lakko.xcodeproj appears.

# 3. Open in Xcode + run on a simulator.
open apps/ios/Lakko.xcodeproj
# In Xcode: select the `Lakko` scheme + an iPhone Simulator
# destination, then ⌘R.
```

When the simulator launches, the app shows:

* `Lakko` title
* `Identity public key` subheading
* The 64-char hex of a freshly-derived
  `Identity("lakko-ios-skeleton")`'s signature public key
* `Phase 1.14c smoke — UniFFI bridge via XCFramework`
  footer

If the hex renders, the XCFramework bridge works on this
device. Every later phase (auth in 1.14d, contacts in 1.14e,
chat in 1.14f) sits on a proven foundation.

CLI equivalent (no Xcode app needed for a build-only smoke):

```bash
cd apps/ios && xcodebuild \
    -project Lakko.xcodeproj \
    -scheme Lakko \
    -destination "platform=iOS Simulator,name=iPhone 15" \
    build
```

## Future source layout (1.14d-f)

```
apps/ios/Lakko/
├── LakkoApp.swift                — @main App struct
├── HelloCoreView.swift           — 1.14c smoke (removed once auth lands)
├── Auth/                         — signup / login / recover (1.14d)
├── Contacts/                     — contacts surface (1.14e)
└── Chat/                         — 1×1 chat surface (1.14f)
```

## Reproducible builds

Pin Xcode + Swift toolchain versions in CI; two clean builds
must produce byte-identical `.ipa` files (mirror of `apps/web`'s
`web-repro` gate from Phase 1.17b + the same posture
documented for Android in 1.15b). Apple's App Store re-signing
on the production publish path breaks byte-identicality on the
shipped `.ipa`; we ship the unsigned XCFramework + the unsigned
app archive as the reproducible artefact + treat the
App-Store-signed `.ipa` as a separate trust root (documented in
the Phase 0 roadmap §11).

`1.14c+` will land a `just package-ios` recipe driving
`xcodebuild archive` + `xcodebuild -exportArchive` for the
ad-hoc / TestFlight distribution flow.

## Bootstrap path (1.14c — SwiftUI skeleton)

After 1.14b ships an installable app shell, 1.14c lands:

```
apps/ios/Sources/
├── LakkoApp.swift               — single @main entry point
├── HelloCore.swift              — smoke surface (mirror of Android's 1.15c)
└── … (auth / contacts / chat land in 1.14d-f)
```

The "Hello core" surface the 1.14c sprint ships will:
1. Build an `Identity("lakko-ios-skeleton")` via the UniFFI-
   generated Swift class.
2. Read `signaturePublicKey()` — 32-byte Ed25519 verifying key.
3. Render the hex on screen. Same surface as
   `apps/web/src/routes/DevProbe.tsx` from 1.16a and the
   Android `MainActivity.HelloCoreScreen` from 1.15c.

## Bootstrap path (1.14d-f — feature parity with `apps/android`)

Each feature sprint mirrors the Android sub-sprint arc 1.15d /
1.15e / 1.15f. The Rust core is identical; only the platform-
specific glue differs:

* **1.14d** — auth: OPAQUE-PAKE wired through UniFFI (the same
  `OpaqueRegister` / `OpaqueLogin` shape both `apps/android` and
  `apps/web` already consume). SwiftUI signup / login / recover
  screens; device-root key in the iOS Keychain (Secure-Enclave-
  backed when present, hardware-AES-backed otherwise — mirrors
  the Android Keystore posture from 1.15d3 but uses the iOS
  primitives).
* **1.14e** — contacts: same `OprfBlinder` + `ContactRequest`
  + `InviteCapability` surface via UniFFI. Compose contacts /
  invite-link UI translates near-mechanically to SwiftUI; the
  StateFlow / `collectAsStateWithLifecycle` pattern maps to
  Swift's `@StateObject` + `ObservableObject`.
* **1.14f** — 1×1 chat: SwiftUI `ScrollView` / `LazyVStack`
  timeline + composer + Material3-ish ack indicators. MLS
  state persistence via the same `Identity.snapshotStorage` /
  `restoreStorage` UniFFI surface 1.15f3 wired, wrapped under
  a Keychain-resident key the way `IdentitySnapshotStore.kt`
  wraps it under Android Keystore.

## First-run instructions (Phase 1.14a)

Pre-reqs above (macOS + Xcode CLI tools + Rust iOS targets).

```bash
# 1. Regenerate Swift bindings — idempotent. The .swift source
#    + the FFI .h header land in target/bindings/swift/.
just regen-bindings

# 2. Cross-compile the staticlib for all three iOS targets +
#    build the XCFramework. ~2 min on an M-series Mac (release
#    profile, OpenMLS + RustCrypto all in).
just build-ios

# 3. Inspect the output.
ls target/ios/LakkoCore.xcframework
# → Info.plist, ios-arm64/, ios-arm64_x86_64-simulator/
```

That's all 1.14a ships. The Xcode project + SwiftUI app are
1.14b / 1.14c.

## Operator notes

* The `.a` artefacts under `target/aarch64-apple-ios{-sim,}/release/`
  + `target/ios/` are build outputs — gitignored via the
  workspace-root `target/` rule.
* `just build-ios` is **macOS-only** (gated via `[macos]`
  recipe attribute in the justfile). On Linux / Windows the
  recipe is silently skipped — same posture as
  Android-toolchain recipes that need `cargo-ndk`.
* `lakko-ffi-uniffi`'s public API is the **stable surface
  contract** with iOS / Android / Tauri-desktop. Changes to it
  require an ADR (mirror of the policy on the wasm-bindgen
  surface for `apps/web` in 1.13a-b and the UniFFI surface for
  `apps/android` in 1.15a).
