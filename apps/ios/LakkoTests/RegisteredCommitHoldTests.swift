// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0091 phase 1 — a device does not skip the commit that moves its
/// conversation. The verdict, the hold tracker, the conversation apply loop
/// (against a real FFI `Identity` + `Group`) and the log lines.
final class RegisteredCommitHoldTests: XCTestCase {

    // MARK: - 1. The verdict

    private func verdict(
        own: Bool = false, sealed: UInt64? = 4, after: UInt64? = 5, local: UInt64? = 4, latest: UInt64? = 5
    ) -> RegisteredCommitVerdict {
        RegisteredCommitVerdict.of(
            isOwnEcho: own, sealedEpoch: sealed, epochAfter: after, localEpoch: local, latestEpoch: latest)
    }

    func testTheRegisteredCommitForTheNextEpochIsHeld() {
        XCTAssertEqual(verdict(), .hold)
        XCTAssertEqual(verdict(latest: 9), .hold, "a registered head further above still holds")
    }

    func testEverythingElseIsSkipped() {
        XCTAssertEqual(verdict(own: true), .skip, "own echo")
        XCTAssertEqual(verdict(sealed: nil), .skip, "unknown sealed epoch")
        XCTAssertEqual(verdict(after: nil), .skip, "unknown epoch after")
        XCTAssertEqual(verdict(local: nil), .skip, "unknown local epoch")
        XCTAssertEqual(verdict(after: 4), .skip, "not a commit")
        XCTAssertEqual(verdict(sealed: 3, after: 4), .skip, "sealed below local")
        XCTAssertEqual(verdict(sealed: 5, after: 6), .skip, "sealed above local")
        XCTAssertEqual(verdict(latest: nil), .skip, "latest unknown")
        XCTAssertEqual(verdict(latest: 4), .skip, "the DS names no later epoch")
        XCTAssertEqual(verdict(sealed: .max, after: .max, local: .max, latest: .max), .skip, "no overflow")
    }

    // MARK: - 2. The tracker

    private final class Clock {
        var ms: Int64 = 1_000_000
    }

    func testTheBackoffScheduleAndExhaustionAtEight() {
        let clock = Clock()
        let hold = RegisteredCommitHold(nowMs: { clock.ms })
        let expected: [Int64] = [2, 5, 15, 30, 60, 120, 300]
        for (index, seconds) in expected.enumerated() {
            let outcome = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
            XCTAssertEqual(outcome, .held(attempt: index + 1, nextAttemptAtMs: clock.ms + seconds * 1000))
            XCTAssertFalse(hold.isDue(groupIdHex: "g"))
            clock.ms += seconds * 1000 - 1
            XCTAssertFalse(hold.isDue(groupIdHex: "g"), "not due a millisecond early")
            clock.ms += 1
            XCTAssertTrue(hold.isDue(groupIdHex: "g"))
        }
        XCTAssertNil(hold.behindMark(groupIdHex: "g"))
        let last = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        XCTAssertEqual(last, .exhausted(attempt: 8))
        XCTAssertNil(hold.hold(groupIdHex: "g"), "the eighth failure drops the hold")
        XCTAssertEqual(hold.behindMark(groupIdHex: "g"), .init(epoch: 3, atMs: clock.ms))
        XCTAssertTrue(hold.isDue(groupIdHex: "g"))
    }

    func testANewRowStartsAgainAtOne() {
        let hold = RegisteredCommitHold(nowMs: { 0 })
        _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        XCTAssertEqual(hold.hold(groupIdHex: "g")?.attempts, 2)
        XCTAssertEqual(
            hold.recordFailure(groupIdHex: "g", rowId: 11, sealedEpoch: 3, localEpoch: 3),
            .held(attempt: 1, nextAttemptAtMs: 2000))
        XCTAssertNil(hold.hold(groupIdHex: "other"), "keyed by group")
    }

