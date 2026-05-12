// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/*
 * Root build script for the Android multi-module project.
 *
 * Plugins are declared with `apply false` here so they're
 * resolved by every subproject that opts in via the `plugins {}`
 * block in its own `build.gradle.kts` (the convention since AGP
 * 7.x; the legacy `buildscript {}` block is gone).
 *
 * Version catalog (`gradle/libs.versions.toml`) is the single
 * source of truth for plugin / library versions — subprojects
 * reference via `libs.plugins.<alias>` / `libs.<group>.<artifact>`.
 */

plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.android) apply false
    alias(libs.plugins.kotlin.compose) apply false
    // kotlinx-serialization plugin — generates `Serializer`
    // implementations for `@Serializable` types in
    // `me.lakko.app.auth.wire` (1.15d2). `apply false` so only
    // the module that needs it (`:app`) enables it.
    alias(libs.plugins.kotlin.serialization) apply false
}
