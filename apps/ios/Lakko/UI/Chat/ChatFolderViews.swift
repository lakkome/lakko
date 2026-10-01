// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// The chat-folder screens (`docs/ux/chat-folders.md`): the folder bar, the
/// editor, the chat picker and Settings > Chat folders.
///
/// Every decision about which chats a folder holds is the shared rule behind
/// ``ChatFolders``; every edit is the registers ``ChatFolders`` builds, handed
/// to ``DeviceGroupSyncRepository/applyFolderChanges(_:)``. These views only
/// draw and collect taps.

// MARK: - Editor request

/// What the editor sheet opens on.
struct FolderEditorRequest: Identifiable {
    let id = UUID()
    /// The folder being edited, or `nil` for a new one.
    let folder: FolderView?
    /// A chat already added to a new folder (Add to folder > New folder...).
    let prefill: FolderConversationRef?
    /// The merged list, for names, the picker and the live match count.
    let rows: [ConversationListRow]

    init(folder: FolderView?, prefill: FolderConversationRef? = nil, rows: [ConversationListRow]) {
        self.folder = folder
        self.prefill = prefill
        self.rows = rows
    }
}

// MARK: - The folder bar (section 2)

/// The strip of tabs above the list: All chats first, then the person's
/// folders in their order, each with its unread-conversation count.
struct FolderBar: View {
    let folders: [FolderView]
    @Binding var selection: String
    /// Folder id hex -> unread conversations, muted ones not counted.
    let counts: [String: Int]
    /// Whether anything at all is unread, for All chats' Mark all as read.
    let anyUnread: Bool
    let onEdit: (FolderView) -> Void
    let onMarkAllRead: (FolderView?) -> Void
    let onReorder: () -> Void
    let onEditFolders: () -> Void
    let onDelete: (FolderView) -> Void

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 20) {
                    tab(id: "", name: "All chats", count: 0, folder: nil)
                    ForEach(folders, id: \.folderId) { folder in
                        let id = ChatFolders.hex(folder.folderId)
                        tab(id: id, name: folder.meta.name, count: counts[id] ?? 0, folder: folder)
                    }
                }
                .padding(.horizontal, 16)
            }
            .onAppear { proxy.scrollTo(selection, anchor: .center) }
            .onChange(of: selection) { _, new in
                withAnimation { proxy.scrollTo(new, anchor: .center) }
            }
        }
        .padding(.top, 4)
        .background(Color.lakkoBackground)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 0.5)
        }
    }

    private func tab(id: String, name: String, count: Int, folder: FolderView?) -> some View {
        let selected = selection == id
        return Button {
            selection = id
        } label: {
            VStack(spacing: 6) {
                HStack(spacing: 4) {
                    Text(name)
                        .font(.subheadline.weight(selected ? .semibold : .regular))
                        .lineLimit(1)
                    if count > 0 {
                        Text("\(count)")
                            .font(.caption.weight(.semibold))
                    }
                }
                .foregroundStyle(selected ? Color.lakkoAccent : Color.lakkoTextMuted)
                Rectangle()
                    .fill(selected ? Color.lakkoAccent : Color.clear)
                    .frame(height: 2)
            }
            .fixedSize(horizontal: true, vertical: false)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .id(id)
        .contextMenu { menu(folder: folder, count: count) }
        .accessibilityLabel(ChatFolders.tabAccessibilityLabel(name: name, count: count))
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
    }

    /// Section 2.2 — the tab menu, in the spec's order.
    @ViewBuilder
    private func menu(folder: FolderView?, count: Int) -> some View {
        if let folder {
            Button { onEdit(folder) } label: {
                Label("Edit folder", systemImage: "pencil")
            }
            if count > 0 {
                Button { onMarkAllRead(folder) } label: {
                    Label("Mark all as read", systemImage: "checkmark.message")
                }
            }
            Button(action: onReorder) {
                Label("Reorder folders", systemImage: "arrow.up.arrow.down")
            }
            Button(role: .destructive) { onDelete(folder) } label: {
                Label("Delete folder", systemImage: "trash")
            }
        } else {
            if anyUnread {
                Button { onMarkAllRead(nil) } label: {
                    Label("Mark all as read", systemImage: "checkmark.message")
                }
            }
            Button(action: onEditFolders) {
                Label("Edit folders", systemImage: "folder")
            }
        }
    }
}

// MARK: - The editor (section 3)

