// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Swift-side mirror of the JSON shapes consumed and produced by
/// `lakko-svc-discovery`.
///
/// Mirror of:
///
///   * `services/discovery/src/{oprf,registry,identity_bundle,keypackage}.rs`
///     (server-side originals)
///   * `apps/web/src/discovery/wire.ts` (1.17e3 + 1.17e6 invite addendum)
///   * `apps/android/.../discovery/Wire.kt` (1.15e3 + 1.15e9 invite addendum)
///
/// **Phase 1.14e1 scope**: OPRF + register + lookup + identity
/// bundle (upload + fetch) + KeyPackage (upload + fetch). The
/// invite codec endpoints land with 1.14e6.
///
/// Reuses ``PowProofPayload`` from `apps/ios/Lakko/Auth/Wire.swift`
/// — the wire shape is byte-identical to what the auth service
/// expects (it's the same Equix proof carried by every expensive
/// endpoint). The **server** keeps its PoW challenge table separate
/// (`services/discovery` has its own `/pow/challenge`); the JSON
/// shape is identical so the Swift struct is shared.

// ----------------------------------------------------------------------------
// /discovery/oprf  (services/discovery/src/oprf.rs)
// ----------------------------------------------------------------------------

/// Body of `POST /discovery/oprf`.
public struct OprfRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// base64(BlindedElement<Ristretto255-SHA512>) — produced by
    /// `OprfBlinder.blind(username).blindedB64()`.
    public let blinded: String

    public init(pow: PowProofPayload, blinded: String) {
        self.pow = pow
        self.blinded = blinded
    }
}

/// Response body of `POST /discovery/oprf`.
public struct OprfResponse: Codable, Equatable {
    /// base64(EvaluationElement). Feed to `blinder.finalize(evaluated:)`.
    public let evaluated: String

    public init(evaluated: String) {
        self.evaluated = evaluated
    }
}

// ----------------------------------------------------------------------------
// /discovery/register  (services/discovery/src/registry.rs)
// ----------------------------------------------------------------------------

/// Body of `POST /discovery/register`.
public struct RegisterRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// base64(account_id) — raw 16 bytes.
    public let accountId: String
    /// base64(VOPRF output) — exactly 64 bytes, the OPRF hash the
    /// client just computed via `blinder.finalize(evaluated:)`.
    public let oprfHash: String

    public init(pow: PowProofPayload, accountId: String, oprfHash: String) {
        self.pow = pow
        self.accountId = accountId
        self.oprfHash = oprfHash
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case oprfHash = "oprf_hash"
    }
}

/// Response body of `POST /discovery/register`. Server returns 409
/// on username collision; that surfaces as
/// ``DiscoveryClientError/http(status:endpoint:body:)`` with
/// `status = 409`.
public struct RegisterResponse: Codable, Equatable {
    public let ok: Bool

    public init(ok: Bool) {
        self.ok = ok
    }
}

// ----------------------------------------------------------------------------
// /discovery/lookup  (services/discovery/src/registry.rs)
// ----------------------------------------------------------------------------

/// Body of `POST /discovery/lookup`.
public struct LookupRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// Same shape as ``RegisterRequest/oprfHash``.
    public let oprfHash: String

    public init(pow: PowProofPayload, oprfHash: String) {
        self.pow = pow
        self.oprfHash = oprfHash
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case oprfHash = "oprf_hash"
    }
}

/// Response body of `POST /discovery/lookup`. 404 (username
/// unknown) is folded into a `nil` in
/// ``DiscoveryClient/lookupRaw(_:)``; this type only describes
/// the 200-OK path.
public struct LookupResponse: Codable, Equatable {
    public let accountId: String

    public init(accountId: String) {
        self.accountId = accountId
    }

    enum CodingKeys: String, CodingKey {
        case accountId = "account_id"
    }
}

// ----------------------------------------------------------------------------
// /discovery/identity   (services/discovery/src/identity_bundle.rs)
// ----------------------------------------------------------------------------

/// Body of `POST /discovery/identity`.
public struct IdentityUploadRequest: Codable, Equatable {
    public let pow: PowProofPayload
    public let accountId: String
    /// base64(Ed25519 public key) — 32 bytes after decode.
    public let signaturePubkey: String
    /// base64(X25519 public key) — 32 bytes after decode.
    public let x25519Pubkey: String

    public init(
        pow: PowProofPayload,
        accountId: String,
        signaturePubkey: String,
        x25519Pubkey: String
    ) {
        self.pow = pow
        self.accountId = accountId
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case signaturePubkey = "signature_pubkey"
        case x25519Pubkey = "x25519_pubkey"
    }
}

/// Response body of `POST /discovery/identity`.
public struct IdentityUploadResponse: Codable, Equatable {
    public let ok: Bool

    public init(ok: Bool) {
        self.ok = ok
    }
}

/// Response body of `GET /discovery/identity/{accountId_b64url}`.
/// 404 surfaces as `nil` in ``DiscoveryClient/identityFetch(accountId:)``.
public struct IdentityFetchResponse: Codable, Equatable {
    public let signaturePubkey: String
    public let x25519Pubkey: String

    public init(signaturePubkey: String, x25519Pubkey: String) {
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
    }

    enum CodingKeys: String, CodingKey {
        case signaturePubkey = "signature_pubkey"
        case x25519Pubkey = "x25519_pubkey"
    }
}

// ----------------------------------------------------------------------------
// /discovery/keypackage   (services/discovery/src/keypackage.rs)
// ----------------------------------------------------------------------------

