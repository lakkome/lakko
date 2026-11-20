// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0089 security fix F3 — every auth call that records the account's
/// identity key carries `identity_proof_b64`, an Ed25519 signature by the
/// key being recorded over `"lakko/auth/identity-proof/v1" ‖ 0x00 ‖
/// account_id ‖ pubkey`.
///
/// The genesis call is run end to end against a stub. The reset, rotate and
/// recall finishes need a real OPAQUE server answer to reach their finish
/// call, which no stub can give; for those the tests show that the proof is
/// made from the key being recorded before any request (a seed whose key is
/// not that key sends nothing), and that the finish body carries the field.
final class IdentityProofTests: XCTestCase {

    private let accountId = Data((0..<16).map { UInt8($0 + 1) })
    private let seed = Data(repeating: 0x5A, count: 64)
    private let otherSeed = Data(repeating: 0x6B, count: 64)

    private func pubkey(_ seed: Data) throws -> Data {
        try Identity.fromSeed(seed: seed).signaturePublicKey()
    }

    /// Verify a base64url proof the way auth does (`verify_identity_proof`).
    private func verifies(_ proofB64: String?, pubkey: Data, accountId: Data) throws -> Bool {
        guard let proofB64, let proof = base64UrlDecodeNoPad(proofB64) else { return false }
        var message = Data("lakko/auth/identity-proof/v1".utf8)
        message.append(0)
        message.append(accountId)
        message.append(pubkey)
        let key = try Curve25519.Signing.PublicKey(rawRepresentation: pubkey)
        return key.isValidSignature(proof, for: message)
    }

    // MARK: - The proof

    func testTheProofIsByTheRecordedKeyOverTheAccountAndKey() throws {
        let key = try pubkey(seed)
        let proof = try IdentityProof.b64(seed: seed, accountId: accountId, expectedPubkey: key)
        XCTAssertEqual(base64UrlDecodeNoPad(proof)?.count, 64)
        XCTAssertTrue(try verifies(proof, pubkey: key, accountId: accountId))
        let otherAccount = Data(repeating: 0xEE, count: 16)
        XCTAssertFalse(try verifies(proof, pubkey: key, accountId: otherAccount), "bound to the account")
        XCTAssertFalse(try verifies(proof, pubkey: try pubkey(otherSeed), accountId: accountId),
                       "bound to the key")
    }

    func testAProofByAnotherKeyThanTheRecordedOneIsRefused() throws {
        XCTAssertThrowsError(try IdentityProof.b64(
            seed: otherSeed, accountId: accountId, expectedPubkey: try pubkey(seed)))
        XCTAssertThrowsError(try IdentityProof.b64(
            seed: seed, accountId: Data(repeating: 1, count: 15), expectedPubkey: try pubkey(seed)))
    }

