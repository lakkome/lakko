// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

// ADR-0092 / `docs/ux/attachments.md` — the paperclip menu (section 2) and
// Lakko's own editors for a poll (section 7.1) and a checklist (section 8.1).

// MARK: - The menu

/// UX section 2 — the one `LakkoMenu` the paperclip opens on a phone: an
/// in-app bottom sheet of 52-point rows, icon and label, no heading and no
/// Cancel row; a hairline band between the sections. Rows come from the
/// core (`attachmentMenu`); a disabled row reads its reason.
struct AttachmentMenuSheet: View {
    let rows: [AttachmentMenuRow]
    let onPick: (AttachmentMenuItem) -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                if index > 0, rows[index - 1].section != row.section {
                    Rectangle().fill(Color.lakkoBorder).frame(height: 1).padding(.vertical, 4)
                }
                Button { onPick(row.item) } label: {
                    HStack(spacing: 14) {
                        LkIconView(AttachmentMenuCopy.icon(row.item), size: 20)
                            .frame(width: 28)
                            .foregroundStyle(Color.lakkoAccent)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(l10n.t(AttachmentMenuCopy.labelKey(row.item)))
                                .foregroundStyle(Color.lakkoText)
                            if let reason = row.disabled {
                                Text(l10n.t(AttachmentMenuCopy.reasonKey(reason)))
                                    .font(.footnote)
                                    .foregroundStyle(Color.lakkoTextMuted)
                            }
                        }
                        Spacer()
                    }
                    .padding(.horizontal, 20)
                    .frame(minHeight: 52)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(row.disabled != nil)
                .opacity(row.disabled == nil ? 1 : 0.45)
                .accessibilityLabel(accessibility(row))
            }
        }
        .padding(.vertical, 12)
        .presentationDetents([.height(CGFloat(rows.count) * 52 + 64)])
        .presentationDragIndicator(.visible)
    }

    private func accessibility(_ row: AttachmentMenuRow) -> String {
        let label = l10n.t(AttachmentMenuCopy.labelKey(row.item))
        guard let reason = row.disabled else { return label }
        return label + ", " + l10n.t(AttachmentMenuCopy.reasonKey(reason))
    }
}

/// The copy and icon of each menu row (UX sections 2.2 and 14).
enum AttachmentMenuCopy {
    static func labelKey(_ item: AttachmentMenuItem) -> String {
        switch item {
        case .photoOrVideo: return "attach.menu.photo_video"
        case .camera: return "attach.menu.camera"
        case .file: return "attach.menu.file"
        case .audio: return "attach.menu.audio"
        case .poll: return "attach.menu.poll"
        case .checklist: return "attach.menu.checklist"
        case .location: return "attach.menu.location"
        case .contact: return "attach.menu.contact"
        case .article: return "attach.menu.article"
        }
    }

    static func icon(_ item: AttachmentMenuItem) -> LkIcon {
        switch item {
        case .photoOrVideo: return .photoStack
        case .camera: return .camera
        case .file: return .doc
        case .audio: return .musicNote
        case .poll: return .chartBar
        case .checklist: return .checklist
        case .location: return .location
        case .contact: return .person
        case .article: return .docText
        }
    }

    static func reasonKey(_ reason: AttachmentMenuDisabled) -> String {
        switch reason {
        case .noCamera: return "attach.menu.no_camera"
        case .cameraInCall: return "attach.menu.camera_in_call"
        case .noLocation: return "attach.menu.no_location"
        }
    }
}

// MARK: - Bounded text

/// Text fields bounded in UTF-8 bytes (the wire bounds are bytes).
enum ContentText {
    static func clamp(_ text: String, bytes: UInt32) -> String {
        var t = text
        while t.utf8.count > Int(bytes) { t.removeLast() }
        return t
    }

    static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

// MARK: - Poll editor

/// UX section 7.1 — the poll editor, full screen.
struct PollEditorScreen: View {
    let anonymity: PollAnonymity
    let onCreate: (ContentDraft) -> Void
    let onCancel: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var question = ""
    @State private var options: [OptionField] = [OptionField(), OptionField()]
    @State private var anonymous = true
    @State private var multiple = false
    @State private var quiz = false
    @State private var correct: UUID?
    @State private var explanation = ""
    @State private var closeAuto = false
    @State private var closeAfter: CloseAfter = .day
    @State private var confirmDiscard = false
    @FocusState private var focused: UUID?

    private let limits = contentLimits()

    struct OptionField: Identifiable, Equatable {
        let id = UUID()
        var text = ""
    }

    enum CloseAfter: Int64, CaseIterable, Hashable {
        case hour = 3_600_000
        case day = 86_400_000
        case week = 604_800_000

