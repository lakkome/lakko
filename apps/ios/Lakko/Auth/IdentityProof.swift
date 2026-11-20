// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0098 security fix F3 — the proof that this device holds the account
/// identity key it asks auth to record.
///
/// Every auth call that records the account's identity key (`POST
/// /auth/genesis/attest`, `POST /account/keys/reset/finish`, `POST
/// /account/keys/rotate/finish`, `POST /account/recall/finish`) carries
/// `identity_proof_b64`: `signIdentityProof(identity, accountId)`, an
/// Ed25519 signature BY THE KEY BEING RECORDED over
/// `"lakko/auth/identity-proof/v1" ‖ 0x00 ‖ account_id ‖ pubkey`. Auth
/// refuses the call without it (`400 identity-proof-invalid`), and a key
/// another account ever held (`409 identity-key-taken`).
public enum IdentityProof {

    /// The two refusals auth answers on the four calls; both take the
    /// existing generic failure path, and only the code is logged.
    public static let invalidCode = "identity-proof-invalid"
    public static let keyTakenCode = "identity-key-taken"

    /// The proof length (an Ed25519 signature).
    public static let length = 64

    /// `identity_proof_b64` (URL-safe base64, no pad) for the identity
    /// seeded by `seed` (the 64-byte phrase seed) on `accountId`. Throws
    /// ``AuthClientError/argument(_:)`` when the seeded key is not
    /// `expectedPubkey` (the proof must be by the key being recorded).
    public static func b64(seed: Data, accountId: Data, expectedPubkey: Data) throws -> String {
        guard accountId.count == 16 else {
            throw AuthClientError.argument("accountId must be 16 bytes")
        }
        let identity = try Identity.fromSeed(seed: seed)
        guard identity.signaturePublicKey() == expectedPubkey else {
            throw AuthClientError.argument("the identity proof must be by the key being recorded")
        }
        return try b64(identity: identity, accountId: accountId)
    }

    /// `identity_proof_b64` signed by `identity` over `accountId`.
    public static func b64(identity: Identity, accountId: Data) throws -> String {
        let proof = try signIdentityProof(accountIdentity: identity, accountId: accountId)
        guard proof.count == length else {
            throw AuthClientError.argument("identity proof must be 64 bytes")
        }
        return base64UrlEncodeNoPad(proof)
    }

    /// The error code of an auth refusal on one of the four calls when it is
    /// one of the two above, else `nil`. Used for logging only.
    public static func refusalCode(of error: Error) -> String? {
        guard case AuthClientError.http(let status, _, let body) = error,
              status == 400 || status == 409,
              let data = body.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let code = object["error"] as? String,
              code == invalidCode || code == keyTakenCode else { return nil }
        return code
    }
}
