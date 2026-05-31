// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/*
 * `app` — installable Android APK (Phase 1.15c).
 *
 * Compose UI on top of the `lakko-core` AAR (1.15b). For 1.15c
 * the only screen is "Hello core" — a smoke surface that
 * constructs a fresh `Identity` via the UniFFI-generated
 * Kotlin bindings, reads `signaturePublicKey()`, renders the
 * hex. Same shape as `apps/web`'s 1.16a `DevProbe.tsx`:
 * proves the JNA cdylib bridge works on a real device before
 * 1.15d builds the OPAQUE-PAKE auth surface on top.
 *
 * No app icon override (default mipmap from AGP), no
 * launcher splash, no localisation — all polish lands later
 * once the real flows take shape.
 */

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    // 1.15d2 — kotlinx-serialization compiler plugin. Generates
    // the `Serializer` implementations for the `@Serializable`
    // JSON data classes under `me.lakko.app.auth.wire`.
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "me.lakko.app"

    compileSdk = libs.versions.android.compile.sdk.get().toInt()

    defaultConfig {
        // `me.lakko.dev` keeps debug installs separate from a
        // future production `me.lakko` package on the same
        // device — handy when smoke-testing alongside a
        // released build. Override per-buildType when prod
        // signing lands in a later sprint.
        applicationId = "me.lakko.dev"

        minSdk    = libs.versions.android.min.sdk.get().toInt()
        targetSdk = libs.versions.android.target.sdk.get().toInt()

        versionCode = 1
        versionName = "0.1.0-dev"

        // Same ABI matrix as `lakko-core` — keep both in
        // lock-step so the APK packager never asks for a .so
        // we didn't ship.
        ndk {
            abiFilters += setOf("arm64-v8a", "x86_64")
        }

        // 1.20d — Firebase Messaging configuration. We deliberately
        // do NOT apply the `com.google.gms:google-services` Gradle
        // plugin (which would require a checked-in
        // `google-services.json` at build time). Instead Firebase
        // is initialised programmatically at runtime via
        // `FirebaseOptions.Builder()` populated from these
        // BuildConfig fields. If every field is empty the runtime
        // init is skipped + the PushSubscriber returns null — the
        // app keeps working without push (right posture for any
        // contributor not actively iterating on push).
        //
        // Source the values from gradle properties (set via
        // `apps/android/local.properties` or `-P` flags) so they
        // never end up in version control. The `?:` fallbacks
        // produce empty strings when unset.
        buildConfigField(
            "String",
            "FCM_PROJECT_ID",
            "\"${project.findProperty("lakko.fcm.projectId") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "FCM_APPLICATION_ID",
            "\"${project.findProperty("lakko.fcm.applicationId") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "FCM_API_KEY",
            "\"${project.findProperty("lakko.fcm.apiKey") as String? ?: ""}\"",
        )
        buildConfigField(
            "String",
            "FCM_SENDER_ID",
            "\"${project.findProperty("lakko.fcm.senderId") as String? ?: ""}\"",
        )

        // G3-4-mobile — wire androidx.test runner for the
        // `app/src/androidTest/` instrumented test source set.
        // `./gradlew :app:connectedDebugAndroidTest` boots a
        // device / emulator + installs `app-debug-androidTest.apk`
        // + invokes AndroidJUnitRunner against the @Test methods
        // there. Without this line the test APK packages but
        // never starts.
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
        // 1.20d — re-enabled so the four `FCM_*` BuildConfig
        // string fields from `defaultConfig` make it onto the
        // runtime classpath. PushSubscriber reads them at app
        // startup to decide whether to initialise Firebase
        // programmatically.
        buildConfig = true
    }

    buildTypes {
        release {
            // Smoke build only — release shrinking is wired
            // properly in 1.15d when there's enough code to
            // shrink. The lakko-core consumer-rules.pro keeps
            // UniFFI / JNA from being renamed when shrinking
            // does land.
            isMinifyEnabled = false
        }
    }

    packaging {
        // JNA ships its own `libjnidispatch.so` per ABI inside
        // the `@aar` artefact. AGP merges that with our Rust
        // `.so` from lakko-core; no extra resources to pick.
        // The `pickFirsts` line below would handle conflicts
        // if any other dep shipped a colliding .so name —
        // left commented as a hint, not enabled today.
        // jniLibs {
        //     pickFirsts += listOf("**/libc++_shared.so")
        // }
    }
}

