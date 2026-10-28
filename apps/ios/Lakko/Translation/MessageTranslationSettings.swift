// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// ADR-0082 / `docs/ux/translation-and-transcription.md` section 6 — the
/// translation and transcription settings of THIS device.
///
/// Device settings, like the interface language (`localization.md` section
/// 4): they live in `UserDefaults.standard`, are never written to a server,
/// the backup, the conversation vault or the DeviceGroup synced state, and stay
/// when an account is forgotten (the theme does too). What an ACCOUNT keeps
/// (the results, the per-conversation choices) is ``MessageTranslationStore``.
///
/// The defaults are the spec's: both buttons on, mobile data off, the
/// reading list empty (the interface language is always read and is added at
/// read time, so a language switch moves it), the target unset (the interface
/// language).
@MainActor
final class MessageTranslationSettings: ObservableObject {

    static let shared = MessageTranslationSettings()

    static let showTranslateKey = "lakko.translation.showTranslate"
    static let showTranscriptKey = "lakko.translation.showTranscript"
    static let readingKey = "lakko.translation.reading"
    static let targetKey = "lakko.translation.target"
    static let mobileDataKey = "lakko.translation.mobileData"

    private let defaults: UserDefaults

    @Published var showTranslate: Bool { didSet { defaults.set(showTranslate, forKey: Self.showTranslateKey) } }
    @Published var showTranscript: Bool { didSet { defaults.set(showTranscript, forKey: Self.showTranscriptKey) } }
    /// Languages added beyond the interface language, in canonical form.
    @Published var addedReading: [String] { didSet { defaults.set(addedReading, forKey: Self.readingKey) } }
    /// `nil` = the interface language.
    @Published var target: String? { didSet { defaults.set(target, forKey: Self.targetKey) } }
    @Published var mobileData: Bool { didSet { defaults.set(mobileData, forKey: Self.mobileDataKey) } }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        showTranslate = defaults.object(forKey: Self.showTranslateKey) as? Bool ?? true
        showTranscript = defaults.object(forKey: Self.showTranscriptKey) as? Bool ?? true
        addedReading = defaults.stringArray(forKey: Self.readingKey) ?? []
        target = defaults.string(forKey: Self.targetKey)
        mobileData = defaults.object(forKey: Self.mobileDataKey) as? Bool ?? false
    }

    /// The interface language, in canonical form.
    var interfaceLanguage: String {
        canonicalLanguageTag(tag: Localization.shared.language) ?? "en"
    }

    /// Every language the person reads: the interface language first, then
    /// the ones they added.
    var readingLanguages: [String] {
        var out = [interfaceLanguage]
        for tag in addedReading where !out.contains(where: { sameLanguage(a: $0, b: tag) }) {
            out.append(tag)
        }
        return out
    }

    /// The language translations are written in.
    var effectiveTarget: String { target ?? interfaceLanguage }

    func addReading(_ tag: String) {
        guard let canonical = canonicalLanguageTag(tag: tag),
              !readingLanguages.contains(where: { sameLanguage(a: $0, b: canonical) })
        else { return }
        addedReading.append(canonical)
    }

    func removeReading(_ tag: String) {
        addedReading.removeAll { sameLanguage(a: $0, b: tag) }
    }
}

/// The name of a language for display: in the interface language (UX 3.2,
/// "The language name is in the interface language"), or in its own language
/// (the picker of UX 3.4 titles each row by its own name, `localization.md`
/// section 2).
enum LanguageNames {

    static func inInterface(_ tag: String) -> String {
        let locale = Localization.currentLocale
        return locale.localizedString(forIdentifier: tag)
            ?? locale.localizedString(forLanguageCode: tag)
            ?? tag
    }

    static func own(_ tag: String) -> String {
        let own = Locale(identifier: tag)
        let name = own.localizedString(forIdentifier: tag) ?? own.localizedString(forLanguageCode: tag) ?? tag
        // Own names start lowercase in several languages; a list title does not.
        return name.prefix(1).uppercased(with: own) + name.dropFirst()
    }
}