    func testSuccessClearsTheHoldAndAMovedEpochLiftsTheMark() {
        let hold = RegisteredCommitHold(nowMs: { 0 })
        _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        hold.recordApplied(groupIdHex: "g", rowId: 9)
        XCTAssertNotNil(hold.hold(groupIdHex: "g"), "another row applying is not the held one")
        hold.recordApplied(groupIdHex: "g", rowId: 10)
        XCTAssertNil(hold.hold(groupIdHex: "g"))

        _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        hold.observeEpoch(groupIdHex: "g", epoch: 3)
        XCTAssertNotNil(hold.hold(groupIdHex: "g"))
        hold.observeEpoch(groupIdHex: "g", epoch: 4)
        XCTAssertNil(hold.hold(groupIdHex: "g"), "an epoch past the held row answers it")

        for _ in 0..<RegisteredCommitHold.maxAttempts {
            _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        }
        hold.observeEpoch(groupIdHex: "g", epoch: 3)
        XCTAssertNotNil(hold.behindMark(groupIdHex: "g"))
        hold.observeEpoch(groupIdHex: "g", epoch: 4)
        XCTAssertNil(hold.behindMark(groupIdHex: "g"), "the epoch moved above the mark")
    }

    func testThePollGateWaitsTheShorterOfTheLongPollAndTheAttempt() {
        let clock = Clock()
        let hold = RegisteredCommitHold(nowMs: { clock.ms })
        XCTAssertEqual(hold.pollGate(groupIdHex: "g", waitSecs: 25), .fetch, "no hold")
        _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        XCTAssertEqual(hold.pollGate(groupIdHex: "g", waitSecs: 25), .wait(ms: 2000))
        XCTAssertEqual(hold.pollGate(groupIdHex: "g", waitSecs: 1), .wait(ms: 1000))
        XCTAssertEqual(hold.pollGate(groupIdHex: "g", waitSecs: 0), .wait(ms: 0))
        clock.ms += 2000
        XCTAssertEqual(hold.pollGate(groupIdHex: "g", waitSecs: 25), .fetch, "due")
    }

    func testTheRepairGuard() {
        let hold = RegisteredCommitHold(nowMs: { 0 })
        XCTAssertFalse(hold.blocksRepair(groupIdHex: "g", notedEpoch: 3))
        _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        XCTAssertTrue(hold.blocksRepair(groupIdHex: "g", notedEpoch: 3))
        for _ in 1..<RegisteredCommitHold.maxAttempts {
            _ = hold.recordFailure(groupIdHex: "g", rowId: 10, sealedEpoch: 3, localEpoch: 3)
        }
        XCTAssertTrue(hold.blocksRepair(groupIdHex: "g", notedEpoch: 3), "mark at the noted epoch")
        XCTAssertTrue(hold.blocksRepair(groupIdHex: "g", notedEpoch: 2), "mark above the noted epoch")
        XCTAssertFalse(hold.blocksRepair(groupIdHex: "g", notedEpoch: 4), "mark below the noted epoch")
    }

    // MARK: - 3. The apply loop (real FFI)

    private func identity(_ byte: UInt8) throws -> Identity {
        try Identity.fromSeed(seed: Data(repeating: byte, count: 32))
    }

    private func batch(_ rows: [(Int64, Data)], latest: UInt64?) -> FetchResult {
        FetchResult(
            messages: rows.map { MailboxMessage(id: $0.0, payload: $0.1, createdAt: "t-\($0.0)") },
            hasMore: true,
            latestEpoch: latest)
    }

    /// A commit framed at epoch 0 (epoch after 1) that a fresh group of
    /// another identity at epoch 0 cannot apply: the fixture of "a registered
    /// commit this device fails on".
    private func unappliableCommit() throws -> Data {
        let committer = try identity(0x52)
        let other = try Group.create(identity: committer, kind: .group)
        let commit = try other.addMemberFromWire(
            identity: committer, keyPackage: try identity(0x53).createKeyPackage()).commit
        XCTAssertEqual(epochOfMessage(bytes: commit), 0)
        XCTAssertEqual(epochAfterMessage(bytes: commit), 1)
        return commit
    }

    private func capturedLines(_ work: () throws -> Void) rethrows -> [String] {
        var lines: [String] = []
        let original = RegisteredCommitLog.sink
        RegisteredCommitLog.sink = { _, line in lines.append(line) }
        defer { RegisteredCommitLog.sink = original }
        try work()
        return lines
    }

