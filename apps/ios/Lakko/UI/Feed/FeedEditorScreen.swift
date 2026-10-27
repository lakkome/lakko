// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// The feed editor (`docs/ux/feed.md` section 4.1): a pushed screen, four
/// sections, and a live preview below that re-ranks as the person changes
/// things. Everything is optional; a blank feed is "Latest". Mirror of web
/// `feed/FeedEditor.tsx`.
///
/// Validity is the core's: Save is disabled with the core's refusal turned
/// into one sentence (``FeedDraft/problem(_:)``), never a check written here.
struct FeedEditorScreen: View {
    @ObservedObject var model: FeedViewModel
    let editing: FeedEditing
    let initial: FeedRule

    @State private var rule: FeedRule
    @State private var hideText: String
    @State private var onlyText: String
    @State private var startFrom: String = "blank"
    @State private var confirmDelete = false
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    init(model: FeedViewModel, editing: FeedEditing, initial: FeedRule) {
        self.model = model
        self.editing = editing
        self.initial = initial
        _rule = State(initialValue: initial)
        _hideText = State(initialValue: initial.hideWords.joined(separator: "\n"))
        _onlyText = State(initialValue: initial.onlyWords.joined(separator: "\n"))
    }

    private var isNew: Bool { editing == .new }

    private enum Kinds: CaseIterable {
        case both, groups, channels

        var labelKey: String {
            switch self {
            case .both: return "feed.editor.sources.both"
            case .groups: return "feed.editor.sources.groups"
            case .channels: return "feed.editor.sources.channels"
            }
        }
    }

    private enum Membership: CaseIterable {
        case any, only, never

        /// The picker's label: a dash for "no choice", else a catalog word.
        var label: String {
            switch self {
            case .any: return "\u{2014}"
            case .only: return Localization.text("feed.editor.membership.only")
            case .never: return Localization.text("feed.editor.membership.never")
            }
        }
    }

    private static let weightRows: [(key: String, path: WritableKeyPath<FeedWeights, UInt8>)] = [
        ("feed.editor.weight.recency", \.recency),
        ("feed.editor.weight.reactions", \.reactions),
        ("feed.editor.weight.replies", \.replies),
        ("feed.editor.weight.from_contacts", \.fromContacts),
        ("feed.editor.weight.mentions", \.mentionsMe),
        ("feed.editor.weight.media", \.hasMedia),
    ]

