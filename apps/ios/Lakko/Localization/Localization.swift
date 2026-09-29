// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// The interface language (ADR-0058, UX spec `docs/ux/localization.md`).
///
/// One observable for the whole app. It resolves the language with the
/// shared `lakko-l10n` rule (the stored choice, else the first supported
/// system preference, else English), loads the compiled catalog the app
/// ships under `L10n/` in its bundle, and answers lookups from memory.
///
/// Not `Bundle` / `NSLocalizedString`: the system bundle localization is
/// fixed for the life of the process, and switching the language here must
/// re-render the running UI without a restart. The root view reads
/// ``locale`` and ``layoutDirection`` from this object and injects them
/// into the environment, and every view that shows a catalog string
/// observes this object, so a switch re-renders everything at once.
///
/// The choice is stored on this device only (the App Group suite, so the
/// notification extension can render the same language) and is never sent
/// to a server: a language is personal data (invariants #1 and #5).
@MainActor
final class Localization: ObservableObject {
    /// The key of the stored choice. Absent = "Same as system".
    static let storageKey = "lakko.l10n.language"

    /// The process-wide instance every screen observes.
    static let shared = Localization()

    /// The stored choice: a supported code, or `nil` for "Same as system".
    @Published private(set) var selected: String?
    /// The language the interface is in right now.
    @Published private(set) var language: String
    /// Whether ``language`` is written right to left.
    @Published private(set) var isRightToLeft: Bool

    /// Every supported language, English first (the `lakko-l10n` table).
    let languages: [LanguageInfo]

    private let defaults: UserDefaults
    private let bundle: Bundle
    private let systemPreferred: () -> [String]
    private let source: L10nSource?
    private var catalog: L10nCatalog?
    private var strings: [String: String] = [:]
    private var localeObserver: NSObjectProtocol?

    /// The store for the choice: the App Group suite when it opens, else the
    /// app's own defaults (an unprovisioned build keeps working).
    nonisolated static func defaultStore() -> UserDefaults {
        SharedStorage.sharedDefaults() ?? .standard
    }

    init(
        defaults: UserDefaults = Localization.defaultStore(),
        bundle: Bundle = .main,
        systemPreferred: @escaping () -> [String] = { Locale.preferredLanguages }
    ) {
        self.defaults = defaults
        self.bundle = bundle
        self.systemPreferred = systemPreferred
        let languages = l10nSupportedLanguages()
        self.languages = languages
        self.source = Localization.loadSource(bundle: bundle)
        let stored = defaults.string(forKey: Localization.storageKey)
        let selected = stored.flatMap { code in
            languages.contains(where: { $0.code == code }) ? code : nil
        }
        self.selected = selected
        let resolved = l10nResolveLanguage(selected: selected, systemPreferred: systemPreferred())
        self.language = resolved
        self.isRightToLeft = languages.first(where: { $0.code == resolved })?.direction == .rtl
        load(resolved)
        // Following the system: pick up a system language change without a
        // restart. A stored choice ignores it.
        localeObserver = NotificationCenter.default.addObserver(
            forName: NSLocale.currentLocaleDidChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshFromSystem() }
        }
    }

    deinit {
        if let localeObserver { NotificationCenter.default.removeObserver(localeObserver) }
    }

    // MARK: - Environment

    /// The locale the platform formatters use for dates, times and numbers.
    var locale: Locale { Locale(identifier: language) }

    /// The layout direction the root view injects.
    var layoutDirection: LayoutDirection { isRightToLeft ? .rightToLeft : .leftToRight }

    // MARK: - Lookup

    /// The string for `key` in the current language. The catalog falls back
    /// to English per key, so a key the source defines is never missing; an
    /// unknown key renders as itself, which a review spots at once.
    func t(_ key: String) -> String {
        strings[key] ?? key
    }

    /// A message with arguments: `texts` are wrapped in bidi isolates by the
    /// formatter, `counts` select the plural form.
    func t(_ key: String, texts: [String: String] = [:], counts: [String: UInt64] = [:]) -> String {
        if texts.isEmpty && counts.isEmpty { return t(key) }
        return catalog?.format(key: key, texts: texts, counts: counts) ?? key
    }

    // MARK: - Languages

    /// The language "Same as system" resolves to on this device.
    var systemLanguage: String {
        l10nResolveLanguage(selected: nil, systemPreferred: systemPreferred())
    }

    /// The table row for `code`, if it is supported.
    func info(for code: String) -> LanguageInfo? {
        languages.first(where: { $0.code == code })
    }

    /// The language's own name ("עברית"), never translated.
    func ownName(of code: String) -> String {
        info(for: code)?.ownName ?? code
    }

    /// The language's name in the current interface language.
    func translatedName(of info: LanguageInfo) -> String {
        let key = "language.name.\(info.code.lowercased())"
        let name = t(key)
        return name == key ? info.englishName : name
    }

    // MARK: - Choice

    /// Store `code` (`nil` = "Same as system") and switch the interface.
    func select(_ code: String?) {
        let code = code.flatMap { info(for: $0) == nil ? nil : $0 }
        if let code {
            defaults.set(code, forKey: Localization.storageKey)
        } else {
            defaults.removeObject(forKey: Localization.storageKey)
        }
        selected = code
        apply(l10nResolveLanguage(selected: code, systemPreferred: systemPreferred()))
    }

    private func refreshFromSystem() {
        guard selected == nil else { return }
        apply(systemLanguage)
    }

    private func apply(_ resolved: String) {
        guard resolved != language || catalog == nil else { return }
        load(resolved)
        isRightToLeft = info(for: resolved)?.direction == .rtl
        language = resolved
    }

    // MARK: - Catalog

    private static func loadSource(bundle: Bundle) -> L10nSource? {
        guard let url = bundle.url(forResource: "source", withExtension: "json", subdirectory: "L10n"),
              let data = try? Data(contentsOf: url) else {
            NSLog("[l10n] ERR source catalog missing from the bundle")
            return nil
        }
        do {
            return try L10nSource(sourceJson: data)
        } catch {
            NSLog("[l10n] ERR source catalog did not load: \(error)")
            return nil
        }
    }

    /// Build the catalog for `code` and read its plain strings once, so a
    /// render needs no FFI call. Any failure falls back to English.
    private func load(_ code: String) {
        guard let source else {
            catalog = nil
            strings = [:]
            return
        }
        let builtIn: Data? = code == "en" ? nil : bundle
            .url(forResource: code, withExtension: "json", subdirectory: "L10n")
            .flatMap { try? Data(contentsOf: $0) }
        let built = (try? L10nCatalog(source: source, language: code, builtIn: builtIn, bundle: nil))
            ?? (try? L10nCatalog(source: source, language: "en", builtIn: nil, bundle: nil))
        catalog = built
        strings = built?.plainStrings() ?? [:]
    }
}

/// Injects the current language into the environment of everything below
/// it: the locale for the platform formatters and the layout direction, so
/// Arabic, Persian and Hebrew mirror the whole app live.
struct LocalizedRoot<Content: View>: View {
    @ObservedObject private var l10n = Localization.shared
    @ViewBuilder let content: () -> Content

    var body: some View {
        content()
            .environment(\.locale, l10n.locale)
            .environment(\.layoutDirection, l10n.layoutDirection)
    }
}
