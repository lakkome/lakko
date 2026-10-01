// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// `docs/ux/chat-folders.md` / ADR-0058 — the client half of chat folders.
///
/// Which chats a folder holds is `folder_matches` in `crates/lakko-ui-rules`,
/// tested there. What is tested here is the mapping this client owns (row to
/// rule, reference to row id), and above all the edit-to-change-set builder:
/// an edit must write ONLY the registers it changed, or a device missing some
/// of a folder's chats would write them away on every other device.
final class ChatFoldersTests: XCTestCase {

    private let groupA = String(repeating: "aa", count: 32)
    private let groupB = String(repeating: "bb", count: 32)
    private let groupC = String(repeating: "cc", count: 32)
    private let channelX = String(repeating: "0f", count: 16)
    private let folderId = Data(repeating: 7, count: 16)

    private func row(
        _ id: String,
        kind: ConversationListKind = .chat,
        at ms: Int64? = nil,
        unread: Int = 0,
        pinned: Bool = false,
        muted: Bool = false,
        archived: Bool = false,
        markedUnread: Bool = false
    ) -> ConversationListRow {
        ConversationListRow(
            id: id, kind: kind, title: id.prefix(4).description, preview: "",
            lastActivityMs: ms, unread: unread, pinned: pinned, muted: muted,
            archived: archived, markedUnread: markedUnread)
    }

    private func ref(_ hex: String, _ kind: FolderConversationKind = .oneToOne) -> FolderConversationRef {
        FolderConversationRef(kind: kind, id: ChatFolders.bytes(fromHex: hex)!)
    }

    private func folder(
        include: UInt32 = 0,
        exclude: UInt32 = 0,
        included: [FolderConversationRef] = [],
        excluded: [FolderConversationRef] = [],
        pinned: [FolderConversationRef] = []
    ) -> FolderView {
        FolderView(
            folderId: folderId,
            meta: FolderMetaRegister(
                hlc: 1, folderId: folderId, name: "Work",
                includeKinds: include, excludeFlags: exclude),
            included: included, excluded: excluded, pinned: pinned)
    }

    private func counter() -> () -> UInt64 {
        var next: UInt64 = 100
        return {
            next += 1
            return next
        }
    }

    // MARK: - Row to rule

    func testRuleCarriesRowIdsOfTheHandSets() {
        let f = folder(
            include: ChatFolders.Include.groups,
            exclude: ChatFolders.Exclude.archived,
            included: [ref(groupA)], excluded: [ref(channelX, .channel)])
        let rule = ChatFolders.rule(f)
        XCTAssertEqual(rule.included, [groupA])
        XCTAssertEqual(rule.excluded, [channelX])
        XCTAssertEqual(rule.includeKinds, ChatFolders.Include.groups)
    }

    func testFfiRowCarriesMutedAndUnread() {
        let r = ChatFolders.ffiRow(row(groupA, unread: 2, muted: true))
        XCTAssertTrue(r.muted)
        XCTAssertTrue(r.hasUnread)
        XCTAssertTrue(ChatFolders.ffiRow(row(groupA, markedUnread: true)).hasUnread)
        XCTAssertFalse(ChatFolders.ffiRow(row(groupA)).hasUnread)
    }

    func testMatchingReachesTheSharedRule() {
        let f = folder(
            include: ChatFolders.Include.channels, exclude: ChatFolders.Exclude.read,
            excluded: [ref(groupB)])
        XCTAssertFalse(ChatFolders.matches(f, row(channelX, kind: .channel)))
        XCTAssertFalse(ChatFolders.matches(f, row(groupA, unread: 1)))
        let byHand = folder(include: 0, exclude: 0, included: [ref(groupA)])
        XCTAssertTrue(ChatFolders.matches(byHand, row(groupA)))
    }

    func testFolderPinsReplaceTheGlobalPinAndLeadTheOrder() {
        let f = folder(include: ChatFolders.Include.oneToOne, pinned: [ref(groupB)])
        let all = [
            row(groupA, at: 300, pinned: true),
            row(groupB, at: 100),
            row(groupC, at: 200),
        ]
        let shown = ChatFolders.rows(in: f, from: all)
        XCTAssertEqual(shown.map(\.id), [groupB, groupA, groupC])
        XCTAssertTrue(shown[0].pinned)
        XCTAssertFalse(shown.first { $0.id == groupA }!.pinned)
    }

    func testUnreadCountCountsConversationsAndSkipsMuted() {
        let f = folder(include: ChatFolders.Include.oneToOne)
        let all = [
            row(groupA, unread: 5),
            row(groupB, unread: 2, muted: true),
            row(groupC, markedUnread: true),
        ]
        XCTAssertEqual(ChatFolders.unreadCount(f, in: all), 2)
        XCTAssertEqual(
            ChatFolders.tabAccessibilityLabel(name: "Work", count: 3),
            Localization.text("folders.tab_a11y", texts: ["name": "Work"], counts: ["count": 3]))
        XCTAssertEqual(ChatFolders.tabAccessibilityLabel(name: "Work", count: 0), "Work")
    }