    func testARegisteredCommitThatFailsIsHeldAndRetriedOnTheNextDuePoll() throws {
        let local = try identity(0x51)
        let group = try Group.create(identity: local, kind: .group)
        let journal = DsClient.pseudonymHex(try group.groupId())
        let commit = try unappliableCommit()
        let clock = Clock()
        let client = Offline.chatClient()
        client.registeredCommitHold = RegisteredCommitHold(nowMs: { clock.ms })
        let garbage = Data([0xFF, 0xFF, 0xFF])

        var first: PollMessagesResult!
        let lines = capturedLines {
            first = client.applyFetchedMessages(
                identity: local, group: group, groupIdHex: journal,
                raw: batch([(5, garbage), (6, commit), (7, garbage)], latest: 1), after: 4)
        }
        XCTAssertEqual(first.cursor, 5, "the cursor stays before the held row; row 5 was skipped")
        XCTAssertFalse(first.hasMore, "a held batch does not ask to be read again at once")
        XCTAssertEqual(client.registeredCommitHold.hold(groupIdHex: journal)?.rowId, 6)
        XCTAssertEqual(client.registeredCommitHold.hold(groupIdHex: journal)?.attempts, 1)
        XCTAssertEqual(lines.count, 2, "row 7 was not consumed: \(lines)")
        XCTAssertTrue(lines[1].contains("sealed=0 after=1 local=0 latest=1 verdict=hold attempt=1/8"), lines[1])
        XCTAssertEqual(
            client.registeredCommitHold.pollGate(groupIdHex: journal, waitSecs: 25), .wait(ms: 2000))

        // The next due poll fetches from the held cursor and retries.
        clock.ms += 2000
        XCTAssertEqual(client.registeredCommitHold.pollGate(groupIdHex: journal, waitSecs: 25), .fetch)
        let second = client.applyFetchedMessages(
            identity: local, group: group, groupIdHex: journal,
            raw: batch([(6, commit), (7, garbage)], latest: 1), after: first.cursor)
        XCTAssertEqual(second.cursor, 5)
        XCTAssertEqual(client.registeredCommitHold.hold(groupIdHex: journal)?.attempts, 2)
    }

    func testAFailingRowTheDsDidNotRegisterIsStillSkipped() throws {
        let local = try identity(0x54)
        let group = try Group.create(identity: local, kind: .group)
        let journal = DsClient.pseudonymHex(try group.groupId())
        let commit = try unappliableCommit()
        let client = Offline.chatClient()
        client.registeredCommitHold = RegisteredCommitHold(nowMs: { 0 })

        for latest: UInt64? in [nil, 0] {
            let result = client.applyFetchedMessages(
                identity: local, group: group, groupIdHex: journal,
                raw: batch([(6, commit), (7, Data([0xFF]))], latest: latest), after: 5)
            XCTAssertEqual(result.cursor, 7, "skipped as before when latest is \(String(describing: latest))")
            XCTAssertNil(client.registeredCommitHold.hold(groupIdHex: journal))
        }
    }

    func testTheEighthFailureSkipsTheRowAndMarksTheDeviceBehind() throws {
        let local = try identity(0x55)
        let group = try Group.create(identity: local, kind: .group)
        let journal = DsClient.pseudonymHex(try group.groupId())
        let commit = try unappliableCommit()
        let clock = Clock()
        let client = Offline.chatClient()
        client.registeredCommitHold = RegisteredCommitHold(nowMs: { clock.ms })
        for _ in 1..<RegisteredCommitHold.maxAttempts {
            let held = client.applyFetchedMessages(
                identity: local, group: group, groupIdHex: journal,
                raw: batch([(6, commit), (7, Data([0xFF]))], latest: 1), after: 5)
            XCTAssertEqual(held.cursor, 5)
            clock.ms += 300_000
        }
        var last: PollMessagesResult!
        let lines = capturedLines {
            last = client.applyFetchedMessages(
                identity: local, group: group, groupIdHex: journal,
                raw: batch([(6, commit), (7, Data([0xFF]))], latest: 1), after: 5)
        }
        XCTAssertEqual(last.cursor, 7, "spent: skipped as today")
        XCTAssertNil(client.registeredCommitHold.hold(groupIdHex: journal))
        XCTAssertEqual(client.registeredCommitHold.behindMark(groupIdHex: journal)?.epoch, 0)
        XCTAssertTrue(lines.contains(
            "chat registered commit skipped after 8 attempts: this device is now behind in a conversation "
                + "(local=0 latest=1)"), "\(lines)")
    }

