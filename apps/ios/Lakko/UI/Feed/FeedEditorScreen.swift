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

    init(model: FeedViewModel, editing: FeedEditing, initial: FeedRule) {
        self.model = model
        self.editing = editing
        self.initial = initial
        _rule = State(initialValue: initial)
        _hideText = State(initialValue: initial.hideWords.joined(separator: "\n"))
        _onlyText = State(initialValue: initial.onlyWords.joined(separator: "\n"))
    }

    private var isNew: Bool { editing == .new }

    private enum Kinds: String, CaseIterable {
        case both = "Both"
        case groups = "All groups"
        case channels = "All channels"
    }

    private enum Membership: String, CaseIterable {
        case any = "—"
        case only = "Only"
        case never = "Never"
    }

    private static let weightRows: [(label: String, path: WritableKeyPath<FeedWeights, UInt8>)] = [
        ("How recent", \.recency),
        ("Reactions", \.reactions),
        ("Replies", \.replies),
        ("From your contacts", \.fromContacts),
        ("Mentions you", \.mentionsMe),
        ("Has photos or video", \.hasMedia),
    ]

    var body: some View {
        let problem = FeedDraft.problem(rule)
        Form {
            Section {
                TextField("Name", text: $rule.name)
                iconGrid
                if isNew {
                    Picker("Start from", selection: $startFrom) {
                        Text("Blank").tag("blank")
                        ForEach(FeedPresetKey.allCases, id: \.self) { key in
                            Text(key.label).tag(key.rawValue)
                        }
                    }
                    .onChange(of: startFrom) { _, value in applyStart(value) }
                }
            }

            sourcesSection
            weightsSection
            filtersSection

            Section("Suggestions") {
                Picker("How often a suggested channel appears", selection: $rule.suggestions) {
                    Text("Never").tag(FeedSuggestions.never)
                    Text("Rarely").tag(FeedSuggestions.rarely)
                    Text("Sometimes").tag(FeedSuggestions.sometimes)
                }
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
                    Button("Delete feed", role: .destructive) { confirmDelete = true }
                        .confirmationDialog(
                            "Delete this feed?", isPresented: $confirmDelete, titleVisibility: .visible
                        ) {
                            Button("Delete feed", role: .destructive) { model.deleteSaved(id) }
                        }
                }
            }

            previewSection(problem: problem)
        }
        .navigationTitle(isNew ? "New feed" : "Edit feed")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(true)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { model.editing = nil }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save") { model.save(rule, editing: editing) }
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
        .accessibilityLabel("Icon")
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
            Picker("Sources", selection: kinds) {
                ForEach(Kinds.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            Toggle("Include muted conversations", isOn: $rule.includeMuted)
            if rule.sourcesStripped {
                Text("Shared feeds arrive without specific conversations. Pick some below to make it yours.")
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if !model.sources.isEmpty {
                Text("\"Only\" limits the feed to the conversations marked so; \"Never\" leaves one out.")
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                ForEach(model.sources, id: \.idHex) { source in
                    Picker(source.title, selection: membership(source.idHex)) {
                        ForEach(Membership.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                    }
                    .accessibilityLabel("\(source.title) in this feed")
                }
            }
        } header: {
            Text("Sources")
        }
    }

    // MARK: - What counts

    private var weightsSection: some View {
        let maxWeight = Double(feedMaxWeight())
        return Section("What counts") {
            ForEach(Self.weightRows, id: \.label) { row in
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.label)
                    Slider(
                        value: Binding(
                            get: { Double(rule.weights[keyPath: row.path]) },
                            set: { rule.weights[keyPath: row.path] = UInt8(max(0, min(maxWeight, $0.rounded()))) }),
                        in: 0...maxWeight
                    ) {
                        Text(row.label)
                    } minimumValueLabel: {
                        Text("Ignore").font(.caption2)
                    } maximumValueLabel: {
                        Text("A lot").font(.caption2)
                    }
                }
            }
            Picker("How fast things fade", selection: $rule.halfLife) {
                Text("A few hours").tag(FeedHalfLife.fewHours)
                Text("A day").tag(FeedHalfLife.day)
                Text("A week").tag(FeedHalfLife.week)
            }
            .pickerStyle(.segmented)
            Stepper(
                "Look back \(rule.windowDays) day\(rule.windowDays == 1 ? "" : "s")",
                value: Binding(
                    get: { Int(rule.windowDays) },
                    set: { rule.windowDays = UInt8(max(1, min(Int(feedMaxWindowDays()), $0))) }),
                in: 1...Int(feedMaxWindowDays()))
        }
    }

    // MARK: - Filters

    private var filtersSection: some View {
        Section("Filters") {
            Toggle("Only what mentions me, replies to me or reacts to mine", isOn: $rule.forMe)
            Toggle("Only with photos or video", isOn: $rule.mediaOnly)
            Toggle("Hide my own messages", isOn: $rule.hideOwn)
            Stepper(
                "At least \(rule.minReactions) reactions",
                value: Binding(
                    get: { Int(rule.minReactions) },
                    set: { rule.minReactions = UInt16(max(0, min(999, $0))) }),
                in: 0...999)
            VStack(alignment: .leading) {
                Text("Hide words (one per line)")
                TextEditor(text: $hideText)
                    .frame(minHeight: 60)
                    .onChange(of: hideText) { _, text in rule.hideWords = FeedDraft.parseWords(text) }
            }
            VStack(alignment: .leading) {
                Text("Only these words (one per line)")
                TextEditor(text: $onlyText)
                    .frame(minHeight: 60)
                    .onChange(of: onlyText) { _, text in rule.onlyWords = FeedDraft.parseWords(text) }
            }
        }
    }

    // MARK: - Preview

    @ViewBuilder
    private func previewSection(problem: String?) -> some View {
        Section("Preview") {
            if problem != nil {
                Text("The preview appears once the feed can be saved.")
                    .foregroundStyle(Color.lakkoTextMuted)
            } else if let lines = model.preview(rule) {
                if lines.isEmpty {
                    Text("No messages match this feed.")
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
                Text("The preview appears once the feed can be saved.")
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
    }
}
