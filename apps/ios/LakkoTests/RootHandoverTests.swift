// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Fakes

/// A statement signer that records the statements it was asked for.
private final class RecordingSigner: DsStatementSignerProtocol, @unchecked Sendable {
    let key: Data
    var calls: [String] = []
    var lastChanges: [DsRegistrarChange] = []
    var lastEntries: [DsRegistrarEntry] = []

    init(key: UInt8) { self.key = Data(repeating: key, count: 32) }

    private func req(_ segment: String) -> DsStatementRequest {
        DsStatementRequest(path: "/ds/group/ab/\(segment)", bodyJson: "{}")
    }

    func claim(journalId: Data, epoch: UInt64, epochKey: Data) throws -> DsStatementRequest {
        calls.append("claim"); return req("claim")
    }
    func publicKey() -> Data { key }
    func pushBinding(journalId: Data, pushGid: Data) throws -> DsStatementRequest {
        calls.append("push-binding"); return req("push-binding")
    }
    func commit(journalId: Data, epoch: UInt64, epochKey: Data, revoke: [Data], row: Data) throws
        -> DsStatementRequest {
        calls.append("commit"); return req("commit")
    }
    func registrars(journalId: Data, changes: [DsRegistrarChange]) throws -> DsStatementRequest {
        calls.append("registrars")
        lastChanges.append(contentsOf: changes)
        return req("registrars")
    }
    func reset(journalId: Data, epoch: UInt64, epochKey: Data, registrars: [DsRegistrarEntry]?) throws
        -> DsStatementRequest {
        calls.append("reset"); return req("reset")
    }
    func registrarsSet(journalId: Data, entries: [DsRegistrarEntry]) throws -> DsStatementRequest {
        calls.append("registrars-set")
        lastEntries = entries
        return req("registrars-set")
    }
    func replaceEpoch(journalId: Data, epoch: UInt64, epochKey: Data, revoke: [Data]) throws
        -> DsStatementRequest {
        calls.append("replace"); return req("access")
    }
    func rootTransfer(journalId: Data, newRoot: Data) throws -> DsStatementRequest {
        calls.append("root"); return req("root")
    }
}

/// A DS that records the statements it saw and answers scripted refusals.
private final class ScriptedDs: DsStatementSending {
    var sent: [DsStatementRequest] = []
    var answers: [Error?] = []

    func putStatement(_ request: DsStatementRequest) async throws {
        sent.append(request)
        if !answers.isEmpty, let error = answers.removeFirst() { throw error }
    }

    var segments: [String] { sent.map { String($0.path.split(separator: "/").last ?? "") } }
}

private struct Boom: Error {}

// MARK: - Tests

/// ADR-0057 "What the S2 client work found", item 1 — the journal root moves
/// with an ownership transfer: the outgoing owner hands its root seed over,
/// the new owner takes the journal over with it.
@MainActor
final class RootHandoverTests: XCTestCase {

    private let gid = Data(repeating: 0xAB, count: 32)
    private let me = Data(repeating: 1, count: 32)
    private let other = Data(repeating: 2, count: 32)
    private let third = Data(repeating: 3, count: 32)

    // MARK: Sending fires only on an owner change away from this account

    func testTheSendingRuleFiresOnlyWhenOwnershipMovesAwayFromThisAccount() {
        XCTAssertEqual(RootHandover.ownerMovedAwayFrom(own: me, before: me, after: other), other)
        XCTAssertNil(RootHandover.ownerMovedAwayFrom(own: me, before: me, after: me), "a no-op commit")
        XCTAssertNil(RootHandover.ownerMovedAwayFrom(own: me, before: other, after: me), "becoming owner")
        XCTAssertNil(RootHandover.ownerMovedAwayFrom(own: me, before: other, after: third),
                     "someone else's transfer")
        XCTAssertNil(RootHandover.ownerMovedAwayFrom(own: me, before: other, after: other))
        XCTAssertNil(RootHandover.ownerMovedAwayFrom(own: me, before: me, after: nil), "unreadable table")
        XCTAssertNil(RootHandover.ownerMovedAwayFrom(own: me, before: nil, after: other))
        XCTAssertNil(RootHandover.ownerMovedAwayFrom(own: nil, before: me, after: other), "signed out")
    }

