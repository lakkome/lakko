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
/// No body carries a proof of work since ADR-0054 S3: the anonymous routes
/// (`/discovery/oprf`, `/discovery/channel/register`, the three consumes)
/// spend an anonymous token in the `x-lakko-anon-token` header instead, and
/// the bearer routes are counted per account server-side.

// ----------------------------------------------------------------------------
// /discovery/oprf  (services/discovery/src/oprf.rs)
// ----------------------------------------------------------------------------

/// Body of `POST /discovery/oprf`.
public struct OprfRequest: Codable, Equatable {
    /// base64(BlindedElement<Ristretto255-SHA512>) — produced by
    /// `OprfBlinder.blind(username).blindedB64()`.
    public let blinded: String

    public init(blinded: String) {
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
    /// base64(account_id) — raw 16 bytes.
    public let accountId: String
    /// base64(VOPRF output) — exactly 64 bytes, the OPRF hash the
    /// client just computed via `blinder.finalize(evaluated:)`.
    public let oprfHash: String

    public init(accountId: String, oprfHash: String) {
        self.accountId = accountId
        self.oprfHash = oprfHash
    }

    enum CodingKeys: String, CodingKey {
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
    /// Same shape as ``RegisterRequest/oprfHash``.
    public let oprfHash: String

    public init(oprfHash: String) {
        self.oprfHash = oprfHash
    }

    enum CodingKeys: String, CodingKey {
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
    /// ADR-0036 (G-30 finding 2): base64 of an optional auth-attested genesis
    /// proof (`POST /auth/genesis/attest`), binding the OPAQUE-authenticated
    /// `account_id` to THIS identity pubkey so the transparency genesis entry
    /// is account-authenticated (anti-squatting), not merely self-consistent.
    /// `nil` for a non-genesis re-publish (the absence is `COALESCE`-preserved
    /// server-side, never stripping a still-valid stored value). Opaque to the
    /// server — verified operator-side + by the peer on a first observation.
    public let genesisAttest: String?

    public init(
        accountId: String,
        signaturePubkey: String,
        x25519Pubkey: String,
        supersessionProof: String? = nil,
        leafAuth: String? = nil,
        genesisAttest: String? = nil
    ) {
        self.accountId = accountId
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
        self.supersessionProof = supersessionProof
        self.leafAuth = leafAuth
        self.genesisAttest = genesisAttest
    }

    enum CodingKeys: String, CodingKey {
        case accountId = "account_id"
        case signaturePubkey = "signature_pubkey"
        case x25519Pubkey = "x25519_pubkey"
        case supersessionProof = "supersession_proof"
        case leafAuth = "leaf_auth"
        case genesisAttest = "genesis_attest"
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
    public let accountId: String
    /// ADR-0050 — base64 of the device leaf's 32-byte Ed25519 signature key.
    /// Discovery keeps one KeyPackage row per `(account_id, device_id)`.
    public let deviceId: String
    /// base64(TLS-encoded MLS KeyPackage). Server caps decoded size
    /// at 2048 bytes — mirrors `MAX_KEYPACKAGE_BYTES` in the Rust
    /// handler.
    public let keypackage: String

    public init(accountId: String, deviceId: String, keypackage: String) {
        self.accountId = accountId
        self.deviceId = deviceId
        self.keypackage = keypackage
    }

    enum CodingKeys: String, CodingKey {
        case accountId = "account_id"
        case deviceId = "device_id"
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

/// One device's entry in ``KeyPackageFetchResponse`` (ADR-0050).
public struct DeviceKeyPackagePayload: Codable, Equatable {
    /// base64 of the device leaf's 32-byte signature key.
    public let deviceId: String
    /// base64(TLS-encoded MLS KeyPackage).
    public let keypackage: String

    public init(deviceId: String, keypackage: String) {
        self.deviceId = deviceId
        self.keypackage = keypackage
    }

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case keypackage
    }
}

/// Response body of `GET /discovery/keypackage/{accountId_b64url}`: every
/// device of the account with a published KeyPackage, oldest first
/// (ADR-0050). 404 surfaces as `nil` in
/// ``DiscoveryClient/keypackageFetch(accountId:)``.
public struct KeyPackageFetchResponse: Codable, Equatable {
    public let keypackages: [DeviceKeyPackagePayload]

    public init(keypackages: [DeviceKeyPackagePayload]) {
        self.keypackages = keypackages
    }
}

// ----------------------------------------------------------------------------
// /discovery/invite   (services/discovery/src/invite.rs — Phase 1.18d1)
// ----------------------------------------------------------------------------
// Added in 1.14e6, parallel to the Android 1.15e9 addendum.

/// Body of `POST /discovery/invite`, `POST /discovery/call-invite` and
/// `POST /discovery/group-invite` (ADR-0028 amendment 2026-09-28, sealed
/// links). The ledger stores the SEALED blob only; what the server used to
/// read out of the capability arrives in clear fields. `label` and
/// `bootstrapReferral` are sent for `/i/` only (`nil` omits them).
public struct InviteUploadRequest: Codable, Equatable {
    /// Standard-alphabet base64 of the sealed link blob (`InviteLinkMint.sealed`).
    public let sealed: String
    /// Standard-alphabet base64 of the 32-byte Ed25519 key that signed the
    /// capability — the key an issuer-signed revoke verifies against.
    public let issuerPubkey: String
    /// The capability's expiry (`0` = never), for the fetch filter and pruner.
    public let expiryMs: UInt64
    /// The consume ceiling sentinel (`0` single use, `UInt32.max` unlimited).
    public let maxUses: UInt32
    /// `/i/` only: the owner's label for its own list.
    public let label: String?
    /// `/i/` only: the capability's bootstrap-referral flag.
    public let bootstrapReferral: Bool?

    public init(
        sealed: String,
        issuerPubkey: String,
        expiryMs: UInt64,
        maxUses: UInt32,
        label: String? = nil,
        bootstrapReferral: Bool? = nil
    ) {
        self.sealed = sealed
        self.issuerPubkey = issuerPubkey
        self.expiryMs = expiryMs
        self.maxUses = maxUses
        self.label = label
        self.bootstrapReferral = bootstrapReferral
    }

    enum CodingKeys: String, CodingKey {
        case sealed
        case issuerPubkey = "issuer_pubkey"
        case expiryMs = "expiry_ms"
        case maxUses = "max_uses"
        case label
        case bootstrapReferral = "bootstrap_referral"
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(sealed, forKey: .sealed)
        try c.encode(issuerPubkey, forKey: .issuerPubkey)
        try c.encode(expiryMs, forKey: .expiryMs)
        try c.encode(maxUses, forKey: .maxUses)
        try c.encodeIfPresent(label, forKey: .label)
        try c.encodeIfPresent(bootstrapReferral, forKey: .bootstrapReferral)
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
    /// Standard-alphabet base64 of the sealed link blob; opened on the device
    /// with the link's fragment secret (`XCapability.openLink`).
    public let sealed: String

    public init(sealed: String) {
        self.sealed = sealed
    }
}

/// Body of `POST /discovery/invite/{blob_id_b64url}/consume`
/// (Phase 1.18d3b mandatory shape).
public struct InviteConsumeRequest: Codable, Equatable {
    /// Standard-alphabet base64 of the 32-byte joiner-tag from
    /// `computeJoinerInviteTag`.
    public let joinerTag: String

    public init(joinerTag: String) {
        self.joinerTag = joinerTag
    }

    enum CodingKeys: String, CodingKey {
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

// ----------------------------------------------------------------------------
// /discovery/channel/{register,lookup}
//   (services/discovery/src/channel_names.rs — G4-5, ADR-0010)
// ----------------------------------------------------------------------------
// Mirror of `apps/web/src/discovery/wire.ts` (channel section). Same OPRF
// ciphersuite + wire shape as the username `RegisterRequest` / `LookupRequest`
// (Ristretto255-SHA512, 64-byte hash) — the `/discovery/oprf` evaluate endpoint
// is namespace-agnostic, so the client reuses `OprfBlinder.blind(handle)`
// verbatim. Only the second endpoint + the id type (a 16-byte channel_id
// pseudonym rather than a 16-byte account UUIDv7) differ.

/// Body of `POST /discovery/channel/register`. Binds an OPRF-hashed
/// `@channelname` to a 16-byte `channel_id` pseudonym. First-come-
/// first-served via a UNIQUE on `oprf_hash`; a repeat of the same hash
/// with a different channel id is 409 Conflict.
public struct ChannelRegisterRequest: Codable, Equatable {
    /// base64(VOPRF output) — exactly 64 bytes, from `blinder.finalize`.
    public let oprfHash: String
    /// base64(channel_id) — raw 16 bytes (ADR-0010 §"Wire surfaces").
    public let channelId: String

    public init(oprfHash: String, channelId: String) {
        self.oprfHash = oprfHash
        self.channelId = channelId
    }

    enum CodingKeys: String, CodingKey {
        case oprfHash = "oprf_hash"
        case channelId = "channel_id"
    }
}

/// Response body of `POST /discovery/channel/register`. Server returns
/// 409 on handle collision; that surfaces as
/// ``DiscoveryClientError/http(status:endpoint:body:)`` with
/// `status == 409`. The type only describes the 200-OK path.
public struct ChannelRegisterResponse: Codable, Equatable {
    public let ok: Bool

    public init(ok: Bool) {
        self.ok = ok
    }
}

/// Body of `POST /discovery/channel/lookup`. Resolves an OPRF-hashed
/// `@channelname` to its `channel_id`. Exact-match only.
public struct ChannelLookupRequest: Codable, Equatable {
    /// Same shape as ``ChannelRegisterRequest/oprfHash``.
    public let oprfHash: String

    public init(oprfHash: String) {
        self.oprfHash = oprfHash
    }

    enum CodingKeys: String, CodingKey {
        case oprfHash = "oprf_hash"
    }
}

/// Response body of `POST /discovery/channel/lookup`. The server
/// returns 404 for an unknown `@channelname`; the high-level client
/// surfaces that as a `nil` rather than an error class. This type only
/// describes the 200-OK path.
public struct ChannelLookupResponse: Codable, Equatable {
    /// base64(channel_id) — raw 16 bytes.
    public let channelId: String

    public init(channelId: String) {
        self.channelId = channelId
    }

    enum CodingKeys: String, CodingKey {
        case channelId = "channel_id"
    }
}
