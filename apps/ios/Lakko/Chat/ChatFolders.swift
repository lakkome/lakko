// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Chat folders (`docs/ux/chat-folders.md`, ADR-0058): the client-side half.
///
/// **Which chats are in a folder, and in what order, is not decided here.**
/// Both are `folder_matches` / `order_conversations_in_folder` in
/// `crates/lakko-ui-rules`, reached over the FFI, so three clients cannot
/// disagree about what is in "Work". What this file owns is the mapping
/// between the row iOS draws and the row the rule reads, the mapping between a
/// folder's conversation references and those rows, and the translation of
/// one edit into the registers it changes - and ONLY those: an edit that
/// rewrote a register it did not change would, on a device still missing some
/// of the folder's chats, write them away on every other device too
/// (ADR-0058 section 5a).
///
/// Pure of SwiftUI and of I/O, so a test can run every piece of it.
public enum ChatFolders {

    // MARK: - Limits (UX spec 3.4; chosen, not measured)

    /// Folders per account.
    public static let maxFolders = 20
    /// Folder name, in characters.
    public static let maxNameLength = 24
    /// Chats added by hand, per folder.
    public static let maxHandAdded = 200
    /// Chats excluded by hand, per folder.
    public static let maxHandExcluded = 200
    /// Pins inside one folder.
    public static let maxPins = 10

    // MARK: - Bits (ADR-0058 section 2)

    /// `include_kinds` bits.
    public enum Include {
        public static let oneToOne: UInt32 = 1 << 0
        public static let groups: UInt32 = 1 << 1
        public static let channels: UInt32 = 1 << 2
        public static let all: UInt32 = oneToOne | groups | channels
    }

    /// `exclude_flags` bits.
    public enum Exclude {
        public static let muted: UInt32 = 1 << 0
        public static let read: UInt32 = 1 << 1
        public static let archived: UInt32 = 1 << 2
    }

    // MARK: - Rows and references

    /// Lowercase hex, the form every row id on this client already has.
    public static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// Decode lowercase or uppercase hex; `nil` on anything else.
    public static func bytes(fromHex hex: String) -> Data? {
        guard hex.count % 2 == 0, !hex.isEmpty else { return nil }
        var out = Data(capacity: hex.count / 2)
        var i = hex.startIndex
        while i < hex.endIndex {
            let next = hex.index(i, offsetBy: 2)
            guard let b = UInt8(hex[i..<next], radix: 16) else { return nil }
            out.append(b)
            i = next
        }
        return out
    }

    /// The row id a reference names. A one-to-one chat and a group are keyed
    /// on the MLS group id (32 bytes, 64 hex characters) and a channel on its
    /// channel id (16 bytes, 32 hex characters) - the same strings the list
    /// already uses as row ids, and two lengths that cannot collide.
    public static func rowId(_ ref: FolderConversationRef) -> String {
        hex(ref.id)
    }

    /// The reference kind a row's kind maps to.
    public static func refKind(_ kind: ConversationListKind) -> FolderConversationKind {
        switch kind {
        case .chat: return .oneToOne
        case .group: return .group
        case .channel: return .channel
        }
    }

    /// A reference to `row`. If `folder` already holds a reference with the
    /// same id, that reference is reused as it is - a register is keyed on the
    /// whole `(kind, id)` pair, so rewriting a chat that another device filed
    /// as a one-to-one under "group" would leave the old register standing.
    public static func ref(
        for row: ConversationListRow, in folder: FolderView? = nil
    ) -> FolderConversationRef? {
        if let existing = folder.flatMap({ existingRef(id: row.id, in: $0) }) {
            return existing
        }
        guard let id = bytes(fromHex: row.id) else { return nil }
        return FolderConversationRef(kind: refKind(row.kind), id: id)
    }

    /// The reference `folder` holds for a row id, hand-added, hand-excluded
    /// or pinned.
    public static func existingRef(id: String, in folder: FolderView) -> FolderConversationRef? {
        (folder.included + folder.excluded + folder.pinned).first { rowId($0) == id }
    }