    /// The envelope carries the OLD root seed sealed to the NEW owner's
    /// account X25519 key: the real core codec, end to end.
    func testTheEnvelopeCarriesTheOldRootSeedSealedToTheNewOwner() async throws {
        let outgoing = try Identity.fromSeed(seed: Data(repeating: 0x71, count: 32))
        let incoming = try Identity.fromSeed(seed: Data(repeating: 0x72, count: 32))
        let stranger = try Identity.fromSeed(seed: Data(repeating: 0x73, count: 32))
        let rootSeed = try dsJournalRootSeed(accountIdentitySeed: outgoing.ed25519Seed(), journalId: gid)
        var sent: Data?
        let outcome = await RootHandover.send(
            ownAccountKey: { outgoing.signaturePublicKey() },
            liveOwner: { incoming.signaturePublicKey() },
            ownerX25519: { key in
                XCTAssertEqual(key, incoming.signaturePublicKey())
                return incoming.x25519PublicKey()
            },
            rootSeed: { rootSeed },
            compose: { newOwner, x25519, seed in
                try composeRootHandoverEnvelope(
                    journalId: self.gid, newOwnerAccountKey: newOwner,
                    newOwnerX25519PublicKey: x25519, rootSeed: seed)
            },
            sendEnvelope: { sent = $0 })
        XCTAssertEqual(outcome, .sent)
        let envelope = try XCTUnwrap(sent)
        XCTAssertTrue(ChatClient.looksLikeRegistrarSeed(envelope), "hidden like any registrar seed")
        let decoded = try decodeRegistrarSeedEnvelope(plaintext: envelope)
        XCTAssertEqual(decoded.rank, .root)
        XCTAssertEqual(decoded.journalId, gid)
        XCTAssertEqual(decoded.memberAccountKey, incoming.signaturePublicKey())
        XCTAssertEqual(try openRegistrarSeedEnvelope(plaintext: envelope, accountIdentity: incoming), rootSeed)
        XCTAssertThrowsError(try openRegistrarSeedEnvelope(plaintext: envelope, accountIdentity: stranger))
        // The seed the new owner opens signs as the old root.
        XCTAssertEqual(
            try dsStatementSigner(seed: rootSeed).publicKey(),
            try dsJournalRootSigner(accountIdentity: outgoing, journalId: gid).publicKey())
    }

    func testNothingIsSentWhileThisAccountIsOwnerOrTheTableIsUnreadable() async {
        var composed = 0
        func attempt(owner: Data?) async -> RootHandover.SendOutcome {
            await RootHandover.send(
                ownAccountKey: { self.me }, liveOwner: { owner },
                ownerX25519: { _ in Data(repeating: 9, count: 32) },
                rootSeed: { Data(repeating: 4, count: 32) },
                compose: { _, _, _ in composed += 1; return Data([1]) },
                sendEnvelope: { _ in XCTFail("never sent") })
        }
        let own = await attempt(owner: me)
        let none = await attempt(owner: nil)
        XCTAssertEqual(own, .cancelled)
        XCTAssertEqual(none, .cancelled)
        XCTAssertEqual(composed, 0)
    }