dependencies {
    // The local AAR — Rust cdylib + UniFFI Kotlin bindings.
    implementation(project(":lakko-core"))

    // G3-sframe-native N-5 — local AAR with the SFrame JNI
    // bridge. Bundles libsframe_jni.so (one per shipped ABI)
    // plus the LakkoFrameEncryptor / Decryptor Kotlin classes
    // that org.webrtc.RtpSender.setFrameEncryptor accepts.
    implementation(project(":lakko-sframe"))

    // Compose stack via BOM — one version pin covers every
    // androidx.compose.* artefact.
    val composeBom = platform(libs.compose.bom)
    implementation(composeBom)
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.material3)
    // Extended Compose icon set — base material3 ships only a
    // tiny glyph table; the in-call UI (3c6-android) needs Mic /
    // Videocam / CallEnd / ScreenShare / PanTool / PushPin /
    // FlipCameraAndroid / Group icons.
    implementation(libs.compose.material.icons.extended)
    // Google Material XML library — for the
    // `Theme.Material3.DayNight.NoActionBar` style the
    // Activity's manifest theme inherits. Separate from
    // `compose.material3` which is the Compose UI toolkit.
    implementation(libs.material.android)
    implementation(libs.activity.compose)

    // Compose tooling (Layout Inspector previews etc.) — debug
    // only so they don't bloat release APKs.
    debugImplementation(libs.compose.ui.tooling)
    implementation(libs.compose.ui.tooling.preview)

    // 1.15d4 — ViewModel + StateFlow ↔ Compose glue.
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.lifecycle.runtime.compose)

    // 1.15d2 — networking + JSON for the OPAQUE auth client.
    //
    // Ktor's `client-core` is the suspending HTTP surface;
    // `client-okhttp` is the engine that actually opens
    // sockets on Android (production-grade HTTP/2 + connection
    // pooling); `client-content-negotiation` is the plug that
    // auto-serializes / -deserializes JSON request bodies; the
    // `serialization-kotlinx-json` artefact is the codec
    // implementation that plug uses.
    implementation(libs.ktor.client.core)
    implementation(libs.ktor.client.okhttp)
    implementation(libs.ktor.client.content.negotiation)
    implementation(libs.ktor.serialization.kotlinx.json)

    // kotlinx-coroutines on Android. Ktor pulls -core in
    // transitively, but the -android dispatcher (`Dispatchers.Main`
    // backed by Looper) is Android-specific and needs to be
    // declared explicitly. Coroutines drive both Ktor's
    // suspending HTTP and our `Dispatchers.Default` PoW solve.
    implementation(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.coroutines.android)

    // kotlinx-serialization runtime — required by the `@Serializable`
    // data classes (Ktor pulls the JSON-side via
    // `serialization-kotlinx-json`, but the core annotation
    // package needs to be on the runtime classpath of any
    // module that declares `@Serializable` types).
    implementation(libs.kotlinx.serialization.json)

    // 1.20d — Firebase Messaging via the BoM. We do NOT apply
    // the `com.google.gms:google-services` plugin (would require
    // a checked-in `google-services.json` at build time); Firebase
    // is initialised programmatically at runtime — see
    // `me.lakko.app.push.PushSubscriber`.
    implementation(platform(libs.firebase.bom))
    implementation(libs.firebase.messaging)
    // 1.20d — `Task<T>.await()` extension that converts the
    // Google Play Services callback-based `Task` into a `suspend`
    // shape. Required because `FirebaseMessaging.getInstance().token`
    // returns `Task<String>`.
    implementation(libs.kotlinx.coroutines.play.services)
    // 1.20d — EncryptedSharedPreferences for the device push
    // pseudonym (mirror of iOS Keychain posture).
    implementation(libs.androidx.security.crypto)

    // 2a2e — Jetpack Credential Manager for WebAuthn passkey
    // enrolment + assertion. `credentials` is the unified
    // public API; `credentials-play-services-auth` bridges to
    // Google Play Services on devices that ship it (everywhere
    // outside CN/RU appstore-only builds).
    implementation(libs.androidx.credentials)
    implementation(libs.androidx.credentials.play.services.auth)

    // 2a3b4 — add-device QR pipeline. CameraX drives the live
    // preview + frame analysis, ML Kit decodes QR codes inside
    // those frames, ZXing-core encodes the pairing payload into
    // the QR matrix the joining device renders. All three together
    // cover the iOS CoreImage + AVCaptureMetadataOutput pair and
    // the web `qrcode + jsqr` pair on a single AAR surface.
    implementation(libs.androidx.camera.core)
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)
    implementation(libs.androidx.camera.view)
    implementation(libs.mlkit.barcode.scanning)
    implementation(libs.zxing.core)

    // G3-2 — WebRTC pod. The artifact ships the Java/Kotlin
    // surface under the `org.webrtc` package the upstream
    // libwebrtc has used since 2018; only the Maven
    // coordinate changed from the abandoned
    // `org.webrtc:google-webrtc` to the community-maintained
    // `io.github.webrtc-sdk:android`. Consumer code under
    // `me.lakko.app.calls.WebRtcPeerConnection` imports
    // `org.webrtc.PeerConnection`, `org.webrtc.SessionDescription`,
    // and friends — unchanged from the abandoned artifact's
    // surface.
    implementation(libs.webrtc.sdk.android)

    // 1.15d2 — JVM-side unit tests. `testDebugUnitTest` runs
    // these on the host JDK without any emulator; perfect for
    // the AuthClient flow tests that drive the high-level
    // signup/login/recover against a Ktor MockEngine.
    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.ktor.client.mock)
    testImplementation(libs.kotlinx.serialization.json)

    // G3-4-mobile — instrumented test stack under
    // `app/src/androidTest/`. The first-pass SfuHealth
    // instrumented test only needs JUnit4 + the
    // AndroidJUnitRunner; Espresso (view-matching) and
    // mockito-android come later, when call UI grows
    // assertions worth pinning against a live emulator.
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.junit)
}
