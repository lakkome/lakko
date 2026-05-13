// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `@main` entry point for the iOS / iPadOS app — Phase 1.14c.
///
/// Single-`WindowGroup` shell that renders the
/// ``HelloCoreView`` smoke surface. Mirror of:
///
///   * `apps/android`'s `MainActivity` from 1.15c — same role
///     (host the smoke screen + the eventual auth /
///     contacts / chat routing).
///   * `apps/web`'s `App.tsx` + `routes.tsx` from 1.16a/b.
///
/// The router lands in 1.14d when the auth surface introduces
/// the Anon / Session split; for now the app shows the same
/// thing on every launch — the hex of a freshly-derived
/// `Identity`'s signature public key. If hex renders, the
/// XCFramework bridge works on this device.
@main
struct LakkoApp: App {
    var body: some Scene {
        WindowGroup {
            HelloCoreView()
        }
    }
}