    /// What the unread badge means (UX spec 3.2): something unread in it, or a
    /// manual "mark as unread". A conversation whose membership ended shows no
    /// badge, so it is read as far as a folder is concerned.
    public static func hasUnread(_ row: ConversationListRow) -> Bool {
        row.showsUnread || row.markedUnread
    }

    /// The row the shared rules read. A channel's muted, archived and pinned
    /// are the synced flags under its channel id (``ChannelFlags``).
    public static func ffiRow(_ row: ConversationListRow) -> ConversationRow {
        ConversationRow(
            id: row.id,
            kind: row.kind,
            title: row.title,
            lastActivityMs: row.lastActivityMs,
            pinned: row.pinned,
            archived: row.archived,
            muted: row.muted,
            hasUnread: hasUnread(row))
    }

    /// The folder's rule in row terms.
    public static func rule(_ folder: FolderView) -> FolderRule {
        FolderRule(
            includeKinds: folder.meta.includeKinds,
            excludeFlags: folder.meta.excludeFlags,
            included: folder.included.map(rowId),
            excluded: folder.excluded.map(rowId))
    }

    /// Whether `row` is in `folder`, decided by the shared rule.
    public static func matches(_ folder: FolderView, _ row: ConversationListRow) -> Bool {
        LakkoCore.folderMatches(folder: rule(folder), row: ffiRow(row))
    }

    /// The rows `folder` shows, in the folder's order, with each row's pin
    /// mark swapped for the folder's own pin (UX spec 2.3: the All-chats pin
    /// does not carry into a folder).
    ///
    /// `pins` replaces the folder's stored pin list (row ids, top first) for
    /// this one drawing: the order a move just wrote, shown at once while its
    /// write is on the way (navigation 2.2.1, "applies at once, on drop").
    public static func rows(
        in folder: FolderView, from all: [ConversationListRow], pins override: [String]? = nil
    ) -> [ConversationListRow] {
        let matching = all.filter { matches(folder, $0) }
        let pins = PinOrder.spelledAsRows(override ?? folder.pinned.map(rowId), rows: matching)
        let pinSet = Set(pins)
        let order = LakkoCore.orderConversationsInFolder(
            rows: matching.map(ffiRow), folderPins: pins)
        return order.compactMap { index -> ConversationListRow? in
            let i = Int(index)
            guard i < matching.count else { return nil }
            return matching[i].withPinned(pinSet.contains(matching[i].id))
        }
    }

    /// The count a tab shows (UX spec 2.1): conversations in the folder with
    /// anything unread, not counting muted ones.
    public static func unreadCount(_ folder: FolderView, in all: [ConversationListRow]) -> Int {
        all.reduce(0) { acc, row in
            (hasUnread(row) && !row.muted && matches(folder, row)) ? acc + 1 : acc
        }
    }

    /// The All-chats count used only to decide whether Mark all as read is
    /// offered on the All-chats tab menu.
    public static func anyUnread(_ all: [ConversationListRow]) -> Bool {
        all.contains { hasUnread($0) }
    }

    /// The accessibility label of a tab (UX spec 9): "Work, 3 unread chats".
    public static func tabAccessibilityLabel(name: String, count: Int) -> String {
        guard count > 0 else { return name }
        return Localization.text(
            "folders.tab_a11y", texts: ["name": name], counts: ["count": UInt64(count)])
    }

    /// "You can have up to 20 folders.", the limit an argument of the message.
    public static var folderLimitLine: String {
        Localization.text("folders.limit", counts: ["count": UInt64(maxFolders)])
    }

    /// "You can pin up to 10 chats in a folder."
    public static var pinLimitLine: String {
        Localization.text("folders.pin_limit", counts: ["count": UInt64(maxPins)])
    }

    /// "12 chats" (UX spec 3.1, 4).
    public static func chatCountLabel(_ n: Int) -> String {
        Localization.text("folders.chat_count", counts: ["count": UInt64(max(0, n))])
    }

