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
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.android) apply false
}