    var body: some View {
        let problem = FeedDraft.problem(rule)
        Form {
            Section {
                TextField(l10n.t("feed.editor.name"), text: $rule.name)
                iconGrid
                if isNew {
                    LakkoPicker(
                        l10n.t("feed.editor.start_from"), selection: $startFrom,
                        options: [LakkoPickerOption(value: "blank", label: l10n.t("feed.editor.start.blank"))]
                            + FeedPresetKey.allCases.map { LakkoPickerOption(value: $0.rawValue, label: $0.label) })
                    .onChange(of: startFrom) { _, value in applyStart(value) }
                }
            }

            sourcesSection
            weightsSection
            filtersSection

            Section(l10n.t("feed.editor.suggestions")) {
                LakkoPicker(l10n.t("feed.editor.suggestions_rate"), selection: $rule.suggestions, options: [
                    LakkoPickerOption(value: FeedSuggestions.never, label: l10n.t("feed.editor.suggestions.never")),
                    LakkoPickerOption(value: FeedSuggestions.rarely, label: l10n.t("feed.editor.suggestions.rarely")),
                    LakkoPickerOption(value: FeedSuggestions.sometimes, label: l10n.t("feed.editor.suggestions.sometimes")),
                ])
            }

            if let problem {
                Section {
                    Text(problem)
                        .foregroundStyle(Color.lakkoDanger)
                        .accessibilityAddTraits(.isStaticText)
                }
            }
            if let error = model.editError {
                Section {
                    Text(error).foregroundStyle(Color.lakkoDanger)
                }
            }

            if !isNew, case .edit(let id) = editing {
                Section {
                    Button(l10n.t("feed.editor.delete"), role: .destructive) { confirmDelete = true }
                        .lakkoDialog(isPresented: $confirmDelete, title: l10n.t("feed.editor.delete_confirm")) {
                            [.danger(l10n.t("feed.editor.delete")) { model.deleteSaved(id) }, .cancel()]
                        }
                }
            }

            previewSection(problem: problem)
        }
        .navigationTitle(l10n.t(isNew ? "feed.new" : "feed.edit_feed"))
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(true)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button(l10n.t("common.cancel")) { model.editing = nil }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button(l10n.t("common.save")) { model.save(rule, editing: editing) }
                    .disabled(problem != nil)
            }
        }
    }

    private func applyStart(_ value: String) {
        let name = rule.name
        let icon = rule.icon
        var next: FeedRule
        if let key = FeedPresetKey(rawValue: value) {
            next = FeedDraft.preset(key, name: name)
        } else {
            next = FeedDraft.blank(name: name)
        }
        // Starting from a preset keeps the icon the person chose.
        next.icon = icon
        rule = next
        hideText = next.hideWords.joined(separator: "\n")
        onlyText = next.onlyWords.joined(separator: "\n")
    }

    // MARK: - Icon (UX spec section 4.1)

    /// The twelve icons, six to a row, under the name.
    private var iconGrid: some View {
        let columns = Array(repeating: GridItem(.flexible(), spacing: 8), count: 6)
        return LazyVGrid(columns: columns, spacing: 8) {
            ForEach(Array(FeedIcons.all.enumerated()), id: \.offset) { index, entry in
                let selected = Int(rule.icon) == index
                    || (Int(rule.icon) >= FeedIcons.all.count && index == Int(FeedIcons.defaultIndex))
                Button {
                    rule.icon = UInt8(index)
                } label: {
                    Image(systemName: entry.symbol)
                        .font(.body)
                        .frame(maxWidth: .infinity, minHeight: 36)
                        .background(
                            RoundedRectangle(cornerRadius: LakkoRadius.control)
                                .fill(selected ? Color.lakkoAccent : Color.lakkoSurfaceSunken))
                        .foregroundStyle(selected ? Color.lakkoAccentForeground : Color.lakkoText)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(entry.label)
                .accessibilityAddTraits(selected ? [.isSelected] : [])
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(l10n.t("feed.editor.icon"))
    }

    // MARK: - Sources

    private var kinds: Binding<Kinds> {
        Binding(
            get: {
                if rule.groups && rule.channels { return .both }
                return rule.groups ? .groups : .channels
            },
            set: { value in
                rule.groups = value != .channels
                rule.channels = value != .groups
            })
    }

    private func membership(_ idHex: String) -> Binding<Membership> {
        let id = Data(feedHex: idHex) ?? Data()
        return Binding(
            get: {
                if rule.include.contains(id) { return .only }
                if rule.exclude.contains(id) { return .never }
                return .any
            },
            set: { value in
                rule.include.removeAll { $0 == id }
                rule.exclude.removeAll { $0 == id }
                if value == .only { rule.include.append(id) }
                if value == .never { rule.exclude.append(id) }
                // Naming a conversation makes this the person's own list again.
                rule.sourcesStripped = false
            })
    }

    private var sourcesSection: some View {
        Section {
            LakkoSegmentedPicker(
                selection: kinds,
                options: Kinds.allCases.map { LakkoPickerOption(value: $0, label: l10n.t($0.labelKey)) })
            .accessibilityElement(children: .contain)
            .accessibilityLabel(l10n.t("feed.editor.sources"))
            Toggle(l10n.t("feed.editor.include_muted"), isOn: $rule.includeMuted)
            if rule.sourcesStripped {
                Text(l10n.t("feed.editor.stripped_hint"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if !model.sources.isEmpty {
                Text(l10n.t("feed.editor.membership_hint"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                ForEach(model.sources, id: \.idHex) { source in
                    LakkoPicker(
                        source.title, selection: membership(source.key),
                        options: Membership.allCases.map { LakkoPickerOption(value: $0, label: $0.label) })
                    .accessibilityLabel(l10n.t("feed.editor.membership.a11y", texts: ["name": source.title]))
                }
            }
        } header: {
            Text(l10n.t("feed.editor.sources"))
        }
    }

    // MARK: - What counts

    private var weightsSection: some View {
        let maxWeight = Double(feedMaxWeight())
        return Section(l10n.t("feed.editor.what_counts")) {
            ForEach(Self.weightRows, id: \.key) { row in
                VStack(alignment: .leading, spacing: 2) {
                    Text(l10n.t(row.key))
                    Slider(
                        value: Binding(
                            get: { Double(rule.weights[keyPath: row.path]) },
                            set: { rule.weights[keyPath: row.path] = UInt8(max(0, min(maxWeight, $0.rounded()))) }),
                        in: 0...maxWeight
                    ) {
                        Text(l10n.t(row.key))
                    } minimumValueLabel: {
                        Text(l10n.t("feed.editor.slider.min")).font(.caption2)
                    } maximumValueLabel: {
                        Text(l10n.t("feed.editor.slider.max")).font(.caption2)
                    }
                }
            }
            LakkoSegmentedPicker(selection: $rule.halfLife, options: [
                LakkoPickerOption(value: FeedHalfLife.fewHours, label: l10n.t("feed.editor.fade.hours")),
                LakkoPickerOption(value: FeedHalfLife.day, label: l10n.t("feed.editor.fade.day")),
                LakkoPickerOption(value: FeedHalfLife.week, label: l10n.t("feed.editor.fade.week")),
            ])
            .accessibilityElement(children: .contain)
            .accessibilityLabel(l10n.t("feed.editor.fade"))
            Stepper(
                l10n.t("feed.editor.look_back_count", counts: ["count": UInt64(rule.windowDays)]),
                value: Binding(
                    get: { Int(rule.windowDays) },
                    set: { rule.windowDays = UInt8(max(1, min(Int(feedMaxWindowDays()), $0))) }),
                in: 1...Int(feedMaxWindowDays()))
        }
    }

    // MARK: - Filters

    private var filtersSection: some View {
        Section(l10n.t("feed.editor.filters")) {
            Toggle(l10n.t("feed.editor.for_me"), isOn: $rule.forMe)
            Toggle(l10n.t("feed.editor.media_only"), isOn: $rule.mediaOnly)
            Toggle(l10n.t("feed.editor.hide_own"), isOn: $rule.hideOwn)
            Stepper(
                l10n.t("feed.min_reactions_count", counts: ["count": UInt64(rule.minReactions)]),
                value: Binding(
                    get: { Int(rule.minReactions) },
                    set: { rule.minReactions = UInt16(max(0, min(999, $0))) }),
                in: 0...999)
            VStack(alignment: .leading) {
                Text(l10n.t("feed.editor.hide_words"))
                TextEditor(text: $hideText)
                    .frame(minHeight: 60)
                    .onChange(of: hideText) { _, text in rule.hideWords = FeedDraft.parseWords(text) }
            }
            VStack(alignment: .leading) {
                Text(l10n.t("feed.editor.only_words"))
                TextEditor(text: $onlyText)
                    .frame(minHeight: 60)
                    .onChange(of: onlyText) { _, text in rule.onlyWords = FeedDraft.parseWords(text) }
            }
        }
    }

    // MARK: - Preview

    @ViewBuilder
    private func previewSection(problem: String?) -> some View {
        Section(l10n.t("feed.editor.preview")) {
            if problem != nil {
                Text(l10n.t("feed.editor.preview_pending"))
                    .foregroundStyle(Color.lakkoTextMuted)
            } else if let lines = model.preview(rule) {
                if lines.isEmpty {
                    Text(l10n.t("feed.empty.no_match"))
                        .foregroundStyle(Color.lakkoTextMuted)
                } else {
                    ForEach(lines) { line in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(line.source)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(Color.lakkoTextMuted)
                            Text(line.text)
                                .font(.subheadline)
                                .lineLimit(2)
                        }
                    }
                }
            } else {
                Text(l10n.t("feed.editor.preview_pending"))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
    }
}