    func testAnUnresolvedKeyIsPendingAndAFailedSendIsRetried() async {
        let pending = await RootHandover.send(
            ownAccountKey: { self.me }, liveOwner: { self.other }, ownerX25519: { _ in nil },
            rootSeed: { XCTFail("no seed derived before the key resolves"); return Data() },
            compose: { _, _, _ in Data([1]) }, sendEnvelope: { _ in })
        XCTAssertEqual(pending, .pending)
        XCTAssertFalse(pending.settlesObligation)

        let failed = await RootHandover.send(
            ownAccountKey: { self.me }, liveOwner: { self.other },
            ownerX25519: { _ in Data(repeating: 9, count: 32) },
            rootSeed: { Data(repeating: 4, count: 32) },
            compose: { _, _, _ in Data([1]) }, sendEnvelope: { _ in throw Boom() })
        XCTAssertEqual(failed, .failed)
        XCTAssertFalse(failed.settlesObligation)
        XCTAssertTrue(RootHandover.SendOutcome.sent.settlesObligation)
        XCTAssertTrue(RootHandover.SendOutcome.cancelled.settlesObligation)
    }

    /// The recipient is the owner the LIVE table names: a table that moved
    /// while the key was fetched sends nothing now (retried with the new one).
    func testTheLiveOwnerIsReReadAfterTheKeyLookup() async {
        var reads = 0
        let outcome = await RootHandover.send(
            ownAccountKey: { self.me },
            liveOwner: { reads += 1; return reads == 1 ? self.other : self.third },
            ownerX25519: { _ in Data(repeating: 9, count: 32) },
            rootSeed: { Data(repeating: 4, count: 32) },
            compose: { _, _, _ in XCTFail("not composed for a stale owner"); return Data() },
            sendEnvelope: { _ in })
        XCTAssertEqual(outcome, .pending)
    }

    // MARK: Receive gate

    func testTheRootHandoverReceiveGate() {
        func verdict(addressee: Data? = nil, journal: Data? = nil, owner: Data?, own: Data? = nil)
            -> RootHandover.Verdict {
            RootHandover.verdict(
                ownAccountKey: own ?? me, envelopeJournal: journal ?? gid,
                envelopeAddressee: addressee ?? me, groupId: gid, owner: owner)
        }
        XCTAssertEqual(verdict(owner: me), .accept)
        XCTAssertEqual(verdict(addressee: other, owner: me), .notAddressedToUs)
        XCTAssertEqual(verdict(owner: other), .notOwner)
        XCTAssertEqual(verdict(owner: nil), .notOwner, "no role table, no owner")
        XCTAssertEqual(verdict(journal: Data(repeating: 0xCD, count: 32), owner: me), .wrongJournal)
        XCTAssertEqual(
            RootHandover.verdict(
                ownAccountKey: nil, envelopeJournal: gid, envelopeAddressee: me, groupId: gid, owner: me),
            .unresolved)
    }

    /// A rank-root envelope never enters the registrar-seed store, whoever
    /// sent it; it starts the takeover only when addressed to this account
    /// and the live table names this account owner.
    func testARootEnvelopeStartsTheTakeoverAndIsNeverStored() {
        var stored = 0
        var takeOvers: [Data] = []
        func receive(rank: DsSeedEnvelopeRank, addressee: Data, owner: Data?, sender: Data?)
            -> RootHandover.SeedEnvelopeOutcome {
            RootHandover.receiveSeedEnvelope(
                rank: rank, envelopeJournal: gid, envelopeAddressee: addressee, groupId: gid,
                ownAccountKey: me, senderAccountKey: sender, owner: owner,
                open: { Data(repeating: 0x5E, count: 32) },
                storeRegistrarSeed: { _, _ in stored += 1 },
                takeOver: { takeOvers.append($0) })
        }
        XCTAssertEqual(receive(rank: .root, addressee: me, owner: me, sender: other), .handoverStarted,
                       "no sender check: the DS is the check")
        XCTAssertEqual(takeOvers, [Data(repeating: 0x5E, count: 32)])
        XCTAssertEqual(stored, 0, "a root seed is never stored")
        XCTAssertEqual(receive(rank: .root, addressee: other, owner: me, sender: other), .dropped)
        XCTAssertEqual(receive(rank: .root, addressee: me, owner: other, sender: other), .dropped,
                       "not owner at receipt")
        XCTAssertEqual(receive(rank: .root, addressee: me, owner: me, sender: me), .handoverStarted)
        XCTAssertEqual(stored, 0)
        XCTAssertEqual(takeOvers.count, 2)

        // A member / admin seed keeps the obligation-4 gate and is stored.
        XCTAssertEqual(receive(rank: .admin, addressee: me, owner: other, sender: other), .registrarStored)
        XCTAssertEqual(receive(rank: .member, addressee: me, owner: other, sender: third), .dropped,
                       "a registrar seed from a non-owner is refused")
        XCTAssertEqual(stored, 1)
        XCTAssertEqual(takeOvers.count, 2, "a registrar seed never starts a takeover")
    }