    // MARK: - Hand-set membership

    /// How `folder` holds a row id by hand.
    public static func handState(_ folder: FolderView, rowId id: String) -> FolderMemberState {
        if folder.excluded.contains(where: { rowId($0) == id }) { return .excluded }
        if folder.included.contains(where: { rowId($0) == id }) { return .included }
        return FolderMemberState.none
    }

    /// Whether the folder's kind toggles cover `row`.
    public static func coveredByKind(_ folder: FolderView, _ row: ConversationListRow) -> Bool {
        let bit: UInt32
        switch row.kind {
        case .chat: bit = Include.oneToOne
        case .group: bit = Include.groups
        case .channel: bit = Include.channels
        }
        return folder.meta.includeKinds & bit != 0
    }

    /// The one register "Add to folder" / "Remove from folder" writes for a
    /// row (UX spec 5), or `nil` when the tap changes nothing:
    ///
    /// * not in the folder -> added by hand, whatever the register held. An
    ///   exclusion is replaced rather than cleared: clearing it would leave a
    ///   chat no kind toggle covers still outside the folder.
    /// * in the folder by a kind toggle -> excluded by hand (the person asked
    ///   to take it out; how the folder was built is not their problem);
    /// * in the folder by hand only -> the hand-add is removed.
    public static func toggleMembership(
        folder: FolderView, row: ConversationListRow, hlc: UInt64
    ) -> FolderChangeSet? {
        guard let ref = ref(for: row, in: folder) else { return nil }
        let current = handState(folder, rowId: row.id)
        let target: FolderMemberState
        if matches(folder, row) {
            target = coveredByKind(folder, row) ? .excluded : FolderMemberState.none
        } else {
            target = .included
        }
        if target == current { return nil }
        return FolderChangeSet(
            metas: [],
            members: [FolderMemberRegister(
                hlc: hlc, folderId: folder.folderId, conversation: ref, state: target)],
            pins: [], removed: [], order: nil)
    }

    /// Whether adding `row` by hand would break the per-folder limit.
    public static func handAddLimitReached(_ folder: FolderView) -> Bool {
        folder.included.count >= maxHandAdded
    }

    // MARK: - Pins inside a folder

    /// Whether `row` is pinned in `folder`.
    public static func isPinned(_ row: ConversationListRow, in folder: FolderView) -> Bool {
        folder.pinned.contains { rowId($0) == row.id }
    }

    /// Pin or unpin `row` inside `folder`: the folder's pin list, as one new
    /// value, written by the shared helpers (navigation 2.2.1): a new pin goes
    /// to the top, an unpin keeps the others in their order, and a stored pin
    /// this device does not show (its chat has not arrived) stays where it
    /// is. `shown` is the folder's pins in the order the list draws them;
    /// `nil` reads the stored list instead. `nil` when pinning would exceed
    /// the limit (the menu disables the item then, so this is the backstop).
    public static func togglePin(
        folder: FolderView, row: ConversationListRow, shown: [String]? = nil, hlc: UInt64
    ) -> FolderChangeSet? {
        let stored = folder.pinned.map(rowId)
        let pinned = isPinned(row, in: folder)
        if !pinned {
            guard folder.pinned.count < maxPins, ref(for: row, in: folder) != nil else { return nil }
        }
        let ids = PinOrder.next(
            pinned ? .unpin : .pin, stored: stored, shown: shown ?? stored, id: row.id)
        return pinsChange(folder: folder, ids: ids, adding: pinned ? nil : row, hlc: hlc)
    }

    /// Move a pin of `folder` to `toIndex` among the shown pins (a drag, or
    /// Move up / Move down). `nil` when `id` is not one of the folder's pins
    /// or the list would not change.
    public static func movePin(
        folder: FolderView, shown: [String], id: String, toIndex: Int, hlc: UInt64
    ) -> FolderChangeSet? {
        let stored = folder.pinned.map(rowId)
        guard stored.contains(where: { $0.caseInsensitiveCompare(id) == .orderedSame }) else {
            return nil
        }
        let ids = PinOrder.next(.move(toIndex: toIndex), stored: stored, shown: shown, id: id)
        guard ids != stored else { return nil }
        return pinsChange(folder: folder, ids: ids, adding: nil, hlc: hlc)
    }

