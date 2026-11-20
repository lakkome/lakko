// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// A statement signer that records which statement it was asked for.
private final class RepairSigner: DsStatementSignerProtocol, @unchecked Sendable {
    var calls: [String] = []
    private func req(_ segment: String) -> DsStatementRequest {
        DsStatementRequest(path: "/ds/group/ab/\(segment)", bodyJson: "{}")
    }
    func claim(journalId: Data, epoch: UInt64, epochKey: Data) throws -> DsStatementRequest {
        calls.append("claim"); return req("claim")
    }
    func publicKey() -> Data { Data(repeating: 0x21, count: 32) }
    func pushBinding(journalId: Data, pushGid: Data) throws -> DsStatementRequest {
        calls.append("push-binding"); return req("push-binding")
    }
    func commit(journalId: Data, epoch: UInt64, epochKey: Data, revoke: [Data], row: Data) throws
        -> DsStatementRequest {
        calls.append("commit:\(epoch)"); return req("commit")
    }
    func registrars(journalId: Data, changes: [DsRegistrarChange]) throws -> DsStatementRequest {
        calls.append("registrars"); return req("registrars")
    }
    func registrarsSet(journalId: Data, entries: [DsRegistrarEntry]) throws -> DsStatementRequest {
        calls.append("registrars-set"); return req("registrars-set")
    }
    func replaceEpoch(journalId: Data, epoch: UInt64, epochKey: Data, revoke: [Data]) throws
        -> DsStatementRequest {
        calls.append("replace:\(epoch)"); return req("access")
    }
    func reset(journalId: Data, epoch: UInt64, epochKey: Data, registrars: [DsRegistrarEntry]?) throws
        -> DsStatementRequest {
        calls.append("reset"); return req("reset")
    }
    func rootTransfer(journalId: Data, newRoot: Data) throws -> DsStatementRequest {
        calls.append("root"); return req("root")
    }
}

private final class RepairDs: DsStatementSending {
    var sent: [DsStatementRequest] = []
    var answers: [DsStatementError?] = []
    func putStatement(_ request: DsStatementRequest) async throws {
        sent.append(request)
        if !answers.isEmpty, let error = answers.removeFirst() { throw error }
    }
}

/// ADR-0057, second security pass (finding 1 + LOW 7): the repair from above.
/// It sends a Replace only after the delay, only when the epoch did not move,
/// only for the root or an admin, and never when the head could not be read.
@MainActor
final class JournalRepairTests: XCTestCase {

    @MainActor
    private final class World {
        var authority: JournalAuthority?
        var epoch: UInt64 = 7
        var polls = 0
        var pollError: Error?
        var advanceOnPoll = false
        let ds = RepairDs()
        var scheduled: [(TimeInterval, @MainActor () -> Void)] = []
        /// ADR-0091 phase 1 — the hold tracker the guard consults.
        var hold = RegisteredCommitHold(nowMs: { 1_000 })

        func repair() -> JournalRepair {
            JournalRepair(ports: .init(
                authority: { _ in self.authority },
                pollToHead: { _ in
                    self.polls += 1
                    if let error = self.pollError { throw error }
                    if self.advanceOnPoll { self.epoch += 1 }
                },
                currentEpochKey: { _ in (epoch: self.epoch, key: Data(repeating: 9, count: 32)) },
                journalId: { _ in Data(repeating: 0xAB, count: 32) },
                ds: ds,
                schedule: { seconds, work in
                    self.scheduled.append((seconds, work))
                    return {}
                },
                registeredCommitKnown: { groupIdHex, epoch in
                    self.hold.blocksRepair(groupIdHex: groupIdHex, notedEpoch: epoch)
                }))
        }
    }

    /// ADR-0091 phase 1 — a held registered commit means the epoch above is
    /// real: no Replace, and the guard's line is logged.
    func testNoReplaceWhileARegisteredCommitIsHeld() async {
        let w = World()
        let root = RepairSigner()
        w.authority = .root(root)
        _ = w.hold.recordFailure(groupIdHex: "ab", rowId: 40, sealedEpoch: 7, localEpoch: 7)
        let outcome = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(outcome, .registeredCommitKnown)
        XCTAssertTrue(root.calls.isEmpty)
        XCTAssertTrue(w.ds.sent.isEmpty)
    }

    /// ADR-0091 phase 1 — a behind mark at or above the noted epoch blocks the
    /// Replace; one below it does not.
    func testABehindMarkAtOrAboveTheEpochBlocksTheReplace() async {
        let w = World()
        let root = RepairSigner()
        w.authority = .root(root)
        for _ in 0..<RegisteredCommitHold.maxAttempts {
            _ = w.hold.recordFailure(groupIdHex: "ab", rowId: 40, sealedEpoch: 7, localEpoch: 7)
        }
        XCTAssertNil(w.hold.hold(groupIdHex: "ab"))
        XCTAssertEqual(w.hold.behindMark(groupIdHex: "ab")?.epoch, 7)
        let blocked = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(blocked, .registeredCommitKnown)
        XCTAssertTrue(root.calls.isEmpty)

        let other = World()
        let otherRoot = RepairSigner()
        other.authority = .root(otherRoot)
        other.epoch = 8
        for _ in 0..<RegisteredCommitHold.maxAttempts {
            _ = other.hold.recordFailure(groupIdHex: "ab", rowId: 40, sealedEpoch: 7, localEpoch: 7)
        }
        let replaced = await other.repair().check(groupIdHex: "ab", epoch: 8)
        XCTAssertEqual(replaced, .replaced, "a mark below the noted epoch does not block")
        XCTAssertEqual(otherRoot.calls, ["replace:8"])
    }

