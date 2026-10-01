// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The interface language a UI test launches the app in (ADR-0061).
///
/// The app takes its interface language from `Locale.preferredLanguages` when
/// the person has not chosen one, so a test that reads a production label in
/// English would fail on a simulator set to any other language. These launch
/// arguments put English first for the app process alone; the simulator's own
/// setting is left as it is.
///
/// Prefer an accessibility identifier where the app has one: an identifier does
/// not change with the language, a label does.
enum UiTestLanguage {
    static let english = ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
}