    /// The pins register for `ids`, each id mapped back to the reference the
    /// folder already holds for it (a register is keyed on the whole
    /// `(kind, id)` pair, see ``ref(for:in:)``), or to a new reference for
    /// the row being pinned.
    static func pinsChange(
        folder: FolderView, ids: [String], adding row: ConversationListRow?, hlc: UInt64
    ) -> FolderChangeSet {
        let pins: [FolderConversationRef] = ids.compactMap { id in
            if let existing = existingRef(id: id.lowercased(), in: folder) { return existing }
            if let row, row.id.caseInsensitiveCompare(id) == .orderedSame {
                return ref(for: row, in: folder)
            }
            return nil
        }
        return FolderChangeSet(
            metas: [], members: [],
            pins: [FolderPinsRegister(hlc: hlc, folderId: folder.folderId, pinned: pins)],
            removed: [], order: nil)
    }

    // MARK: - Folder-level edits

    /// Delete a folder. Final on every device (ADR-0058 section 3).
    public static func remove(folderId: Data, hlc: UInt64) -> FolderChangeSet {
        FolderChangeSet(
            metas: [], members: [], pins: [],
            removed: [FolderRemovedRegister(hlc: hlc, folderId: folderId)], order: nil)
    }

    /// A new folder order, as one value.
    public static func reorder(folderIds: [Data], hlc: UInt64) -> FolderChangeSet {
        FolderChangeSet(
            metas: [], members: [], pins: [], removed: [],
            order: FolderOrderRegister(hlc: hlc, folderIds: folderIds))
    }

    /// Move the folder at `source` offsets to `destination`, the `.onMove`
    /// contract, over plain ids.
    public static func moved(_ ids: [Data], from source: IndexSet, to destination: Int) -> [Data] {
        var out = ids
        let moving = source.sorted().map { out[$0] }
        for index in source.sorted(by: >) { out.remove(at: index) }
        let removedBefore = source.filter { $0 < destination }.count
        let insertAt = max(0, min(out.count, destination - removedBefore))
        out.insert(contentsOf: moving, at: insertAt)
        return out
    }

    /// The folder order a CREATE writes beside the new folder's registers:
    /// the folders as the bar lists them, then the new one, never twice.
    ///
    /// Without it the new folder sits outside the order register, where the
    /// fold places it by the stamp of its CURRENT meta register (ADR-0058),
    /// so every later rename moved the folder to the end of the bar (seen
    /// live on web 2026-09-30).
    public static func appendedToOrder(_ listedFolderIds: [Data], newFolderId: Data) -> [Data] {
        listedFolderIds.filter { $0 != newFolderId } + [newFolderId]
    }

    // MARK: - The editor

    /// What the editor holds while the person edits.
    public struct Draft: Equatable {
        public var name: String
        public var includeKinds: UInt32
        public var excludeFlags: UInt32
        public var included: [FolderConversationRef]
        public var excluded: [FolderConversationRef]

        public init(
            name: String, includeKinds: UInt32, excludeFlags: UInt32,
            included: [FolderConversationRef], excluded: [FolderConversationRef]
        ) {
            self.name = name
            self.includeKinds = includeKinds
            self.excludeFlags = excludeFlags
            self.included = included
            self.excluded = excluded
        }

        /// A new folder: nothing included, Archived excluded (UX spec 3.1),
        /// optionally with one chat already added (Add to folder > New folder).
        public static func new(prefill: FolderConversationRef? = nil) -> Draft {
            Draft(
                name: "", includeKinds: 0, excludeFlags: Exclude.archived,
                included: prefill.map { [$0] } ?? [], excluded: [])
        }