    func testARootEnvelopeThatDoesNotOpenOrHasTheWrongWidthIsDropped() {
        let unopened = RootHandover.receiveSeedEnvelope(
            rank: .root, envelopeJournal: gid, envelopeAddressee: me, groupId: gid,
            ownAccountKey: me, senderAccountKey: nil, owner: me,
            open: { throw Boom() }, storeRegistrarSeed: { _, _ in XCTFail() },
            takeOver: { _ in XCTFail() })
        XCTAssertEqual(unopened, .dropped)
        let short = RootHandover.receiveSeedEnvelope(
            rank: .root, envelopeJournal: gid, envelopeAddressee: me, groupId: gid,
            ownAccountKey: me, senderAccountKey: nil, owner: me,
            open: { Data(repeating: 1, count: 31) }, storeRegistrarSeed: { _, _ in XCTFail() },
            takeOver: { _ in XCTFail() })
        XCTAssertEqual(short, .dropped)
    }

    // MARK: Takeover

    private struct Takeover {
        let ds = ScriptedDs()
        let newRoot = RecordingSigner(key: 0x77)
        var transfers: [(seed: Data, newRoot: Data)] = []
        var resets = 0
        var reconciles = 0
        var sleeps: [UInt64] = []
        var marked = 0
        var probe: JournalOwnership.RootProbe = .notRoot
    }

    private func runTakeover(
        _ t: inout Takeover,
        stillOwner: @escaping () -> Bool = { true },
        members: [Data]? = nil
    ) async -> RootHandover.TakeOverOutcome {
        let oldSeed = Data(repeating: 0x0D, count: 32)
        var transfers: [(Data, Data)] = []
        var resets = 0
        var reconciles = 0
        var sleeps: [UInt64] = []
        var marked = 0
        let newRoot = t.newRoot
        let probe = t.probe
        let entries = (members ?? [me, other, third]).map {
            DsRegistrarEntry(publicKey: Data($0.reversed()) + Data([0xEE]), rank: .member)
        }
        let outcome = await RootHandover.takeOver(
            journalId: gid,
            oldRootSeed: oldSeed,
            ownRootPublicKey: { newRoot.publicKey() },
            stillOwner: stillOwner,
            oldRootTransfer: { seed, key in
                transfers.append((seed, key))
                return DsStatementRequest(path: "/ds/group/ab/root", bodyJson: "{}")
            },
            probeOwnRoot: { probe },
            markRootHeld: { marked += 1 },
            newRoot: { newRoot },
            desiredRegistrarEntries: { entries },
            ds: t.ds,
            resetAppointments: { resets += 1 },
            reconcile: { reconciles += 1 },
            sleep: { sleeps.append($0) })
        t.transfers = transfers.map { (seed: $0.0, newRoot: $0.1) }
        t.resets = resets
        t.reconciles = reconciles
        t.sleeps = sleeps
        t.marked = marked
        return outcome
    }

