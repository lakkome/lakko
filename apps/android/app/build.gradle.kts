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
        // BuildConfig disabled in lakko-core; here we'd want
        // it for VERSION_NAME but not on the smoke build —
        // re-enable when the auth surface needs a version
        // banner.
        buildConfig = false
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

    // Compose stack via BOM — one version pin covers every
    // androidx.compose.* artefact.
    val composeBom = platform(libs.compose.bom)
    implementation(composeBom)
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.material3)
    implementation(libs.activity.compose)

    // Compose tooling (Layout Inspector previews etc.) — debug
    // only so they don't bloat release APKs.
    debugImplementation(libs.compose.ui.tooling)
    implementation(libs.compose.ui.tooling.preview)
}