        /// The editor's starting state for an existing folder.
        public static func editing(_ folder: FolderView) -> Draft {
            Draft(
                name: folder.meta.name,
                includeKinds: folder.meta.includeKinds,
                excludeFlags: folder.meta.excludeFlags,
                included: folder.included,
                excluded: folder.excluded)
        }

        /// The name as it would be saved.
        public var trimmedName: String {
            name.trimmingCharacters(in: .whitespacesAndNewlines)
        }

        /// UX spec 3.1: Save is disabled while the name is empty or nothing is
        /// included. The character rule is the shared one.
        public var canSave: Bool {
            !trimmedName.isEmpty
                && LakkoCore.isValidFolderName(name: trimmedName)
                && (includeKinds != 0 || !included.isEmpty)
        }

        /// The rule this draft would save, for the live match count.
        public var rule: FolderRule {
            FolderRule(
                includeKinds: includeKinds, excludeFlags: excludeFlags,
                included: included.map(rowId), excluded: excluded.map(rowId))
        }

        /// Whether the draft has a rule worth counting against.
        public var hasRule: Bool { includeKinds != 0 || !included.isEmpty }
    }

    /// Cut a typed name to the limit (UX spec 3.4: the field stops accepting
    /// input, no error text).
    public static func clampName(_ name: String) -> String {
        name.count > maxNameLength ? String(name.prefix(maxNameLength)) : name
    }

    /// The registers an editor Save writes: only the ones the edit changed,
    /// each with a fresh stamp from `mintHlc`.
    ///
    /// For a new folder (`original == nil`) that is the meta register, one
    /// member register per hand-set chat, and the order register: the folders
    /// in `listedFolderIds` (the bar order) with the new one appended, see
    /// ``appendedToOrder(_:newFolderId:)``. An edit of an existing folder
    /// never writes the order, and ignores `listedFolderIds`.
    public static func changes(
        original: FolderView?,
        draft: Draft,
        folderId: Data,
        listedFolderIds: [Data],
        mintHlc: () -> UInt64
    ) -> FolderChangeSet {
        var metas: [FolderMetaRegister] = []
        let name = draft.trimmedName
        if original == nil
            || original?.meta.name != name
            || original?.meta.includeKinds != draft.includeKinds
            || original?.meta.excludeFlags != draft.excludeFlags {
            metas.append(FolderMetaRegister(
                hlc: mintHlc(), folderId: folderId, name: name,
                includeKinds: draft.includeKinds, excludeFlags: draft.excludeFlags))
        }

        let before: [String: (FolderConversationRef, FolderMemberState)] = {
            var out: [String: (FolderConversationRef, FolderMemberState)] = [:]
            for r in original?.included ?? [] { out[rowId(r)] = (r, .included) }
            // Excluded by hand beats included by hand (UX spec 3.2).
            for r in original?.excluded ?? [] { out[rowId(r)] = (r, .excluded) }
            return out
        }()
        var after: [String: (FolderConversationRef, FolderMemberState)] = [:]
        for r in draft.included { after[rowId(r)] = (r, .included) }
        for r in draft.excluded { after[rowId(r)] = (r, .excluded) }

        var members: [FolderMemberRegister] = []
        // Stable order, so the same edit always produces the same bytes.
        let ids = Set(before.keys).union(after.keys).sorted()
        for id in ids {
            let old = before[id]?.1 ?? FolderMemberState.none
            let new = after[id]?.1 ?? FolderMemberState.none
            guard old != new, let ref = before[id]?.0 ?? after[id]?.0 else { continue }
            members.append(FolderMemberRegister(
                hlc: mintHlc(), folderId: folderId, conversation: ref, state: new))
        }
        let order = original == nil
            ? FolderOrderRegister(
                hlc: mintHlc(),
                folderIds: appendedToOrder(listedFolderIds, newFolderId: folderId))
            : nil
        return FolderChangeSet(metas: metas, members: members, pins: [], removed: [], order: order)
    }

