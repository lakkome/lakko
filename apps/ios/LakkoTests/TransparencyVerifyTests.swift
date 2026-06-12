// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// KT-log S3d — iOS transparency-log verification tests.
///
/// Mirror of the web `apps/web/src/transparency/{last-seen-root,verify}.test.ts`
/// (S3c). Two halves:
///
///   * ``LastSeenRoot`` rollback logic — pure, no native lib: the
///     no-rollback high-water gate (advance on `>=`, refuse + never
///     advance on a shrink, persist across instances).
///   * ``TransparencyVerify``'s ``LogVerifyStatus`` state machine — a
///     fake ``TransparencyFetching`` supplies synthetic root / latest /
///     proof fixtures, and the inclusion gate is injected via the
///     test-only seam (a valid Merkle proof can only be minted by the
///     Rust log builder, which is not FFI-exposed). The account-key
///     derivation step uses the REAL `transparencyAccountKey` FFI from
///     LakkoCore — no mocking of the verifier internals.
final class TransparencyVerifyTests: XCTestCase {

    // MARK: - Fixtures

    /// An isolated `UserDefaults` suite so each test starts clean and
    /// never touches `.standard`.
    private func freshDefaults(
        _ name: String = "lakko.test.transparency.\(UUID().uuidString)"
    ) -> UserDefaults {
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    private let accountId = Data([
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ])
    private let servedPubkey = Data(repeating: 0xAB, count: 32)
    private let otherPubkey = Data(repeating: 0xCD, count: 32)
    private let operatorKey = Data(repeating: 0x07, count: 32)

    private func rootAt(_ logSize: UInt64) -> SignedRoot {
        SignedRoot(
            treeRoot: Data(repeating: 0x11, count: 32),
            logSize: logSize,
            timestampUnix: 1_700_000_000,
            signature: Data(repeating: 0x22, count: 64)
        )
    }

    private func latestWith(_ pubkey: Data, isReset: Bool = false) -> LatestEntry {
        LatestEntry(idx: 3, version: 1, identityPubkey: pubkey, isReset: isReset)
    }

    private var proofFixture: InclusionProof {
        InclusionProof(
            idx: 3,
            leaf: Data([0xDE, 0xAD]),
            auditPath: Data(repeating: 0x33, count: 64),
            logSize: 10
        )
    }

    // MARK: - LastSeenRoot (pure rollback logic)

    func testLastSeenStartsAtZero() {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.loadLastSeenLogSize(), 0)
    }