    func testTheCheckIsScheduledThirtySecondsLaterAndReplacesWhenTheEpochDidNotMove() async {
        let w = World()
        let admin = RepairSigner()
        w.authority = .registrar(admin, .admin)
        let repair = w.repair()
        XCTAssertTrue(repair.note(groupIdHex: "ab", epoch: 7, reason: .appendConflict))
        XCTAssertEqual(w.scheduled.map(\.0), [30])
        XCTAssertEqual(JournalRepair.delaySeconds, 30)
        XCTAssertTrue(w.ds.sent.isEmpty, "nothing before the delay")
        XCTAssertFalse(repair.note(groupIdHex: "ab", epoch: 7, reason: .refusedAtHead),
                       "one pending check per group")

        let outcome = await repair.check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(outcome, .replaced)
        XCTAssertEqual(w.polls, 1, "polled to the head first")
        XCTAssertEqual(admin.calls, ["replace:7"], "Replace at the device's own epoch, never an Append")
    }

    func testNoReplaceWhenTheGroupMovedPastTheEpoch() async {
        let w = World()
        let root = RepairSigner()
        w.authority = .root(root)
        w.advanceOnPoll = true
        let outcome = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(outcome, .moved)
        XCTAssertTrue(root.calls.isEmpty)
        XCTAssertTrue(w.ds.sent.isEmpty)
    }

    func testAMemberNeverRepairs() async {
        let w = World()
        let member = RepairSigner()
        w.authority = .registrar(member, .member)
        let repair = w.repair()
        XCTAssertFalse(repair.note(groupIdHex: "ab", epoch: 7, reason: .appendConflict))
        XCTAssertFalse(repair.note(groupIdHex: "ab", epoch: 7, reason: .refusedAtHead))
        XCTAssertTrue(w.scheduled.isEmpty)
        w.authority = nil
        XCTAssertFalse(repair.note(groupIdHex: "ab", epoch: 7, reason: .appendConflict))
        let direct = await repair.check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(direct, .notPermitted, "and a check that finds a member sends nothing")
        w.authority = .registrar(member, .member)
        let asMember = await repair.check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(asMember, .notPermitted)
        XCTAssertTrue(member.calls.isEmpty)
    }

    func testNoReplaceWhenTheHeadPollFailsForAReasonOtherThan401() async {
        let w = World()
        let root = RepairSigner()
        w.authority = .root(root)
        w.pollError = DsClientError.http(status: 503, endpoint: "/ds", body: "")
        let down = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(down, .pollFailed)
        w.pollError = DsClientError.network(endpoint: "/ds", underlying: URLError(.notConnectedToInternet))
        let offline = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(offline, .pollFailed)
        w.pollError = DsClientError.clockSkew(endpoint: "/ds")
        let skewed = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(skewed, .pollFailed)
        XCTAssertTrue(root.calls.isEmpty, "no Replace on a head this device could not read")
    }

    func testA401HeadPollIsTheStateBeingRepaired() async {
        let w = World()
        let root = RepairSigner()
        w.authority = .root(root)
        w.pollError = ChatError.wrapped(
            "DS fetch failed", underlying: DsClientError.http(status: 401, endpoint: "/ds", body: ""))
        let outcome = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(outcome, .replaced)
        XCTAssertEqual(root.calls, ["replace:7"])
    }

    func testA403MeansOutrankedAndStops() async {
        let w = World()
        let admin = RepairSigner()
        w.authority = .registrar(admin, .admin)
        w.ds.answers = [.rankShortfall]
        let outcome = await w.repair().check(groupIdHex: "ab", epoch: 7)
        XCTAssertEqual(outcome, .outranked)
        XCTAssertEqual(w.ds.sent.count, 1)
    }

    /// The scheduled work runs the check and frees the group for a new note.
    func testTheScheduledWorkRunsTheCheck() async {
        let w = World()
        let root = RepairSigner()
        w.authority = .root(root)
        let repair = w.repair()
        repair.note(groupIdHex: "ab", epoch: 7, reason: .refusedAtHead)
        XCTAssertTrue(repair.isPending(groupIdHex: "ab"))
        w.scheduled.first?.1()
        for _ in 0..<50 where repair.isPending(groupIdHex: "ab") { await Task.yield() }
        XCTAssertFalse(repair.isPending(groupIdHex: "ab"))
        XCTAssertEqual(root.calls, ["replace:7"])
    }

    /// The DS client tells the auth's hook about a refusal that survived every
    /// retry (and not about one blamed on the clock).
    func testTheClientReportsARefusalAtTheHead() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 401, body: Data())])
        var told = 0
        do {
            _ = try await DsAccessURLProtocol.client(scope).fetchGroup(
                groupId: Data([0xAB]), after: 0, waitSecs: 0,
                auth: DsAuth(signer: RefusedSigner(), onRefusedAtHead: { told += 1 }))
            XCTFail("a 401 throws")
        } catch {}
        XCTAssertEqual(told, 1)
    }
}

private final class RefusedSigner: DsRequestSigner {
    func epoch() -> UInt64 { 7 }
    func sign(method: String, path: String, query: String, body: Data) -> String { "v1.proof" }
}