    /// 204: `root` names this account's own derived root, the "root held"
    /// marker is set, then ONE `registrars-set` under the NEW root carries the
    /// whole set the roster calls for (every old registrar not listed goes
    /// with it), then every member's seed is re-sent.
    func testA204MovesTheRootResetsTheRegistrarSetAndReAppoints() async {
        var t = Takeover()
        let outcome = await runTakeover(&t)
        XCTAssertEqual(outcome, .tookOver)
        XCTAssertEqual(t.ds.segments, ["root", "registrars-set"])
        XCTAssertEqual(t.transfers.count, 1)
        XCTAssertEqual(t.transfers.first?.newRoot, t.newRoot.publicKey(), "names this account's root")
        XCTAssertEqual(t.transfers.first?.seed, Data(repeating: 0x0D, count: 32), "signed by the old root")
        XCTAssertEqual(t.newRoot.calls, ["registrars-set"], "no per-key removals")
        XCTAssertEqual(t.newRoot.lastEntries.count, 3)
        XCTAssertEqual(t.newRoot.lastEntries.first?.publicKey, Data(me.reversed()) + Data([0xEE]))
        XCTAssertEqual(t.marked, 1, "the new owner signs with its root from here")
        XCTAssertEqual(t.resets, 1)
        XCTAssertEqual(t.reconciles, 1)
    }

    /// 401 and this account's own probe also answers 401: the seed is not the
    /// current root (stale or forged). Dropped, never retried with it; nothing
    /// else is sent, reset or marked.
    func testA401DropsTheSeedWithoutRetry() async {
        var t = Takeover()
        t.ds.answers = [DsStatementError.unauthorized]
        let outcome = await runTakeover(&t)
        XCTAssertEqual(outcome, .stale)
        XCTAssertTrue(outcome.settlesSeed)
        XCTAssertEqual(t.ds.segments, ["root"], "one attempt")
        XCTAssertTrue(t.sleeps.isEmpty)
        XCTAssertEqual(t.resets, 0)
        XCTAssertEqual(t.reconciles, 0)
        XCTAssertEqual(t.marked, 0)
    }

    /// 401 but this account's own probe answers 204: another device of this
    /// account took over already. The marker is set; the seed is dropped.
    func testA401WithTheOwnRootHeldMarksTheRootHeld() async {
        var t = Takeover()
        t.ds.answers = [DsStatementError.unauthorized]
        t.probe = .root
        let outcome = await runTakeover(&t)
        XCTAssertEqual(outcome, .heldElsewhere)
        XCTAssertTrue(outcome.settlesSeed)
        XCTAssertEqual(t.marked, 1)
        XCTAssertEqual(t.reconciles, 1)
    }

    /// Network errors and 5xx are retried twice; other 4xx are not.
    func testTransientFailuresRetryTwiceAndOther4xxDoNot() async {
        var t = Takeover()
        t.ds.answers = [URLError(.timedOut), DsStatementError.http(status: 503, body: ""), nil]
        let recovered = await runTakeover(&t)
        XCTAssertEqual(recovered, .tookOver)
        XCTAssertEqual(t.ds.segments, ["root", "root", "root", "registrars-set"])
        XCTAssertEqual(t.sleeps, RootHandover.retryDelaysMs)

        var exhausted = Takeover()
        exhausted.ds.answers = Array(repeating: DsStatementError.http(status: 500, body: ""), count: 3)
        let failed = await runTakeover(&exhausted)
        XCTAssertEqual(failed, .failed)
        XCTAssertFalse(failed.settlesSeed, "the seed is kept for a later retry")
        XCTAssertEqual(exhausted.ds.segments, ["root", "root", "root"], "two retries, then given up")
        XCTAssertEqual(exhausted.reconciles, 0)

        var refused = Takeover()
        refused.ds.answers = [DsStatementError.rankShortfall]
        let rank = await runTakeover(&refused)
        XCTAssertEqual(rank, .failed)
        XCTAssertEqual(refused.ds.segments, ["root"], "a 4xx is never retried")
    }