        var labelKey: String {
            switch self {
            case .hour: return "poll.editor.close_1h"
            case .day: return "poll.editor.close_1d"
            case .week: return "poll.editor.close_1w"
            }
        }
    }

    var filledOptions: [String] { options.map { ContentText.trimmed($0.text) }.filter { !$0.isEmpty } }

    var canCreate: Bool {
        !ContentText.trimmed(question).isEmpty && filledOptions.count >= Int(limits.minPollOptions)
            && (!quiz || correctIndex != nil)
    }

    private var correctIndex: Int? {
        guard let correct else { return nil }
        let filled = options.filter { !ContentText.trimmed($0.text).isEmpty }
        return filled.firstIndex { $0.id == correct }
    }

    private var hasInput: Bool {
        !question.isEmpty || options.contains { !$0.text.isEmpty } || !explanation.isEmpty
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(l10n.t("poll.editor.question"), text: Binding(
                        get: { question },
                        set: { question = ContentText.clamp($0, bytes: limits.maxPollQuestionBytes) }),
                        axis: .vertical)
                }
                Section(l10n.t("poll.editor.options")) {
                    ForEach($options) { $option in
                        let index = options.firstIndex { $0.id == option.id } ?? 0
                        HStack {
                            if quiz {
                                Button {
                                    correct = option.id
                                } label: {
                                    LkIconView(correct == option.id ? .radioOn : .circle, size: 20)
                                        .foregroundStyle(correct == option.id ? Color.lakkoSuccess : Color.lakkoTextMuted)
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel(l10n.t("poll.editor.correct"))
                            }
                            TextField(l10n.t("poll.editor.option", texts: ["n": "\(index + 1)"]), text: Binding(
                                get: { option.text },
                                set: { option.text = ContentText.clamp($0, bytes: limits.maxPollOptionBytes) }))
                                .focused($focused, equals: option.id)
                        }
                    }
                    .onMove { options.move(fromOffsets: $0, toOffset: $1) }
                }
                Section {
                    if anonymity == .switchDefaultOn {
                        Toggle(l10n.t("poll.editor.anonymous"), isOn: $anonymous)
                    }
                    Text(l10n.t(effectiveAnonymous ? "poll.editor.anonymous_on" : "poll.editor.anonymous_off"))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                    Toggle(l10n.t("poll.editor.multiple"), isOn: $multiple).disabled(quiz)
                    Toggle(l10n.t("poll.editor.quiz"), isOn: Binding(
                        get: { quiz }, set: { quiz = $0; if $0 { multiple = false } }))
                    if quiz {
                        TextField(l10n.t("poll.editor.explanation"), text: Binding(
                            get: { explanation },
                            set: { explanation = ContentText.clamp($0, bytes: limits.maxPollExplanationBytes) }),
                            axis: .vertical)
                    }
                    Toggle(l10n.t("poll.editor.close_auto"), isOn: $closeAuto)
                    if closeAuto {
                        LakkoPicker(
                            l10n.t("poll.editor.close_auto"), selection: $closeAfter,
                            options: CloseAfter.allCases.map { LakkoPickerOption(value: $0, label: l10n.t($0.labelKey)) })
                    }
                }
            }
            .environment(\.editMode, .constant(.active))
            .tint(Color.lakkoAccent)
            .navigationTitle(l10n.t("poll.editor.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) {
                        if hasInput { confirmDiscard = true } else { onCancel() }
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(l10n.t("poll.editor.create")) { create() }.disabled(!canCreate)
                }
            }
            .onChange(of: options) { _, new in grow(new) }
            .onChange(of: focused) { old, _ in shrink(leaving: old) }
            .lakkoDialog(isPresented: $confirmDiscard, title: l10n.t("poll.editor.discard")) {
                [.danger(l10n.t("editor.discard")) { onCancel() },
                 .secondary(l10n.t("editor.keep_editing")) {}]
            }
        }
    }

    private var effectiveAnonymous: Bool {
        switch anonymity {
        case .alwaysAnonymous: return true
        case .notOffered: return false
        case .switchDefaultOn: return anonymous
        }
    }

    /// A new field appears when the last one is typed into (up to the cap).
    private func grow(_ list: [OptionField]) {
        if let last = list.last, !last.text.isEmpty, list.count < Int(limits.maxPollOptions) {
            options.append(OptionField())
        }
    }

    /// A field emptied and left is removed (at least two stay).
    private func shrink(leaving id: UUID?) {
        guard let id, let i = options.firstIndex(where: { $0.id == id }),
              options[i].text.isEmpty, options.count > Int(limits.minPollOptions), i != options.count - 1 else { return }
        options.remove(at: i)
    }

    private func create() {
        let opts = filledOptions
        let quizValue: PollQuiz? = quiz
            ? correctIndex.map { PollQuiz(correct: UInt8($0), explanation: ContentText.trimmed(explanation)) }
            : nil
        let closeAt = closeAuto ? ChatRepository.nowMs() + closeAfter.rawValue : nil
        onCreate(.poll(
            question: ContentText.trimmed(question), options: Array(opts.prefix(Int(limits.maxPollOptions))),
            multiple: multiple && !quiz, quiz: quizValue, closeAtMs: closeAt, anonymous: effectiveAnonymous))
    }
}