    func testLastSeenAdvancesOnGrowth() {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(10), .ok)
        XCTAssertEqual(store.loadLastSeenLogSize(), 10)
        XCTAssertEqual(store.checkAndAdvance(42), .ok)
        XCTAssertEqual(store.loadLastSeenLogSize(), 42)
    }

    func testLastSeenEqualIsOkAndIdempotent() {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(15), .ok)
        // A `==` re-observation is allowed (and does not regress).
        XCTAssertEqual(store.checkAndAdvance(15), .ok)
        XCTAssertEqual(store.loadLastSeenLogSize(), 15)
    }

    func testLastSeenRefusesShrinkAndNeverAdvances() {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(20), .ok)
        // A smaller size is an unambiguous rollback — refuse + keep 20.
        XCTAssertEqual(store.checkAndAdvance(10), .rollback)
        XCTAssertEqual(store.loadLastSeenLogSize(), 20)
        // The store stays usable: a forward move still advances.
        XCTAssertEqual(store.checkAndAdvance(25), .ok)
        XCTAssertEqual(store.loadLastSeenLogSize(), 25)
    }

    func testLastSeenPersistsAcrossInstances() {
        let suite = "lakko.test.transparency.persist.\(UUID().uuidString)"
        let first = LastSeenRoot(defaults: freshDefaults(suite))
        XCTAssertEqual(first.checkAndAdvance(99), .ok)

        let second = LastSeenRoot(defaults: UserDefaults(suiteName: suite)!)
        XCTAssertEqual(second.loadLastSeenLogSize(), 99)
        // The second instance sees the first's mark — a smaller size is
        // a rollback across "relaunch".
        XCTAssertEqual(second.checkAndAdvance(50), .rollback)
    }

    func testLastSeenClear() {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(7), .ok)
        store.clear()
        XCTAssertEqual(store.loadLastSeenLogSize(), 0)
    }

    // MARK: - LogVerifyStatus state machine

    /// Build a verifier over an isolated last-seen store + the supplied
    /// fake client + inclusion seam.
    private func verifier(
        client: TransparencyFetching?,
        operatorKey: Data? = nil,
        inclusion: ((TransparencyVerify.InclusionArgs) throws -> Void)? = { _ in }
    ) -> TransparencyVerify {
        TransparencyVerify(
            client: client,
            operatorKey: operatorKey ?? self.operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: inclusion
        )
    }

    private func run(_ v: TransparencyVerify) async -> LogVerifyResult {
        await v.verifyIdentityInLog(
            accountId: accountId,
            publishedSignaturePubkey: servedPubkey
        )
    }

    /// Convenience for the existing status-only assertions — pulls the
    /// ``LogVerifyStatus`` out of the ``LogVerifyResult`` (KT-log S4
    /// added the `isReset` field alongside it).
    private func runStatus(_ v: TransparencyVerify) async -> LogVerifyStatus {
        await run(v).status
    }

    func testOffWhenClientNil() async {
        let v = verifier(client: nil)
        let status = await runStatus(v)
        XCTAssertEqual(status, .off)
    }

    func testOffWhenOperatorKeyNil() async {
        let v = TransparencyVerify(
            client: FakeFetcher(root: rootAt(10), latest: latestWith(servedPubkey), proof: proofFixture),
            operatorKey: nil,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        let status = await runStatus(v)
        XCTAssertEqual(status, .off)
    }

    func testConfirmedOnFullyValidChain() async {
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey),
                proof: proofFixture
            ),
            inclusion: { _ in /* pass */ }
        )
        let status = await runStatus(v)
        XCTAssertEqual(status, .confirmed)
    }

    func testRollbackWhenRootShrinksBelowLastSeen() async {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(20), .ok)
        let fake = FakeFetcher(root: rootAt(10), latest: latestWith(servedPubkey), proof: proofFixture)
        let v = TransparencyVerify(
            client: fake,
            operatorKey: operatorKey,
            lastSeenRoot: store,
            verifyInclusion: { _ in }
        )
        let status = await runStatus(v)
        XCTAssertEqual(status, .rollback)
        // Rollback short-circuits — the latest lookup must NOT run.
        XCTAssertEqual(fake.fetchLatestCalls, 0)
    }

    func testAbsentWhenLatest404s() async {
        let v = verifier(
            client: FakeFetcher(root: rootAt(10), latest: nil, proof: proofFixture)
        )
        let status = await runStatus(v)
        XCTAssertEqual(status, .absent)
    }

    func testMismatchWhenLogKeyDiffers() async {
        let v = verifier(
            client: FakeFetcher(root: rootAt(10), latest: latestWith(otherPubkey), proof: proofFixture)
        )
        let status = await runStatus(v)
        XCTAssertEqual(status, .mismatch)
    }

    func testMismatchWhenInclusionProofFails() async {
        let v = verifier(
            client: FakeFetcher(root: rootAt(10), latest: latestWith(servedPubkey), proof: proofFixture),
            inclusion: { _ in throw FakeError.inclusionFailed }
        )
        let status = await runStatus(v)
        XCTAssertEqual(status, .mismatch)
    }

    func testUnreachableOnRootNetworkError() async {
        let v = verifier(client: FakeFetcher(rootError: FakeError.network))
        let status = await runStatus(v)
        XCTAssertEqual(status, .unreachable)
    }

    func testUnreachableOnLatestNetworkError() async {
        let v = verifier(client: FakeFetcher(root: rootAt(10), latestError: FakeError.network))
        let status = await runStatus(v)
        XCTAssertEqual(status, .unreachable)
    }

    func testUnreachableOnProofNetworkError() async {
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey),
                proofError: FakeError.network
            )
        )
        let status = await runStatus(v)
        XCTAssertEqual(status, .unreachable)
    }

    func testAdvancesLastSeenOnConfirmedThenRollsBackOnShrink() async {
        let store = LastSeenRoot(defaults: freshDefaults())
        let v1 = TransparencyVerify(
            client: FakeFetcher(root: rootAt(42), latest: latestWith(servedPubkey), proof: proofFixture),
            operatorKey: operatorKey,
            lastSeenRoot: store,
            verifyInclusion: { _ in }
        )
        let r1 = await runStatus(v1)
        XCTAssertEqual(r1, .confirmed)
        XCTAssertEqual(store.loadLastSeenLogSize(), 42)

        // A subsequent smaller root over the SAME store is a rollback.
        let v2 = TransparencyVerify(
            client: FakeFetcher(root: rootAt(41), latest: latestWith(servedPubkey), proof: proofFixture),
            operatorKey: operatorKey,
            lastSeenRoot: store,
            verifyInclusion: { _ in }
        )
        let r2 = await runStatus(v2)
        XCTAssertEqual(r2, .rollback)
    }

    /// The verifier binds the REAL `transparencyAccountKey` FFI as the
    /// `/latest` lookup key — confirm the fake observed exactly that
    /// 32-byte derivation (so a server impostor keying on the wrong hash
    /// would not match).
    func testUsesRealAccountKeyDerivation() async {
        let fake = FakeFetcher(root: rootAt(10), latest: latestWith(servedPubkey), proof: proofFixture)
        let v = TransparencyVerify(
            client: fake,
            operatorKey: operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        _ = await run(v)
        let expected = transparencyAccountKey(accountId: accountId)
        XCTAssertEqual(fake.lastUsernameHash, expected)
        XCTAssertEqual(expected.count, 32)
    }

    // MARK: - KT-log S4: isReset marker on verifyIdentityInLog

    func testConfirmedCarriesResetTrueMarker() async {
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: true),
                proof: proofFixture
            ),
            inclusion: { _ in /* pass */ }
        )
        let result = await run(v)
        XCTAssertEqual(result.status, .confirmed)
        XCTAssertEqual(result.isReset, true)
    }

    func testConfirmedCarriesResetFalseMarker() async {
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: false),
                proof: proofFixture
            ),
            inclusion: { _ in /* pass */ }
        )
        let result = await run(v)
        XCTAssertEqual(result.status, .confirmed)
        XCTAssertEqual(result.isReset, false)
    }

    func testMismatchCarriesResetMarker() async {
        // A latest entry EXISTS (it just commits to a different key) —
        // the marker still rides out on a `.mismatch`.
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(otherPubkey, isReset: true),
                proof: proofFixture
            )
        )
        let result = await run(v)
        XCTAssertEqual(result.status, .mismatch)
        XCTAssertEqual(result.isReset, true)
    }

    func testAbsentHasNilResetMarker() async {
        // No latest entry — there is nothing to read the marker from.
        let v = verifier(
            client: FakeFetcher(root: rootAt(10), latest: nil, proof: proofFixture)
        )
        let result = await run(v)
        XCTAssertEqual(result.status, .absent)
        XCTAssertNil(result.isReset)
    }

    func testOffHasNilResetMarker() async {
        let v = verifier(client: nil)
        let result = await run(v)
        XCTAssertEqual(result.status, .off)
        XCTAssertNil(result.isReset)
    }

    func testRollbackHasNilResetMarker() async {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(20), .ok)
        let v = TransparencyVerify(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: true),
                proof: proofFixture
            ),
            operatorKey: operatorKey,
            lastSeenRoot: store,
            verifyInclusion: { _ in }
        )
        let result = await run(v)
        XCTAssertEqual(result.status, .rollback)
        // Short-circuited before the latest lookup — no marker.
        XCTAssertNil(result.isReset)
    }

    func testUnreachableHasNilResetMarker() async {
        let v = verifier(client: FakeFetcher(rootError: FakeError.network))
        let result = await run(v)
        XCTAssertEqual(result.status, .unreachable)
        XCTAssertNil(result.isReset)
    }

    // MARK: - KT-log S4: lookupServedKeyReset best-effort helper

    private func lookup(
        _ v: TransparencyVerify,
        pubkey: Data
    ) async -> Bool? {
        await v.lookupServedKeyReset(accountId: accountId, servedPubkey: pubkey)
    }

    func testLookupReturnsTrueWhenLatestCommitsToServedResetKey() async {
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: true),
                proof: proofFixture
            )
        )
        let r = await lookup(v, pubkey: servedPubkey)
        XCTAssertEqual(r, true)
    }

    func testLookupReturnsFalseWhenLatestCommitsToServedNonResetKey() async {
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: false),
                proof: proofFixture
            )
        )
        let r = await lookup(v, pubkey: servedPubkey)
        XCTAssertEqual(r, false)
    }

    func testLookupReturnsNilWhenLatestCommitsToDifferentKey() async {
        // The log's latest pubkey diverges from the served key — the
        // marker describes a different key, so it is unknown (nil).
        let v = verifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(otherPubkey, isReset: true),
                proof: proofFixture
            )
        )
        let r = await lookup(v, pubkey: servedPubkey)
        XCTAssertNil(r)
    }

    func testLookupReturnsNilWhenAbsent() async {
        let v = verifier(
            client: FakeFetcher(root: rootAt(10), latest: nil, proof: proofFixture)
        )
        let r = await lookup(v, pubkey: servedPubkey)
        XCTAssertNil(r)
    }

    func testLookupReturnsNilWhenOff() async {
        let v = verifier(client: nil)
        let r = await lookup(v, pubkey: servedPubkey)
        XCTAssertNil(r)
    }

    func testLookupReturnsNilOnNetworkFault() async {
        let v = verifier(
            client: FakeFetcher(root: rootAt(10), latestError: FakeError.network)
        )
        let r = await lookup(v, pubkey: servedPubkey)
        XCTAssertNil(r)
    }

    func testLookupDoesNotRunInclusionOrRollbackGate() async {
        // The best-effort helper must NOT consult the root (no rollback
        // gate) — only `/latest`. A pre-advanced store that WOULD trip a
        // rollback in `verifyIdentityInLog` is irrelevant here.
        let fake = FakeFetcher(
            root: rootAt(10),
            latest: latestWith(servedPubkey, isReset: true),
            proof: proofFixture
        )
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(99), .ok) // would be a rollback for root=10
        let v = TransparencyVerify(
            client: fake,
            operatorKey: operatorKey,
            lastSeenRoot: store,
            verifyInclusion: { _ in throw FakeError.inclusionFailed }
        )
        let r = await lookup(v, pubkey: servedPubkey)
        XCTAssertEqual(r, true)
        XCTAssertEqual(fake.fetchLatestCalls, 1)
    }

    // MARK: - KT-log S4: SupersessionRefused carries loggedReset

    /// A changed key with no proof refuses (`.proofAbsent`); the
    /// enrichment populates `loggedReset` from the best-effort log lookup
    /// of the served key. The gate refuses REGARDLESS of the log — this
    /// only colours the error.
    func testRefusalCarriesLoggedResetTrue() async {
        let pinStore = PeerPinStore(defaults: freshDefaults())
        let oldPubkey = Data(repeating: 0x01, count: 32)
        pinStore.setPin(
            accountIdB64: base64Encode(accountId),
            pinnedSignaturePubkeyB64: base64Encode(oldPubkey),
            rotationMarker: 0
        )
        // The bundle serves a DIFFERENT key (servedPubkey) and NO proof.
        let bundle = IdentityFetchResponse(
            signaturePubkey: base64Encode(servedPubkey),
            x25519Pubkey: base64Encode(Data(repeating: 0x09, count: 32)),
            supersessionProof: nil
        )
        // The transparency log's latest commits to exactly the served
        // key and marks it an owner reset.
        let tv = TransparencyVerify(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: true),
                proof: proofFixture
            ),
            operatorKey: operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        let client = ContactsClient(
            pinStore: pinStore,
            transparencyVerify: tv
        )
        do {
            try await client.verifyBundleAccountKey(accountId: accountId, bundle: bundle)
            XCTFail("expected SupersessionRefused")
        } catch let refused as SupersessionRefused {
            XCTAssertEqual(refused.reason, .proofAbsent)
            XCTAssertEqual(refused.loggedReset, true)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testRefusalCarriesLoggedResetFalse() async {
        let pinStore = PeerPinStore(defaults: freshDefaults())
        let oldPubkey = Data(repeating: 0x01, count: 32)
        pinStore.setPin(
            accountIdB64: base64Encode(accountId),
            pinnedSignaturePubkeyB64: base64Encode(oldPubkey),
            rotationMarker: 0
        )
        let bundle = IdentityFetchResponse(
            signaturePubkey: base64Encode(servedPubkey),
            x25519Pubkey: base64Encode(Data(repeating: 0x09, count: 32)),
            supersessionProof: nil
        )
        let tv = TransparencyVerify(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: false),
                proof: proofFixture
            ),
            operatorKey: operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        let client = ContactsClient(pinStore: pinStore, transparencyVerify: tv)
        do {
            try await client.verifyBundleAccountKey(accountId: accountId, bundle: bundle)
            XCTFail("expected SupersessionRefused")
        } catch let refused as SupersessionRefused {
            XCTAssertEqual(refused.reason, .proofAbsent)
            XCTAssertEqual(refused.loggedReset, false)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testRefusalCarriesLoggedResetNilWhenLogOff() async {
        let pinStore = PeerPinStore(defaults: freshDefaults())
        let oldPubkey = Data(repeating: 0x01, count: 32)
        pinStore.setPin(
            accountIdB64: base64Encode(accountId),
            pinnedSignaturePubkeyB64: base64Encode(oldPubkey),
            rotationMarker: 0
        )
        let bundle = IdentityFetchResponse(
            signaturePubkey: base64Encode(servedPubkey),
            x25519Pubkey: base64Encode(Data(repeating: 0x09, count: 32)),
            supersessionProof: nil
        )
        // Transparency integration OFF (client nil) → loggedReset nil.
        let tv = TransparencyVerify(
            client: nil,
            operatorKey: nil,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        let client = ContactsClient(pinStore: pinStore, transparencyVerify: tv)
        do {
            try await client.verifyBundleAccountKey(accountId: accountId, bundle: bundle)
            XCTFail("expected SupersessionRefused")
        } catch let refused as SupersessionRefused {
            XCTAssertEqual(refused.reason, .proofAbsent)
            XCTAssertNil(refused.loggedReset)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    // MARK: - KT-log S4: reset-aware message mapping

    func testMessageLoggedResetTrueIsConfirmedOwnerReset() {
        let msg = ContactsViewModel.supersessionRefusedMessage(
            loggedReset: true, peerLabel: "@alice"
        )
        XCTAssertTrue(msg.contains("@alice"))
        XCTAssertTrue(msg.contains("reset their secure identity"))
        XCTAssertTrue(msg.contains("owner-initiated reset"))
        XCTAssertTrue(msg.contains("re-verify"))
        // A confirmed owner reset must NOT be framed as a compromise.
        XCTAssertFalse(msg.contains("compromise"))
    }

    func testMessageLoggedResetFalseIsPossibleCompromise() {
        let msg = ContactsViewModel.supersessionRefusedMessage(
            loggedReset: false, peerLabel: "@bob"
        )
        XCTAssertTrue(msg.contains("@bob"))
        XCTAssertTrue(msg.contains("does NOT show an owner reset"))
        XCTAssertTrue(msg.contains("possible account compromise"))
        XCTAssertTrue(msg.contains("Verify out of band"))
    }

    func testMessageLoggedResetNilIsNeutralCouldNotConfirm() {
        let msg = ContactsViewModel.supersessionRefusedMessage(
            loggedReset: nil, peerLabel: "@carol"
        )
        XCTAssertTrue(msg.contains("@carol"))
        XCTAssertTrue(msg.contains("could not be confirmed"))
        XCTAssertTrue(msg.contains("Verify out of band"))
        // The neutral message must not assert a compromise OR a reset.
        XCTAssertFalse(msg.contains("compromise"))
        XCTAssertFalse(msg.contains("owner-initiated reset"))
    }

    func testMessageFallsBackToThisContactWhenNoLabel() {
        XCTAssertTrue(
            ContactsViewModel.supersessionRefusedMessage(loggedReset: true, peerLabel: nil)
                .contains("This contact")
        )
        XCTAssertTrue(
            ContactsViewModel.supersessionRefusedMessage(loggedReset: true, peerLabel: "")
                .contains("This contact")
        )
    }

    func testMessageThreeVerdictsAreDistinct() {
        let a = ContactsViewModel.supersessionRefusedMessage(loggedReset: true, peerLabel: "@x")
        let b = ContactsViewModel.supersessionRefusedMessage(loggedReset: false, peerLabel: "@x")
        let c = ContactsViewModel.supersessionRefusedMessage(loggedReset: nil, peerLabel: "@x")
        XCTAssertEqual(Set([a, b, c]).count, 3)
    }
}

// MARK: - Test doubles

private enum FakeError: Error {
    case network
    case inclusionFailed
}

/// A fake ``TransparencyFetching`` returning fixed fixtures or throwing
/// fixed errors per endpoint. Records call counts + the last
/// account-key it was queried with, so a test can assert short-circuit
/// ordering and the real account-key derivation.
private final class FakeFetcher: TransparencyFetching {
    private let root: SignedRoot?
    private let rootError: Error?
    private let latest: LatestEntry?
    private let latestError: Error?
    private let proof: InclusionProof?
    private let proofError: Error?

    private(set) var fetchLatestCalls = 0
    private(set) var lastUsernameHash: Data?

    /// `latest == nil` AND `latestError == nil` is the explicit-404
    /// case (the account is not yet in the log) — `fetchLatest` returns
    /// `nil`. A non-nil `latest` is the present-entry case; a non-nil
    /// `latestError` is the network-fault case.
    init(
        root: SignedRoot? = nil,
        rootError: Error? = nil,
        latest: LatestEntry? = nil,
        latestError: Error? = nil,
        proof: InclusionProof? = nil,
        proofError: Error? = nil
    ) {
        self.root = root
        self.rootError = rootError
        self.latest = latest
        self.latestError = latestError
        self.proof = proof
        self.proofError = proofError
    }

    func fetchRoot() async throws -> SignedRoot {
        if let e = rootError { throw e }
        guard let root = root else { throw FakeError.network }
        return root
    }

    func fetchLatest(usernameHash: Data) async throws -> LatestEntry? {
        fetchLatestCalls += 1
        lastUsernameHash = usernameHash
        if let e = latestError { throw e }
        // No fixture + no error → the documented 404 (account absent).
        return latest
    }

    func fetchProof(idx: UInt64) async throws -> InclusionProof {
        if let e = proofError { throw e }
        guard let proof = proof else { throw FakeError.network }
        return proof
    }
}
