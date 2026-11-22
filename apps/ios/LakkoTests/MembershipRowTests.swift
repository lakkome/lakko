// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// `docs/ux/second-device.md` section 4, "Membership rows" (decided
/// 2026-10-29): a batch that changes the people of a group names them on its
/// last commit row; nothing else draws a commit row.
@MainActor
final class MembershipRowTests: XCTestCase {

    private func key(_ byte: UInt8) -> String { base64Encode(Data(repeating: byte, count: 32)) }

    private var own: String { key(0x01) }
    private var anna: String { key(0x0A) }
    private var bob: String { key(0x0B) }

    private func verdict(
        isGroup: Bool = true,
        hadCommit: Bool = true,
        before: [String]?,
        after: [String]?,
        accountOfKey: @escaping (String) -> String? = { _ in nil },
        isSuperseded: @escaping (String) -> Bool = { _ in false },
        awaited: [String: Int64] = [:]
    ) -> MembershipRow.BatchVerdict {
        MembershipRow.forBatch(
            isGroup: isGroup, hadCommit: hadCommit,
            personKeysBefore: before, personKeysAfter: after,
            ownPersonKeyB64: own,
            accountOfKey: accountOfKey, isSuperseded: isSuperseded,
            awaited: awaited, nowMs: 1_000_000)
    }