struct FolderEditorScreen: View {
    let request: FolderEditorRequest
    /// The folders in bar order: their count is the 20-folder limit on a new
    /// one, and a new folder is appended to this order when saved.
    let listedFolderIds: [Data]
    let onSave: (FolderChangeSet) -> Void
    /// Called after the person confirmed the deletion.
    let onDelete: (FolderView) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var draft: ChatFolders.Draft
    @State private var initial: ChatFolders.Draft
    @State private var picker: PickerMode?
    @State private var confirmingDiscard = false
    @State private var confirmingDelete = false

    enum PickerMode: String, Identifiable {
        case include
        case exclude
        var id: String { rawValue }
    }

    init(
        request: FolderEditorRequest,
        listedFolderIds: [Data],
        onSave: @escaping (FolderChangeSet) -> Void,
        onDelete: @escaping (FolderView) -> Void
    ) {
        self.request = request
        self.listedFolderIds = listedFolderIds
        self.onSave = onSave
        self.onDelete = onDelete
        let start = request.folder.map(ChatFolders.Draft.editing)
            ?? ChatFolders.Draft.new(prefill: request.prefill)
        _draft = State(initialValue: start)
        _initial = State(initialValue: start)
    }

    private var rowsById: [String: ConversationListRow] {
        Dictionary(request.rows.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
    }

    private var dirty: Bool { draft != initial }

    private var matchCount: Int {
        let rule = draft.rule
        return request.rows.filter {
            LakkoCore.folderMatches(folder: rule, row: ChatFolders.ffiRow($0))
        }.count
    }

    private var atFolderLimit: Bool {
        request.folder == nil && listedFolderIds.count >= ChatFolders.maxFolders
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Folder name", text: $draft.name)
                        .onChange(of: draft.name) { _, new in
                            let clamped = ChatFolders.clampName(new)
                            if clamped != new { draft.name = clamped }
                        }
                }

                Section {
                    kindRow("One-to-one chats", bit: ChatFolders.Include.oneToOne)
                    kindRow("Groups", bit: ChatFolders.Include.groups)
                    kindRow("Channels", bit: ChatFolders.Include.channels)
                    handRows(\.included)
                    Button("Add chats") { picker = .include }
                } header: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Included")
                        if draft.hasRule {
                            Text(ChatFolders.chatCountLabel(matchCount))
                                .textCase(nil)
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    }
                }

                Section("Excluded") {
                    flagRow("Muted", bit: ChatFolders.Exclude.muted)
                    flagRow("Read", bit: ChatFolders.Exclude.read)
                    flagRow("Archived", bit: ChatFolders.Exclude.archived)
                    handRows(\.excluded)
                    Button("Exclude chats") { picker = .exclude }
                }

                if request.folder != nil {
                    Section {
                        Button("Delete folder", role: .destructive) { confirmingDelete = true }
                    }
                }

