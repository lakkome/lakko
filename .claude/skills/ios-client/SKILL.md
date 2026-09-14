---
name: ios-client
description: Background knowledge for Lakko iOS/iPadOS/macOS/watchOS client development — Swift/SwiftUI patterns, UniFFI bindings, Xcode build, SFrame Obj-C++ shim, WebRTC pod. Auto-applies when working on apps/ios/.
user-invocable: false
---

# Lakko iOS client — domain knowledge

## Project structure

- `apps/ios/Lakko/` — main app target (SwiftUI)
- `apps/ios/Lakko.xcodeproj/` + `apps/ios/project.yml` — Xcode project (xcodegen)
- `apps/ios/Package.swift` — SPM dependencies
- `apps/ios/LakkoTests/` — XCTest target
- `apps/ios/LakkoNotificationService/` — push notification extension

### Module layout inside `Lakko/`

| Module | Purpose |
|---|---|
| `Auth/` | OPAQUE-PAKE + WebAuthn passkey flows |
| `Backup/` | Encrypted backup UI |
| `Calls/` | WebRTC pod, SFrame Obj-C++ shim, CallSession, SfuCallSession |
| `Channels/` | Channel broadcast UI + RotationCron |
| `Chat/` | ChatScreen, ChatViewModel, ChatRepository |
| `Contacts/` | Contact invite flows |
| `Devices/` | Multi-device pairing, PairingOrchestrator, DeviceGroup journal |
| `DS/` | MLS Delivery Service client |
| `Discovery/` | Username lookup (OPRF) |
| `Push/` | APNs encrypted push |
| `Recovery/` | Password recovery flow |
| `Storage/` | Ciphertext blob proxy |
| `UI/` | Shared UI components |
| `Webauthn/` | WebAuthn ceremony helpers |

## FFI surface

- **UniFFI** bindings from `lakko-ffi-uniffi` — Swift types generated via `just regen-bindings`
- **LakkoCore.xcframework** — fat framework for `aarch64-apple-ios` + `aarch64-apple-ios-sim` + `x86_64-apple-ios`
- Build with `just build-ios` after any Rust crate change
- The xcframework must be rebuilt before `xcodebuild` if the Rust API surface changed

## SFrame native shim

- `apps/ios/Lakko/Calls/SFrameAttach.{h,mm}` — Obj-C++ bridge to `lakko-sframe-native` C ABI
- `apps/shared/sframe-bridge/` — shared C++ bridge layer (consumed by both iOS and Android)
- Static lib `liblakko_sframe_native.a` built via `just cross-build-sframe-native-ios`
- `SFrameAvailability` boot-time probe enforces security invariant #4

## WebRTC pod

- SPM dependency: `stasel/WebRTC` (from: 125.0.0)
- `WebRTCPeerConnection.swift` wraps `RTCPeerConnectionFactory` + `RTCPeerConnection`
- `LocalCameraCapture.swift` wraps `RTCCameraVideoCapturer` (front camera, 720p/30fps)
- `CallSession.swift` — 1x1 P2P call session
- `SfuCallSession.swift` — SFU multi-party call session
- `SfuSignalingClient.swift` — WebSocket signaling over `URLSessionWebSocketTask`

## Build & verify

```bash
just build-ios                       # rebuild xcframework
just cross-build-sframe-native-ios   # rebuild SFrame static lib
# Then in Xcode or:
xcodebuild -project apps/ios/Lakko.xcodeproj -scheme Lakko -destination 'platform=iOS Simulator,name=iPhone 15' build
```

## Key patterns

- `@MainActor` on all UI-driving classes
- `@Published` properties for reactive state (Combine)
- `CallUiState` + `driveFrom(session:)` pattern for call UI binding
- `DeviceGroupJournalEnvelope` for attestation transport (ADR-0015)
- `DeviceAttestKey` with build-time pin via `Info.plist LakkoDeviceAttestKey`
- `SfuConfig` reads SFU URL from `Info.plist LakkoSfuUrl`

## Platform-specific config keys (Info.plist)

| Key | Purpose |
|---|---|
| `LakkoDeviceAttestKey` | Build-time pinned Ed25519 verifying key for device attestation |
| `LakkoApiBaseUrl` | Native API base (`$(LAKKO_API_BASE_URL)`, per configuration in `project.yml`: Debug `https://test.lakko.me/native`, Release empty). `ApiEndpoints` derives every REST client's base (`<base>/auth`, `/ds`, `/storage`, `/discovery`, `/push-gw`, `/channels`, `/gifs`); empty -> `localhost` dev binds. Also in the NSE Info.plist |
| `LakkoSfuUrl` | SFU base URL for group calls (`$(LAKKO_SFU_URL)`: Debug `wss://test.lakko.me/sfu`, Release empty) |