    /// The owner is re-checked before every root PUT.
    func testTheOwnerIsReCheckedBeforeTheRootPut() async {
        var t = Takeover()
        let outcome = await runTakeover(&t, stillOwner: { false })
        XCTAssertEqual(outcome, .notOwner)
        XCTAssertTrue(t.ds.sent.isEmpty)

        var retried = Takeover()
        retried.ds.answers = [DsStatementError.http(status: 502, body: "")]
        var checks = 0
        let later = await runTakeover(&retried, stillOwner: { checks += 1; return checks == 1 })
        XCTAssertEqual(later, .notOwner, "demoted during the retry wait")
        XCTAssertEqual(retried.ds.segments, ["root"])
    }

    /// A refused registrar set does not undo the takeover: the root moved, the
    /// reconcile still runs (and sends the set again).
    func testAFailedRegistrarSetStillReAppoints() async {
        var t = Takeover()
        t.ds.answers = [nil, DsStatementError.rankShortfall]
        let outcome = await runTakeover(&t)
        XCTAssertEqual(outcome, .tookOver)
        XCTAssertEqual(t.resets, 1)
        XCTAssertEqual(t.reconciles, 1)
    }

    /// The real statement signer over the handed-over seed produces the
    /// `root` route, and the old registrar key is the one the old root
    /// appointed.
    func testTheRealOldRootSignsTheRootStatement() throws {
        let outgoing = try Identity.fromSeed(seed: Data(repeating: 0x81, count: 32))
        let incoming = try Identity.fromSeed(seed: Data(repeating: 0x82, count: 32))
        let oldSeed = try dsJournalRootSeed(accountIdentitySeed: outgoing.ed25519Seed(), journalId: gid)
        let newRoot = try dsJournalRootSigner(accountIdentity: incoming, journalId: gid).publicKey()
        let request = try dsStatementSigner(seed: oldSeed).rootTransfer(journalId: gid, newRoot: newRoot)
        XCTAssertEqual(request.path, "/ds/group/\(DsClient.pseudonymHex(gid))/root")
        XCTAssertTrue(request.bodyJson.contains("statement_b64"))
        XCTAssertNotEqual(newRoot, try dsStatementSigner(seed: oldSeed).publicKey())
    }

    // MARK: The owed record

