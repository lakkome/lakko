// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/*
 * `lakko-core` AAR (Phase 1.15b).
 *
 * Wraps the workspace `lakko-ffi-uniffi` Rust cdylib together
 * with the UniFFI-generated Kotlin bindings into an Android
 * library:
 *
 *   * Native `.so` files come from `just build-android` (drops
 *     them under `src/main/jniLibs/{arm64-v8a,x86_64}/`).
 *   * Kotlin bindings come from `just regen-bindings` (drops
 *     them under `src/main/kotlin/uniffi/lakko_ffi_uniffi/`).
 *   * JNA loads the cdylib at runtime via
 *     `System.loadLibrary("lakko_ffi_uniffi")` and forwards
 *     calls into the Rust ABI through the UniFFI scaffolding.
 *
 * The module exports no Compose / activity surface — that
 * arrives with the `app` module in 1.15c. Consumers (the
 * upcoming app + future feature modules) include this AAR via
 * `implementation(project(":lakko-core"))`.
 *
 * No resources, no AIDL — pure JVM Kotlin facade over the
 * native lib. AGP's `library` plugin still demands a manifest
 * (`AndroidManifest.xml` in `src/main/`) but it's empty save
 * for the package name.
 */

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "me.lakko.core"

    compileSdk = libs.versions.android.compile.sdk.get().toInt()

    defaultConfig {
        minSdk = libs.versions.android.min.sdk.get().toInt()

        // We ship only arm64-v8a + x86_64. 32-bit Android is
        // <5% of active devices in 2026 and Play Store hasn't
        // required 32-bit binaries in the same APK since 2019.
        // `just build-android` drops only these two ABIs into
        // src/main/jniLibs; declaring them here makes AGP refuse
        // to include any stray .so for other ABIs that might
        // sneak in via a transitive dep.
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
        // AGP 8.x stable on JVM 17 bytecode. Matches the
        // sourceCompatibility above so generated stubs line up.
        jvmTarget = "17"
    }

    buildTypes {
        release {
            // No code shrinking in lakko-core — the UniFFI
            // bindings cross the FFI boundary by name; R8
            // renaming would break the `System.loadLibrary`
            // and JNA reflection paths. The consumer-rules.pro
            // file below tells downstream R8 to leave our
            // surface alone too.
            isMinifyEnabled = false
        }
    }

    // Skip BuildConfig — we don't need the BUILD_TYPE /
    // VERSION_CODE constants in a library module.
    buildFeatures {
        buildConfig = false
    }
}

dependencies {
    // JNA — the UniFFI runtime. The generated Kotlin code
    // calls `Native.load("lakko_ffi_uniffi", ...)` which JNA
    // resolves against the bundled `.so` files in jniLibs.
    implementation(libs.jna) {
        // The default JNA distribution ships native binaries
        // for every JVM platform under the sun (Linux x86_64,
        // macOS x86_64, ...). On Android we only want the
        // `@aar` artefact which carries the Android-relevant
        // libjnidispatch.so per ABI.
        artifact {
            type = "aar"
        }
    }
}
