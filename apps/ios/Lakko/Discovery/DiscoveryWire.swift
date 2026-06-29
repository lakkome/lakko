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
    /// G-4 (ADR-0016): base64 of an optional old-key-signed supersession
    /// proof bridging an AccountIdentity rotation. `nil` for a normal upload
    /// — its absence clears any stored proof server-side.
    public let supersessionProof: String?
    /// G-30 #2 (ADR-0019): base64 of an optional client leaf authorisation
    /// (a genesis / fresh-start self-signature over the `(account_key,
    /// pubkey)` binding) so the transparency append can verify the account
    /// authorised a NEW log entry. `nil` for a same-key re-publish (the
    /// append is idempotent and needs no authorisation). Opaque to the
    /// server.
    public let leafAuth: String?

    public init(
        pow: PowProofPayload,
        accountId: String,
        signaturePubkey: String,
        x25519Pubkey: String,
        supersessionProof: String? = nil,
        leafAuth: String? = nil
    ) {
        self.pow = pow
        self.accountId = accountId
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
        self.supersessionProof = supersessionProof
        self.leafAuth = leafAuth
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case accountId = "account_id"
        case signaturePubkey = "signature_pubkey"
        case x25519Pubkey = "x25519_pubkey"
        case supersessionProof = "supersession_proof"
        case leafAuth = "leaf_auth"
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
    /// G-4 (ADR-0016): base64 of the stored supersession proof when the
    /// account rotated recently; `nil` otherwise. Verified locally by the
    /// peer — the server passes the bytes through opaquely.
    public let supersessionProof: String?

    public init(
        signaturePubkey: String,
        x25519Pubkey: String,
        supersessionProof: String? = nil
    ) {
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
        self.supersessionProof = supersessionProof
    }

    enum CodingKeys: String, CodingKey {
        case signaturePubkey = "signature_pubkey"
        case x25519Pubkey = "x25519_pubkey"
        case supersessionProof = "supersession_proof"
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

/// Body of `POST /discovery/group-invite` (ADR-0028, G-3). Mirror of
/// ``InviteUploadRequest`` but carries an extra `max_uses`: a
/// `GroupInviteCapability` does NOT embed `max_uses` in its signed wire (it
/// conveys only `group_id` + `issuer_pubkey` + `expiry_ms`), so the uploader
/// supplies the SOFT consume ceiling out-of-band. It is an anti-spam throttle,
/// not an access control — the security boundary is the admin approval gate
/// (ADR-0027). The response reuses ``InviteUploadResponse`` (`blob_id`).
public struct GroupInviteUploadRequest: Codable, Equatable {
    public let pow: PowProofPayload
    /// Standard-alphabet base64 of the `composeGroupInviteCapability(...)`
    /// output. Server runs `decode_and_verify` at ingress + caps decoded size
    /// at 512 bytes.
    public let capability: String
    /// SOFT consume ceiling — `0` = single-use, `0xFFFFFFFF` = unlimited, any
    /// other N = at most N consumes. NOT part of the signed capability.
    public let maxUses: UInt32

    public init(pow: PowProofPayload, capability: String, maxUses: UInt32) {
        self.pow = pow
        self.capability = capability
        self.maxUses = maxUses
    }

    enum CodingKeys: String, CodingKey {
        case pow
        case capability
        case maxUses = "max_uses"
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

/// Body of `POST /discovery/invite/by-owner` (Phase
/// 1.18d2b-list). The owner signs the canonical bytes
/// (`OWNER_LIST_DOMAIN_V1` ‖ `owner_account_id` ‖
/// `signed_at_ms` BE) with their identity Ed25519 key; the
/// server resolves the verifying key from
/// `identity_bundles.signature_pubkey` for the given
/// `account_id` and verifies. The session-token middleware on
/// the route is a DoS gate, not the ownership proof.
public struct InviteOwnerListRequest: Codable, Equatable {
    /// Standard-alphabet base64 of the owner's 16-byte UUIDv7
    /// `account_id`.
    public let ownerAccountId: String
    /// UNIX millis at which the body was signed. Same ±10 min
    /// replay window as the revoke endpoint.
    public let signedAtMs: Int64
    /// Standard-alphabet base64 of the 64-byte Ed25519 signature
    /// from `signOwnerListQuery`.
    public let signature: String

    public init(ownerAccountId: String, signedAtMs: Int64, signature: String) {
        self.ownerAccountId = ownerAccountId
        self.signedAtMs = signedAtMs
        self.signature = signature
    }

    enum CodingKeys: String, CodingKey {
        case ownerAccountId = "owner_account_id"
        case signedAtMs = "signed_at_ms"
        case signature
    }
}

/// One entry in the `/discovery/invite/by-owner` response —
/// wire-shape mirror of the server's `OwnerListEntry`. The
/// camelCase `Codable` properties remap to the snake_case JSON
/// the server emits.
public struct InviteOwnerListEntry: Codable, Equatable {
    public let blobIdSlug: String
    public let expiryMs: UInt64
    public let maxUses: UInt32
    public let usedCount: UInt32
    public let label: String
    public let bootstrapReferral: Bool
    public let createdAt: String

    public init(
        blobIdSlug: String,
        expiryMs: UInt64,
        maxUses: UInt32,
        usedCount: UInt32,
        label: String,
        bootstrapReferral: Bool,
        createdAt: String
    ) {
        self.blobIdSlug = blobIdSlug
        self.expiryMs = expiryMs
        self.maxUses = maxUses
        self.usedCount = usedCount
        self.label = label
        self.bootstrapReferral = bootstrapReferral
        self.createdAt = createdAt
    }

    enum CodingKeys: String, CodingKey {
        case blobIdSlug = "blob_id_slug"
        case expiryMs = "expiry_ms"
        case maxUses = "max_uses"
        case usedCount = "used_count"
        case label
        case bootstrapReferral = "bootstrap_referral"
        case createdAt = "created_at"
    }
}

/// Wire-level response shape from
/// `POST /discovery/invite/by-owner`.
public struct InviteOwnerListResponse: Codable, Equatable {
    public let invites: [InviteOwnerListEntry]

    public init(invites: [InviteOwnerListEntry]) {
        self.invites = invites
    }
}
