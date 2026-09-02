<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0002: Shared Rust core + native UI on every platform

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Phase 0

## Context

Lakko must run on iOS, iPadOS, macOS, watchOS, Android, Windows, Linux and Web with identical crypto, protocol, network and storage behaviour. At the same time, the UX must be native on every platform (CallKit, Live Activities, MediaSession, Compose, SwiftUI), so that quality and responsiveness match Telegram / Zoom.

Any divergence in the crypto / MLS logic between platforms is a risk of a protocol-level bug breaking interop or introducing a security regression.

## Decision

**Shared Rust core (Cargo workspace) + native UI per platform**.

### Core

All crates in `crates/`:

- `lakko-core` (umbrella) re-exports the public API
- `lakko-crypto`, `lakko-mls`, `lakko-proto`, `lakko-transport`, `lakko-storage`, `lakko-sync`, `lakko-calls`, `lakko-contacts`, `lakko-presence`, `lakko-push` — domain crates
- `lakko-ffi-uniffi` — Swift / Kotlin bindings via [UniFFI](https://github.com/mozilla/uniffi-rs)
- `lakko-ffi-wasm` — Web / Tauri bindings via [wasm-bindgen](https://rustwasm.github.io/wasm-bindgen/)

`#![forbid(unsafe_code)]` on all domain crates; unsafe is allowed only inside the FFI crates and must carry an explicit `// SAFETY:` comment.

> **The exception moved (recorded 2026-09-02).** This ADR — and the code-style
> rule in `CLAUDE.md` that restates it — grants the exception to `lakko-ffi-*`
> by *name*. That is no longer where unsafe lives. As of this audit **all three
> `lakko-ffi-*` crates carry `#![forbid(unsafe_code)]`** (`lakko-ffi-uniffi`,
> `lakko-ffi-wasm`, `lakko-ffi-extension`) — UniFFI and wasm-bindgen generate
> their own glue, so the hand-written surface needs none — and the **only**
> crate in the workspace with `#![allow(unsafe_code)]` is
> [`lakko-sframe-native`](../../crates/lakko-sframe-native/src/lib.rs), the flat
> C ABI the iOS Obj-C++ shim and the Android JNI module link against. That crate
> is an FFI crate by function and not by prefix, so a name-based rule silently
> stopped describing the tree.
>
> The intent is unchanged and is what should be read here: **unsafe is confined
> to the FFI boundary, everywhere else forbids it.** Stated as a property rather
> than a prefix, that is still exactly true — and it is now true of a *smaller*
> surface than this ADR assumed, which is the direction to want.

### Per-platform UI

| Platform | UI stack | Binding |
|---|---|---|
| iOS / iPadOS / macOS / watchOS | SwiftUI + platform APIs (CallKit, Live Activities, PushKit, AVFoundation) | XCFramework via UniFFI |
| Android | Jetpack Compose + platform APIs (MediaSession, ConnectionService, FCM) | AAR via UniFFI |
| Windows / macOS / Linux desktop | Tauri 2.x + Solid.js | Native Rust integration in the host process |
| Web | Solid.js + WASM | wasm-bindgen |

Solid.js was picked for both desktop and web because of its fine-grained reactivity (signals) — surgical UI updates without full re-renders, which matches the security-critical "no full-screen reloads" invariant (a well-known phishing / UI-redress vector).

### watchOS — special case

Carrying the full MLS state for many groups is too heavy for a watch. `apps/watchos` will be a thin presenter talking to the phone via a paired secure channel; it will not run MLS itself for groups > 50. Details — separate ADR in Phase 5.

## Alternatives

### Flutter + Rust core

Flutter for all clients + Rust core. Pro: a single UI codebase for iOS / Android / desktop. Cons: compromised native UX, watchOS not supported at all, Live Activities / CallKit / MediaSession all mediated through platform channels.

**Rejected:** the requirement of "Telegram-grade UX" with deep native integration with system services outweighs the savings on UI code.

### React Native + Electron + Web (shared TypeScript)

Maximum reuse of TS code. Cons: JS crypto on the client, Electron overhead, watchOS as a separate codebase, harder security audit (V8 + Node attack surface).

**Rejected:** for a security project, JS crypto is unacceptable, and Electron-based desktop loses on memory footprint and nativeness.

### Fully native (Swift / Kotlin / C++)

Best UX, but duplicating the crypto / protocol logic in 3+ languages is an unacceptable risk of protocol-level divergence and doubles the audit surface.

**Rejected:** a unified core is critical for security; UX is achieved by keeping **the UI** native, not everything.

## Consequences

### Positive

- A single crypto codebase → one audit point and one formal-verification target
- No inter-platform protocol-divergence bugs (single source of truth — `lakko-mls`)
- Native UX on every platform via a per-platform native UI stack
- Tauri instead of Electron → 30× smaller binaries, no extra runtime
- The AI-agent workflow (see CLAUDE.md) maps cleanly onto a Cargo workspace

### Negative

- FFI boundaries (UniFFI, wasm-bindgen) — a surface for bugs that are hard to catch in tests
- Every breaking change to the core public API → regenerate bindings and update every client
- The team needs Rust + Swift / Kotlin / TS expertise simultaneously
- The build pipeline is more complex: XCFramework + AAR + WASM + Tauri target — 4 different artefacts per core release

### Neutral

- watchOS as a thin presenter — a functionality compromise, normal for the smart-watch form factor

## Open questions

- How to version the FFI surface — semver, or a meaningful contract test? (Phase 1 will decide)
- Desktop: is Tauri 2.x mature enough? If we hit a wall, fallback to native Win32 / Cocoa / GTK + Rust business logic. Decide by the end of Phase 2

## References

- [UniFFI — Rust → Swift / Kotlin / Python](https://github.com/mozilla/uniffi-rs)
- [wasm-bindgen](https://rustwasm.github.io/wasm-bindgen/)
- [Tauri 2.0](https://tauri.app/)
- [Solid.js — fine-grained reactivity](https://www.solidjs.com/)
- ADR-0001 — MLS as crypto core
- ADR-0006 — licensing (AGPL affects FFI boundaries)