    func testRowIdsOfKindsCannotCollide() {
        XCTAssertNotEqual(groupA.count, channelX.count)
        XCTAssertEqual(ChatFolders.rowId(ref(groupA)), groupA)
    }

    // MARK: - Edit to change set

    func testNewFolderWritesMetaAndOneMemberPerHandSetChat() {
        var draft = ChatFolders.Draft.new(prefill: ref(groupA))
        draft.name = "  Work "
        draft.includeKinds = ChatFolders.Include.channels
        let earlier = Data(repeating: 1, count: 16), later = Data(repeating: 2, count: 16)
        let c = ChatFolders.changes(
            original: nil, draft: draft, folderId: folderId,
            listedFolderIds: [earlier, later], mintHlc: counter())
        XCTAssertEqual(c.metas.count, 1)
        XCTAssertEqual(c.metas[0].name, "Work")
        XCTAssertEqual(c.metas[0].excludeFlags, ChatFolders.Exclude.archived)
        XCTAssertEqual(c.members.count, 1)
        XCTAssertEqual(c.members[0].state, .included)
        XCTAssertEqual(c.order?.folderIds, [earlier, later, folderId],
                       "a new folder takes its place at the end of the bar order")
        XCTAssertTrue(c.pins.isEmpty)
        let stamps = c.metas.map(\.hlc) + c.members.map(\.hlc) + [c.order?.hlc ?? 0]
        XCTAssertEqual(Set(stamps).count, stamps.count, "every register gets its own stamp")
    }

    func testRenameWritesOnlyTheMeta() {
        let original = folder(
            include: ChatFolders.Include.groups, included: [ref(groupA)],
            excluded: [ref(groupB)])
        var draft = ChatFolders.Draft.editing(original)
        draft.name = "Office"
        let c = ChatFolders.changes(
            original: original, draft: draft, folderId: folderId,
            listedFolderIds: [folderId], mintHlc: counter())
        XCTAssertEqual(c.metas.map(\.name), ["Office"])
        XCTAssertTrue(c.members.isEmpty, "unchanged members must not be rewritten")
        XCTAssertNil(c.order, "a rename never rewrites the bar order")
    }

    func testMemberEditsWriteOnlyTheChangedRegistersWithFreshStamps() {
        let unresolved = ref(String(repeating: "dd", count: 32))
        let original = folder(
            include: 0, included: [ref(groupA), unresolved], excluded: [ref(groupB)])
        var draft = ChatFolders.Draft.editing(original)
        draft.included.removeAll { $0 == ref(groupA) }
        draft.excluded.removeAll { $0 == ref(groupB) }
        draft.included.append(ref(groupB))
        draft.excluded.append(ref(groupC, .group))
        let c = ChatFolders.changes(
            original: original, draft: draft, folderId: folderId,
            listedFolderIds: [folderId], mintHlc: counter())
        XCTAssertTrue(c.metas.isEmpty)
        let states = Dictionary(uniqueKeysWithValues: c.members.map {
            (ChatFolders.rowId($0.conversation), $0.state)
        })
        XCTAssertEqual(states[groupA], FolderMemberState.none)
        XCTAssertEqual(states[groupB], .included)
        XCTAssertEqual(states[groupC], .excluded)
        XCTAssertNil(states[String(repeating: "dd", count: 32)],
                     "a reference this device cannot resolve is never written")
        XCTAssertEqual(Set(c.members.map(\.hlc)).count, c.members.count)
    }

    func testUnchangedEditIsEmpty() {
        let original = folder(include: ChatFolders.Include.groups, included: [ref(groupA)])
        let c = ChatFolders.changes(
            original: original, draft: .editing(original), folderId: folderId,
            listedFolderIds: [folderId], mintHlc: counter())
        XCTAssertTrue(ChatFolders.isEmpty(c))
    }

    func testCanSaveNeedsANameAndSomethingIncluded() {
        var d = ChatFolders.Draft.new()
        XCTAssertFalse(d.canSave)
        d.name = "Work"
        XCTAssertFalse(d.canSave)
        d.includeKinds = ChatFolders.Include.groups
        XCTAssertTrue(d.canSave)
        XCTAssertEqual(ChatFolders.clampName(String(repeating: "a", count: 30)).count, 24)
    }

    // MARK: - Add to folder (section 5)

    func testAddToFolderByHand() {
        let f = folder(include: ChatFolders.Include.groups)
        let c = ChatFolders.toggleMembership(folder: f, row: row(groupA), hlc: 5)
        XCTAssertEqual(c?.members.first?.state, .included)
    }

    func testAddingAHandExcludedChatWritesIncluded() {
        let f = folder(include: ChatFolders.Include.oneToOne, excluded: [ref(groupA)])
        let c = ChatFolders.toggleMembership(folder: f, row: row(groupA), hlc: 5)
        XCTAssertEqual(c?.members.first?.state, .included)
    }

