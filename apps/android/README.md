<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# `apps/android` — Lakko for Android

**Phase 1.15.** Compose UI on top of `lakko-ffi-uniffi` Kotlin bindings.
Mirror of `apps/web` end-to-end (`auth → contacts → chat → invite-link`)
on JVM with the same Rust core.

## Status

| Sub-sprint | Scope | Status |
|---|---|---|
| 1.15a | Kotlin bindings generation pipeline (`just regen-bindings`) | ✅ |
| 1.15b | Android NDK cross-compile + Gradle scaffold (AAR build) | ✅ |
| 1.15c | Compose project skeleton + "Hello core" smoke screen | ✅ |
| 1.15d | OPAQUE-PAKE auth (signup / login / recover) | TODO |
| 1.15e | Contacts (search / send / accept / invite-link) | TODO |
| 1.15f | 1×1 chat UI (timeline + composer + ack indicators) | TODO |

`1.15a` produces the Kotlin source that the upcoming Gradle module
will consume. **No actual Android Studio project lives here yet** —
that's `1.15c`.

## What 1.15a ships

`just regen-bindings` produces, alongside the existing Swift output,
a Kotlin tree under `target/bindings/kotlin/uniffi/lakko_ffi_uniffi/`:

```
target/bindings/kotlin/
└── uniffi/
    └── lakko_ffi_uniffi/
        └── lakko_ffi_uniffi.kt          ~2200 lines, JNA-based
```

The `.kt` carries the full Phase-0 MLS surface (`Identity`, `Group`,
`AddMember`, `ProcessedMessage` sealed class, `FfiException` sealed
hierarchy). It compiles against `net.java.dev.jna:jna` 5.x —
Android's NDK ships the loader; the Kotlin code calls into the
`lakko_ffi_uniffi` cdylib via JNA at runtime.

The bindgen runs through a **crate-local** `uniffi-bindgen` bin
(see `crates/lakko-ffi-uniffi/src/bin/uniffi-bindgen.rs`) compiled
from the same workspace `Cargo.lock` as the cdylib. That's the
only reliable way to keep the generator and the runtime on the
same UniFFI version — a globally-installed `cargo install
uniffi-bindgen` would float independently and silently drift on a
UniFFI minor release.

### Regenerating

```bash
just regen-bindings   # rebuilds cdylib + emits both Swift and Kotlin
```

`os()` in the recipe picks the correct dynamic-library extension
(`.so` / `.dylib` / `.dll`), so the recipe runs on any host.

## Bootstrap path forward (1.15b — Android NDK + AAR)

The Kotlin file alone is not enough — Android needs the cdylib
cross-compiled to the four ABIs Google still requires for store
publish (`armeabi-v7a`, `arm64-v8a`, `x86`, `x86_64`). Plan:

1. **Install Android NDK r26+** (or whatever is current at the
   time):

   ```bash
   sdkmanager "ndk;26.1.10909125"
   export ANDROID_NDK_HOME=$HOME/Library/Android/sdk/ndk/26.1.10909125
   ```

2. **Add Rust toolchains** for the four targets:

   ```bash
   rustup target add aarch64-linux-android armv7-linux-androideabi \
                     i686-linux-android x86_64-linux-android
   ```

3. **`cargo-ndk`** to drive the cross-compile without hand-rolling
   `RUSTFLAGS` / `CC=` for each target:

   ```bash
   cargo install cargo-ndk
   cargo ndk -t arm64-v8a -t armeabi-v7a -t x86 -t x86_64 \
       -o jniLibs build --release -p lakko-ffi-uniffi
   ```

   Output: `jniLibs/{arm64-v8a,armeabi-v7a,x86,x86_64}/liblakko_ffi_uniffi.so`
   — the layout the Gradle Android plugin expects.

4. **Gradle module** under `apps/android/lakko-core/` that:
   - Pins `kotlin = "2.0.0"`, `agp = "8.5.0"`-class.
   - Adds `net.java.dev.jna:jna:5.13.0@aar`.
   - Sources the `target/bindings/kotlin/uniffi/...` tree as
     `src/main/kotlin`.
   - Sources the `jniLibs/` tree as `src/main/jniLibs`.
   - Publishes an AAR consumed by `apps/android/app`.