// MARK: - Checklist editor

/// UX section 8.1 — the checklist editor; also `Edit checklist` (ticks stay
/// with their items: an existing item keeps its id).
struct ChecklistEditorScreen: View {
    let initial: ChecklistBody?
    /// UX 8.1 — the two switches are not shown in a channel.
    var showsSwitches = true
    let onDone: (ChecklistBody) -> Void
    let onCancel: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var title = ""
    @State private var items: [ItemField] = [ItemField()]
    @State private var othersAdd = false
    @State private var othersMark = true
    @State private var loaded = false
    @State private var confirmDiscard = false

    private let limits = contentLimits()

    struct ItemField: Identifiable, Equatable {
        let id = UUID()
        var itemId: UInt32?
        var text = ""
    }

    private var filled: [ItemField] { items.filter { !ContentText.trimmed($0.text).isEmpty } }
    private var canSave: Bool { !ContentText.trimmed(title).isEmpty && !filled.isEmpty }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(l10n.t("checklist.editor.name"), text: Binding(
                        get: { title },
                        set: { title = ContentText.clamp($0, bytes: limits.maxChecklistTitleBytes) }),
                        axis: .vertical)
                }
                Section {
                    ForEach($items) { $item in
                        let index = items.firstIndex { $0.id == item.id } ?? 0
                        TextField(l10n.t("checklist.editor.item", texts: ["n": "\(index + 1)"]), text: Binding(
                            get: { item.text },
                            set: { item.text = ContentText.clamp($0, bytes: limits.maxChecklistItemBytes) }))
                    }
                    .onMove { items.move(fromOffsets: $0, toOffset: $1) }
                    .onDelete { offsets in
                        items.remove(atOffsets: offsets)
                        if items.isEmpty { items = [ItemField()] }
                    }
                }
                if showsSwitches {
                    Section {
                        Toggle(l10n.t("checklist.others_add"), isOn: $othersAdd)
                        Toggle(l10n.t("checklist.others_mark"), isOn: $othersMark)
                    }
                }
            }
            .environment(\.editMode, .constant(.active))
            .tint(Color.lakkoAccent)
            .navigationTitle(l10n.t(initial == nil ? "checklist.editor.title" : "checklist.editor.edit_title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) {
                        if initial == nil, !title.isEmpty || !filled.isEmpty { confirmDiscard = true } else { onCancel() }
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(l10n.t(initial == nil ? "checklist.editor.create" : "checklist.editor.save")) { save() }
                        .disabled(!canSave)
                }
            }
            .onChange(of: items) { _, list in
                if let last = list.last, !last.text.isEmpty, list.count < Int(limits.maxChecklistItems) {
                    items.append(ItemField())
                }
            }
            .onAppear(perform: load)
            .lakkoDialog(isPresented: $confirmDiscard, title: l10n.t("checklist.editor.discard")) {
                [.danger(l10n.t("editor.discard")) { onCancel() },
                 .secondary(l10n.t("editor.keep_editing")) {}]
            }
        }
    }

    private func load() {
        guard !loaded else { return }
        loaded = true
        guard let initial else { return }
        title = initial.title
        othersAdd = initial.othersAdd
        othersMark = initial.othersMark
        items = initial.items.map { ItemField(itemId: $0.id, text: $0.text) } + [ItemField()]
    }

    private func save() {
        let fields = Array(filled.prefix(Int(limits.maxChecklistItems)))
        var next = (fields.compactMap(\.itemId).max() ?? 0)
        let built = fields.map { field -> ChecklistItem in
            if let id = field.itemId { return ChecklistItem(id: id, text: ContentText.trimmed(field.text)) }
            next += 1
            return ChecklistItem(id: next, text: ContentText.trimmed(field.text))
        }
        onDone(ChecklistBody(
            title: ContentText.trimmed(title), othersAdd: showsSwitches && othersAdd,
            othersMark: showsSwitches && othersMark, items: built))
    }
}
