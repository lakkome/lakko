---
name: android-client
description: Background knowledge for Lakko Android client development — Kotlin/Compose patterns, UniFFI bindings, Gradle build, SFrame JNI module, WebRTC pod. Auto-applies when working on apps/android/.
user-invocable: false
---

# Lakko Android client — domain knowledge

## Project structure

- `apps/android/app/` — main app module (Jetpack Compose)
- `apps/android/lakko-core/` — Gradle module wrapping the UniFFI-generated Kotlin bindings
- `apps/android/lakko-sframe/` — Gradle module wrapping the SFrame JNI native lib
- `apps/android/build.gradle.kts` — root build script
- `apps/android/gradle/libs.versions.toml` — version catalog

### Package layout inside `app/src/main/kotlin/me/lakko/app/`

| Package | Purpose |
|---|---|
| `auth/` | OPAQUE-PAKE + WebAuthn passkey flows |
| `backup/` | Encrypted backup UI |
| `calls/` | WebRTC pod, SFrame JNI, CallSession, SfuCallSession |
| `channels/` | Channel broadcast UI + RotationCron |
| `chat/` | ChatScreen, ChatViewModel, ChatRepository |
| `contacts/` | Contact invite flows |
| `devices/` | Multi-device pairing, PairingOrchestrator, DeviceGroup journal |
| `ds/` | MLS Delivery Service client |
| `discovery/` | Username lookup (OPRF) |
| `push/` | FCM / UnifiedPush encrypted push |
| `recovery/` | Password recovery flow |
| `storage/` | Ciphertext blob proxy |
| `ui/` | Shared Compose UI components |
| `webauthn/` | WebAuthn ceremony helpers |

## FFI surface

- **UniFFI** bindings from `lakko-ffi-uniffi` — Kotlin types generated via `just regen-bindings`
- `apps/android/lakko-core/` wraps the generated `.kt` files + the native `.so`
- Build with `just build-android` after any Rust crate change
- Cross-compile via `cargo-ndk` for `aarch64-linux-android`, `armv7-linux-androideabi`, `x86_64-linux-android`

## SFrame JNI module

- `apps/android/lakko-sframe/` — Gradle module with JNI bridge
- Links `liblakko_sframe_native.a` (static) built via `just cross-build-sframe-native-android`
- `apps/shared/sframe-bridge/` — shared C++ bridge layer (same as iOS)
- `SFrameAvailability` boot-time probe enforces security invariant #4

## WebRTC pod

- Gradle dependency: `io.github.webrtc-sdk:android` (pinned `125.6422.07`)
- `WebRtcPeerConnection.kt` wraps `PeerConnectionFactory` + `PeerConnection`
- `CameraVideoCapture.kt` wraps `Camera2Enumerator` + `CameraVideoCapturer` (front camera, 720p/30fps)
- `CallSession.kt` — 1x1 P2P call session (coroutine-based, `suspendCancellableCoroutine` around SdpObserver)
- `SfuCallSession.kt` — SFU multi-party call session
- `SfuSignalingClient.kt` — WebSocket signaling over Ktor WebSockets (OkHttp engine)
- Permissions: `RECORD_AUDIO` + `MODIFY_AUDIO_SETTINGS` + `CAMERA` in AndroidManifest.xml

## Build & verify

```bash
just build-android                       # rebuild native libs
just cross-build-sframe-native-android   # rebuild SFrame .so per ABI
# Then:
cd apps/android && ./gradlew :app:compileDebugKotlin
# or full build:
cd apps/android && ./gradlew :app:assembleDebug
```

## Key patterns

- `@Composable` functions for all UI (Jetpack Compose)
- `StateFlow<T>` for reactive state (coroutines)
- `viewModelScope` for coroutine scoping in ViewModels
- `CallUiState` + `driveFrom(session)` / `rebuildFromSession()` for call UI binding
- `DeviceGroupJournalEnvelope` for attestation transport (ADR-0015)
- `DeviceAttestKey` with build-time pin via `BuildConfig.LAKKO_DEVICE_ATTEST_KEY`
- `SfuConfig` reads SFU URL from `BuildConfig.LAKKO_SFU_URL`

## Platform-specific config keys (build.gradle.kts buildConfigField)

| Key | Purpose |
|---|---|
| `LAKKO_DEVICE_ATTEST_KEY` | Build-time pinned Ed25519 verifying key (gradle property `lakko.device.attest.key`) |
| `LAKKO_API_BASE_URL` | Native API base (gradle property `lakko.api.url`; debug default `https://test.lakko.me/native`, release empty). `config/ApiEndpoints` derives every REST client's `DEFAULT_BASE_URL` (`<base>/auth`, `/ds`, ...); empty -> `http://10.0.2.2:<port>` dev binds (cleartext allowed for those hosts by the debug-only network security config) |
| `LAKKO_SFU_URL` | SFU base URL for group calls (gradle property `lakko.sfu.url`; debug default `wss://test.lakko.me/sfu`, release empty) |

## Dependencies to watch

- Ktor WebSockets (OkHttp engine) — `libs.ktor.client.websockets` is a production dep (promoted from androidTest for SFU signaling)
- `suspendCancellableCoroutine` wrappers around libwebrtc callbacks run on libwebrtc threads — hop to main via the CoroutineScope
