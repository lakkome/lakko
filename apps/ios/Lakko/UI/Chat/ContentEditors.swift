// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

// ADR-0092 / `docs/ux/attachments.md` — the paperclip menu (section 2) and
// Lakko's own editors for a poll (section 7.1) and a checklist (section 8.1),
// laid out per section 7.1a.

// MARK: - The menu

/// UX section 2 — the one `LakkoMenu` the paperclip opens on a phone: an
/// in-app bottom sheet of 48-point rows, icon and label start-aligned, no
/// heading, no Cancel row and no separators (UX 2.1, 2.2). Rows come from
/// the core (`attachmentMenu`); a disabled row reads its reason.
struct AttachmentMenuSheet: View {
    let rows: [AttachmentMenuRow]
    let onPick: (AttachmentMenuItem) -> Void
    @ObservedObject private var l10n = Localization.shared

    static let rowHeight: CGFloat = 48

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                Button { onPick(row.item) } label: {
                    HStack(spacing: 12) {
                        LkIconView(AttachmentMenuCopy.icon(row.item), size: 22)
                            .frame(width: 24)
                            .foregroundStyle(Color.lakkoAccent)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(l10n.t(AttachmentMenuCopy.labelKey(row.item)))
                                .foregroundStyle(Color.lakkoText)
                            if let reason = row.disabled {
                                Text(l10n.t(AttachmentMenuCopy.reasonKey(reason)))
                                    .font(.footnote)
                                    .foregroundStyle(Color.lakkoTextMuted)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 16)
                    .frame(minHeight: Self.rowHeight)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(row.disabled != nil)
                .opacity(row.disabled == nil ? 1 : 0.45)
                .accessibilityLabel(accessibility(row))
            }
        }
        .padding(.top, 20)
        .padding(.bottom, 8)
        .frame(maxHeight: .infinity, alignment: .top)
        .presentationDetents([.height(Self.sheetHeight(rows))])
        .presentationDragIndicator(.visible)
    }

    /// Rows, the extra line a disabled row's reason takes, the room for the
    /// drag indicator at the top and a little at the bottom.
    static func sheetHeight(_ rows: [AttachmentMenuRow]) -> CGFloat {
        let reasons = rows.filter { $0.disabled != nil }.count
        return CGFloat(rows.count) * rowHeight + CGFloat(reasons) * 16 + 28
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

// MARK: - Editor rows (UX 7.1a)

/// One entry of the poll options or the checklist items.
protocol EditorRowField: Identifiable, Equatable where ID == UUID {
    var id: UUID { get }
    var text: String { get set }
}

/// UX section 7.1a — what the rows of an editor do, independent of the view:
/// add up to the maximum, remove down to the minimum, Return moves on (and on
/// the last row adds one), a row moves by one position.
enum EditorRows {
    /// The `Add option` / `Add item` row is shown below the maximum only.
    static func canAdd(count: Int, max: Int) -> Bool { count < max }

    /// The remove (x) button is shown above the minimum only.
    static func canRemove(count: Int, min: Int) -> Bool { count > min }

    enum ReturnAction: Equatable {
        /// Focus the row at this index.
        case focus(Int)
        /// Add a row at the end and focus it.
        case append
        /// The last row at the maximum: leave the field.
        case done
    }

    static func onReturn(at index: Int, count: Int, max: Int) -> ReturnAction {
        if index + 1 < count { return .focus(index + 1) }
        return canAdd(count: count, max: max) ? .append : .done
    }

    /// Moves the row at `index` by `delta` (-1 up, +1 down). Returns the new
    /// index, or nil when the row is already at that end.
    @discardableResult
    static func move<T>(_ list: inout [T], at index: Int, by delta: Int) -> Int? {
        let target = index + delta
        guard list.indices.contains(index), list.indices.contains(target) else { return nil }
        list.swapAt(index, target)
        return target
    }

    /// The index a row lands at after `List.onMove(fromOffsets:toOffset:)`.
    static func destination(from source: Int, toOffset offset: Int) -> Int {
        offset > source ? offset - 1 : offset
    }
}

/// The rows of one editor section: single-line fields, a remove (x) inside
/// the end of each field above the minimum, the platform drag handle (the
/// list is in edit mode), VoiceOver move actions, and the accent `Add` row
/// below them until the maximum.
struct EditorRowsSection<Field: EditorRowField, Leading: View>: View {
    let header: String
    @Binding var rows: [Field]
    let minRows: Int
    let maxRows: Int
    let maxBytes: UInt32
    let placeholder: (Int) -> String
    let removeLabel: (Int) -> String
    let addLabel: String
    let makeRow: () -> Field
    var focused: FocusState<UUID?>.Binding
    var onRemove: (Field) -> Void = { _ in }
    @ViewBuilder let leading: (Field) -> Leading

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Section {
            ForEach($rows) { $row in
                let index = rows.firstIndex { $0.id == row.id } ?? 0
                HStack(spacing: 10) {
                    leading(row)
                    TextField(placeholder(index), text: Binding(
                        get: { row.text },
                        set: { row.text = ContentText.clamp($0, bytes: maxBytes) }))
                        .focused(focused, equals: row.id)
                        .submitLabel(.next)
                        .onSubmit { submitted(at: index) }
                    if EditorRows.canRemove(count: rows.count, min: minRows) {
                        Button { remove(row) } label: {
                            LkIconView(.xmarkCircleFill, size: 18)
                                .foregroundStyle(Color.lakkoTextMuted)
                                // One text line high, so a row does not
                                // grow when its remove button appears; the
                                // touch area reaches past it.
                                .frame(width: 28, height: 20)
                                .contentShape(Rectangle().inset(by: -10))
                        }
                        .buttonStyle(.borderless)
                        .accessibilityLabel(removeLabel(index))
                    }
                }
                .listRowBackground(Color.lakkoSurface)
                .accessibilityElement(children: .contain)
                .accessibilityAction(named: l10n.t("poll.editor.move_up")) { move(at: index, by: -1) }
                .accessibilityAction(named: l10n.t("poll.editor.move_down")) { move(at: index, by: 1) }
            }
            .onMove { source, offset in
                rows.move(fromOffsets: source, toOffset: offset)
                if let first = source.first {
                    announce(EditorRows.destination(from: first, toOffset: offset))
                }
            }
            if EditorRows.canAdd(count: rows.count, max: maxRows) {
                Button(action: append) {
                    HStack(spacing: 10) {
                        LkIconView(.plus, size: 18)
                        Text(addLabel)
                    }
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .listRowBackground(Color.lakkoSurface)
            }
        } header: {
            Text(header)
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
                .textCase(nil)
        }
    }

    private func submitted(at index: Int) {
        switch EditorRows.onReturn(at: index, count: rows.count, max: maxRows) {
        case .focus(let next): focused.wrappedValue = rows[next].id
        case .append: append()
        case .done: focused.wrappedValue = nil
        }
    }

    private func append() {
        guard EditorRows.canAdd(count: rows.count, max: maxRows) else { return }
        let row = makeRow()
        rows.append(row)
        // The field exists on the next pass; focus it then.
        DispatchQueue.main.async { focused.wrappedValue = row.id }
    }

    private func remove(_ row: Field) {
        guard EditorRows.canRemove(count: rows.count, min: minRows) else { return }
        rows.removeAll { $0.id == row.id }
        onRemove(row)
    }

    private func move(at index: Int, by delta: Int) {
        if let landed = EditorRows.move(&rows, at: index, by: delta) { announce(landed) }
    }

    private func announce(_ index: Int) {
        UIAccessibility.post(
            notification: .announcement, argument: l10n.t("editor.moved", texts: ["n": "\(index + 1)"]))
    }
}

/// A switch with its explaining line under the label (UX 7.1a).
struct EditorSwitchRow: View {
    let title: String
    var note: String?
    @Binding var isOn: Bool

    var body: some View {
        Toggle(isOn: $isOn) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).foregroundStyle(Color.lakkoText)
                if let note {
                    Text(note).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                }
            }
        }
        .tint(Color.lakkoAccent)
        .listRowBackground(Color.lakkoSurface)
    }
}