                if atFolderLimit {
                    Section {
                        Text("You can have up to 20 folders.")
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
            }
            .navigationTitle(request.folder == nil ? "New folder" : "Edit folder")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        if dirty { confirmingDiscard = true } else { dismiss() }
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", action: save)
                        .disabled(!draft.canSave || atFolderLimit)
                }
            }
            .interactiveDismissDisabled(dirty)
            .confirmationDialog(
                "Discard changes?", isPresented: $confirmingDiscard, titleVisibility: .visible
            ) {
                Button("Discard", role: .destructive) { dismiss() }
                Button("Keep editing", role: .cancel) {}
            }
            .alert(
                "Delete folder \u{201C}\(request.folder?.meta.name ?? "")\u{201D}?",
                isPresented: $confirmingDelete
            ) {
                Button("Cancel", role: .cancel) {}
                Button("Delete", role: .destructive) {
                    if let folder = request.folder { onDelete(folder) }
                    dismiss()
                }
            } message: {
                Text("The chats in it stay in All chats.")
            }
            .sheet(item: $picker) { mode in
                pickerSheet(for: mode)
            }
        }
    }

    // MARK: Rows

    /// A kind toggle: a row with a checkmark, a real checkable control for
    /// VoiceOver (section 9).
    private func kindRow(_ title: String, bit: UInt32) -> some View {
        checkRow(title, isOn: draft.includeKinds & bit != 0) {
            draft.includeKinds ^= bit
        }
    }

    private func flagRow(_ title: String, bit: UInt32) -> some View {
        checkRow(title, isOn: draft.excludeFlags & bit != 0) {
            draft.excludeFlags ^= bit
        }
    }

    private func checkRow(_ title: String, isOn: Bool, toggle: @escaping () -> Void) -> some View {
        Button(action: toggle) {
            HStack {
                Text(title).foregroundStyle(Color.lakkoText)
                Spacer()
                if isOn {
                    Image(systemName: "checkmark")
                        .foregroundStyle(Color.lakkoAccent)
                }
            }
            .contentShape(Rectangle())
        }
        .accessibilityAddTraits(isOn ? [.isToggle, .isSelected] : .isToggle)
        .accessibilityValue(isOn ? "On" : "Off")
    }

    /// The chats set by hand, each a compact row with a remove control. A
    /// reference this device cannot open yet is kept in the draft and not
    /// drawn (never a placeholder row, section 2.4).
    @ViewBuilder
    private func handRows(_ keyPath: WritableKeyPath<ChatFolders.Draft, [FolderConversationRef]>) -> some View {
        let byId = rowsById
        ForEach(draft[keyPath: keyPath], id: \.self) { ref in
            if let row = byId[ChatFolders.rowId(ref)] {
                HStack(spacing: 10) {
                    InitialsAvatar(name: row.title, identity: row.id, size: 28)
                    Text(row.title)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                    Spacer()
                    Button {
                        draft[keyPath: keyPath].removeAll { $0.id == ref.id }
                    } label: {
                        Image(systemName: "minus.circle.fill")
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Remove \(row.title)")
                }
            }
        }
    }

    // MARK: Picker

    private func pickerSheet(for mode: PickerMode) -> some View {
        let keyPath: WritableKeyPath<ChatFolders.Draft, [FolderConversationRef]> =
            mode == .include ? \.included : \.excluded
        let other: WritableKeyPath<ChatFolders.Draft, [FolderConversationRef]> =
            mode == .include ? \.excluded : \.included
        let byId = rowsById
        let current = draft[keyPath: keyPath]
        let unresolved = current.filter { byId[ChatFolders.rowId($0)] == nil }
        let limit = mode == .include ? ChatFolders.maxHandAdded : ChatFolders.maxHandExcluded
        return FolderChatPicker(
            title: mode == .include ? "Add chats" : "Exclude chats",
            rows: request.rows,
            selected: Set(current.map(ChatFolders.rowId)),
            capacity: max(0, limit - unresolved.count),
            limitLine: mode == .include
                ? "A folder can hold up to 200 chats added one by one. Use the kind toggles for more."
                : "A folder can hold up to 200 chats excluded one by one. Use the kind toggles for more."
        ) { chosen in
            var refs = unresolved
            for id in chosen.sorted() {
                if let existing = current.first(where: { ChatFolders.rowId($0) == id }) {
                    refs.append(existing)
                } else if let row = byId[id], let ref = ChatFolders.ref(for: row, in: request.folder) {
                    refs.append(ref)
                }
            }
            draft[keyPath: keyPath] = refs
            // A chat is either added or excluded by hand, never both.
            draft[keyPath: other].removeAll { chosen.contains(ChatFolders.rowId($0)) }
        }
    }

    private func save() {
        let folderId = request.folder?.folderId ?? LakkoCore.mintFolderId()
        let changes = ChatFolders.changes(
            original: request.folder, draft: draft, folderId: folderId,
            listedFolderIds: listedFolderIds, mintHlc: Hlc.mint)
        onSave(changes)
        dismiss()
    }
}

// MARK: - The chat picker (section 3.3)

struct FolderChatPicker: View {
    let title: String
    let rows: [ConversationListRow]
    /// How many rows may be checked at once.
    let capacity: Int
    let limitLine: String
    let onDone: (Set<String>) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var selected: Set<String>
    @State private var search = ""

    init(
        title: String,
        rows: [ConversationListRow],
        selected: Set<String>,
        capacity: Int,
        limitLine: String,
        onDone: @escaping (Set<String>) -> Void
    ) {
        self.title = title
        self.rows = rows
        self.capacity = capacity
        self.limitLine = limitLine
        self.onDone = onDone
        _selected = State(initialValue: selected)
    }

    private var atLimit: Bool { selected.count >= capacity }

    var body: some View {
        NavigationStack {
            List {
                if atLimit {
                    Text(limitLine)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                ForEach(ConversationList.filtered(rows, query: search)) { row in
                    let isChecked = selected.contains(row.id)
                    Button {
                        if isChecked {
                            selected.remove(row.id)
                        } else if !atLimit {
                            selected.insert(row.id)
                        }
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: isChecked ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(isChecked ? Color.lakkoAccent : Color.lakkoTextMuted)
                            ConversationRowView(row: row, timeLabel: nil)
                        }
                    }
                    .buttonStyle(.plain)
                    .disabled(!isChecked && atLimit)
                    .accessibilityAddTraits(isChecked ? [.isToggle, .isSelected] : .isToggle)
                }
            }
            .listStyle(.plain)
            .searchable(text: $search, prompt: "Search")
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        onDone(selected)
                        dismiss()
                    }
                }
            }
        }
    }
}