    /// A batch of two commits filed into a group record: the row lands on
    /// the LAST commit row of the batch, the earlier rows stay plain.
    private func recordWithBatch() throws -> (GroupRecord, firstNewRow: Int) {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x51, count: 32))
        let group = try Group.create(identity: identity, kind: .group)
        let record = GroupRecord(group: group, groupId: Data(repeating: 0x77, count: 32))
        record.appendLocalRow(text: MembershipRow.plainCommitText, messageId: 1, createdAt: "")
        let firstNewRow = record.messages.count
        record.consumeEvents([
            .system(messageId: 2, systemKind: .commit, createdAt: ""),
            .system(messageId: 3, systemKind: .commit, createdAt: ""),
        ])
        return (record, firstNewRow)
    }

    private func drawn(_ messages: [ChatMessage], isGroup: Bool) -> [ChatMessage] {
        messages.filter { !ChatTimeline.isHiddenSystemRow($0, isGroup: isGroup) }
    }

    private func names(_ key: String) -> String {
        [anna: "Anna", bob: "Bob"][key] ?? ResolvedDisplayName.placeholderMember
    }

    func testABatchAddingAPersonWritesJoinedOnItsLastCommitRowAndDrawsJoined() throws {
        let result = verdict(before: [own, bob], after: [own, bob, anna])
        let row = try XCTUnwrap(result.row)
        XCTAssertEqual(row.joinedKeysB64, [anna])
        XCTAssertEqual(row.leftKeysB64, [])

        let (record, firstNewRow) = try recordWithBatch()
        XCTAssertTrue(record.recordMembershipChange(row, fromIndex: firstNewRow))
        XCTAssertEqual(record.messages.map(\.text), [
            MembershipRow.plainCommitText, MembershipRow.plainCommitText, row.marker,
        ])
        XCTAssertEqual(MembershipRow.parse(record.messages[2].text), row, "stored and read back")

        let shown = drawn(record.messages, isGroup: true)
        XCTAssertEqual(shown.map(\.messageId), [3], "only the named row is drawn")
        let drawnRow = try XCTUnwrap(ChatTimeline.membershipRow(shown[0]))
        XCTAssertEqual(
            drawnRow.sentences(name: names),
            [Localization.text("chat.system.member_joined", texts: ["name": "Anna"])])
    }

    func testAPersonLeavingDrawsLeft() throws {
        let row = try XCTUnwrap(verdict(before: [own, anna, bob], after: [own, bob]).row)
        XCTAssertEqual(row.joinedKeysB64, [])
        XCTAssertEqual(row.leftKeysB64, [anna])
        XCTAssertEqual(
            row.sentences(name: names),
            [Localization.text("chat.system.member_left", texts: ["name": "Anna"])])
    }

    func testSeveralPeopleOneSentenceEachJoinedFirst() throws {
        let row = try XCTUnwrap(verdict(before: [own, anna], after: [own, bob]).row)
        XCTAssertEqual(row.sentences(name: names), [
            Localization.text("chat.system.member_joined", texts: ["name": "Bob"]),
            Localization.text("chat.system.member_left", texts: ["name": "Anna"]),
        ])
        let unknown = try XCTUnwrap(MembershipRow(joinedKeysB64: [key(0x0C)], leftKeysB64: []))
        XCTAssertEqual(
            unknown.sentences(name: names),
            [Localization.text("chat.system.member_joined", texts: ["name": ResolvedDisplayName.placeholderMember])],
            "an unknown person reads Member")
    }

    func testADeviceAddOrRemoveWritesNothingAndDrawsNothing() throws {
        // The person keys are the same before and after: only devices moved.
        let result = verdict(before: [own, anna], after: [own, anna])
        XCTAssertNil(result.row)
        let (record, _) = try recordWithBatch()
        XCTAssertEqual(drawn(record.messages, isGroup: true), [], "plain commit rows are never drawn")
    }

    func testAResetsTwoKeysAreOnePerson() {
        let annaOld = key(0x2A)
        let accounts = [annaOld: "acct-anna", anna: "acct-anna"]
        let result = verdict(
            before: [own, annaOld], after: [own, anna],
            accountOfKey: { accounts[$0] }, isSuperseded: { $0 == annaOld })
        XCTAssertNil(result.row, "the same person under a new key is not a join and a leave")
    }

    func testThisDevicesOwnPersonIsNeverNamed() {
        XCTAssertNil(verdict(before: [anna], after: [anna, own]).row)
        XCTAssertNil(verdict(before: [own, anna], after: [anna]).row)
    }

    func testAStoredCommitRowWithoutKeysIsNotDrawn() {
        let plain = ChatMessage(messageId: 1, direction: .system, text: MembershipRow.plainCommitText, createdAt: "")
        XCTAssertTrue(ChatTimeline.isHiddenSystemRow(plain, isGroup: true))
        XCTAssertNil(ChatTimeline.membershipRow(plain))
        let malformed = ChatMessage(
            messageId: 2, direction: .system, text: "[mls commit joined=notakey left=]", createdAt: "")
        XCTAssertTrue(ChatTimeline.isHiddenSystemRow(malformed, isGroup: true))
    }

    func testAOneToOneDrawsNothing() throws {
        XCTAssertNil(verdict(isGroup: false, before: [own], after: [own, anna]).row, "nothing is written")
        let row = try XCTUnwrap(MembershipRow(joinedKeysB64: [anna], leftKeysB64: []))
        let stored = ChatMessage(messageId: 1, direction: .system, text: row.marker, createdAt: "")
        XCTAssertTrue(ChatTimeline.isHiddenSystemRow(stored, isGroup: false), "and a stored one is not drawn")
        XCTAssertFalse(ChatTimeline.isHiddenSystemRow(stored, isGroup: true))
    }

    func testABatchWithoutACommitOrWithATornRosterWritesNothing() {
        XCTAssertNil(verdict(hadCommit: false, before: [own], after: [own, anna]).row)
        XCTAssertNil(verdict(before: nil, after: [own, anna]).row)
        XCTAssertNil(verdict(before: [own], after: nil).row)
    }

    func testThePreviewSkipsTheRow() throws {
        let row = try XCTUnwrap(MembershipRow(joinedKeysB64: [anna], leftKeysB64: [bob]))
        let messages = [
            ChatMessage(messageId: 1, direction: .incoming, text: "hi", createdAt: ""),
            ChatMessage(messageId: 2, direction: .system, text: row.marker, createdAt: ""),
        ]
        // The chat list asks with the default, as `ChatListViewModel.preview` does.
        XCTAssertEqual(messages.last(where: { !ChatTimeline.isHiddenSystemRow($0) })?.messageId, 1)
    }

    /// The committing device never receives its own commit back: when its
    /// own commit merges it writes the same row itself, at the commit's DS
    /// message id, in a group only.
    func testTheCommitterWritesTheRowForItsOwnMergedCommit() throws {
        for kind in [GroupKind.group, GroupKind.oneToOne] {
            let committer = try Identity.fromSeed(seed: Data(repeating: 0x61, count: 32))
            let joiner = try Identity.fromSeed(seed: Data(repeating: 0x62, count: 32))
            let group = try Group.create(identity: committer, kind: kind)
            let groupIdHex = "aa\(kind == .group ? "01" : "02")"
            let record = GroupRecord(group: group, groupId: Data(repeating: 0x63, count: 32))
            let repo = ChatRepository(
                chatClient: Offline.chatClient(), dsClient: Offline.ds(), discovery: Offline.discovery())
            repo.groups[groupIdHex] = record

            let before = ChatRepository.personKeysB64(of: group)
            _ = try group.addMemberFromWire(identity: committer, keyPackage: joiner.createKeyPackage())
            repo.noteOwnCommitMembershipChange(
                groupIdHex: groupIdHex, record: record, personKeysBefore: before, commitMessageId: 42)

            if kind == .group {
                let stored = try XCTUnwrap(record.messages.last)
                XCTAssertEqual(stored.messageId, 42, "filed at the own commit's DS id")
                XCTAssertEqual(
                    ChatTimeline.membershipRow(stored)?.joinedKeysB64,
                    [base64Encode(joiner.signaturePublicKey())])
                XCTAssertFalse(ChatTimeline.isHiddenSystemRow(stored, isGroup: true))
                // A second call for the same commit files nothing more.
                repo.noteOwnCommitMembershipChange(
                    groupIdHex: groupIdHex, record: record, personKeysBefore: before,
                    commitMessageId: 42)
                XCTAssertEqual(record.messages.filter { $0.messageId == 42 }.count, 1)
            } else {
                XCTAssertTrue(record.messages.isEmpty, "a one-to-one gets no row")
            }
        }
    }

    /// The row survives the message log, and the history bundle still leaves
    /// it out like every other bookkeeping row.
    func testTheRowIsPersistedAndStaysOutOfTheHistoryBundle() throws {
        let row = try XCTUnwrap(MembershipRow(joinedKeysB64: [anna], leftKeysB64: [bob]))
        let message = ChatMessage(messageId: 7, direction: .system, text: row.marker, createdAt: "")
        let persisted = PersistedMessage(from: message)
        let restored = try XCTUnwrap(persisted.toChatMessage())
        XCTAssertEqual(ChatTimeline.membershipRow(restored), row)
        XCTAssertTrue(HistoryBundleCodec.isBookkeepingSystemRow(persisted))
    }

    // MARK: - Each commit of a batch on its own (seen live 2026-11-22)

    private func commits(
        before: [String]?,
        _ commits: [(Int64, [String]?)],
        isSuperseded: @escaping (String) -> Bool = { _ in false },
        accountOfKey: @escaping (String) -> String? = { _ in nil }
    ) -> MembershipRow.CommitsVerdict {
        MembershipRow.forCommits(
            isGroup: true,
            personKeysBefore: before,
            commits: commits.map { (messageId: $0.0, personKeysAfter: $0.1) },
            ownPersonKeyB64: own,
            accountOfKey: accountOfKey, isSuperseded: isSuperseded,
            awaited: [:], nowMs: 1_000_000)
    }

    /// The live defect: the owner removed a person and added them back, and a
    /// member received both commits in one poll. Judged as one batch the
    /// people were the same before and after, so nothing was drawn; judged
    /// commit by commit each row names its own change.
    func testARemovalAndAReAddInOneBatchNameBothOnTheirOwnRows() throws {
        XCTAssertNil(
            verdict(before: [own, anna, bob], after: [own, anna, bob]).row,
            "the batch-wide rule sees no change at all")

        let result = commits(before: [own, anna, bob], [(2, [own, anna]), (3, [own, anna, bob])])
        XCTAssertEqual(result.rows[2], MembershipRow(joinedKeysB64: [], leftKeysB64: [bob]))
        XCTAssertEqual(result.rows[3], MembershipRow(joinedKeysB64: [bob], leftKeysB64: []))

        let (record, firstNewRow) = try recordWithBatch()
        for (id, row) in result.rows {
            XCTAssertTrue(record.recordMembershipChange(row, messageId: id, fromIndex: firstNewRow))
        }
        let shown = drawn(record.messages, isGroup: true)
        XCTAssertEqual(shown.map(\.messageId), [2, 3], "both lines are drawn, in journal order")
        XCTAssertEqual(
            shown.compactMap(ChatTimeline.membershipRow).flatMap { $0.sentences(name: names) },
            [
                Localization.text("chat.system.member_left", texts: ["name": "Bob"]),
                Localization.text("chat.system.member_joined", texts: ["name": "Bob"]),
            ])
    }

    func testOneCommitPerBatchIsJudgedAsBefore() {
        let result = commits(before: [own, bob], [(7, [own, bob, anna])])
        XCTAssertEqual(result.rows, [7: MembershipRow(joinedKeysB64: [anna], leftKeysB64: [])!])
    }

    /// A torn roster read after a commit records nothing for it, nor for the
    /// next commit, whose "before" is then unknown; the one after that is
    /// judged again.
    func testATornReadRecordsNothingForThatCommitAndTheNext() {
        let result = commits(
            before: [own, bob], [(2, nil), (3, [own, bob, anna]), (4, [own, anna])])
        XCTAssertNil(result.rows[2])
        XCTAssertNil(result.rows[3])
        XCTAssertEqual(result.rows[4], MembershipRow(joinedKeysB64: [], leftKeysB64: [bob]))
    }

    /// A commit that only admitted another device of a person already there
    /// names nobody, and does not hide the person change of the next commit.
    func testADeviceOnlyCommitNamesNobody() {
        let result = commits(before: [own, bob], [(2, [own, bob]), (3, [own])])
        XCTAssertNil(result.rows[2])
        XCTAssertEqual(result.rows[3], MembershipRow(joinedKeysB64: [], leftKeysB64: [bob]))
    }

    /// A reset's device swap across two commits of one batch is still one
    /// person: the memory carries from the first commit to the second.
    func testAResetSwapAcrossTwoCommitsOfOneBatchStaysSilent() {
        let old = key(0x0C)
        let new = key(0x0D)
        let result = commits(
            before: [own, old], [(2, [own]), (3, [own, new])],
            isSuperseded: { $0 == old },
            accountOfKey: { [old: "acct", new: "acct"][$0] })
        XCTAssertTrue(result.rows.isEmpty)
    }

    /// The row goes on the commit with that DS id, not on the batch's last.
    func testTheRowIsWrittenOnTheCommitOfItsId() throws {
        let (record, firstNewRow) = try recordWithBatch()
        let row = try XCTUnwrap(MembershipRow(joinedKeysB64: [anna], leftKeysB64: []))
        XCTAssertTrue(record.recordMembershipChange(row, messageId: 2, fromIndex: firstNewRow))
        XCTAssertEqual(record.messages.map(\.text), [
            MembershipRow.plainCommitText, row.marker, MembershipRow.plainCommitText,
        ])
        XCTAssertFalse(
            record.recordMembershipChange(row, messageId: 1, fromIndex: firstNewRow),
            "a row before the batch is not touched")
        XCTAssertFalse(record.recordMembershipChange(row, messageId: 9, fromIndex: firstNewRow))
    }
}
