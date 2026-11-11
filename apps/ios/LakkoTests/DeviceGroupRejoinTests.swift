// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0081 decisions 3 and 4 on iOS — a device that lost or fell behind its
/// DeviceGroup rejoins through a sibling; with no sibling answering within
/// the answer window it founds (third amendment); every DeviceGroup snapshot write fails closed and a stale
/// handle's write is refused. Mirrors
/// `apps/web/src/devices/device-group-rejoin.test.ts`,
/// `device-group-snapshot-store.test.ts` and `lifecycle-founding.test.ts`.
@MainActor
final class DeviceGroupRejoinTests: XCTestCase {

    private let sibling = Data(repeating: 0xAA, count: 32)
    private let lost = Data(repeating: 0xBB, count: 32)
    private let other = Data(repeating: 0xCC, count: 32)

    // MARK: Behind

    func testBehindIsADrainedReadStillBehindTheDsWithNoOwnCommitInFlight() {
        XCTAssertTrue(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: 7, commitPending: false))
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: 5, commitPending: false))
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: false, latestEpoch: 7, commitPending: false),
                       "rows still to read may bring it forward")
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: nil, commitPending: false),
                       "an unclaimed journal names no epoch")
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: 6, commitPending: true),
                       "its own commit in flight")
    }

    // MARK: A sibling's admission rule

    private func input(_ change: (inout DeviceGroupRejoin.AdmissionInput) -> Void = { _ in }) -> DeviceGroupRejoin.AdmissionInput {
        var i = DeviceGroupRejoin.AdmissionInput(
            knockId: 10, knockAtMs: 1_000_000, key: lost, ownKey: sibling,
            roster: [.init(leaf: 0, key: sibling), .init(leaf: 1, key: lost), .init(leaf: 2, key: other)],
            handled: [], watchingSinceMs: 500_000, lastAddedAtMs: nil, listed: true,
            policyRequiresAttestation: false, nowMs: 1_000_100)
        change(&i)
        return i
    }

    func testALostDeviceWhosePreviousLeafIsStillInTheRosterIsAdmitted() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input()), .admit)
    }

    func testADeviceWhoseLeafIsGoneTooIsAdmitted() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.roster = [.init(leaf: 0, key: self.sibling), .init(leaf: 2, key: self.other)]
        }), .admit)
    }

    func testARemovedDeviceIsNeverAdmitted() {
        // Deleted from the directory and tombstoned there (ADR-0074): not listed.
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.listed = false }), .notListed)
    }

    func testAKnockIsNotAnsweredTwice() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.lastAddedAtMs = 1_000_050 }), .alreadyAdmitted)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.handled = [10] }), .handled)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.lastAddedAtMs = 900_000 }), .admit,
                       "a knock after the last addition (the device lost its state again) is new")
    }

    func testAKnockFromBeforeWatchingIsIgnored() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.watchingSinceMs = 1_000_001 }), .beforeWatching)
    }

    func testEverySiblingThatPassesTheChecksAdmitsAtOnce() {
        // Third amendment: no designated admitter, no fallback wait. The DS
        // commit route lets one commit take the epoch.
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.ownKey = self.other }), .admit)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.nowMs = $0.knockAtMs }), .admit)
    }

    func testTheKnockingKeyIsNeverItsOwnAdmitter() {
        let roster: [DeviceGroupRejoin.RosterMember] = [.init(leaf: 0, key: lost), .init(leaf: 1, key: sibling)]
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.roster = roster }), .admit)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.roster = roster; $0.ownKey = self.lost }), .ownKey)
    }

    func testUnderThePolicyASelfRemovalProofThatCoversTheReplacementAdmits() {
        // ADR-0081 amendment.
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.policyRequiresAttestation = true
            $0.selfRemovalProofCovers = true
        }), .admit)
    }

    func testAKnockWhoseKeyPackageIsAlreadyALeafWasAnswered() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.keyPackageAlreadyIn = true }), .alreadyAdmitted)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.keyPackageAlreadyIn = true
            $0.selfRemovalProofCovers = true
        }), .alreadyAdmitted)
    }

    func testUnderTheAttestedRemovalPolicyAPresentPreviousLeafWithoutACoveringProofWaitsForThePerson() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.policyRequiresAttestation = true }), .needsAttestation)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.policyRequiresAttestation = true
            $0.roster = [.init(leaf: 0, key: self.sibling)]
        }), .admit)
    }

    func testTheRecordKeepsTheLatestAdditionAndABoundedHandledSet() throws {
        var r = DeviceGroupAdmissionRecord(watchingSinceMs: 5)
        r = r.noteAdded(keyHex: "bb", atMs: 100).noteAdded(keyHex: "bb", atMs: 50)
        XCTAssertEqual(r.lastAddedAtMs["bb"], 100)
        for i in 0..<300 { r = r.noteHandled(Int64(i)) }
        XCTAssertEqual(r.handled.count, 256)
        XCTAssertEqual(r.handled.first, 44)
        XCTAssertEqual(DeviceGroupAdmissionRecord.decode(try r.encoded()), r)
        XCTAssertNil(DeviceGroupAdmissionRecord.decode(Data(#"{"v":2}"#.utf8)))
    }

    // MARK: Third amendment — answer window, presence, silence

    func testTheAnswerWindowIsFourRoundTripsAndAtLeastOneSecond() {
        XCTAssertEqual(DeviceGroupRejoin.answerWindowMs(rttMs: 0), 1_000)
        XCTAssertEqual(DeviceGroupRejoin.answerWindowMs(rttMs: 249), 1_000)
        XCTAssertEqual(DeviceGroupRejoin.answerWindowMs(rttMs: 250), 1_000)
        XCTAssertEqual(DeviceGroupRejoin.answerWindowMs(rttMs: 300), 1_200)
        XCTAssertEqual(DeviceGroupRejoin.answerWindowMs(rttMs: -5), 1_000, "a clock that went back")
        XCTAssertEqual(DeviceGroupRejoin.answerWindowMs(rttMs: .max), .max)
    }

    func testASiblingAnswersAFreshKnockAndNotAnOldOrOwnOrHandledOne() {
        func answer(at: Int64 = 1_000_000, watching: Int64 = 500_000, listening: Int64 = 990_000,
                    handled: Set<Int64> = [], own: Bool = false, now: Int64 = 1_000_100) -> Bool {
            DeviceGroupRejoin.shouldAnswer(
                knockAtMs: at, watchingSinceMs: watching, listeningSinceMs: listening, handled: handled,
                knockId: 10, isOwnKey: own, nowMs: now)
        }
        XCTAssertTrue(answer())
        XCTAssertFalse(answer(own: true))
        XCTAssertFalse(answer(handled: [10]))
        XCTAssertFalse(answer(watching: 1_000_001), "before watching: it would not admit it")
        XCTAssertFalse(answer(listening: 1_000_000 + DeviceGroupRejoin.waitingKnockMaxAgeMs + 1, now: 1_040_000),
                       "waiting more than 30 s when this listen began")
        XCTAssertTrue(answer(listening: 1_000_000 + DeviceGroupRejoin.waitingKnockMaxAgeMs, now: 1_040_000))
        XCTAssertFalse(answer(listening: 0, now: 1_000_000 + DeviceGroupRejoin.reknockAfterMs),
                       "the knocker knocked again")
    }

    func testARowIsAPresenceNoteOnlyWhenThePredicateSaysSo() {
        XCTAssertEqual(DeviceGroupRejoin.classify(row: Data([1]), isPresence: { _ in true }), .presence)
        XCTAssertEqual(DeviceGroupRejoin.classify(row: Data([2]), isPresence: { _ in false }), .welcome(Data([2])))
    }

    func testTheCoreNoteIsAPresenceNoteAndAWelcomeIsNot() {
        let note = composeRejoinPresent()
        XCTAssertEqual(note.count, 25)
        XCTAssertTrue(isRejoinPresent(row: note))
        XCTAssertFalse(isRejoinPresent(row: note + Data([0])))
        XCTAssertFalse(isRejoinPresent(row: Data([0x00, 0x01, 0x00, 0x03])))
    }

    func testLostIsThreeRefusedReadsOrAnOlderChainNeverAPendingFounding() {
        func lost(_ refused: Int, founding: Bool = false, local: UInt64? = nil, drained: Bool = false,
                  latest: UInt64? = nil, pending: Bool = false) -> Bool {
            DeviceGroupRejoin.isLost(
                refusedReads: refused, foundingClaimPending: founding, localEpoch: local,
                drained: drained, latestEpoch: latest, commitPending: pending)
        }
        XCTAssertFalse(lost(1), "one refusal may race a registration")
        XCTAssertFalse(lost(2))
        XCTAssertTrue(lost(3))
        XCTAssertFalse(lost(5, founding: true), "its own founding whose claim did not land is claimed again")
        XCTAssertTrue(lost(0, local: 7, drained: true, latest: 2))
        XCTAssertFalse(lost(0, local: 7, drained: true, latest: 7))
        XCTAssertFalse(lost(0, local: 7, drained: false, latest: 2))
        XCTAssertFalse(lost(0, local: 7, drained: true, latest: 2, pending: true))
        XCTAssertFalse(lost(0, local: 7, drained: true, latest: nil))
    }

    func testAnErrorLabelCarriesNoIdentifier() {
        let hex = String(repeating: "ab", count: 32)
        let label = DeviceGroupRejoin.errorLabel(
            DsClientError.http(status: 401, endpoint: "/ds/group/\(hex)/knocks", body: hex))
        XCTAssertEqual(label, "DsClientError.http(401)")
        XCTAssertFalse(DeviceGroupRejoin.errorLabel(
            DsClientError.argument("rendezvous \(hex)")).contains(hex))
        XCTAssertEqual(DeviceGroupRejoin.errorLabel(DeviceGroupSyncError.notReady), "DeviceGroupSyncError.notReady")
        XCTAssertFalse(DeviceGroupRejoin.errorLabel(DsStatementError.malformed(hex)).contains(hex))
    }

    // MARK: The rejoining device's run

    @MainActor
    private final class FakeWatch: DeviceGroupRendezvousWatch {
        /// Each row with the clock time it arrives at.
        var rows: [(atMs: Int64, row: DeviceGroupRejoin.RendezvousRow)]
        unowned let model: RunModel
        var deadlines: [Int64] = []
        var closed = false

        init(model: RunModel, rows: [(atMs: Int64, row: DeviceGroupRejoin.RendezvousRow)]) {
            self.model = model
            self.rows = rows
        }

        func next(untilMs: Int64) async -> DeviceGroupRejoin.RendezvousRow? {
            deadlines.append(untilMs)
            if let first = rows.first, first.atMs <= untilMs {
                rows.removeFirst()
                model.now = max(model.now, first.atMs)
                return first.row
            }
            model.now = max(model.now, untilMs)
            return nil
        }

        func close() { closed = true }
    }

    @MainActor
    private final class RunModel {
        var now: Int64 = 1_000
        var log: [String] = []
        var lines: [String] = []
        var state: DeviceGroupRejoinState?
        var rttMs: Int64 = 100
        var canKnock = true
        /// Rows relative to the moment the knock post finished.
        var answer: [(afterMs: Int64, row: DeviceGroupRejoin.RendezvousRow)] = []
        var watches: [FakeWatch] = []
        var postEndedAt: Int64 = 0
        var joinFails = false
        /// How many times the run said a device of the account answered.
        var answeredNotes = 0
        struct JoinFailed: Error {}

        var ports: DeviceGroupRejoinPorts {
            DeviceGroupRejoinPorts(
                dropLocalState: { [unowned self] in self.log.append("drop") },
                publishDirectoryKeyPackage: { [unowned self] in self.log.append("directory-kp") },
                canKnock: { [unowned self] in self.canKnock },
                loadState: { [unowned self] in self.state },
                saveState: { [unowned self] in self.state = $0 },
                prepareKnock: { [unowned self] at in
                    self.log.append("prepare")
                    let state = DeviceGroupRejoinState(rendezvousB64: "r\(at)", joinerSnapshotB64: "s", knockedAtMs: at)
                    self.state = state
                    return DeviceGroupPreparedKnock(state: state, payload: Data([9]))
                },
                openWatch: { [unowned self] _ in
                    self.log.append("watch")
                    let w = FakeWatch(model: self, rows: [])
                    self.watches.append(w)
                    return w
                },
                postKnock: { [unowned self] _ in
                    self.log.append("knock")
                    self.now += self.rttMs
                    self.postEndedAt = self.now
                    self.watches.last?.rows = self.answer.map { (self.now + $0.afterMs, $0.row) }
                },
                join: { [unowned self] _, _ in
                    self.log.append("join")
                    if self.joinFails { throw JoinFailed() }
                },
                clearState: { [unowned self] in self.log.append("clear"); self.state = nil },
                now: { [unowned self] in self.now },
                noteAnswered: { [unowned self] in self.answeredNotes += 1 },
                log: { [unowned self] in self.lines.append($0) }
            )
        }
    }

    func testTheWatchIsOpenBeforeTheKnockAndTheWindowFollowsTheRoundTrip() async throws {
        let m = RunModel()
        m.rttMs = 400
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(outcome, .found)
        XCTAssertEqual(m.log, ["drop", "directory-kp", "prepare", "watch", "knock"])
        XCTAssertEqual(m.watches.first?.deadlines, [m.postEndedAt + 1_600])
        XCTAssertEqual(m.watches.first?.closed, true)
        XCTAssertTrue(m.lines.contains { $0.contains("no answer within 1600 ms") })
    }

    func testSilenceWithinTheWindowFounds() async throws {
        let m = RunModel()
        m.rttMs = 50
        m.answer = [(1_200, .presence)]  // after the 1 s window
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(outcome, .found)
        XCTAssertFalse(m.log.contains("join"))
    }

    func testAPresenceNoteWaitsForTheWelcomeWithNoAnswerDeadline() async throws {
        let m = RunModel()
        // Presence at once, the Welcome ten minutes later (a sibling's commit
        // took long): still joined, no founding.
        m.answer = [(30, .presence), (30, .presence), (600_000, .welcome(Data([7])))]
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(outcome, .joined)
        XCTAssertEqual(m.log.suffix(2), ["join", "clear"])
        let knockedAt = try XCTUnwrap(m.watches.first?.deadlines.dropFirst().first)
        XCTAssertEqual(knockedAt, 1_000 + DeviceGroupRejoin.reknockAfterMs,
                       "after the presence note it waits until the re-knock time")
        XCTAssertTrue(m.lines.contains { $0.contains("answered after 130 ms") })
    }

    func testAPresenceNoteWithoutAWelcomeKeepsWaitingAndThenKnocksAgain() async throws {
        let m = RunModel()
        m.answer = [(20, .presence)]
        let first = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(first, .waiting)
        XCTAssertEqual(m.state?.answered, true)
        // The re-knock time has passed: a second pass knocks again with a
        // fresh window; nobody answers now (the device went away) -> found.
        m.answer = []
        let second = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: false)
        XCTAssertEqual(second, .found)
        XCTAssertEqual(m.log.filter { $0 == "knock" }.count, 2)
    }

    func testAWelcomeInsideTheWindowJoins() async throws {
        let m = RunModel()
        m.answer = [(10, .welcome(Data([7])))]
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(outcome, .joined)
        XCTAssertEqual(m.log, ["drop", "directory-kp", "prepare", "watch", "knock", "join", "clear"])
    }

    func testARestartResumesAnAnsweredKnock() async throws {
        let m = RunModel()
        m.state = DeviceGroupRejoinState(rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000, answered: true)
        let outcome: DeviceGroupRejoin.PassOutcome
        // The kept watch's Welcome arrives at once.
        let ports = m.ports
        var wrapped = ports
        wrapped.openWatch = { [unowned m] state in
            let w = ports.openWatch(state) as! FakeWatch
            w.rows = [(m.now + 5, .welcome(Data([1])))]
            return w
        }
        outcome = try await DeviceGroupRejoin.rejoinOnce(ports: wrapped, firstPass: false)
        XCTAssertEqual(outcome, .joined)
        XCTAssertEqual(m.log, ["watch", "join", "clear"])
    }

    func testAKnockNoPresenceNoteAnsweredIsMadeAgainAfterARestart() async throws {
        let m = RunModel()
        m.state = DeviceGroupRejoinState(rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000)
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: false)
        XCTAssertEqual(outcome, .found)
        XCTAssertEqual(m.log, ["prepare", "watch", "knock"])
    }

    func testARowThatDoesNotJoinMakesTheNextPassKnockAgain() async throws {
        let m = RunModel()
        m.answer = [(10, .presence), (20, .welcome(Data([0xFF])))]
        m.joinFails = true
        do {
            _ = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
            XCTFail("the join failure surfaces")
        } catch {}
        XCTAssertEqual(m.state?.answered, false)
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(m.state, nowMs: m.now))
    }

    // MARK: Section 7.2 — the answer the chat list reads

    func testAPresenceNoteIsNotedAsAnsweredBeforeTheWelcomeWait() async throws {
        let m = RunModel()
        m.answer = [(20, .presence)]
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(outcome, .waiting, "no Welcome yet")
        XCTAssertEqual(m.answeredNotes, 1, "the answer is kept without waiting for the Welcome")
    }

    func testAWelcomeInTheWindowIsNotedAsAnswered() async throws {
        let m = RunModel()
        m.answer = [(10, .welcome(Data([7])))]
        _ = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(m.answeredNotes, 1)
    }

    func testSilenceAndNoLoginHandleAreNotNotedAsAnswered() async throws {
        let silent = RunModel()
        let silentOutcome = try await DeviceGroupRejoin.rejoinOnce(ports: silent.ports, firstPass: true)
        XCTAssertEqual(silentOutcome, .found)
        XCTAssertEqual(silent.answeredNotes, 0)
        let late = RunModel()
        late.answer = [(5_000, .presence)]  // after the window
        let lateOutcome = try await DeviceGroupRejoin.rejoinOnce(ports: late.ports, firstPass: true)
        XCTAssertEqual(lateOutcome, .found)
        XCTAssertEqual(late.answeredNotes, 0)
        let noHandle = RunModel()
        noHandle.canKnock = false
        let noHandleOutcome = try await DeviceGroupRejoin.rejoinOnce(ports: noHandle.ports, firstPass: true)
        XCTAssertEqual(noHandleOutcome, .found)
        XCTAssertEqual(noHandle.answeredNotes, 0)
    }

    func testARestartWithAnAnsweredKnockNotesItAgainAndAnUnansweredOneDoesNot() async throws {
        let answered = RunModel()
        answered.state = DeviceGroupRejoinState(
            rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000, answered: true)
        _ = try await DeviceGroupRejoin.rejoinOnce(ports: answered.ports, firstPass: false)
        XCTAssertGreaterThanOrEqual(answered.answeredNotes, 1)
        let unanswered = RunModel()
        unanswered.state = DeviceGroupRejoinState(rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000)
        _ = try await DeviceGroupRejoin.rejoinOnce(ports: unanswered.ports, firstPass: false)
        XCTAssertEqual(unanswered.answeredNotes, 0)
    }

    func testNoLoginHandleFoundsWithoutKnocking() async throws {
        let m = RunModel()
        m.canKnock = false
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(outcome, .found)
        XCTAssertEqual(m.log, ["drop", "directory-kp"])
    }

    func testTheReknockRuleAndTheStateCodec() throws {
        let s = DeviceGroupRejoinState(rendezvousB64: "r", joinerSnapshotB64: "s", knockedAtMs: 10, answered: true)
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(nil, nowMs: 0))
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(
            DeviceGroupRejoinState(rendezvousB64: "r", joinerSnapshotB64: "s", knockedAtMs: 10), nowMs: 11),
            "no presence note answered it")
        XCTAssertFalse(DeviceGroupRejoinState.shouldKnockAgain(s, nowMs: 10 + DeviceGroupRejoin.reknockAfterMs - 1))
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(s, nowMs: 10 + DeviceGroupRejoin.reknockAfterMs))
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(s, nowMs: 5), "a clock that went back")
        XCTAssertEqual(DeviceGroupRejoinState.decode(try s.encoded()), s)
        XCTAssertNil(DeviceGroupRejoinState.decode(Data("[]".utf8)))
    }

    // MARK: Decision 3 — the store: generation, pending rows, Forget device

    private func store() -> DeviceGroupSnapshotStore {
        DeviceGroupSnapshotStore(directoryName: "lakko-dg-test-\(UUID().uuidString)")
    }

    func testAConditionalWriteLandsOnlyOnTheGenerationItRead() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        let g1 = try s.putSnapshotDurable(accountIdB64: account, blob: Data("epoch N".utf8))
        XCTAssertEqual(s.generation(accountIdB64: account), g1)
        // Another handle made the commit's N+1 current.
        let g2 = try s.putSnapshotDurable(accountIdB64: account, blob: Data("epoch N+1".utf8))
        XCTAssertEqual(g2, g1 + 1)
        // The long-lived handle, still at N, tries to persist a batch.
        XCTAssertThrowsError(try s.putSnapshotDurable(
            accountIdB64: account, blob: Data("epoch N again".utf8), expectedGeneration: g1)) {
            XCTAssertTrue($0 is DeviceGroupSnapshotStore.SnapshotChanged)
        }
        XCTAssertEqual(s.getSnapshot(accountIdB64: account), Data("epoch N+1".utf8))
        // On the current generation it lands.
        XCTAssertNoThrow(try s.putSnapshotDurable(
            accountIdB64: account, blob: Data("epoch N+1, batch".utf8), expectedGeneration: g2))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testAPendingRowNeverReplacesTheSnapshot() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("epoch N".utf8))
        try s.putPending(accountIdB64: account, kind: .commit, blob: Data("entry".utf8))
        XCTAssertEqual(s.getSnapshot(accountIdB64: account), Data("epoch N".utf8))
        XCTAssertEqual(s.getPending(accountIdB64: account, kind: .commit), Data("entry".utf8))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testARejoinDropsTheSnapshotTheCommitAndTheRecordAndKeepsTheKnock() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("behind".utf8))
        try s.putPending(accountIdB64: account, kind: .commit, blob: Data("c".utf8))
        try s.putPending(accountIdB64: account, kind: .admissions, blob: Data("a".utf8))
        try s.putPending(accountIdB64: account, kind: .rejoin, blob: Data("r".utf8))
        s.dropStateForRejoin(accountIdB64: account)
        XCTAssertNil(s.getSnapshot(accountIdB64: account))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .commit))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .admissions))
        XCTAssertNotNil(s.getPending(accountIdB64: account, kind: .rejoin))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testForgetDeviceRemovesTheOutboxWithTheSnapshot() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("s".utf8))
        try s.putPending(accountIdB64: account, kind: .commit, blob: Data("c".utf8))
        try s.putPending(accountIdB64: account, kind: .rejoin, blob: Data("r".utf8))
        s.deleteSnapshot(accountIdB64: account)
        XCTAssertNil(s.getSnapshot(accountIdB64: account))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .commit))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .rejoin))
    }

    // MARK: Decision 4 — founding is a single-device account's (the real core)

    private let deviceSeed = Data(repeating: 0x41, count: 32)
    private let accountSeed = Data(repeating: 0x42, count: 64)

    func testWithNoStateAndNoFoundingDecisionNothingIsFounded() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        XCTAssertThrowsError(try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, snapshotStore: s)) {
            XCTAssertEqual($0 as? DeviceGroupLifecycle.NotJoined, .init(reason: .noState))
        }
        XCTAssertNil(s.getSnapshot(accountIdB64: account), "nothing written")
    }

    func testASingleDeviceAccountFoundsAndTheSnapshotIsWritten() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, found: true, snapshotStore: s)
        XCTAssertTrue(ensured.bootstrapped)
        XCTAssertNotNil(s.getSnapshot(accountIdB64: account))
        XCTAssertEqual(ensured.generation, s.generation(accountIdB64: account))
        // Loaded again without a founding decision: the same DeviceGroup.
        let again = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, snapshotStore: s)
        XCTAssertFalse(again.bootstrapped)
        XCTAssertEqual(try again.group.groupId(), try ensured.group.groupId())
        s.deleteSnapshot(accountIdB64: account)
    }

    func testAnUnusableSnapshotIsDroppedAndTheRejoinRowStays() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("not a provider".utf8))
        try s.putPending(accountIdB64: account, kind: .rejoin, blob: Data("r".utf8))
        XCTAssertThrowsError(try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, snapshotStore: s))
        XCTAssertNil(s.getSnapshot(accountIdB64: account))
        XCTAssertNotNil(s.getPending(accountIdB64: account, kind: .rejoin))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testJoiningFromAWelcomeClearsALeftoverFoundingMark() throws {
        // A founding mark left on this journal would make claim-on-open reset
        // the chain this device just joined.
        let suite = "lakko-marks-test-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let marks = JournalFoundingMarks(defaults: defaults)
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, found: true, snapshotStore: s)
        let hex = DsClient.pseudonymHex(try ensured.group.groupId())
        marks.mark(journalIdHex: hex)
        marks.mark(journalIdHex: "other")
        DeviceGroupLifecycle.clearFoundingMark(joined: ensured.group, marks: marks)
        XCTAssertFalse(marks.isMarked(journalIdHex: hex))
        XCTAssertTrue(marks.isMarked(journalIdHex: "other"))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testAStaleHandlesPersistIsRefused() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, found: true, snapshotStore: s)
        try s.putSnapshotDurable(accountIdB64: account, blob: try ensured.identity.snapshotStorage())
        XCTAssertThrowsError(try DeviceGroupLifecycle.persistSnapshotOrThrow(
            identity: ensured.identity, accountIdB64: account,
            expectedGeneration: ensured.generation, snapshotStore: s)) {
            XCTAssertTrue($0 is DeviceGroupSnapshotStore.SnapshotChanged)
        }
        s.deleteSnapshot(accountIdB64: account)
    }
}