    func testAHandExcludedChatNoToggleCoversEntersTheFolder() {
        // Excluded by hand, and no kind toggle covers it: clearing the
        // exclusion would leave it outside, so the tap must include it.
        let f = folder(include: ChatFolders.Include.channels, excluded: [ref(groupA)])
        let c = ChatFolders.toggleMembership(folder: f, row: row(groupA), hlc: 5)
        XCTAssertEqual(c?.members.count, 1)
        XCTAssertEqual(c?.members.first?.state, .included)
        let after = FolderView(
            folderId: f.folderId, meta: f.meta, included: [ref(groupA)], excluded: [],
            pinned: [])
        XCTAssertTrue(ChatFolders.matches(after, row(groupA)))
    }

    func testRemovingAChatInByKindToggleExcludesIt() {
        let f = folder(include: ChatFolders.Include.groups)
        let c = ChatFolders.toggleMembership(folder: f, row: row(groupA, kind: .group), hlc: 5)
        XCTAssertEqual(c?.members.first?.state, .excluded)
        XCTAssertEqual(c?.members.first?.conversation.kind, .group)
    }

    func testRemovingAHandAddedChatClearsIt() {
        let f = folder(include: 0, included: [ref(groupA)])
        let c = ChatFolders.toggleMembership(folder: f, row: row(groupA), hlc: 5)
        XCTAssertEqual(c?.members.first?.state, FolderMemberState.none)
    }

    func testAnExistingReferenceKeepsItsKind() {
        // Another device filed the chat as a group; the register key is the
        // whole (kind, id) pair, so the edit reuses it.
        let f = folder(include: 0, included: [ref(groupA, .group)])
        let c = ChatFolders.toggleMembership(folder: f, row: row(groupA, kind: .chat), hlc: 5)
        XCTAssertEqual(c?.members.first?.conversation.kind, .group)
    }

    // MARK: - Pins, order, presets, tab

    func testFolderPinToggleAndLimit() {
        let f = folder(include: ChatFolders.Include.oneToOne, pinned: [ref(groupB)])
        let pin = ChatFolders.togglePin(folder: f, row: row(groupA), hlc: 9)
        XCTAssertEqual(pin?.pins.first?.pinned.map(ChatFolders.rowId), [groupA, groupB])
        let unpin = ChatFolders.togglePin(folder: f, row: row(groupB), hlc: 9)
        XCTAssertEqual(unpin?.pins.first?.pinned, [])
        let full = folder(pinned: (0..<10).map { ref(String(format: "%064x", $0)) })
        XCTAssertNil(ChatFolders.togglePin(folder: full, row: row(groupA), hlc: 9))
    }

    func testMovedFollowsOnMoveSemantics() {
        let a = Data([1]), b = Data([2]), c = Data([3])
        XCTAssertEqual(ChatFolders.moved([a, b, c], from: IndexSet(integer: 0), to: 3), [b, c, a])
        XCTAssertEqual(ChatFolders.moved([a, b, c], from: IndexSet(integer: 2), to: 0), [c, a, b])
    }

    func testAppendedToOrderPutsTheNewFolderLast() {
        let a = Data([1]), b = Data([2]), n = Data([9])
        XCTAssertEqual(ChatFolders.appendedToOrder([a, b], newFolderId: n), [a, b, n])
    }

    func testAppendedToOrderOnAnEmptyBar() {
        let n = Data([9])
        XCTAssertEqual(ChatFolders.appendedToOrder([], newFolderId: n), [n])
    }

    func testAppendedToOrderNeverListsTheNewFolderTwice() {
        let a = Data([1]), b = Data([2]), n = Data([9])
        XCTAssertEqual(ChatFolders.appendedToOrder([a, n, b], newFolderId: n), [a, b, n])
    }

    func testCreatingAPresetAlsoWritesTheBarOrder() {
        let a = Data([1]), b = Data([2])
        let preset = ChatFolders.presets[2]
        let c = ChatFolders.create(
            preset: preset, folderId: folderId, listedFolderIds: [a, b], mintHlc: counter())
        XCTAssertEqual(c.metas.map(\.name), [preset.name])
        XCTAssertEqual(c.metas.first?.folderId, folderId)
        XCTAssertEqual(c.order?.folderIds, [a, b, folderId])
        XCTAssertNotEqual(c.order?.hlc, c.metas.first?.hlc)
    }

    func testMissingPresetsCompareByRule() {
        let personal = FolderView(
            folderId: folderId,
            meta: FolderMetaRegister(
                hlc: 1, folderId: folderId, name: "Mine",
                includeKinds: ChatFolders.Include.oneToOne, excludeFlags: 0),
            included: [], excluded: [], pinned: [])
        XCTAssertEqual(
            ChatFolders.missingPresets([personal]).map(\.nameKey),
            ["folders.suggested.unread.name", "folders.suggested.groups.name",
             "folders.suggested.channels.name"])
    }

    func testSelectedTabFallsBackToAllChats() {
        let f = folder()
        let hex = ChatFolders.hex(folderId)
        XCTAssertEqual(ChatFolders.resolveSelectedTab(stored: hex, folders: [f]), hex)
        XCTAssertNil(ChatFolders.resolveSelectedTab(stored: hex, folders: []))
        XCTAssertNil(ChatFolders.resolveSelectedTab(stored: "", folders: [f]))
    }
}
