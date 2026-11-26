// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

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
/// (`/discovery/oprf`, `/discovery/handle/claim`, the three consumes)
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
    /// base64(EvaluationElement). Feed to `blinder.finalize(evaluatedB64:)`,
    /// which returns an `OprfResult`: its `lookupKey` is the only half that
    /// goes back to discovery; its `output` never leaves the device (ADR-0060
    /// section 5).
    public let evaluated: String

    public init(evaluated: String) {
        self.evaluated = evaluated
    }
}

/// Body of `POST /discovery/oprf/batch` (maintainer, 2026-11-04): the blinded
/// elements of every term the account publishes about its own name, in one
/// request with one `discovery-register` token (1..=65 entries).
public struct OprfBatchRequest: Codable, Equatable {
    public let blinded: [String]

    public init(blinded: [String]) {
        self.blinded = blinded
    }
}

/// Response body of `POST /discovery/oprf/batch`: one evaluation per blinded
/// element, in the same order.
public struct OprfBatchResponse: Codable, Equatable {
    public let evaluated: [String]

    public init(evaluated: [String]) {
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
    /// base64 of the 64-byte lookup key (`OprfResult.lookupKey` from
    /// `blinder.finalize(evaluatedB64:)`), NEVER the OPRF output: public and
    /// introduction records are sealed under keys derived from the output, so
    /// discovery must not hold it (ADR-0060 section 5). The wire name stays
    /// `oprf_hash` and the width stays 64 bytes.
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
    /// `"person"`, or `"handle"` for a public group or channel (ADR-0087:
    /// one namespace, one lookup). Absent on an older server.
    public let kind: String?
    /// A person only: a group or channel names no account.
    public let accountId: String?
    /// base64 of the account's public record (ADR-0060 section 5), opened
    /// with the OPRF **output** of the looked-up username (`openPublicRecord`).
    /// Optional on the wire so an older server still decodes.
    public let publicRecord: String?

    public init(accountId: String?, publicRecord: String? = nil, kind: String? = nil) {
        self.kind = kind
        self.accountId = accountId
        self.publicRecord = publicRecord
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case accountId = "account_id"
        case publicRecord = "public_record_b64"
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
    /// Standard base64 of the 64-byte signature by the account's Ed25519 key
    /// binding `x25519Pubkey` to it (`signX25519Binding`). Discovery refuses
    /// an upload without a valid one (`400 x25519-binding-invalid`), and a
    /// reader seals nothing to an X25519 key it does not bind.
    public let x25519Binding: String
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
    /// ADR-0073 section 4.1: standard base64 of the 145-byte key-reset
    /// attestation `POST /account/keys/reset/finish` minted, carried on the
    /// upload that installs the reset identity. Discovery verifies it under
    /// its pinned attest key, requires a generation strictly above the
    /// stored one, and deletes every KeyPackage row of the account in the
    /// same transaction. `nil` on every other upload; an unattested,
    /// unproven key CHANGE is refused (`409 identity-change-needs-attestation`).
    public let keyResetAttest: String?
    /// ADR-0074 section 3.7: standard base64 of the 201-byte two-way
    /// conversation link a rotation mints (signed by the key the
    /// conversations are bound to and by this key). Opaque to the server,
    /// which keeps it across a same-key re-upload and clears it on a key
    /// change that carries none. `nil` on every upload but a rotation's.
    public let conversationLink: String?

    public init(
        accountId: String,
        signaturePubkey: String,
        x25519Pubkey: String,
        x25519Binding: String,
        supersessionProof: String? = nil,
        leafAuth: String? = nil,
        genesisAttest: String? = nil,
        keyResetAttest: String? = nil,
        conversationLink: String? = nil
    ) {
        self.accountId = accountId
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
        self.x25519Binding = x25519Binding
        self.supersessionProof = supersessionProof
        self.leafAuth = leafAuth
        self.genesisAttest = genesisAttest
        self.keyResetAttest = keyResetAttest
        self.conversationLink = conversationLink
    }

    enum CodingKeys: String, CodingKey {
        case accountId = "account_id"
        case signaturePubkey = "signature_pubkey"
        case x25519Pubkey = "x25519_pubkey"
        case x25519Binding = "x25519_binding"
        case supersessionProof = "supersession_proof"
        case leafAuth = "leaf_auth"
        case genesisAttest = "genesis_attest"
        case keyResetAttest = "key_reset_attest"
        case conversationLink = "conversation_link"
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
    /// Standard base64 of the account key's signature binding
    /// `x25519Pubkey` to `signaturePubkey`; absent on a row that has none.
    /// Read only through ``verifiedX25519Pubkey``: nothing is sealed to an
    /// X25519 key this binding does not verify.
    public let x25519Binding: String?
    /// G-4 (ADR-0016): base64 of the stored supersession proof when the
    /// account rotated recently; `nil` otherwise. Verified locally by the
    /// peer — the server passes the bytes through opaquely.
    public let supersessionProof: String?
    /// ADR-0073 section 4.1: the key generation the published identity
    /// belongs to (1 = never reset). A device compares it with the one its
    /// session pair carries (``KeyResetLanding``). Absent in a response of a
    /// scripted test directory, it reads as 1.
    public let keyGeneration: UInt64
    /// ADR-0073 R.3: standard base64 of the auth-signed key-reset
    /// attestation of the account's LATEST reset, absent when it never
    /// reset. Opaque to the server at fetch; verified locally
    /// (`verifyKeyResetAttestation`). After a later proven rotation it no
    /// longer binds the fetched key, and that path carries a supersession
    /// proof instead.
    public let keyResetAttest: String?
    /// ADR-0074 section 3.7: standard base64 of the conversation link of the
    /// account's newest rotation, absent when it never rotated (or reset
    /// since). Verified locally against the key the gate left trusted
    /// (`verifyConversationLink`); one that does not verify is ignored.
    public let conversationLink: String?
    /// ADR-0101 section 2: standard base64 of the 105-byte continuity
    /// binding of an account that reset its keys; served only while
    /// unexpired. A peer verifies it under the attested new key before
    /// writing to the mailbox it names (``ContinuityTargets``).
    public let continuityBindingB64: String?

    public init(
        signaturePubkey: String,
        x25519Pubkey: String,
        x25519Binding: String? = nil,
        supersessionProof: String? = nil,
        keyGeneration: UInt64 = 1,
        keyResetAttest: String? = nil,
        conversationLink: String? = nil,
        continuityBindingB64: String? = nil
    ) {
        self.continuityBindingB64 = continuityBindingB64
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
        self.x25519Binding = x25519Binding
        self.supersessionProof = supersessionProof
        self.keyGeneration = keyGeneration
        self.keyResetAttest = keyResetAttest
        self.conversationLink = conversationLink
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        signaturePubkey = try c.decode(String.self, forKey: .signaturePubkey)
        x25519Pubkey = try c.decode(String.self, forKey: .x25519Pubkey)
        x25519Binding = try c.decodeIfPresent(String.self, forKey: .x25519Binding)
        supersessionProof = try c.decodeIfPresent(String.self, forKey: .supersessionProof)
        keyGeneration = try c.decodeIfPresent(UInt64.self, forKey: .keyGeneration) ?? 1
        keyResetAttest = try c.decodeIfPresent(String.self, forKey: .keyResetAttest)
        conversationLink = try c.decodeIfPresent(String.self, forKey: .conversationLink)
        continuityBindingB64 = try c.decodeIfPresent(String.self, forKey: .continuityBindingB64)
    }

    enum CodingKeys: String, CodingKey {
        case signaturePubkey = "signature_pubkey"
        case x25519Pubkey = "x25519_pubkey"
        case x25519Binding = "x25519_binding"
        case supersessionProof = "supersession_proof"
        case keyGeneration = "key_generation"
        case keyResetAttest = "key_reset_attest"
        case conversationLink = "conversation_link"
        case continuityBindingB64 = "continuity_binding_b64"
    }
}

/// Body of `PUT /discovery/identity/continuity` (ADR-0101 section 2).
public struct ContinuityBindingUploadRequest: Codable, Equatable {
    public let continuityBindingB64: String

    public init(continuityBindingB64: String) {
        self.continuityBindingB64 = continuityBindingB64
    }

    enum CodingKeys: String, CodingKey {
        case continuityBindingB64 = "continuity_binding_b64"
    }
}

extension IdentityFetchResponse {
    /// The X25519 key to seal to, ONLY when the account's Ed25519 key signed
    /// a binding of it (`verifyX25519Binding`). `nil` for an absent, malformed
    /// or non-verifying binding, or a malformed key: a caller then refuses
    /// (the ordinary failure state of its flow) and never seals to the
    /// unbound key. `signaturePubkey` is the key in THIS answer; a caller
    /// that holds another (pinned) key for the person checks that first.
    public var verifiedX25519Pubkey: Data? {
        guard let binding = x25519Binding.flatMap({ Data(base64Encoded: $0) }),
              let ed = Data(base64Encoded: signaturePubkey), ed.count == 32,
              let x = Data(base64Encoded: x25519Pubkey), x.count == 32,
              verifyX25519Binding(binding: binding, ed25519PublicKey: ed, x25519PublicKey: x)
        else { return nil }
        return x
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
/// read out of the capability arrives in clear fields. Since ADR-0078
/// section 10 nothing names the owner or the issuer: the row holds the
/// public half of a revocation key minted for the one link.
public struct InviteUploadRequest: Codable, Equatable {
    /// Standard-alphabet base64 of the sealed link blob (`InviteLinkMint.sealed`).
    public let sealed: String
    /// Standard-alphabet base64 of the link's own 32-byte revocation public
    /// key (``LinkRevocationKey``), the key a revoke verifies against.
    public let revocationPubkey: String
    /// The capability's expiry (`0` = never), for the fetch filter and pruner.
    public let expiryMs: UInt64
    /// The consume ceiling sentinel (`0` single use, `UInt32.max` unlimited).
    public let maxUses: UInt32

    public init(sealed: String, revocationPubkey: String, expiryMs: UInt64, maxUses: UInt32) {
        self.sealed = sealed
        self.revocationPubkey = revocationPubkey
        self.expiryMs = expiryMs
        self.maxUses = maxUses
    }

    enum CodingKeys: String, CodingKey {
        case sealed
        case revocationPubkey = "revocation_pubkey"
        case expiryMs = "expiry_ms"
        case maxUses = "max_uses"
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
    /// Every use of the link is spent. The row is still served, because a
    /// person who already used the link must reach "already in your contacts"
    /// and only a consume can tell them apart: this answers only a visitor
    /// with no session. Absent reads as not used up.
    public let usedUp: Bool?

    public init(sealed: String, usedUp: Bool? = nil) {
        self.sealed = sealed
        self.usedUp = usedUp
    }

    enum CodingKeys: String, CodingKey {
        case sealed
        case usedUp = "used_up"
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