/// The editor frame: the list on the background colour, the close (x) at
/// the start of the bar, one full-width primary button at the bottom.
private struct EditorChrome: ViewModifier {
    let title: String
    let primary: String
    let primaryEnabled: Bool
    let onClose: () -> Void
    let onPrimary: () -> Void
    @ObservedObject private var l10n = Localization.shared

    func body(content: Content) -> some View {
        content
            .environment(\.editMode, .constant(.active))
            .scrollContentBackground(.hidden)
            .background(Color.lakkoBackground)
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(action: onClose) {
                        LkIconView(.xmark, size: 18).foregroundStyle(Color.lakkoText)
                    }
                    .accessibilityLabel(l10n.t("common.close"))
                }
            }
            .safeAreaInset(edge: .bottom) {
                AuthPrimaryButton(title: primary, disabled: !primaryEnabled, action: onPrimary)
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                    .padding(.bottom, 8)
                    .background(Color.lakkoBackground)
            }
    }
}

// MARK: - Poll editor

/// UX section 7.1 / 7.1a — the poll editor, full screen.
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

    struct OptionField: EditorRowField {
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
            List {
                Section {
                    TextField(l10n.t("poll.editor.question"), text: Binding(
                        get: { question },
                        set: { question = ContentText.clamp($0, bytes: limits.maxPollQuestionBytes) }),
                        axis: .vertical)
                        .listRowBackground(Color.lakkoSurface)
                }
                EditorRowsSection(
                    header: l10n.t("poll.editor.options"),
                    rows: $options,
                    minRows: Int(limits.minPollOptions),
                    maxRows: Int(limits.maxPollOptions),
                    maxBytes: limits.maxPollOptionBytes,
                    placeholder: { l10n.t("poll.editor.option", texts: ["n": "\($0 + 1)"]) },
                    removeLabel: { l10n.t("poll.editor.remove_option", texts: ["n": "\($0 + 1)"]) },
                    addLabel: l10n.t("poll.editor.add_option"),
                    makeRow: { OptionField() },
                    focused: $focused,
                    onRemove: { if correct == $0.id { correct = nil } }
                ) { option in
                    if quiz {
                        Button {
                            correct = option.id
                        } label: {
                            LkIconView(correct == option.id ? .radioOn : .circle, size: 20)
                                .foregroundStyle(correct == option.id ? Color.lakkoSuccess : Color.lakkoTextMuted)
                        }
                        .buttonStyle(.borderless)
                        .accessibilityLabel(l10n.t("poll.editor.correct"))
                        .accessibilityAddTraits(correct == option.id ? .isSelected : [])
                    }
                }
                Section {
                    if anonymity == .switchDefaultOn {
                        EditorSwitchRow(
                            title: l10n.t("poll.editor.anonymous"),
                            note: l10n.t(anonymous ? "poll.editor.anonymous_on" : "poll.editor.anonymous_off"),
                            isOn: $anonymous)
                    } else {
                        Text(l10n.t(effectiveAnonymous ? "poll.editor.anonymous_on" : "poll.editor.anonymous_off"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .listRowBackground(Color.lakkoSurface)
                    }
                    EditorSwitchRow(title: l10n.t("poll.editor.multiple"), isOn: $multiple)
                        .disabled(quiz)
                    EditorSwitchRow(title: l10n.t("poll.editor.quiz"), isOn: Binding(
                        get: { quiz }, set: { quiz = $0; if $0 { multiple = false } }))
                    if quiz {
                        TextField(l10n.t("poll.editor.explanation"), text: Binding(
                            get: { explanation },
                            set: { explanation = ContentText.clamp($0, bytes: limits.maxPollExplanationBytes) }),
                            axis: .vertical)
                            .listRowBackground(Color.lakkoSurface)
                    }
                    EditorSwitchRow(title: l10n.t("poll.editor.close_auto"), isOn: $closeAuto)
                    if closeAuto {
                        LakkoPicker(
                            l10n.t("poll.editor.close_auto"), selection: $closeAfter,
                            options: CloseAfter.allCases.map { LakkoPickerOption(value: $0, label: l10n.t($0.labelKey)) })
                            .listRowBackground(Color.lakkoSurface)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .modifier(EditorChrome(
                title: l10n.t("poll.editor.title"), primary: l10n.t("poll.editor.create"),
                primaryEnabled: canCreate,
                onClose: { if hasInput { confirmDiscard = true } else { onCancel() } },
                onPrimary: { if canCreate { create() } }))
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

/// UX section 8.1 / 7.1a — the checklist editor; also `Edit checklist`
/// (ticks stay with their items: an existing item keeps its id).
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
    @FocusState private var focused: UUID?

    private let limits = contentLimits()

    struct ItemField: EditorRowField {
        let id = UUID()
        var itemId: UInt32?
        var text = ""
    }

    /// UX 8.1 — at least one item row stays.
    static let minItems = 1

    private var filled: [ItemField] { items.filter { !ContentText.trimmed($0.text).isEmpty } }
    private var canSave: Bool { !ContentText.trimmed(title).isEmpty && !filled.isEmpty }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    TextField(l10n.t("checklist.editor.name"), text: Binding(
                        get: { title },
                        set: { title = ContentText.clamp($0, bytes: limits.maxChecklistTitleBytes) }),
                        axis: .vertical)
                        .listRowBackground(Color.lakkoSurface)
                }
                EditorRowsSection(
                    header: l10n.t("checklist.editor.items"),
                    rows: $items,
                    minRows: Self.minItems,
                    maxRows: Int(limits.maxChecklistItems),
                    maxBytes: limits.maxChecklistItemBytes,
                    placeholder: { l10n.t("checklist.editor.item", texts: ["n": "\($0 + 1)"]) },
                    removeLabel: { l10n.t("checklist.editor.remove_item", texts: ["n": "\($0 + 1)"]) },
                    addLabel: l10n.t("checklist.editor.add_item"),
                    makeRow: { ItemField() },
                    focused: $focused
                ) { _ in EmptyView() }
                if showsSwitches {
                    Section {
                        EditorSwitchRow(title: l10n.t("checklist.others_add"), isOn: $othersAdd)
                        EditorSwitchRow(title: l10n.t("checklist.others_mark"), isOn: $othersMark)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .modifier(EditorChrome(
                title: l10n.t(initial == nil ? "checklist.editor.title" : "checklist.editor.edit_title"),
                primary: l10n.t(initial == nil ? "checklist.editor.create" : "checklist.editor.save"),
                primaryEnabled: canSave,
                onClose: {
                    if initial == nil, !title.isEmpty || !filled.isEmpty { confirmDiscard = true } else { onCancel() }
                },
                onPrimary: { if canSave { save() } }))
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
        // No empty trailing row (UX 7.1a); `Add item` adds one.
        let existing = initial.items.map { ItemField(itemId: $0.id, text: $0.text) }
        items = existing.isEmpty ? [ItemField()] : existing
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