    func testTheOwedRecordSurvivesAStoreRebuildAndIsClearedWithTheAccount() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "owed-\(UUID().uuidString)"))
        let store = RegistrarSeedStore(defaults: defaults)
        store.markHandoverOwed(accountIdB64: "acct", groupIdHex: "AB12")
        store.markHandoverOwed(accountIdB64: "acct", groupIdHex: "cd34")
        XCTAssertEqual(RegistrarSeedStore(defaults: defaults).owedHandovers(accountIdB64: "acct"),
                       ["ab12", "cd34"], "group ids only, lowercase, survives a restart")
        store.clearHandoverOwed(accountIdB64: "acct", groupIdHex: "ab12")
        XCTAssertEqual(store.owedHandovers(accountIdB64: "acct"), ["cd34"])
        store.saveDelivered(accountIdB64: "acct", groupIdHex: "cd34", [Data([7]): .member])
        store.forgetDelivered(accountIdB64: "acct", groupIdHex: "cd34")
        XCTAssertTrue(store.delivered(accountIdB64: "acct", groupIdHex: "cd34").isEmpty)
        store.clear(accountIdB64: "acct")
        XCTAssertTrue(store.owedHandovers(accountIdB64: "acct").isEmpty, "forget device")
    }

    // MARK: Second security pass, finding 4 — the owed handover is reliable

    /// The outgoing owner keeps re-sending while its probe says it is still
    /// the root (at most every ten minutes), and the obligation settles only
    /// when the probe answers 401.
    func testTheOutgoingOwnerReSendsUntilTheProbeAnswers401() async {
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        var sends = 0
        let send: () async -> RootHandover.SendOutcome = { sends += 1; return .sent }

        let first = await RootHandover.runOwed(probe: { .root }, lastSentAt: nil, now: t0, send: send)
        XCTAssertEqual(first, .sent)
        XCTAssertFalse(first.settlesObligation, "kept until the probe says the root moved")

        let soon = await RootHandover.runOwed(
            probe: { .root }, lastSentAt: t0, now: t0.addingTimeInterval(60), send: send)
        XCTAssertEqual(soon, .throttled)

        let later = await RootHandover.runOwed(
            probe: { .root }, lastSentAt: t0,
            now: t0.addingTimeInterval(RootHandover.resendInterval + 1), send: send)
        XCTAssertEqual(later, .sent)
        XCTAssertEqual(sends, 2)

        let landed = await RootHandover.runOwed(
            probe: { .notRoot }, lastSentAt: t0, now: t0.addingTimeInterval(5), send: send)
        XCTAssertEqual(landed, .done)
        XCTAssertTrue(landed.settlesObligation)
        XCTAssertEqual(sends, 2, "nothing is sent once the root moved")
        XCTAssertEqual(RootHandover.resendInterval, 600)
    }

    /// A probe that cannot run (offline, 5xx) keeps the obligation and sends
    /// nothing; a failed send keeps it too; an owner again cancels it.
    func testAFailedProbeOrSendKeepsTheObligation() async {
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        let noProbe = await RootHandover.runOwed(
            probe: { throw URLError(.timedOut) }, lastSentAt: nil, now: t0,
            send: { XCTFail("no send without a probe"); return .sent })
        XCTAssertEqual(noProbe, .retry)
        XCTAssertFalse(noProbe.settlesObligation)
        let failed = await RootHandover.runOwed(probe: { .root }, lastSentAt: nil, now: t0, send: { .failed })
        XCTAssertEqual(failed, .retry)
        let pending = await RootHandover.runOwed(probe: { .root }, lastSentAt: nil, now: t0, send: { .pending })
        XCTAssertEqual(pending, .retry)
        let cancelled = await RootHandover.runOwed(probe: { .root }, lastSentAt: nil, now: t0, send: { .cancelled })
        XCTAssertTrue(cancelled.settlesObligation)
    }

    /// The receiver keeps the opened old root seed sealed under the device key
    /// across a restart until the takeover settles, then wipes it.
    func testTheReceivedRootSeedSurvivesARestartAndIsWipedAfter() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "pending-\(UUID().uuidString)"))
        let seed = Data(repeating: 0x5A, count: 32)
        RegistrarSeedStore(defaults: defaults).putPendingRootSeed(
            accountIdB64: "acct", groupIdHex: "AB12", seed: seed)
        let restarted = RegistrarSeedStore(defaults: defaults)
        XCTAssertEqual(restarted.pendingRootSeed(accountIdB64: "acct", groupIdHex: "ab12"), seed)
        XCTAssertEqual(restarted.pendingRootSeedGroups(accountIdB64: "acct"), ["ab12"])
        XCTAssertNil(restarted.get(accountIdB64: "acct", groupIdHex: "ab12"),
                     "never in the registrar-seed rows")
        let raw = defaults.dictionaryRepresentation().values.compactMap { $0 as? Data }
        XCTAssertFalse(raw.contains { $0.range(of: seed) != nil }, "stored sealed, never in the clear")
        restarted.putPendingRootSeed(accountIdB64: "acct", groupIdHex: "ab12", seed: Data(repeating: 1, count: 31))
        XCTAssertEqual(restarted.pendingRootSeed(accountIdB64: "acct", groupIdHex: "ab12"), seed,
                       "a wrong-width seed is refused")
        restarted.removePendingRootSeed(accountIdB64: "acct", groupIdHex: "ab12")
        XCTAssertNil(restarted.pendingRootSeed(accountIdB64: "acct", groupIdHex: "ab12"))
        restarted.putPendingRootSeed(accountIdB64: "acct", groupIdHex: "cd", seed: seed)
        restarted.clear(accountIdB64: "acct")
        XCTAssertTrue(restarted.pendingRootSeedGroups(accountIdB64: "acct").isEmpty, "forget device")
    }
}
