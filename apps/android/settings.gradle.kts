// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/*
 * Phase 1.15b — Gradle multi-module root for the Android side
 * of Lakko.
 *
 * `lakko-core` is the only module today: it wraps the Rust
 * cdylib (`.so` per ABI under `src/main/jniLibs/`) + the
 * UniFFI-generated Kotlin bindings (`src/main/kotlin/`) into a
 * single AAR. Future modules (`app` for the actual Compose
 * UI, `feature-*` per screen group) plug in alongside.
 */

pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "lakko-android"

include(":lakko-core")
include(":app")
