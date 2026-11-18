// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0091 phase 1 — a device does not skip the commit that moves its
/// conversation. The verdict, the hold tracker, the conversation apply loop
/// (against a real FFI `Identity` + `Group`) and the log lines.
final class RegisteredCommitHoldTests: XCTestCase {

    // MARK: - 1. The verdict (the same truth table as web and Android)

    /// Row 7967 is the commit the DS registered as leaving epoch 6.
    private let registered: [UInt64: Int64] = [7: 7967, 8: 8100]

    private func verdict(
        own: Bool = false, rowId: Int64 = 7967, local: UInt64? = 6, commits: [UInt64: Int64]?? = .none
    ) -> RegisteredCommitVerdict {
        RegisteredCommitVerdict.of(
            isOwnEcho: own, rowId: rowId, localEpoch: local,
            registeredCommits: commits ?? registered)
    }

    func testTheRegisteredCommitLeavingThisEpochIsHeld() {
        XCTAssertEqual(verdict(), .hold)
        XCTAssertEqual(verdict(rowId: 8100, local: 7), .hold)
    }

    func testEverythingElseIsSkipped() {
        XCTAssertEqual(verdict(own: true), .skip, "own echo")
        XCTAssertEqual(verdict(local: nil), .skip, "unknown local epoch")
        XCTAssertEqual(verdict(commits: .some(nil)), .skip, "a read that named no commits")
        XCTAssertEqual(verdict(commits: .some([:])), .skip, "an empty list")
        XCTAssertEqual(verdict(rowId: 7966), .skip, "another row at the same epoch (plain publish)")
        XCTAssertEqual(verdict(rowId: 8100), .skip, "the commit of the epoch after the next")
        XCTAssertEqual(verdict(local: 7), .skip, "a commit this device is already past")
        XCTAssertEqual(verdict(local: .max), .skip, "no overflow")
    }

    func testTheCommitLeavingAnEpoch() {
        XCTAssertEqual(RegisteredCommitVerdict.leaving(registeredCommits: registered, localEpoch: 6), 7967)
        XCTAssertNil(RegisteredCommitVerdict.leaving(registeredCommits: registered, localEpoch: 8))
        XCTAssertNil(RegisteredCommitVerdict.leaving(registeredCommits: nil, localEpoch: 6))
        XCTAssertNil(RegisteredCommitVerdict.leaving(registeredCommits: registered, localEpoch: nil))
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

    /// A batch; `registered` maps an epoch to the row the DS registered as
    /// reaching it (present only where `latest` is, as on the wire).
    private func batch(
        _ rows: [(Int64, Data)], latest: UInt64?, registered: [UInt64: Int64] = [:]
    ) -> FetchResult {
        FetchResult(
            messages: rows.map { MailboxMessage(id: $0.0, payload: $0.1, createdAt: "t-\($0.0)") },
            hasMore: true,
            latestEpoch: latest,
            registeredCommits: latest == nil ? nil : registered)
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
                raw: batch([(5, garbage), (6, commit), (7, garbage)], latest: 1, registered: [1: 6]),
                after: 4)
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
            raw: batch([(6, commit), (7, garbage)], latest: 1, registered: [1: 6]), after: first.cursor)
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