    /// Whether a change set writes anything at all.
    public static func isEmpty(_ c: FolderChangeSet) -> Bool {
        c.metas.isEmpty && c.members.isEmpty && c.pins.isEmpty && c.removed.isEmpty
            && c.order == nil && c.saved.isEmpty && c.conversationEnds.isEmpty
    }

    // MARK: - Suggested folders (UX spec 4)

    public struct Preset: Equatable, Identifiable {
        /// The catalog key of the suggested name (`folders.suggested.<x>.name`).
        public let nameKey: String
        /// The catalog key of the one-line description.
        public let descriptionKey: String
        public let includeKinds: UInt32
        public let excludeFlags: UInt32
        public var id: String { nameKey }
        /// The suggested name in the interface language of this moment.
        /// Adding the suggestion stores it as the folder's own name, which
        /// from then on is the person's text and is never translated.
        public var name: String { Localization.text(nameKey) }
        /// The description in the interface language of this moment.
        public var description: String { Localization.text(descriptionKey) }
    }

    /// The four suggestions, with the rules the spec gives them.
    public static let presets: [Preset] = [
        Preset(
            nameKey: "folders.suggested.unread.name",
            descriptionKey: "folders.suggested.unread.description",
            includeKinds: Include.all, excludeFlags: Exclude.read),
        Preset(
            nameKey: "folders.suggested.personal.name",
            descriptionKey: "folders.suggested.personal.description",
            includeKinds: Include.oneToOne, excludeFlags: 0),
        Preset(
            nameKey: "folders.suggested.groups.name",
            descriptionKey: "folders.suggested.groups.description",
            includeKinds: Include.groups, excludeFlags: 0),
        Preset(
            nameKey: "folders.suggested.channels.name",
            descriptionKey: "folders.suggested.channels.description",
            includeKinds: Include.channels, excludeFlags: 0),
    ]

    /// The suggestions the person does not already have, compared by rule,
    /// not by name: same toggles and nothing set by hand.
    public static func missingPresets(_ folders: [FolderView]) -> [Preset] {
        presets.filter { preset in
            !folders.contains { f in
                f.meta.includeKinds == preset.includeKinds
                    && f.meta.excludeFlags == preset.excludeFlags
                    && f.included.isEmpty && f.excluded.isEmpty
            }
        }
    }

    /// The registers that create a preset as a new folder: its meta register
    /// and the order register that places it at the end of the bar
    /// (`listedFolderIds` is the bar order before the create).
    public static func create(
        preset: Preset, folderId: Data, listedFolderIds: [Data], mintHlc: () -> UInt64
    ) -> FolderChangeSet {
        FolderChangeSet(
            metas: [FolderMetaRegister(
                hlc: mintHlc(), folderId: folderId, name: preset.name,
                includeKinds: preset.includeKinds, excludeFlags: preset.excludeFlags)],
            members: [], pins: [], removed: [],
            order: FolderOrderRegister(
                hlc: mintHlc(),
                folderIds: appendedToOrder(listedFolderIds, newFolderId: folderId)))
    }

    // MARK: - The selected tab (per device, never synced; UX spec 2.5)

    /// The tab to open on: the stored folder when it still exists, else All
    /// chats (`nil`).
    public static func resolveSelectedTab(stored: String?, folders: [FolderView]) -> String? {
        guard let stored, !stored.isEmpty else { return nil }
        return folders.contains { hex($0.folderId) == stored } ? stored : nil
    }
}

extension ConversationListRow {
    /// The same row with its pin mark replaced (a folder's own pin).
    func withPinned(_ pinned: Bool) -> ConversationListRow {
        ConversationListRow(
            id: id, kind: kind, title: title, preview: preview,
            lastActivityMs: lastActivityMs, unread: unread, pinned: pinned,
            muted: muted, archived: archived, endedLine: endedLine,
            markedUnread: markedUnread, personKey: personKey, photoGroupIdHex: photoGroupIdHex,
            hasUnseenReactions: hasUnseenReactions, previewMedia: previewMedia)
    }
}
