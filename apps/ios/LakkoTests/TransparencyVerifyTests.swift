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

    // MARK: - G-30 #2 (ADR-0019): verifyRotationChain outcome mapping

    private let pinnedOldPubkey = Data(repeating: 0x01, count: 32)
    private let supersessionProofBytes = Data(repeating: 0xEE, count: 145)

    /// Build a verifier whose rotation-chain gate is driven by the test
    /// seam (a valid Merkle + supersession proof can only be minted by
    /// the Rust log builder, which is not FFI-exposed).
    private func chainVerifier(
        client: TransparencyFetching?,
        operatorKey: Data? = nil,
        lastSeenRoot: LastSeenRoot? = nil,
        chain: ((TransparencyVerify.ChainArgs) throws -> Data)? = nil
    ) -> TransparencyVerify {
        TransparencyVerify(
            client: client,
            operatorKey: operatorKey ?? self.operatorKey,
            lastSeenRoot: lastSeenRoot ?? LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in },
            verifyChain: chain
        )
    }

    private func runChain(_ v: TransparencyVerify) async -> RotationChainResult {
        await v.verifyRotationChain(
            accountId: accountId,
            servedPubkey: servedPubkey,
            ctx: RotationContext(
                supersessionProof: supersessionProofBytes,
                pinnedOldPubkey: pinnedOldPubkey,
                pinnedLogVersion: 0
            )
        )
    }

    func testChainConfirmedCarriesVersion() async {
        // The latest entry commits to the served key at version 7; the
        // chain gate passes → `.confirmed` with version == 7.
        let v = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: LatestEntry(
                    idx: 3, version: 7, identityPubkey: servedPubkey, isReset: false
                ),
                proof: proofFixture
            ),
            chain: { _ in self.servedPubkey } // pass → authorised new pubkey
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .confirmed)
        XCTAssertEqual(result.version, 7)
        XCTAssertEqual(result.isReset, false)
    }

    func testChainConfirmedCarriesResetMarker() async {
        let v = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: true),
                proof: proofFixture
            ),
            chain: { _ in self.servedPubkey }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .confirmed)
        XCTAssertEqual(result.isReset, true)
        // `latestWith` fixes version == 1.
        XCTAssertEqual(result.version, 1)
    }

    func testChainMismatchDoesNotThrowAndHasNilVersion() async {
        // The chain gate THROWS (the log contradicts the proof) →
        // `.mismatch`, no version, marker still rides out, NEVER throws.
        let v = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: true),
                proof: proofFixture
            ),
            chain: { _ in throw FakeError.inclusionFailed }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .mismatch)
        XCTAssertNil(result.version)
        XCTAssertEqual(result.isReset, true)
    }

    func testChainAbsentWhenLatest404s() async {
        let v = chainVerifier(
            client: FakeFetcher(root: rootAt(10), latest: nil, proof: proofFixture),
            chain: { _ in self.servedPubkey }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .absent)
        XCTAssertNil(result.version)
        XCTAssertNil(result.isReset)
    }

    func testChainAbsentWhenLatestCommitsToDifferentKey() async {
        // The log's latest pubkey is NOT the served key — we cannot
        // chain-prove the served key, so `.absent` (no chain
        // confirmation), the chain gate is NOT invoked, and we do NOT
        // block. The marker still rides out (a latest existed).
        var chainCalls = 0
        let v = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(otherPubkey, isReset: true),
                proof: proofFixture
            ),
            chain: { _ in chainCalls += 1; return self.servedPubkey }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .absent)
        XCTAssertNil(result.version)
        XCTAssertEqual(result.isReset, true)
        XCTAssertEqual(chainCalls, 0)
    }

    func testChainRollbackWhenRootShrinks() async {
        let store = LastSeenRoot(defaults: freshDefaults())
        XCTAssertEqual(store.checkAndAdvance(20), .ok)
        let fake = FakeFetcher(root: rootAt(10), latest: latestWith(servedPubkey), proof: proofFixture)
        let v = chainVerifier(client: fake, lastSeenRoot: store, chain: { _ in self.servedPubkey })
        let result = await runChain(v)
        XCTAssertEqual(result.status, .rollback)
        XCTAssertNil(result.version)
        XCTAssertNil(result.isReset)
        // Rollback short-circuits — the latest lookup must NOT run.
        XCTAssertEqual(fake.fetchLatestCalls, 0)
    }

    func testChainOffWhenClientNil() async {
        let v = chainVerifier(client: nil, chain: { _ in self.servedPubkey })
        let result = await runChain(v)
        XCTAssertEqual(result.status, .off)
        XCTAssertNil(result.version)
        XCTAssertNil(result.isReset)
    }

    func testChainOffWhenOperatorKeyNil() async {
        let v = TransparencyVerify(
            client: FakeFetcher(root: rootAt(10), latest: latestWith(servedPubkey), proof: proofFixture),
            operatorKey: nil,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in },
            verifyChain: { _ in self.servedPubkey }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .off)
        XCTAssertNil(result.version)
    }

    func testChainUnreachableOnRootNetworkError() async {
        let v = chainVerifier(
            client: FakeFetcher(rootError: FakeError.network),
            chain: { _ in self.servedPubkey }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .unreachable)
        XCTAssertNil(result.version)
        XCTAssertNil(result.isReset)
    }

    func testChainUnreachableOnProofNetworkError() async {
        let v = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey),
                proofError: FakeError.network
            ),
            chain: { _ in self.servedPubkey }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .unreachable)
        XCTAssertNil(result.version)
    }

    func testChainPassesPinnedFloorAndProofToSeam() async {
        // Confirm the chain seam receives the pinned old key, the proof
        // bytes, and the pinned-version floor from the context.
        var observed: TransparencyVerify.ChainArgs?
        let v = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: LatestEntry(idx: 3, version: 5, identityPubkey: servedPubkey, isReset: false),
                proof: proofFixture
            ),
            chain: { args in observed = args; return self.servedPubkey }
        )
        let result = await v.verifyRotationChain(
            accountId: accountId,
            servedPubkey: servedPubkey,
            ctx: RotationContext(
                supersessionProof: supersessionProofBytes,
                pinnedOldPubkey: pinnedOldPubkey,
                pinnedLogVersion: 4
            )
        )
        XCTAssertEqual(result.status, .confirmed)
        let args = observed
        XCTAssertNotNil(args)
        XCTAssertEqual(args?.pinnedOldPubkey, pinnedOldPubkey)
        XCTAssertEqual(args?.supersessionProof, supersessionProofBytes)
        XCTAssertEqual(args?.pinnedLogVersion, 4)
        XCTAssertEqual(args?.servedPubkey, servedPubkey)
        XCTAssertEqual(args?.version, 5)
    }

    func testChainAdvancesLastSeenFloor() async {
        // A `.confirmed` chain advances the no-rollback high-water mark to
        // the root's log size (the verifier runs `checkAndAdvance`).
        let store = LastSeenRoot(defaults: freshDefaults())
        let v = chainVerifier(
            client: FakeFetcher(root: rootAt(42), latest: latestWith(servedPubkey), proof: proofFixture),
            lastSeenRoot: store,
            chain: { _ in self.servedPubkey }
        )
        let result = await runChain(v)
        XCTAssertEqual(result.status, .confirmed)
        XCTAssertEqual(store.loadLastSeenLogSize(), 42)
    }

    // MARK: - G-30 #2: ContactsClient routes a rotation through the chain

    /// A verified rotation (gate 1 re-pins via a real supersession proof)
    /// then routes gate 2 through the CHAIN verifier (not the single-key
    /// inclusion check) and persists the chain-confirmed version as the
    /// downgrade floor. We assert the floor advanced — proof the rotation
    /// branch ran and `recordLogVersion` fired on `.confirmed`.
    ///
    /// We cannot mint a real supersession proof here, so we drive gate 1
    /// with a TOFU pin whose pubkey EQUALS the served key (no rotation in
    /// gate 1 — it just matches + proceeds) and feed gate 2 the rotation
    /// context directly via `checkTransparencyLog`. That isolates the
    /// gate-2 routing + version-persist without needing a valid proof
    /// through gate 1's `verifySupersession` FFI.
    func testRotationRoutesThroughChainAndPersistsFloor() async {
        let pinStore = PeerPinStore(defaults: freshDefaults())
        let accountIdB64 = base64Encode(accountId)
        // Seed an OLD pin keyed on a DIFFERENT pubkey + a stale floor.
        pinStore.setPin(
            accountIdB64: accountIdB64,
            pinnedSignaturePubkeyB64: base64Encode(pinnedOldPubkey),
            rotationMarker: 0
        )
        pinStore.recordLogVersion(accountIdB64: accountIdB64, version: 2)

        let tv = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: LatestEntry(idx: 3, version: 9, identityPubkey: servedPubkey, isReset: false),
                proof: proofFixture
            ),
            chain: { _ in self.servedPubkey } // chain passes
        )
        let client = ContactsClient(pinStore: pinStore, transparencyVerify: tv)

        // Call gate 2 directly with the rotation context (oldPin's key
        // differs from the served key + a non-empty proof) → routes
        // through the chain.
        try? await client.checkTransparencyLog(
            accountId: accountId,
            accountIdB64: accountIdB64,
            servedPubkey: servedPubkey,
            oldPin: pinStore.readPin(accountIdB64: accountIdB64),
            supersessionProofB64: base64Encode(supersessionProofBytes)
        )

        // The chain-confirmed version (9) replaced the stale floor (2).
        XCTAssertEqual(pinStore.readLogVersion(accountIdB64: accountIdB64), 9)
        XCTAssertEqual(pinStore.readLogStatus(accountIdB64: accountIdB64), "confirmed")
    }

    /// A chain `.mismatch` (the chain gate threw) must NOT clear a
    /// previously-confirmed floor — `recordLogVersion` is skipped on a
    /// non-confirmed verdict (version == nil), and it does NOT block.
    func testChainMismatchKeepsPreviousFloorAndDoesNotBlock() async {
        let pinStore = PeerPinStore(defaults: freshDefaults())
        let accountIdB64 = base64Encode(accountId)
        pinStore.setPin(
            accountIdB64: accountIdB64,
            pinnedSignaturePubkeyB64: base64Encode(pinnedOldPubkey),
            rotationMarker: 0
        )
        pinStore.recordLogVersion(accountIdB64: accountIdB64, version: 5)

        let tv = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey),
                proof: proofFixture
            ),
            chain: { _ in throw FakeError.inclusionFailed } // chain throws
        )
        let client = ContactsClient(pinStore: pinStore, transparencyVerify: tv)

        var threw = false
        do {
            try await client.checkTransparencyLog(
                accountId: accountId,
                accountIdB64: accountIdB64,
                servedPubkey: servedPubkey,
                oldPin: pinStore.readPin(accountIdB64: accountIdB64),
                supersessionProofB64: base64Encode(supersessionProofBytes)
            )
        } catch {
            threw = true
        }
        XCTAssertFalse(threw) // mismatch surfaces, never blocks
        // The previously-confirmed floor (5) is untouched.
        XCTAssertEqual(pinStore.readLogVersion(accountIdB64: accountIdB64), 5)
        XCTAssertEqual(pinStore.readLogStatus(accountIdB64: accountIdB64), "mismatch")
    }

    /// A non-rotation fetch (no oldPin / unchanged key / no proof) keeps
    /// the SINGLE-KEY inclusion check — the chain seam is NOT invoked.
    func testNonRotationKeepsSingleKeyInclusionCheck() async {
        let pinStore = PeerPinStore(defaults: freshDefaults())
        let accountIdB64 = base64Encode(accountId)
        // Pin already EQUALS the served key — not a rotation.
        pinStore.setPin(
            accountIdB64: accountIdB64,
            pinnedSignaturePubkeyB64: base64Encode(servedPubkey),
            rotationMarker: 0
        )
        var chainCalls = 0
        var inclusionCalls = 0
        let tv = TransparencyVerify(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey),
                proof: proofFixture
            ),
            operatorKey: operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in inclusionCalls += 1 },
            verifyChain: { _ in chainCalls += 1; return self.servedPubkey }
        )
        let client = ContactsClient(pinStore: pinStore, transparencyVerify: tv)
        try? await client.checkTransparencyLog(
            accountId: accountId,
            accountIdB64: accountIdB64,
            servedPubkey: servedPubkey,
            oldPin: pinStore.readPin(accountIdB64: accountIdB64),
            supersessionProofB64: nil
        )
        XCTAssertEqual(chainCalls, 0)
        XCTAssertEqual(inclusionCalls, 1)
        // No version persisted on the single-key path.
        XCTAssertNil(pinStore.readLogVersion(accountIdB64: accountIdB64))
    }

    // MARK: - G-30 #2: gate 1 is NOT weakened (fail-closed still throws)

    /// Sanity that gate 1 (the SupersessionRefused path) still throws
    /// fail-closed on a changed key with no proof — the chain wiring in
    /// gate 2 must NEVER make gate 1 more permissive. (Gate 2 never runs
    /// here — gate 1 short-circuits.)
    func testGate1StillFailsClosedOnChangedKeyNoProof() async {
        let pinStore = PeerPinStore(defaults: freshDefaults())
        let oldPubkey = Data(repeating: 0x01, count: 32)
        pinStore.setPin(
            accountIdB64: base64Encode(accountId),
            pinnedSignaturePubkeyB64: base64Encode(oldPubkey),
            rotationMarker: 0
        )
        // Served a DIFFERENT key with NO proof → gate 1 must refuse.
        let bundle = IdentityFetchResponse(
            signaturePubkey: base64Encode(servedPubkey),
            x25519Pubkey: base64Encode(Data(repeating: 0x09, count: 32)),
            supersessionProof: nil
        )
        let tv = chainVerifier(
            client: FakeFetcher(
                root: rootAt(10),
                latest: latestWith(servedPubkey, isReset: true),
                proof: proofFixture
            ),
            chain: { _ in self.servedPubkey }
        )
        let client = ContactsClient(pinStore: pinStore, transparencyVerify: tv)
        do {
            try await client.verifyBundleAccountKey(accountId: accountId, bundle: bundle)
            XCTFail("expected SupersessionRefused")
        } catch let refused as SupersessionRefused {
            XCTAssertEqual(refused.reason, .proofAbsent)
            // Gate-1 enrichment still attaches the loggedReset context.
            XCTAssertEqual(refused.loggedReset, true)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    // MARK: - G-30 #2: PeerPinStore.recordLogVersion / readLogVersion

    func testRecordLogVersionRoundTrip() {
        let store = PeerPinStore(defaults: freshDefaults())
        let key = base64Encode(accountId)
        store.setPin(accountIdB64: key, pinnedSignaturePubkeyB64: base64Encode(servedPubkey), rotationMarker: 0)
        XCTAssertNil(store.readLogVersion(accountIdB64: key))
        XCTAssertTrue(store.recordLogVersion(accountIdB64: key, version: 12))
        XCTAssertEqual(store.readLogVersion(accountIdB64: key), 12)
    }

    func testRecordLogVersionClear() {
        let store = PeerPinStore(defaults: freshDefaults())
        let key = base64Encode(accountId)
        store.setPin(accountIdB64: key, pinnedSignaturePubkeyB64: base64Encode(servedPubkey), rotationMarker: 0)
        store.recordLogVersion(accountIdB64: key, version: 8)
        XCTAssertEqual(store.readLogVersion(accountIdB64: key), 8)
        XCTAssertTrue(store.recordLogVersion(accountIdB64: key, version: nil))
        XCTAssertNil(store.readLogVersion(accountIdB64: key))
    }

    func testRecordLogVersionNoOpWithoutPin() {
        let store = PeerPinStore(defaults: freshDefaults())
        let key = base64Encode(accountId)
        // No pin → no write, returns false, nothing to read.
        XCTAssertFalse(store.recordLogVersion(accountIdB64: key, version: 3))
        XCTAssertNil(store.readLogVersion(accountIdB64: key))
    }

    func testRecordLogVersionCarriedAcrossRePin() {
        let store = PeerPinStore(defaults: freshDefaults())
        let key = base64Encode(accountId)
        store.setPin(accountIdB64: key, pinnedSignaturePubkeyB64: base64Encode(pinnedOldPubkey), rotationMarker: 0)
        store.recordLogVersion(accountIdB64: key, version: 6)
        // A forward re-pin to a new key must CARRY the version (it is a
        // separately-written field; the fetch path re-records it).
        XCTAssertEqual(
            store.setPin(accountIdB64: key, pinnedSignaturePubkeyB64: base64Encode(servedPubkey), rotationMarker: 10),
            .set
        )
        XCTAssertEqual(store.readLogVersion(accountIdB64: key), 6)
        // recordLogStatus / recordLogReset must also preserve it.
        store.recordLogStatus(accountIdB64: key, logStatus: "confirmed")
        store.recordLogReset(accountIdB64: key, isReset: true)
        XCTAssertEqual(store.readLogVersion(accountIdB64: key), 6)
    }

    func testRecordLogVersionIdempotentNoChange() {
        let store = PeerPinStore(defaults: freshDefaults())
        let key = base64Encode(accountId)
        store.setPin(accountIdB64: key, pinnedSignaturePubkeyB64: base64Encode(servedPubkey), rotationMarker: 0)
        store.recordLogVersion(accountIdB64: key, version: 4)
        // Re-writing the same value is idempotent (still returns true).
        XCTAssertTrue(store.recordLogVersion(accountIdB64: key, version: 4))
        XCTAssertEqual(store.readLogVersion(accountIdB64: key), 4)
    }

    func testReadLogVersionDecodesBackCompatBlobWithoutVersion() {
        // A pin blob written BEFORE G-30 #2 (no `pinnedLogVersion` key)
        // must decode with version == nil (synthesized Codable). Simulate
        // by writing a pin via the v1-shape init and reading it back.
        let store = PeerPinStore(defaults: freshDefaults())
        let key = base64Encode(accountId)
        store.setPin(accountIdB64: key, pinnedSignaturePubkeyB64: base64Encode(servedPubkey), rotationMarker: 0)
        // setPin writes no version → absent key → nil on read.
        XCTAssertNil(store.readLogVersion(accountIdB64: key))
        // And the pin itself is intact.
        XCTAssertNotNil(store.readPin(accountIdB64: key))
    }

    // MARK: - G-30 #2 (ADR-0019) first-observation genesis self-sig

    /// A REAL identity whose `signaturePublicKey` is the served key, plus a
    /// real genesis self-sig over `(account_key, served_key)`. The inclusion
    /// gate is stubbed to pass so the genesis gate is reached; the genesis
    /// check itself runs the REAL `verifyLeafGenesis` FFI.
    private func genesisFixture(tamper: Bool) throws -> (Data, LatestEntry) {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x5A, count: 32))
        let served = identity.signaturePublicKey()
        let ak = transparencyAccountKey(accountId: accountId)
        var sig = try signLeafGenesis(ed25519Seed: identity.ed25519Seed(), usernameHash: ak)
        if tamper {
            sig[0] ^= 0x01
        }
        let latest = LatestEntry(
            idx: 3, version: 1, identityPubkey: served, isReset: false, leafAuth: sig
        )
        return (served, latest)
    }

    func testGenesisSelfSigConfirmsWhenValid() async throws {
        let (served, latest) = try genesisFixture(tamper: false)
        let v = TransparencyVerify(
            client: FakeFetcher(root: rootAt(10), latest: latest, proof: proofFixture),
            operatorKey: operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        let result = await v.verifyIdentityInLog(
            accountId: accountId, publishedSignaturePubkey: served
        )
        XCTAssertEqual(result.status, .confirmed)
    }

    func testGenesisSelfSigMismatchWhenTampered() async throws {
        let (served, latest) = try genesisFixture(tamper: true)
        let v = TransparencyVerify(
            client: FakeFetcher(root: rootAt(10), latest: latest, proof: proofFixture),
            operatorKey: operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        let result = await v.verifyIdentityInLog(
            accountId: accountId, publishedSignaturePubkey: served
        )
        XCTAssertEqual(result.status, .mismatch)
    }

    func testGenesisCheckSkippedForRotationLengthLeafAuth() async throws {
        // A 145-byte supersession proof is the rotation case — the genesis
        // gate skips it (the chain verifies a rotation), so a clean inclusion
        // is still confirmed even though the bytes are not a genesis self-sig.
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x5A, count: 32))
        let served = identity.signaturePublicKey()
        let latest = LatestEntry(
            idx: 3, version: 1, identityPubkey: served, isReset: false,
            leafAuth: Data(repeating: 0x52, count: 145)
        )
        let v = TransparencyVerify(
            client: FakeFetcher(root: rootAt(10), latest: latest, proof: proofFixture),
            operatorKey: operatorKey,
            lastSeenRoot: LastSeenRoot(defaults: freshDefaults()),
            verifyInclusion: { _ in }
        )
        let result = await v.verifyIdentityInLog(
            accountId: accountId, publishedSignaturePubkey: served
        )
        XCTAssertEqual(result.status, .confirmed)
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
