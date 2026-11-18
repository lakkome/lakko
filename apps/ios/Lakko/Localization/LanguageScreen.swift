// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Settings > Language (UX spec `docs/ux/localization.md` section 3), also
/// shown as a sheet from the sign-in and sign-up screens.
///
/// "Same as system" first, then every language by its own name with its
/// name in the current interface language underneath, sorted by that
/// translated name. A tap switches the whole interface at once, this screen
/// included (it re-sorts, its title changes, the layout flips); there is no
/// confirmation and no Apply button.
struct LanguageScreen: View {
    /// ADR-0082 — Settings > Language carries the entry to Translation and
    /// transcription; the sheet shown before sign-in does not.
    var showsTranslationSettings = false
    @ObservedObject private var l10n = Localization.shared
    @State private var query = ""

    /// Rows matching the search, sorted by the translated name in the
    /// current interface language.
    private var rows: [LanguageInfo] {
        let locale = l10n.locale
        let matching = l10n.languages.filter { matches($0) }
        return matching.sorted { a, b in
            l10n.translatedName(of: a).compare(
                l10n.translatedName(of: b),
                options: [.caseInsensitive],
                locale: locale
            ) == .orderedAscending
        }
    }

    /// "Same as system" matches the search by its own label.
    private var showsSystemRow: Bool {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return needle.isEmpty || fold(l10n.t("settings.language.same_as_system")).contains(fold(needle))
    }

    var body: some View {
        List {
            if showsTranslationSettings, query.isEmpty {
                Section {
                    NavigationLink {
                        TranslationSettingsScreen()
                    } label: {
                        Label(l10n.t("settings.translation.title"), icon: .translate)
                            .foregroundStyle(Color.lakkoText)
                    }
                }
            }
            if showsSystemRow {
                let systemName = l10n.ownName(of: l10n.systemLanguage)
                row(
                    title: l10n.t("settings.language.same_as_system"),
                    subtitle: systemName,
                    checked: l10n.selected == nil
                ) { l10n.select(nil) }
                .accessibilityLabel(
                    l10n.t("settings.language.same_as_system_detail", texts: ["language": systemName]))
            }
            ForEach(rows, id: \.code) { info in
                row(
                    title: info.ownName,
                    subtitle: l10n.translatedName(of: info),
                    checked: l10n.selected == info.code
                ) { l10n.select(info.code) }
            }
        }
        .listStyle(.insetGrouped)
        // A UIKit-backed List keeps the mirroring it was built with when the
        // environment's layout direction flips under it (its glyphs come out
        // mirrored), so the list is rebuilt when the direction changes.
        .id(l10n.layoutDirection)
        .overlay {
            if rows.isEmpty && !showsSystemRow {
                Text(l10n.t("settings.language.no_results"))
                    .font(.body)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .searchable(text: $query, prompt: Text(l10n.t("settings.language.search")))
        .navigationTitle(l10n.t("settings.language.title"))
        .navigationBarTitleDisplayMode(.inline)
    }

    private func row(
        title: String,
        subtitle: String,
        checked: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.body)
                        .foregroundStyle(Color.lakkoText)
                    Text(subtitle)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                if checked {
                    LkIconView(.checkmark, size: 20)
                        .foregroundStyle(Color.lakkoAccent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(checked ? .isSelected : [])
    }

    /// Search matches the own name and the translated name, case- and
    /// diacritic-insensitive.
    private func matches(_ info: LanguageInfo) -> Bool {
        let needle = fold(query.trimmingCharacters(in: .whitespacesAndNewlines))
        if needle.isEmpty { return true }
        return fold(info.ownName).contains(needle)
            || fold(l10n.translatedName(of: info)).contains(needle)
            || fold(info.englishName).contains(needle)
    }

    private func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
    }
}

/// The globe button in the top corner of the sign-in and sign-up screens: a
/// person who cannot read English switches before creating an account. It
/// shows the current language's own name and opens the same list as a sheet.
struct LanguageButton: View {
    @ObservedObject private var l10n = Localization.shared
    @State private var presented = false

    var body: some View {
        Button {
            presented = true
        } label: {
            HStack(spacing: 6) {
                LkIconView(.globe, size: 20)
                Text(l10n.ownName(of: l10n.language))
                    .lineLimit(1)
            }
            .font(.subheadline)
            .foregroundStyle(Color.lakkoText)
            .frame(minHeight: 44)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(l10n.t("auth.language.button"))
        .accessibilityValue(l10n.ownName(of: l10n.language))
        .sheet(isPresented: $presented) {
            NavigationStack {
                LanguageScreen()
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button {
                                presented = false
                            } label: {
                                LkIconView(.xmark, size: 24)
                            }
                            .accessibilityLabel(l10n.t("common.close"))
                        }
                    }
            }
            // A sheet is its own presentation; carry the language in.
            .environment(\.locale, l10n.locale)
            .environment(\.layoutDirection, l10n.layoutDirection)
            .presentationDragIndicator(.visible)
        }
    }
}