/// A publish stopped by a handle that fell behind the disk is sealed once
/// more on the reloaded handle, so the message is not lost to that race.
@MainActor
final class StaleHandlePublishTests: XCTestCase {

    private struct Other: Error {}

    func testAStaleHandleIsRetriedOnce() async throws {
        var calls = 0
        let id: Int64 = try await DeviceGroupSyncRepository.retryingOnceOnStaleHandle {
            calls += 1
            if calls == 1 { throw DeviceGroupSnapshotStore.SnapshotChanged() }
            return 7
        }
        XCTAssertEqual(id, 7)
        XCTAssertEqual(calls, 2)
    }

    func testASecondStaleHandleIsTheCallers() async {
        var calls = 0
        do {
            let _: Int64 = try await DeviceGroupSyncRepository.retryingOnceOnStaleHandle {
                calls += 1
                throw DeviceGroupSnapshotStore.SnapshotChanged()
            }
            XCTFail("must throw")
        } catch {
            XCTAssertTrue(error is DeviceGroupSnapshotStore.SnapshotChanged)
        }
        XCTAssertEqual(calls, 2)
    }

    func testAnyOtherErrorIsNotRetried() async {
        var calls = 0
        do {
            let _: Int64 = try await DeviceGroupSyncRepository.retryingOnceOnStaleHandle {
                calls += 1
                throw Other()
            }
            XCTFail("must throw")
        } catch {
            XCTAssertTrue(error is Other)
        }
        XCTAssertEqual(calls, 1)
    }
}
