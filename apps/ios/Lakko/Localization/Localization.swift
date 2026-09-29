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
    nonisolated static let storageKey = "lakko.l10n.language"

    /// The process-wide instance every screen observes. It also publishes
    /// the current catalog to ``text(_:texts:counts:)`` for code off the main
    /// actor.
    static let shared = Localization(publishesSnapshot: true)

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
    private let publishesSnapshot: Bool

    /// The store for the choice: the App Group suite when it opens, else the
    /// app's own defaults (an unprovisioned build keeps working).
    nonisolated static func defaultStore() -> UserDefaults {
        SharedStorage.sharedDefaults() ?? .standard
    }

    init(
        defaults: UserDefaults = Localization.defaultStore(),
        bundle: Bundle = .main,
        systemPreferred: @escaping () -> [String] = { Locale.preferredLanguages },
        publishesSnapshot: Bool = false
    ) {
        self.publishesSnapshot = publishesSnapshot
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

    nonisolated fileprivate static func loadSource(bundle: Bundle) -> L10nSource? {
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
        let built = Localization.buildCatalog(source: source, bundle: bundle, code: code)
        catalog = built
        strings = built?.plainStrings() ?? [:]
        if publishesSnapshot {
            Localization.snapshot.publish(catalog: built, strings: strings)
        }
    }

    /// The catalog for `code` from the compiled files in `bundle`, English
    /// when that language does not load.
    nonisolated fileprivate static func buildCatalog(
        source: L10nSource,
        bundle: Bundle,
        code: String
    ) -> L10nCatalog? {
        let builtIn: Data? = code == "en" ? nil : bundle
            .url(forResource: code, withExtension: "json", subdirectory: "L10n")
            .flatMap { try? Data(contentsOf: $0) }
        return (try? L10nCatalog(source: source, language: code, builtIn: builtIn, bundle: nil))
            ?? (try? L10nCatalog(source: source, language: "en", builtIn: nil, bundle: nil))
    }

    // MARK: - Off the main actor

    /// The catalog ``shared`` has loaded, readable from any thread.
    nonisolated fileprivate static let snapshot = CatalogSnapshot()

    /// The string for `key` in the current interface language, for code
    /// that is not on the main actor (an error's description, a UIKit
    /// delegate). A view reads ``t(_:)`` on an observed instance instead, so
    /// it re-renders on a language switch; text from here is fixed when it
    /// is produced.
    nonisolated static func text(
        _ key: String,
        texts: [String: String] = [:],
        counts: [String: UInt64] = [:]
    ) -> String {
        snapshot.lookup(key, texts: texts, counts: counts)
    }

    // MARK: - Composition helpers

    /// A message with one `{link}` part, split around it: the text before
    /// the link and the text after it, so a view can put a tappable title
    /// between them while the translator still sees one sentence.
    func linkParts(_ key: String, texts: [String: String] = [:]) -> (lead: String, trail: String) {
        parts(key, around: "link", texts: texts)
    }

    /// A message split around its `{arg}` part, for a view that renders that
    /// part as its own element (bold, a code font, a link). The bidi isolates
    /// the formatter puts around the argument stay with it, so none leaks
    /// into the text around it or into a copied value.
    func parts(
        _ key: String,
        around arg: String,
        texts: [String: String] = [:]
    ) -> (lead: String, trail: String) {
        let marker = "\u{E000}\(arg)\u{E001}"
        var args = texts
        args[arg] = marker
        let whole = t(key, texts: args)
        guard let range = whole.range(of: marker) else { return (whole, "") }
        let isolates = CharacterSet(charactersIn: "\u{2066}\u{2067}\u{2068}\u{2069}")
        let lead = String(whole[..<range.lowerBound])
            .trimmingCharacters(in: isolates)
        let trail = String(whole[range.upperBound...])
            .trimmingCharacters(in: isolates)
        return (lead, trail)
    }

    /// A list of word positions for a `{positions}` argument: the numbers
    /// stay in Latin digits (ADR-0058 open question 2) and are joined by the
    /// list formatter of the interface language.
    func positions(_ numbers: [Int]) -> String {
        let formatter = ListFormatter()
        formatter.locale = locale
        let items = numbers.map { String($0) }
        return formatter.string(from: items) ?? items.joined(separator: ", ")
    }
}

/// The catalog of ``Localization/shared``, behind a lock so code off the
/// main actor can read it. Until the shared instance has loaded, it loads
/// the same language the shared instance will (the stored choice, else the
/// system preference) from the main bundle on first use.
final class CatalogSnapshot: @unchecked Sendable {
    private let lock = NSLock()
    private var catalog: L10nCatalog?
    private var strings: [String: String] = [:]
    private var loaded = false

    func publish(catalog: L10nCatalog?, strings: [String: String]) {
        lock.withLock {
            self.catalog = catalog
            self.strings = strings
            loaded = true
        }
    }

    func lookup(_ key: String, texts: [String: String], counts: [String: UInt64]) -> String {
        lock.withLock {
            if !loaded { loadDefault() }
            if texts.isEmpty && counts.isEmpty { return strings[key] ?? key }
            return catalog?.format(key: key, texts: texts, counts: counts) ?? key
        }
    }

    /// Called with the lock held.
    private func loadDefault() {
        loaded = true
        guard let source = Localization.loadSource(bundle: .main) else { return }
        let languages = l10nSupportedLanguages()
        let stored = Localization.defaultStore().string(forKey: Localization.storageKey)
        let selected = stored.flatMap { code in
            languages.contains(where: { $0.code == code }) ? code : nil
        }
        let code = l10nResolveLanguage(selected: selected, systemPreferred: Locale.preferredLanguages)
        catalog = Localization.buildCatalog(source: source, bundle: .main, code: code)
        strings = catalog?.plainStrings() ?? [:]
    }
}

/// A message kept as the way to render it rather than as rendered text, so
/// an error or notice already on screen follows a live language switch
/// (the Android `UiText` counterpart).
///
/// A view that shows one observes ``Localization/shared`` (it re-renders on
/// a switch) and reads ``text``, which renders in the language of that
/// moment. Two values are equal only when they are the same value, so
/// setting a fresh one always publishes.
public struct UiText: Equatable {
    private let id = UUID()
    private let make: () -> String

    /// Text produced by `make` at every render, e.g. a copy function over an
    /// error: `UiText { AuthErrorCopy.login(error) }`.
    public init(_ make: @escaping () -> String) {
        self.make = make
    }

    /// A catalog message with its arguments.
    public static func key(
        _ key: String,
        texts: [String: String] = [:],
        counts: [String: UInt64] = [:]
    ) -> UiText {
        UiText { Localization.text(key, texts: texts, counts: counts) }
    }

    /// The text in the current interface language.
    public var text: String { make() }

    public static func == (lhs: UiText, rhs: UiText) -> Bool { lhs.id == rhs.id }
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
