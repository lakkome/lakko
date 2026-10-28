// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Settings > Language > Translation and transcription
/// (`docs/ux/translation-and-transcription.md` section 6).
///
/// Every pack on iOS is the system's: Apple downloads, updates and shares the
/// translation languages and the speech models between apps, and Lakko cannot
/// delete them, so each is listed with `Managed by iOS` and no Remove. The
/// size is not shown: neither framework reports one.
struct TranslationSettingsScreen: View {
    @ObservedObject private var center = MessageTranslationCenter.shared
    @ObservedObject private var settings = MessageTranslationSettings.shared
    @ObservedObject private var l10n = Localization.shared

    @State private var sources: [String] = []
    @State private var packs: [(tag: String, translation: Bool, transcription: Bool)] = []
    @State private var confirmingDelete: Int?
    @State private var addingReading = false

    var body: some View {
        List {
            translationSection
            transcriptionSection
            Section {
                Toggle(l10n.t("settings.translation.mobile_data"), isOn: $settings.mobileData)
                    .tint(Color.lakkoAccent)
            }
            packsSection
            Section {
                Button(role: .destructive) {
                    confirmingDelete = center.storedCount()
                } label: {
                    Text(l10n.t("settings.translation.delete_all")).foregroundStyle(Color.lakkoDanger)
                }
            } footer: {
                Text(l10n.t("settings.translation.footer"))
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("settings.translation.title"))
        .navigationBarTitleDisplayMode(.inline)
        .task(id: settings.effectiveTarget) { await load() }
        // Lakko's own dialog and action sheet (never a system alert, menu
        // or picker).
        .lakkoDialog(item: $confirmingDelete) { count in
            LakkoDialogContent(
                title: l10n.t("settings.translation.delete_all.title"),
                message: l10n.t("settings.translation.delete_all.body", counts: ["count": UInt64(count)]),
                actions: [
                    .cancel(),
                    .danger(l10n.t("settings.translation.delete_all.confirm")) { center.deleteAll() },
                ])
        }
        .lakkoActionSheet(isPresented: $addingReading, title: l10n.t("settings.translation.reading.add")) {
            TranslationLanguageChoices.rows(
                sources.filter { tag in !settings.readingLanguages.contains { sameLanguage(a: $0, b: tag) } },
                checked: nil
            ) { tag in settings.addReading(tag) }
        }
    }

    @ViewBuilder
    private var translationSection: some View {
        Section {
            if center.translationSupported {
                Toggle(l10n.t("settings.translation.show_translate"), isOn: $settings.showTranslate)
                    .tint(Color.lakkoAccent)
                LakkoPicker(
                    l10n.t("settings.translation.target"),
                    selection: Binding(
                        get: { canonicalLanguageTag(tag: settings.effectiveTarget) ?? settings.effectiveTarget },
                        set: { tag in
                            settings.target = sameLanguage(a: tag, b: settings.interfaceLanguage)
                                ? nil : canonicalLanguageTag(tag: tag)
                        }),
                    options: targetOptions)
            } else {
                Text(l10n.t("settings.translation.unavailable.translation"))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        if center.translationSupported {
            Section(l10n.t("settings.translation.reading")) {
                ForEach(settings.readingLanguages, id: \.self) { tag in
                    HStack {
                        Text(LanguageNames.inInterface(tag)).foregroundStyle(Color.lakkoText)
                        Spacer()
                        // The interface language is always read.
                        if !sameLanguage(a: tag, b: settings.interfaceLanguage) {
                            Button { settings.removeReading(tag) } label: {
                                Image(systemName: "minus.circle").foregroundStyle(Color.lakkoTextMuted)
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel(l10n.t(
                                "settings.translation.reading.remove",
                                texts: ["language": LanguageNames.inInterface(tag)]))
                        }
                    }
                }
                Button(l10n.t("settings.translation.reading.add")) { addingReading = true }
                    .foregroundStyle(Color.lakkoAccent)
            }
        }
    }

    @ViewBuilder
    private var transcriptionSection: some View {
        Section {
            if center.transcriptionSupported {
                Toggle(l10n.t("settings.translation.show_transcript"), isOn: $settings.showTranscript)
                    .tint(Color.lakkoAccent)
            } else {
                Text(l10n.t("settings.translation.unavailable.transcription"))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
    }

    private var packsSection: some View {
        Section(l10n.t("settings.translation.packs")) {
            if packs.isEmpty {
                Text(l10n.t("settings.translation.packs.empty")).foregroundStyle(Color.lakkoTextMuted)
            }
            ForEach(packs, id: \.tag) { pack in
                VStack(alignment: .leading, spacing: 2) {
                    Text(LanguageNames.inInterface(pack.tag)).foregroundStyle(Color.lakkoText)
                    Text(usage(pack) + " · " + l10n.t("settings.translation.pack.managed.ios"))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
        }
    }

    private func usage(_ pack: (tag: String, translation: Bool, transcription: Bool)) -> String {
        switch (pack.translation, pack.transcription) {
        case (true, true): return l10n.t("settings.translation.pack.for_both")
        case (false, true): return l10n.t("settings.translation.pack.for_transcription")
        default: return l10n.t("settings.translation.pack.for_translation")
        }
    }

    private func load() async {
        sources = await center.translationEngine.sourceLanguages()
        let translated = await center.translationEngine.installedLanguages(target: settings.effectiveTarget)
        let transcribed = await center.transcriptionEngine.installedLanguages()
        var merged: [String: (Bool, Bool)] = [:]
        for tag in translated { merged[tag, default: (false, false)].0 = true }
        for tag in transcribed {
            let key = merged.keys.first { sameLanguage(a: $0, b: tag) } ?? tag
            merged[key, default: (false, false)].1 = true
        }
        packs = merged.map { (tag: $0.key, translation: $0.value.0, transcription: $0.value.1) }
            .sorted { LanguageNames.inInterface($0.tag) < LanguageNames.inInterface($1.tag) }
    }

    /// Every language this device can translate between, and the current
    /// target even when the engine does not list it.
    private var targetOptions: [LakkoPickerOption<String>] {
        var tags = sources
        let current = canonicalLanguageTag(tag: settings.effectiveTarget) ?? settings.effectiveTarget
        if !tags.contains(where: { sameLanguage(a: $0, b: current) }) { tags.append(current) }
        return tags
            .sorted { LanguageNames.inInterface($0) < LanguageNames.inInterface($1) }
            .map { LakkoPickerOption(value: $0, label: LanguageNames.inInterface($0)) }
    }
}