5. **Reproducible builds** — pin `cargo-ndk` version, NDK version,
   Gradle wrapper, `kotlin` version, and `agp` version in `Cargo.toml`'s
   workspace metadata + `gradle/wrapper/gradle-wrapper.properties`.
   Two clean builds must produce byte-identical AARs (mirror of
   `apps/web`'s `web-repro` CI gate from Phase 1.17b).

The `1.15b` sprint will land a `just build-android` recipe and a
GH Actions workflow (`build-android.yml`) that drives the full
pipeline, gated on `workflow_dispatch` initially (NDK download is
heavy — pre-warming through CI cache).

## Bootstrap path (1.15c — Compose skeleton)

After 1.15b ships an installable AAR, 1.15c lands the Compose UI
shell:

```
apps/android/
├── app/                              # the installable APK
│   ├── build.gradle.kts
│   └── src/main/
│       ├── kotlin/me/lakko/app/
│       │   ├── MainActivity.kt
│       │   ├── ui/                   # Compose screens
│       │   └── data/                 # Room / DataStore
│       └── res/                      # icons, themes, strings
├── lakko-core/                       # AAR module from 1.15b
│   └── build.gradle.kts
├── settings.gradle.kts
├── build.gradle.kts                  # root
├── gradle.properties
└── gradle/wrapper/
```

The "Hello core" smoke screen the 1.15c sprint ships will
construct an `Identity`, derive a signature pubkey, and render the
hex on screen. Same surface as `apps/web/src/routes/DevProbe.tsx`
from 1.16a.

## Bootstrap path (1.15d-f — feature parity with `apps/web`)

Each feature sprint mirrors the web's `1.16c-e` arc. The Rust core
is identical; only the platform-specific glue differs:

* **1.15d** — auth: OPAQUE-PAKE wired through UniFFI (the same
  `OpaqueRegister` / `OpaqueLogin` shape `apps/web` consumes —
  just needs UniFFI exposure beyond the current MLS-only Phase-0
  surface). Compose form + Android Keystore for the device-root
  key (StrongBox-backed when available, hardware-AES otherwise).
* **1.15e** — contacts: search, ContactRequest send / accept,
  invite-link generate / consume, all driven through the
  `lakko-contacts` UniFFI extension that 1.15d enables.
* **1.15f** — 1×1 chat: timeline (LazyColumn) + composer +
  Material3 ack indicators. MLS state persistence via Android's
  EncryptedFile under the device root key.

## First-run instructions (Phase 1.15c)

Pre-reqs from 1.15b: JDK 17/21, Android SDK + NDK, `cargo-ndk`,
Rust Android targets — see the install notes in
`docs/ROADMAP.md` 1.15b entry.

Build + install on a device or emulator:

```bash
# 1. Regenerate Kotlin bindings (idempotent; needs to run once
#    after a fresh checkout — the generated tree is gitignored).
just regen-bindings

# 2. Cross-compile the Rust cdylib for both supported ABIs and
#    drop the .so files into lakko-core/src/main/jniLibs/.
just build-android

# 3. Open `apps/android/` in Android Studio. The first launch
#    populates `local.properties` with the SDK path (auto). The
#    Gradle sync resolves AGP 8.6.1, Kotlin 2.0.21, the Compose
#    BOM 2024.10.x + JNA — single-digit-MB download on a warm
#    Android Studio install.
open -a "Android Studio" apps/android

# 4. From Android Studio: Run → "app" → pick a device (physical
#    via USB / wireless ADB / emulator). The Hello-core screen
#    appears with the hex-encoded Ed25519 verifying key of a
#    fresh `Identity("lakko-android-skeleton")` — if you see hex,
#    the JNA cdylib bridge is alive on that device.
```

CLI equivalent (no Android Studio needed for the build, only
for the AVD / device picker):

```bash
cd apps/android && ./gradlew :app:assembleDebug
# → app/build/outputs/apk/debug/app-debug.apk
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n me.lakko.dev/me.lakko.app.MainActivity
```

The Gradle wrapper (`gradlew`) is created on the first Android
Studio open; if you want to skip the IDE entirely run
`gradle wrapper --gradle-version 8.10` from `apps/android/` once
before the `gradlew` invocation above.

## Operator notes

* The bindings live in `target/bindings/kotlin/` — that's a build
  artefact, not source. **Do NOT commit it.** The `.gitignore` at
  the workspace root already covers `target/`.

* When CI runs `just regen-bindings` (Phase 1.15b adds it to a
  manual-dispatch workflow), the diff against the previous run
  surfaces wire-format changes that need a coordinated client +
  server release. A non-empty diff that wasn't expected indicates
  a UniFFI surface change someone forgot to call out in the PR.

* `lakko-ffi-uniffi`'s public API is the **stable surface contract**
  with iOS / Android / Tauri-desktop. Changes to it require an ADR
  (mirror of the policy on the wasm-bindgen surface for `apps/web`
  in 1.13a-b).

## Push notifications (Phase 1.20d)

We do **not** apply the `com.google.gms:google-services` Gradle
plugin — that would require checking in a real
`google-services.json` at build time. Instead Firebase is
initialised programmatically at runtime via
`FirebaseOptions.Builder()` populated from four `BuildConfig`
string fields generated from Gradle properties:

```bash
# In `apps/android/local.properties` (gitignored), or pass via
# `-P` flags on each Gradle invocation.
lakko.fcm.projectId=lakko-dev-12345
lakko.fcm.applicationId=1:1234567890:android:abcdef0123456789
lakko.fcm.apiKey=AIzaSyExampleExampleExampleExampleExample
lakko.fcm.senderId=1234567890
```

The four values come from a Firebase project's **Project Settings
→ General → Your apps** page (Android section). Create a Firebase
project + register an Android app with package name `me.lakko.dev`
to get them.

When all four fields are **empty** (the default), `PushSubscriber`
short-circuits and returns `null`. The rest of the app keeps
working without push — the integration code is in place, the
actual registration just doesn't run. This is the right posture
for any contributor not actively iterating on push.