/// Body of `POST /discovery/keypackage`.
public struct KeyPackageUploadRequest: Codable, Equatable {
    public let pow: PowProofPayload
    public let accountId: String
    /// base64(TLS-encoded MLS KeyPackage). Server caps decoded size
    /// at 2048 bytes — mirrors `MAX_KEYPACKAGE_BYTES` in the Rust
    /// handler.
    public let keypackage: String

    public init(pow: PowProofPayload, accountId: String, keypackage: String) {
        self.pow = pow
        self.accountId = accountId
        self.keypackage = keypackage
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case keypackage
    }
}

/// Response body of `POST /discovery/keypackage`.
public struct KeyPackageUploadResponse: Codable, Equatable {
    public let ok: Bool

    public init(ok: Bool) {
        self.ok = ok
    }
}

/// Response body of `GET /discovery/keypackage/{accountId_b64url}`.
/// 404 surfaces as `nil` in
/// ``DiscoveryClient/keypackageFetch(accountId:)``.
public struct KeyPackageFetchResponse: Codable, Equatable {
    public let keypackage: String

    public init(keypackage: String) {
        self.keypackage = keypackage
    }
}

// ----------------------------------------------------------------------------
// /discovery/invite   (services/discovery/src/invite.rs — Phase 1.18d1)
// ----------------------------------------------------------------------------
// Added in 1.14e6, parallel to the Android 1.15e9 addendum.

/// Body of `POST /discovery/invite`.
public struct InviteUploadRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// Standard-alphabet base64 of the `composeInviteCapability(...)`
    /// output. Server caps decoded size at 2048 bytes (mirrored
    /// client-side in ``DiscoveryClient/uploadInvite(capabilityBytes:)``
    /// so a misbehaving caller fails fast).
    public let capability: String

    public init(pow: PowProofPayload, capability: String) {
        self.pow = pow
        self.capability = capability
    }
}

/// Response body of `POST /discovery/invite`.
public struct InviteUploadResponse: Codable, Equatable {
    /// base64url-no-pad of `SHA-256(capability_bytes)` — same bytes
    /// ⇒ same id; a retried upload is a no-op (server-side upsert).
    public let blobId: String

    public init(blobId: String) {
        self.blobId = blobId
    }

    enum CodingKeys: String, CodingKey {
        case blobId = "blob_id"
    }
}

/// Response body of `GET /discovery/invite/{blob_id_b64url}`. 404
/// surfaces as `nil` in ``DiscoveryClient/inviteFetch(blobId:)``
/// (server-side expiry filter folds expired-blob into the same
/// status).
public struct InviteFetchResponse: Codable, Equatable {
    public let capability: String

    public init(capability: String) {
        self.capability = capability
    }
}

/// Body of `POST /discovery/invite/{blob_id_b64url}/consume`
/// (Phase 1.18d3b mandatory shape).
public struct InviteConsumeRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// Standard-alphabet base64 of the 32-byte joiner-tag from
    /// `computeJoinerInviteTag`.
    public let joinerTag: String

    public init(pow: PowProofPayload, joinerTag: String) {
        self.pow = pow
        self.joinerTag = joinerTag
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case joinerTag = "joiner_tag"
    }
}

/// Response body of `POST /discovery/invite/{blob_id_b64url}/consume`.
public struct InviteConsumeResponse: Codable, Equatable {
    /// Post-increment counter. UI surfaces "N of MAX uses" copy
    /// when the invite has a finite `max_uses`. When
    /// `alreadyConsumed` is `true` the counter was NOT bumped —
    /// the value reflects pre-call state.
    public let usedCount: Int
    /// Echo of the row's `max_uses`. Sentinel mapping preserved:
    /// `0` = single-use, `0xFFFFFFFF` = unlimited, any other N =
    /// N-use ceiling.
    public let maxUses: Int64
    /// `true` when the side-table PK on `(blob_id, joiner_tag)`
    /// already had a row — i.e. this joiner has consumed the
    /// invite before (Phase 1.18d3b). The handler returns 200 with
    /// the counter unchanged; the UI surfaces "you've already
    /// accepted this invite" + Decline-only and skips the
    /// ContactRequest send.
    public let alreadyConsumed: Bool

    public init(usedCount: Int, maxUses: Int64, alreadyConsumed: Bool) {
        self.usedCount = usedCount
        self.maxUses = maxUses
        self.alreadyConsumed = alreadyConsumed
    }

    enum CodingKeys: String, CodingKey {
        case usedCount = "used_count"
        case maxUses = "max_uses"
        case alreadyConsumed = "already_consumed"
    }
}

/// Body of `DELETE /discovery/invite/{blob_id_b64url}` (Phase
/// 1.18d2b). The `signature` field carries proof that the request
/// came from the same Ed25519 key that signed the capability.
public struct InviteRevokeRequest: Codable, Equatable {
    /// UNIX millis at which the body was signed. Must be within
    /// `±REVOKE_REPLAY_WINDOW_MS` (10 min) of the server clock or
    /// the request is rejected with 400.
    public let signedAtMs: Int64
    /// Standard-alphabet base64 of the 64-byte Ed25519 signature
    /// from `signInviteRevocation`.
    public let signature: String

    public init(signedAtMs: Int64, signature: String) {
        self.signedAtMs = signedAtMs
        self.signature = signature
    }

    enum CodingKeys: String, CodingKey {
        case signedAtMs = "signed_at_ms"
        case signature
    }
}