        // No commits named (an unclaimed journal), none for this epoch, or
        // another row named (row 6 is a commit-shaped row posted through the
        // plain publish route).
        for (latest, registered) in [(nil, [:]), (UInt64(0), [:]), (UInt64(1), [UInt64(1): Int64(9)])]
            as [(UInt64?, [UInt64: Int64])] {
            let result = client.applyFetchedMessages(
                identity: local, group: group, groupIdHex: journal,
                raw: batch([(6, commit), (7, Data([0xFF]))], latest: latest, registered: registered), after: 5)
            XCTAssertEqual(result.cursor, 7, "skipped as before with \(registered)")
            XCTAssertNil(client.registeredCommitHold.hold(groupIdHex: journal))
        }
    }

    // MARK: - 3b. ADR-0091 phase 1b — the cursor epoch

    func testTheRewindRule() {
        let commits: [UInt64: Int64] = [7: 500, 8: 640]
        XCTAssertEqual(CursorEpoch.rewindFrom(after: 700, loaded: 6, recorded: 8, registeredCommits: commits), 499)
        XCTAssertEqual(CursorEpoch.rewindFrom(after: 500, loaded: 6, recorded: 7, registeredCommits: commits), 499,
                       "at the cursor itself")
        XCTAssertNil(CursorEpoch.rewindFrom(after: 700, loaded: 6, recorded: nil, registeredCommits: commits))
        XCTAssertNil(CursorEpoch.rewindFrom(after: 700, loaded: nil, recorded: 8, registeredCommits: commits))
        XCTAssertNil(CursorEpoch.rewindFrom(after: 700, loaded: 8, recorded: 8, registeredCommits: commits))
        XCTAssertNil(CursorEpoch.rewindFrom(after: 700, loaded: 9, recorded: 8, registeredCommits: commits))
        XCTAssertNil(CursorEpoch.rewindFrom(after: 400, loaded: 6, recorded: 8, registeredCommits: commits),
                     "the commit is still ahead of the cursor")
        XCTAssertNil(CursorEpoch.rewindFrom(after: 700, loaded: 4, recorded: 8, registeredCommits: commits),
                     "no commit named for the loaded epoch")
        XCTAssertNil(CursorEpoch.rewindFrom(after: 700, loaded: 6, recorded: 8, registeredCommits: nil))
        XCTAssertEqual(
            CursorEpoch.rewindLine(loaded: 6, recorded: 8),
            "chat snapshot behind its cursor: loaded=6 recorded=8; "
                + "reading again from the commit that leaves the loaded epoch")
    }

    func testTheRecordedEpochNeverMovesBackwardAndTravelsWithTheCursor() throws {
        XCTAssertEqual(CursorEpoch.advanced(nil, to: 3), 3)
        XCTAssertEqual(CursorEpoch.advanced(5, to: 3), 5)
        XCTAssertEqual(CursorEpoch.advanced(5, to: 9), 9)
        XCTAssertEqual(CursorEpoch.advanced(5, to: nil), 5)

        let slot = PersistedGroup(cursor: 42, messages: [], cursorEpoch: 7)
        let decoded = try JSONDecoder().decode(PersistedGroup.self, from: JSONEncoder().encode(slot))
        XCTAssertEqual(decoded.cursorEpoch, 7)
        XCTAssertEqual(decoded.cursor, 42)
        let legacy = try JSONDecoder().decode(
            PersistedGroup.self, from: Data(#"{"cursor":3,"messages":[]}"#.utf8))
        XCTAssertNil(legacy.cursorEpoch)
    }

    /// A rewound batch (`readFrom` below `after`) applies from the rewind point
    /// but never reports a cursor below `after`.
    func testARewoundBatchNeverWindsTheCursorBackward() throws {
        let alice = try identity(0x5A)
        let bob = try identity(0x5B)
        let aliceGroup = try Group.create(identity: alice, kind: .group)
        let added = try aliceGroup.addMemberFromWire(identity: alice, keyPackage: try bob.createKeyPackage())
        let bobGroup = try Group.joinFromWelcome(welcome: added.welcome, identity: bob)
        let next = try aliceGroup.addMemberFromWire(
            identity: alice, keyPackage: try identity(0x5C).createKeyPackage()).commit
        let journal = DsClient.pseudonymHex(try bobGroup.groupId())
        let epoch = try bobGroup.epoch()
        let client = Offline.chatClient()
        client.registeredCommitHold = RegisteredCommitHold(nowMs: { 0 })
        // The commit that leaves Bob's epoch is row 20; Bob's cursor is 30.
        let result = client.applyFetchedMessages(
            identity: bob, group: bobGroup, groupIdHex: journal,
            raw: batch([(20, next), (25, Data([0xFF]))], latest: epoch + 1, registered: [epoch + 1: 20]),
            after: 30, readFrom: 19)
        XCTAssertEqual(try bobGroup.epoch(), epoch + 1, "the commit behind the cursor applied")
        XCTAssertEqual(result.cursor, 30, "the cursor never moves below `after`")
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
                raw: batch([(6, commit), (7, Data([0xFF]))], latest: 1, registered: [1: 6]), after: 5)
            XCTAssertEqual(held.cursor, 5)
            clock.ms += 300_000
        }
        var last: PollMessagesResult!
        let lines = capturedLines {
            last = client.applyFetchedMessages(
                identity: local, group: group, groupIdHex: journal,
                raw: batch([(6, commit), (7, Data([0xFF]))], latest: 1, registered: [1: 6]), after: 5)
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
            raw: batch([(6, next)], latest: epoch + 1, registered: [epoch + 1: 6]), after: 5)
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
                raw: batch([(rowId - 1, Data([0xFF])), (rowId, commit)], latest: 1, registered: [1: rowId]),
                after: 0)
            _ = client.failedRowIsHeld(
                groupIdHex: journal, rowId: rowId, payload: commit, localEpoch: 0, latestEpoch: 1,
                registeredCommits: [1: rowId],
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
