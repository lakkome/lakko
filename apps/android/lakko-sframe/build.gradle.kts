// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/*
 * `lakko-sframe` AAR (G3-sframe-native N-5).
 *
 * JNI bridge between Android's libwebrtc binary (org.webrtc.*
 * Java surface from io.github.webrtc-sdk:android 125.6422.07)
 * and the Lakko SFrame native stack:
 *
 *   apps/shared/sframe-bridge          (C++ N-3, subclasses
 *                                       webrtc::FrameEncryptor
 *                                       Interface)
 *   crates/lakko-sframe-native         (Rust N-1, C ABI)
 *   crates/lakko-calls/sframe          (RFC 9605 AEAD pipeline)
 *
 * What this AAR ships:
 *
 *   - Kotlin classes `LakkoFrameEncryptor` /
 *     `LakkoFrameDecryptor` implementing the public
 *     org.webrtc.FrameEncryptor / FrameDecryptor interfaces.
 *     Each holds a `long` native pointer that libwebrtc's
 *     Java-side glue passes to its C++ side via the standard
 *     `getNativeFrameEncryptor()` method.
 *   - Native `libsframe_jni.so` files (one per shipped ABI)
 *     under `src/main/jniLibs/{arm64-v8a,x86_64}/` — populated
 *     by `just cross-build-sframe-native-android` which cross-
 *     compiles the Rust staticlib for each NDK triple, then
 *     compiles+links the JNI bridge via the NDK toolchain.
 *   - `SFrameAvailability` boot probe (mirror of iOS N-4) —
 *     refuse-to-start on missing setFrameEncryptor /
 *     setFrameDecryptor methods on org.webrtc.RtpSender /
 *     RtpReceiver. Security invariant #4: no fallback to non-
 *     E2EE.
 *
 * Why a separate module from `lakko-core`:
 *
 *   lakko-core is the UniFFI surface (chat / contacts / MLS /
 *   storage). lakko-sframe is the Real-Time-Encryption surface
 *   bound to libwebrtc's binary ABI. Cycling between R8 /
 *   ProGuard rules + Android NDK build settings for two
 *   unrelated native libraries inside one AAR has been a
 *   recurring papercut on prior Lakko phases; the cleaner shape
 *   is two AARs, two consumer-rules.pro files, two CMakeLists.
 */

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "me.lakko.sframe"

    compileSdk = libs.versions.android.compile.sdk.get().toInt()

    defaultConfig {
        minSdk = libs.versions.android.min.sdk.get().toInt()

        // Same ABI policy as lakko-core (1.15b): arm64-v8a +
        // x86_64. 32-bit Android is dead in 2026.
        ndk {
            abiFilters += setOf("arm64-v8a", "x86_64")
        }

        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    buildTypes {
        release {
            // R8 / ProGuard would mangle the native-method
            // names the JNI bridge resolves by full
            // `Java_me_lakko_sframe_*` symbol path — leave them
            // intact. `consumer-rules.pro` carries the same
            // posture for downstream apps that consume this
            // AAR.
            isMinifyEnabled = false
        }
    }

    buildFeatures {
        buildConfig = false
    }

    // We DON'T let AGP drive the CMakeLists.txt under
    // `src/main/cpp/`. Instead, `just cross-build-sframe-native-
    // android` produces the .so files directly via the NDK
    // standalone toolchain + drops them into
    // `src/main/jniLibs/{abi}/`. Reasons:
    //
    //   1. The Rust staticlib that libsframe_jni.so links
    //      against is cross-compiled by `cargo build --target
    //      <abi>` for each NDK triple. AGP's `externalNativeBuild
    //      = cmake { ... }` block expects CMake to be the
    //      orchestration root — but here CMake is a dependency
    //      of cargo's output, not the other way around. The
    //      cleaner sequencing is: cargo first, then `cmake +
    //      ndk-build` against cargo's output.
    //
    //   2. The staticlib output paths are determined by cargo's
    //      target-triple conventions; replicating those inside
    //      an AGP-managed CMake build adds a brittle indirection
    //      every time cargo's triple-naming changes.
    //
    // The just recipe is the documented build path; CI follows
    // the same recipe.
    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

dependencies {
    // The libwebrtc Android binary. We compile against
    // org.webrtc.{FrameEncryptor,FrameDecryptor,RtpSender,
    // RtpReceiver}; the actual binary is supplied by the app
    // target at runtime (the AAR already pulls it transitively
    // when the app declares `implementation libs.webrtc.sdk.
    // android`).
    compileOnly(libs.webrtc.sdk.android)
}
