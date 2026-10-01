// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// G-4 (ADR-0016) — iOS identity-supersession transport tests.
///
/// Covers the two security-critical client halves the iOS sprints
/// landed:
///
///   * Sprint 2i — the ``PeerPinStore`` persistence + monotonic
///     marker (rules #3 + #4).
///   * Sprint 5i — ``ContactsClient.verifySupersessionGate`` decision
///     table: TOFU on first sight, no-op on an unchanged key, a
///     verified re-pin on a rotation with a valid proof, and the
///     fail-closed refusals (rule #2) when the proof is absent,
///     invalid, names the wrong key, or moves the marker backwards.
///
/// Proofs are minted in-test via the real `signSupersessionProof`
/// FFI over two `Identity` seeds, so the verify path runs against
/// genuine cryptographic artefacts — no mocking of the verifier.
final class SupersessionTests: XCTestCase {

    // MARK: - Fixtures

    /// 32-byte export-key seed `b` derived from a single filler byte.
    private func seed(_ byte: UInt8) -> Data {
        Data(repeating: byte, count: 32)
    }

    /// An isolated `UserDefaults` suite so each test starts clean and
    /// never touches `.standard`.
    private func freshDefaults(
        _ name: String = "lakko.test.pins.\(UUID().uuidString)"
    ) -> UserDefaults {
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    private let nowSecs = Int64(Date().timeIntervalSince1970)

    /// Mint a real supersession proof from two export-key seeds.
    private func mintProof(
        oldSeed oldExport: Data,
        newSeed newExport: Data,
        issued: Int64,
        expires: Int64
    ) throws -> Data {
        let oldEd = try Identity.fromSeed(seed: oldExport).ed25519Seed()
        let newEd = try Identity.fromSeed(seed: newExport).ed25519Seed()
        return try signSupersessionProof(
            oldEd25519Seed: oldEd,
            newEd25519Seed: newEd,
            issuedAtUnixSecs: issued,
            expiresAtUnixSecs: expires
        )
    }

    private func sigPubB64(forSeed export: Data) throws -> String {
        let identity = try Identity.fromSeed(seed: export)
        return base64Encode(identity.signaturePublicKey())
    }

    // MARK: - Sprint 2i: PeerPinStore

    func testPinStoreTofuThenReadBack() {
        let store = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "test-owner")
        let acct = base64Encode(Data(repeating: 0xA1, count: 16))

        XCTAssertNil(store.readPin(accountIdB64: acct))

        let result = store.setPin(
            accountIdB64: acct,
            pinnedSignaturePubkeyB64: "PUBKEY_A",
            rotationMarker: 0
        )
        XCTAssertEqual(result, .set)

        let pin = store.readPin(accountIdB64: acct)
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, "PUBKEY_A")
        XCTAssertEqual(pin?.rotationMarker, 0)
    }

    func testPinStorePersistsAcrossInstances() {
        // A second `PeerPinStore` over the SAME suite must see the
        // first store's write — i.e. it survives "relaunch".
        let suite = "lakko.test.pins.persist.\(UUID().uuidString)"
        let acct = base64Encode(Data(repeating: 0xB2, count: 16))

        let first = PeerPinStore(defaults: freshDefaults(suite), ownerAccountIdB64: "test-owner")
        first.setPin(
            accountIdB64: acct,
            pinnedSignaturePubkeyB64: "PUBKEY_PERSIST",
            rotationMarker: 42
        )

        let second = PeerPinStore(defaults: UserDefaults(suiteName: suite)!, ownerAccountIdB64: "test-owner")
        let pin = second.readPin(accountIdB64: acct)
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, "PUBKEY_PERSIST")
        XCTAssertEqual(pin?.rotationMarker, 42)
    }

    func testPinStoreMonotonicMarker() {
        let store = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "test-owner")
        let acct = base64Encode(Data(repeating: 0xC3, count: 16))

        // Initial pin at marker 100.
        XCTAssertEqual(
            store.setPin(
                accountIdB64: acct,
                pinnedSignaturePubkeyB64: "PUB_100",
                rotationMarker: 100
            ),
            .set
        )

        // Forward advance to 200 — allowed.
        XCTAssertEqual(
            store.setPin(
                accountIdB64: acct,
                pinnedSignaturePubkeyB64: "PUB_200",
                rotationMarker: 200
            ),
            .set
        )
        XCTAssertEqual(store.readPin(accountIdB64: acct)?.rotationMarker, 200)

        // Same marker (idempotent re-observation) — allowed.
        XCTAssertEqual(
            store.setPin(
                accountIdB64: acct,
                pinnedSignaturePubkeyB64: "PUB_200B",
                rotationMarker: 200
            ),
            .set
        )

        // Backward to 150 — REFUSED, pin untouched.
        XCTAssertEqual(
            store.setPin(
                accountIdB64: acct,
                pinnedSignaturePubkeyB64: "PUB_150",
                rotationMarker: 150
            ),
            .rejectedBackward
        )
        let pin = store.readPin(accountIdB64: acct)
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, "PUB_200B")
        XCTAssertEqual(pin?.rotationMarker, 200)
    }

    func testPinStoreRemove() {
        let store = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "test-owner")
        let acct = base64Encode(Data(repeating: 0xD4, count: 16))
        store.setPin(
            accountIdB64: acct,
            pinnedSignaturePubkeyB64: "PUB",
            rotationMarker: 0
        )
        XCTAssertNotNil(store.readPin(accountIdB64: acct))
        store.removePin(accountIdB64: acct)
        XCTAssertNil(store.readPin(accountIdB64: acct))
        // Remove of an absent entry is a no-op (no crash).
        store.removePin(accountIdB64: acct)
    }

    // MARK: - Sprint 5i: verifySupersessionGate decision table

    /// Build a `ContactsClient` over an isolated pin suite so the
    /// verify tests don't share state.
    private func clientWithFreshPins() -> (ContactsClient, PeerPinStore) {
        let store = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "test-owner")
        let client = ContactsClient(pinStore: store)
        return (client, store)
    }

    private func bundle(
        sigPubB64: String,
        x25519B64: String = base64Encode(Data(repeating: 0x09, count: 32)),
        proofB64: String? = nil
    ) -> IdentityFetchResponse {
        IdentityFetchResponse(
            signaturePubkey: sigPubB64,
            x25519Pubkey: x25519B64,
            supersessionProof: proofB64
        )
    }

    /// 1. No pin → TOFU-pin, no throw, marker 0.
    func testVerifyTofuOnFirstSight() throws {
        let (client, store) = clientWithFreshPins()
        let accountId = Data(repeating: 0x11, count: 16)
        let pubB64 = try sigPubB64(forSeed: seed(1))

        try client.verifySupersessionGate(
            accountId: accountId,
            bundle: bundle(sigPubB64: pubB64)
        )

        let pin = store.readPin(accountIdB64: base64Encode(accountId))
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, pubB64)
        XCTAssertEqual(pin?.rotationMarker, 0)
    }

    /// 2. Pin equals fetched key → no-op, no throw.
    func testVerifyUnchangedKeyIsNoop() throws {
        let (client, _) = clientWithFreshPins()
        let accountId = Data(repeating: 0x22, count: 16)
        let pubB64 = try sigPubB64(forSeed: seed(2))
        let b = bundle(sigPubB64: pubB64)

        try client.verifySupersessionGate(accountId: accountId, bundle: b)
        // Second call with the same key must not throw either.
        XCTAssertNoThrow(
            try client.verifySupersessionGate(accountId: accountId, bundle: b)
        )
    }

    /// 3. Changed key + VALID proof → re-pins, marker advances.
    func testVerifyChangedKeyWithValidProofRepins() throws {
        let (client, store) = clientWithFreshPins()
        let accountId = Data(repeating: 0x33, count: 16)
        let acctB64 = base64Encode(accountId)
        let oldExport = seed(10)
        let newExport = seed(11)
        let oldPubB64 = try sigPubB64(forSeed: oldExport)
        let newPubB64 = try sigPubB64(forSeed: newExport)
        let expires = nowSecs + 14 * 24 * 60 * 60

        // TOFU the OLD key first.
        try client.verifySupersessionGate(
            accountId: accountId,
            bundle: bundle(sigPubB64: oldPubB64)
        )

        // Now fetch the NEW key carrying a valid proof.
        let proof = try mintProof(
            oldSeed: oldExport,
            newSeed: newExport,
            issued: nowSecs,
            expires: expires
        )
        try client.verifySupersessionGate(
            accountId: accountId,
            bundle: bundle(sigPubB64: newPubB64, proofB64: base64Encode(proof))
        )

        let pin = store.readPin(accountIdB64: acctB64)
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, newPubB64)
        XCTAssertEqual(pin?.rotationMarker, expires)
    }

    /// 3a. Changed key + ABSENT proof → fail-closed `.proofAbsent`,
    /// pin untouched.
    func testVerifyChangedKeyWithoutProofFailsClosed() throws {
        let (client, store) = clientWithFreshPins()
        let accountId = Data(repeating: 0x44, count: 16)
        let acctB64 = base64Encode(accountId)
        let oldPubB64 = try sigPubB64(forSeed: seed(20))
        let newPubB64 = try sigPubB64(forSeed: seed(21))

        try client.verifySupersessionGate(
            accountId: accountId,
            bundle: bundle(sigPubB64: oldPubB64)
        )

        XCTAssertThrowsError(
            try client.verifySupersessionGate(
                accountId: accountId,
                bundle: bundle(sigPubB64: newPubB64, proofB64: nil)
            )
        ) { error in
            guard let refused = error as? SupersessionRefused else {
                return XCTFail("expected SupersessionRefused, got \(error)")
            }
            XCTAssertEqual(refused.reason, .proofAbsent)
        }
        // Pin must still point at the OLD key.
        XCTAssertEqual(
            store.readPin(accountIdB64: acctB64)?.pinnedSignaturePubkeyB64,
            oldPubB64
        )
    }

    /// 3b. Changed key + proof minted under the WRONG old key →
    /// fail-closed `.proofInvalid` (the verifier rejects the
    /// old-key mismatch), pin untouched.
    func testVerifyChangedKeyWithWrongOldKeyProofFailsClosed() throws {
        let (client, store) = clientWithFreshPins()
        let accountId = Data(repeating: 0x55, count: 16)
        let acctB64 = base64Encode(accountId)
        let realOldExport = seed(30)
        let unrelatedExport = seed(31)
        let newExport = seed(32)
        let realOldPubB64 = try sigPubB64(forSeed: realOldExport)
        let newPubB64 = try sigPubB64(forSeed: newExport)

        // TOFU the REAL old key.
        try client.verifySupersessionGate(
            accountId: accountId,
            bundle: bundle(sigPubB64: realOldPubB64)
        )

        // Proof signed by an UNRELATED key, not the pinned one.
        let badProof = try mintProof(
            oldSeed: unrelatedExport,
            newSeed: newExport,
            issued: nowSecs,
            expires: nowSecs + 14 * 24 * 60 * 60
        )
        XCTAssertThrowsError(
            try client.verifySupersessionGate(
                accountId: accountId,
                bundle: bundle(
                    sigPubB64: newPubB64,
                    proofB64: base64Encode(badProof)
                )
            )
        ) { error in
            guard let refused = error as? SupersessionRefused else {
                return XCTFail("expected SupersessionRefused, got \(error)")
            }
            XCTAssertEqual(refused.reason, .proofInvalid)
        }
        XCTAssertEqual(
            store.readPin(accountIdB64: acctB64)?.pinnedSignaturePubkeyB64,
            realOldPubB64
        )
    }

    /// 3c. Changed key + a proof that VERIFIES against the pinned old
    /// key but authorises a THIRD key (not the served one) →
    /// fail-closed `.keyMismatch`. A malicious server splices a
    /// valid proof onto a substituted served key.
    func testVerifyChangedKeyProofAuthorisesDifferentKeyFailsClosed() throws {
        let (client, store) = clientWithFreshPins()
        let accountId = Data(repeating: 0x66, count: 16)
        let acctB64 = base64Encode(accountId)
        let oldExport = seed(40)
        let realNewExport = seed(41)
        let servedExport = seed(42) // a third, unrelated key
        let oldPubB64 = try sigPubB64(forSeed: oldExport)
        let servedPubB64 = try sigPubB64(forSeed: servedExport)

        try client.verifySupersessionGate(
            accountId: accountId,
            bundle: bundle(sigPubB64: oldPubB64)
        )

        // Proof old→realNew (valid against the pinned old key), but
        // the bundle serves a DIFFERENT (servedExport) key.
        let proof = try mintProof(
            oldSeed: oldExport,
            newSeed: realNewExport,
            issued: nowSecs,
            expires: nowSecs + 14 * 24 * 60 * 60
        )
        XCTAssertThrowsError(
            try client.verifySupersessionGate(
                accountId: accountId,
                bundle: bundle(
                    sigPubB64: servedPubB64,
                    proofB64: base64Encode(proof)
                )
            )
        ) { error in
            guard let refused = error as? SupersessionRefused else {
                return XCTFail("expected SupersessionRefused, got \(error)")
            }
            XCTAssertEqual(refused.reason, .keyMismatch)
        }
        XCTAssertEqual(
            store.readPin(accountIdB64: acctB64)?.pinnedSignaturePubkeyB64,
            oldPubB64
        )
    }

    /// 3d. Garbage (non-base64) proof on a changed key → fail-closed
    /// `.proofInvalid`.
    func testVerifyChangedKeyWithMalformedProofFailsClosed() throws {
        let (client, _) = clientWithFreshPins()
        let accountId = Data(repeating: 0x77, count: 16)
        let oldPubB64 = try sigPubB64(forSeed: seed(50))
        let newPubB64 = try sigPubB64(forSeed: seed(51))

        try client.verifySupersessionGate(
            accountId: accountId,
            bundle: bundle(sigPubB64: oldPubB64)
        )

        XCTAssertThrowsError(
            try client.verifySupersessionGate(
                accountId: accountId,
                bundle: bundle(
                    sigPubB64: newPubB64,
                    proofB64: "!!!not base64!!!"
                )
            )
        ) { error in
            guard let refused = error as? SupersessionRefused else {
                return XCTFail("expected SupersessionRefused, got \(error)")
            }
            XCTAssertEqual(refused.reason, .proofInvalid)
        }
    }

    /// The marker advances to the proof's own `expires_at` — verify
    /// the offset-73 big-endian read matches the value we signed.
    func testReadProofExpiresAtMatchesSignedValue() throws {
        let oldExport = seed(60)
        let newExport = seed(61)
        let expires = nowSecs + 9 * 24 * 60 * 60
        let proof = try mintProof(
            oldSeed: oldExport,
            newSeed: newExport,
            issued: nowSecs,
            expires: expires
        )
        let parsed = try ContactsClient.readProofExpiresAtSecs(proof)
        XCTAssertEqual(parsed, expires)
    }
}