    func testTheTwoRefusalCodesAreRecognisedAndNothingElse() {
        XCTAssertEqual(IdentityProof.refusalCode(of: AuthClientError.http(
            status: 400, endpoint: "/auth/genesis/attest", body: #"{"error":"identity-proof-invalid"}"#)),
            "identity-proof-invalid")
        XCTAssertEqual(IdentityProof.refusalCode(of: AuthClientError.http(
            status: 409, endpoint: "/account/keys/reset/finish", body: #"{"error":"identity-key-taken"}"#)),
            "identity-key-taken")
        XCTAssertNil(IdentityProof.refusalCode(of: AuthClientError.http(
            status: 409, endpoint: "/auth/genesis/attest", body: #"{"error":"identity-key-mismatch"}"#)))
        XCTAssertNil(IdentityProof.refusalCode(of: URLError(.timedOut)))
    }

    /// Both refusals take the genesis path's existing generic handling: a
    /// `400` is an unusable key, a `409 identity-key-taken` is tried again at
    /// the next landing; neither is recorded.
    func testTheGenesisRefusalsTakeTheExistingPaths() {
        XCTAssertEqual(IdentityKeyRecord.classify(status: 400, body: #"{"error":"identity-proof-invalid"}"#),
                       .unusableKey)
        XCTAssertEqual(IdentityKeyRecord.classify(status: 409, body: #"{"error":"identity-key-taken"}"#),
                       .retryLater)
    }

    // MARK: - The four calls

    func testTheGenesisCallSendsAProofThatVerifies() async throws {
        let host = NameRouteStub.scope { _, path, _ in
            path == "/auth/genesis/attest" ? (200, #"{"genesis_attest_b64":"AAAA"}"#) : (503, "")
        }
        let identity = try Identity.fromSeed(seed: seed)
        _ = try await NameRouteStub.client(host).mintGenesisAttestation(
            sessionToken: "tok", identity: identity, accountId: accountId)
        let request = try XCTUnwrap(NameRouteStub.requests(host).first { $0.path == "/auth/genesis/attest" })
        let key = try pubkey(seed)
        XCTAssertEqual(request.json["pubkey_b64"] as? String, base64UrlEncodeNoPad(key))
        XCTAssertTrue(try verifies(request.json["identity_proof_b64"] as? String, pubkey: key, accountId: accountId))
    }

    func testTheResetIsRefusedBeforeAnyRequestWhenTheSeedIsNotTheRecordedKey() async throws {
        let host = NameRouteStub.scope { _, _, _ in (503, "") }
        do {
            _ = try await NameRouteStub.client(host).resetAccountKeys(
                sessionToken: "tok", freshGrant: "g", seed: otherSeed,
                newSignaturePubkey: try pubkey(seed), accountId: accountId)
            XCTFail("a proof by another key is never sent")
        } catch AuthClientError.argument {}
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty)

        // With the matching seed the proof is made and the flow starts.
        _ = try? await NameRouteStub.client(host).resetAccountKeys(
            sessionToken: "tok", freshGrant: "g", seed: seed,
            newSignaturePubkey: try pubkey(seed), accountId: accountId)
        XCTAssertEqual(NameRouteStub.requests(host).map(\.path), ["/account/keys/reset/start"])
    }

    func testTheRotationIsRefusedBeforeAnyRequestWhenTheSeedIsNotTheRecordedKey() async throws {
        let host = NameRouteStub.scope { _, _, _ in (503, "") }
        do {
            _ = try await NameRouteStub.client(host).rotateAccountKeys(
                sessionToken: "tok", freshGrant: "g", seed: otherSeed,
                newSignaturePubkey: try pubkey(seed), supersessionProof: Data(repeating: 1, count: 145),
                revokeDeviceIds: [], accountId: accountId)
            XCTFail("a proof by another key is never sent")
        } catch AuthClientError.argument {}
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty)

        _ = try? await NameRouteStub.client(host).rotateAccountKeys(
            sessionToken: "tok", freshGrant: "g", seed: seed,
            newSignaturePubkey: try pubkey(seed), supersessionProof: Data(repeating: 1, count: 145),
            revokeDeviceIds: [], accountId: accountId)
        XCTAssertEqual(NameRouteStub.requests(host).map(\.path), ["/account/keys/rotate/start"])
    }

    func testTheRecallIsRefusedBeforeAnyRequestWhenTheSeedIsNotTheRecordedKey() async throws {
        let host = NameRouteStub.scope { _, _, _ in (503, "") }
        let record = RecallRecord(
            accountIdB64: base64Encode(accountId), oldSeed: Data(repeating: 7, count: 32),
            username: "alice", learnedAtMs: 1_000)
        func attempt(_ newSeed: Data) throws -> RecallAttempt {
            RecallAttempt(
                record: record, anchorSeq: 3, oldPassword: Data("old".utf8), newPassword: Data("new".utf8),
                newSeed: newSeed, newSignaturePubkey: try pubkey(seed), deviceId: nil)
        }
        do {
            _ = try await NameRouteStub.client(host).recallAccount(try attempt(otherSeed))
            XCTFail("a proof by another key is never sent")
        } catch AuthClientError.argument {}
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty)

        _ = try? await NameRouteStub.client(host).recallAccount(try attempt(seed))
        XCTAssertEqual(NameRouteStub.requests(host).first?.path, "/account/recall/start")
    }

    func testEveryFinishBodyCarriesTheProof() throws {
        func json<T: Encodable>(_ value: T) throws -> [String: Any] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(value)) as? [String: Any])
        }
        let bodies: [[String: Any]] = [
            try json(GenesisAttestRequest(pubkeyB64: "pk", identityProofB64: "ip")),
            try json(KeyResetFinishRequest(
                freshGrant: "g", recoveryUpload: "u", newSignaturePubkeyB64: "pk", identityProofB64: "ip")),
            try json(KeyRotateFinishRequest(
                freshGrant: "g", recoveryUpload: "u", newSignaturePubkeyB64: "pk", supersessionProofB64: "p",
                revokeDeviceIdsB64: [], identityProofB64: "ip")),
            try json(RecallFinishRequest(
                sessionId: "s", accountIdB64: "a", anchorSeq: 1, loginFinish: "l", passwordUpload: "p",
                recoveryUpload: "r", newSignaturePubkeyB64: "pk", deviceIdB64: nil, statementB64: "st",
                identityProofB64: "ip")),
        ]
        for body in bodies {
            XCTAssertEqual(body["identity_proof_b64"] as? String, "ip")
        }
    }
}