// MARK: - Settings > Chat folders (section 4)

struct ChatFoldersSettingsScreen: View {
    /// Open in the reorder state (the tab menu's Reorder folders).
    let startInReorder: Bool
    @ObservedObject var viewModel: ChatListViewModel
    @ObservedObject var channels: ChannelsUiState
    /// The channel unread counts the chat list shows (ADR-0061), so a
    /// channel row in the folder editor's picker carries the same count.
    @ObservedObject var feedReading: FeedReadingModel
    @EnvironmentObject private var sync: DeviceGroupSyncRepository

    @State private var editMode: EditMode
    @State private var editor: FolderEditorRequest?

    init(
        startInReorder: Bool, viewModel: ChatListViewModel, channels: ChannelsUiState,
        feedReading: FeedReadingModel
    ) {
        self.startInReorder = startInReorder
        self.viewModel = viewModel
        self.channels = channels
        self.feedReading = feedReading
        _editMode = State(initialValue: startInReorder ? .active : .inactive)
    }

    private var rows: [ConversationListRow] {
        ChatListScreen.allRows(
            viewModel: viewModel, channels: channels,
            flags: sync.flags, readCursors: sync.readCursors,
            channelUnread: feedReading.channelUnread)
    }

    private var atLimit: Bool { sync.folders.count >= ChatFolders.maxFolders }

    var body: some View {
        let folders = sync.folders
        let all = rows
        let missing = ChatFolders.missingPresets(folders)
        List {
            if sync.pendingSync {
                Section {
                    Text("Changes will reach your other devices when this device is back online.")
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }

            if folders.isEmpty {
                Section {
                    Text(
                        "Group your chats the way you think about them. "
                            + "Folders appear on all your devices."
                    )
                    .foregroundStyle(Color.lakkoTextMuted)
                }
            } else {
                Section("Your folders") {
                    Text("All chats")
                        .foregroundStyle(Color.lakkoText)
                        .moveDisabled(true)
                    ForEach(folders, id: \.folderId) { folder in
                        Button {
                            editor = FolderEditorRequest(folder: folder, rows: all)
                        } label: {
                            HStack {
                                Text(folder.meta.name)
                                    .foregroundStyle(Color.lakkoText)
                                Spacer()
                                Text(ChatFolders.chatCountLabel(
                                    all.filter { ChatFolders.matches(folder, $0) }.count))
                                    .foregroundStyle(Color.lakkoTextMuted)
                            }
                        }
                    }
                    .onMove { source, destination in
                        move(folders: folders, from: source, to: destination)
                    }
                }
            }

            Section {
                Button {
                    editor = FolderEditorRequest(folder: nil, rows: all)
                } label: {
                    Label("New folder", systemImage: "folder.badge.plus")
                }
                .disabled(atLimit)
            } footer: {
                if atLimit {
                    Text("You can have up to 20 folders.")
                }
            }

            if !missing.isEmpty {
                Section("Suggested") {
                    ForEach(missing) { preset in
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(preset.name)
                                    .foregroundStyle(Color.lakkoText)
                                Text(preset.description)
                                    .font(.footnote)
                                    .foregroundStyle(Color.lakkoTextMuted)
                            }
                            Spacer()
                            Button("Add") { add(preset) }
                                .buttonStyle(.bordered)
                                .disabled(atLimit)
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Chat folders")
        .toolbar {
            if !folders.isEmpty {
                ToolbarItem(placement: .navigationBarTrailing) { EditButton() }
            }
        }
        .environment(\.editMode, $editMode)
        .sheet(item: $editor) { request in
            FolderEditorScreen(
                request: request,
                listedFolderIds: folders.map(\.folderId),
                onSave: { changes in publish(changes) },
                onDelete: { folder in
                    publish(ChatFolders.remove(folderId: folder.folderId, hlc: sync.mintHlc()))
                })
        }
    }

    private func move(folders: [FolderView], from source: IndexSet, to destination: Int) {
        let ids = ChatFolders.moved(folders.map(\.folderId), from: source, to: destination)
        publish(ChatFolders.reorder(folderIds: ids, hlc: sync.mintHlc()))
    }

    private func add(_ preset: ChatFolders.Preset) {
        publish(ChatFolders.create(
            preset: preset, folderId: LakkoCore.mintFolderId(),
            listedFolderIds: sync.folders.map(\.folderId), mintHlc: Hlc.mint))
    }

    private func publish(_ changes: FolderChangeSet) {
        Task {
            do {
                try await sync.applyFolderChanges(changes)
            } catch {
                NSLog("folder edit failed: %@", error.localizedDescription)
            }
        }
    }
}