    func testTheHeldRowApplyingDropsTheHold() throws {
        let alice = try identity(0x56)
        let bob = try identity(0x57)
        let aliceGroup = try Group.create(identity: alice, kind: .group)
        let added = try aliceGroup.addMemberFromWire(identity: alice, keyPackage: try bob.createKeyPackage())
        let bobGroup = try Group.joinFromWelcome(welcome: added.welcome, identity: bob)
        let next = try aliceGroup.addMemberFromWire(
            identity: alice, keyPackage: try identity(0x58).createKeyPackage()).commit
        let journal = DsClient.pseudonymHex(try bobGroup.groupId())
        let client = Offline.chatClient()
        client.registeredCommitHold = RegisteredCommitHold(nowMs: { 0 })
        let epoch = try bobGroup.epoch()
        _ = client.registeredCommitHold.recordFailure(
            groupIdHex: journal, rowId: 6, sealedEpoch: epoch, localEpoch: epoch)

        let result = client.applyFetchedMessages(
            identity: bob, group: bobGroup, groupIdHex: journal,
            raw: batch([(6, next)], latest: epoch + 1), after: 5)
        XCTAssertEqual(result.cursor, 6)
        XCTAssertEqual(try bobGroup.epoch(), epoch + 1)
        XCTAssertNil(client.registeredCommitHold.hold(groupIdHex: journal))
    }

    // MARK: - 4. The log lines carry no identifier

    func testTheLogLinesNameNoGroupAndNoRow() throws {
        let local = try identity(0x59)
        let group = try Group.create(identity: local, kind: .group)
        let journal = DsClient.pseudonymHex(try group.groupId())
        let commit = try unappliableCommit()
        let client = Offline.chatClient()
        client.registeredCommitHold = RegisteredCommitHold(nowMs: { 0 })
        let rowId: Int64 = 987_654_321
        let lines = capturedLines {
            _ = client.applyFetchedMessages(
                identity: local, group: group, groupIdHex: journal,
                raw: batch([(rowId - 1, Data([0xFF])), (rowId, commit)], latest: 1), after: 0)
            _ = client.failedRowIsHeld(
                groupIdHex: journal, rowId: rowId, payload: commit, localEpoch: 0, latestEpoch: 1,
                error: FfiError.MembershipNotOwner(message: "refused in \(journal) row \(rowId)"))
        }
        XCTAssertFalse(lines.isEmpty)
        for line in lines {
            XCTAssertFalse(line.contains(journal), line)
            XCTAssertFalse(line.lowercased().contains(journal.prefix(16).lowercased()), line)
            XCTAssertFalse(line.contains(String(rowId)), line)
            XCTAssertFalse(line.contains(String(rowId - 1)), line)
        }
        XCTAssertTrue(lines.contains(
            "chat registered commit refused by this device's rules (refused:notOwner): "
                + "the other devices applied it; a bug or version skew"), "\(lines)")
    }

    func testTheFailureKinds() {
        XCTAssertEqual(FailedApplyKind.of(FfiError.MembershipAddNotOwnerOrAdmin(message: "x")),
                       "refused:addNotOwnerOrAdmin")
        XCTAssertEqual(FailedApplyKind.of(FfiError.Message(message: "ValidationError(WrongEpoch)")),
                       "wrong-epoch")
        XCTAssertEqual(FfiError.Message(message: "CannotDecryptOwnMessage").kindForTest, "own-message")
        XCTAssertEqual(FailedApplyKind.of(FfiError.Message(message: "InvalidCommit(MissingProposal)")),
                       "missing-proposal")
        XCTAssertEqual(FailedApplyKind.of(FfiError.Message(message: "AeadError")), "decrypt")
        XCTAssertEqual(FailedApplyKind.of(FfiError.Codec(message: "short")), "other")
        XCTAssertEqual(
            RegisteredCommitLog.failedRow(
                kind: "decrypt", sealed: nil, after: nil, local: 2, latest: nil, verdict: .skip, attempt: nil),
            "chat row not applied: kind=decrypt sealed=- after=- local=2 latest=- verdict=skip attempt=-/8")
    }
}

private extension FfiError {
    var kindForTest: String { FailedApplyKind.of(self) }
}
