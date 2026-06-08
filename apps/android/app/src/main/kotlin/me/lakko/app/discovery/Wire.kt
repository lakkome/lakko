// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:Suppress("ConstructorParameterNaming")
// `account_id`, `oprf_hash`, `signature_pubkey`, etc. mirror the
// server's snake_case `serde` defaults verbatim. Same posture as
// `me.lakko.app.auth.Wire` (1.15d2).

package me.lakko.app.discovery

import kotlinx.serialization.Serializable
import me.lakko.app.auth.PowProofPayload

/**
 * Kotlin-side mirror of the JSON shapes consumed and produced by
 * `lakko-svc-discovery`.
 *
 * The Rust originals live in `services/discovery/src/{oprf,registry,
 * identity_bundle}.rs`; we mirror them by hand the same way
 * `apps/web/src/discovery/wire.ts` does. When a field is added on
 * the server side, mirror it here in the same commit.
 *
 * **Phase 1.15e3 scope**: OPRF + register + lookup + identity
 * bundle (upload + fetch). The KeyPackage endpoints and the invite
 * codec live in their own files when 1.15f / 1.15e6 land.
 *
 * Reuses [PowProofPayload] from `me.lakko.app.auth` — the wire
 * shape is byte-identical to what the auth service expects (it's
 * the same Equix proof carried by every expensive endpoint). The
 * **server** keeps its PoW challenge table separate
 * (`services/discovery` has its own `/pow/challenge`); the JSON
 * shape is identical so the Kotlin DTO is shared.
 */

// ----------------------------------------------------------------------------
// /discovery/oprf  (services/discovery/src/oprf.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/oprf`. */
@Serializable
data class OprfRequest(
    val pow: PowProofPayload,
    /** base64(BlindedElement<Ristretto255-SHA512>) — produced by
     *  `OprfBlinder.blind(username).blindedB64()`. */
    val blinded: String,
)

/** Response body of `POST /discovery/oprf`. */
@Serializable
data class OprfResponse(
    /** base64(EvaluationElement). Feed to `blinder.finalize(evaluated)`. */
    val evaluated: String,
)

// ----------------------------------------------------------------------------
// /discovery/register  (services/discovery/src/registry.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/register`. */
@Serializable
data class RegisterRequest(
    val pow: PowProofPayload,
    /** base64(account_id) — raw 16 bytes. */
    val account_id: String,
    /** base64(VOPRF output) — exactly 64 bytes, the OPRF hash the
     *  client just computed via `blinder.finalize`. */
    val oprf_hash: String,
)

/** Response body of `POST /discovery/register`. Server returns
 *  409 on username collision; that surfaces as
 *  [DiscoveryHttpError] with `status = 409`. */
@Serializable
data class RegisterResponse(
    val ok: Boolean,
)

// ----------------------------------------------------------------------------
// /discovery/lookup  (services/discovery/src/registry.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/lookup`. */
@Serializable
data class LookupRequest(
    val pow: PowProofPayload,
    /** Same shape as [RegisterRequest.oprf_hash]. */
    val oprf_hash: String,
)

/** Response body of `POST /discovery/lookup`. 404 (username
 *  unknown) is folded into a `null` in [DiscoveryClient.lookupRaw];
 *  this type only describes the 200-OK path. */
@Serializable
data class LookupResponse(
    val account_id: String,
)

// ----------------------------------------------------------------------------
// /discovery/identity   (services/discovery/src/identity_bundle.rs — 1.16d3a)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/identity`. */
@Serializable
data class IdentityUploadRequest(
    val pow: PowProofPayload,
    val account_id: String,
    /** base64(Ed25519 public key) — 32 bytes after decode. */
    val signature_pubkey: String,
    /** base64(X25519 public key) — 32 bytes after decode. */
    val x25519_pubkey: String,
    /** G-4 (ADR-0016): base64 of an optional old-key-signed supersession
     *  proof bridging an AccountIdentity rotation. `null` for a normal
     *  upload — its absence clears any stored proof server-side. */
    val supersession_proof: String? = null,
)

/** Response body of `POST /discovery/identity`. */
@Serializable
data class IdentityUploadResponse(
    val ok: Boolean,
)

/** Response body of `GET /discovery/identity/{accountId_b64url}`.
 *  404 surfaces as `null` in [DiscoveryClient.identityFetch]. */
@Serializable
data class IdentityFetchResponse(
    val signature_pubkey: String,
    val x25519_pubkey: String,
    /** G-4 (ADR-0016): base64 of the stored supersession proof when the
     *  account rotated recently; `null` otherwise. Verified locally by the
     *  peer — the server passes the bytes through opaquely. */
    val supersession_proof: String? = null,
)

// ----------------------------------------------------------------------------
// /discovery/keypackage   (services/discovery/src/keypackage.rs — 1.16e1)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/keypackage`. */
@Serializable
data class KeyPackageUploadRequest(
    val pow: PowProofPayload,
    val account_id: String,
    /** base64(TLS-encoded MLS KeyPackage). Server caps decoded
     *  size at 2048 bytes — mirrors `MAX_KEYPACKAGE_BYTES` in
     *  the Rust handler. */
    val keypackage: String,
)

/** Response body of `POST /discovery/keypackage`. */
@Serializable
data class KeyPackageUploadResponse(
    val ok: Boolean,
)

/** Response body of `GET /discovery/keypackage/{accountId_b64url}`.
 *  404 surfaces as `null` in [DiscoveryClient.keypackageFetch]. */
@Serializable
data class KeyPackageFetchResponse(
    val keypackage: String,
)

// ----------------------------------------------------------------------------
// /discovery/invite   (services/discovery/src/invite.rs — Phase 1.18d1)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/invite`. */
@Serializable
data class InviteUploadRequest(
    val pow: PowProofPayload,
    /** Standard-alphabet base64 of the
     *  `composeInviteCapability(...)` output. Server caps
     *  decoded size at 2048 bytes (mirrored client-side in
     *  [DiscoveryClient.uploadInvite] so a misbehaving caller
     *  fails fast). */
    val capability: String,
)

/** Response body of `POST /discovery/invite`. */
@Serializable
data class InviteUploadResponse(
    /** base64url-no-pad of `SHA-256(capability_bytes)` — same
     *  bytes ⇒ same id; a retried upload is a no-op (server-
     *  side upsert). The client substitutes this directly into
     *  the `https://<host>/i/<blob_id>` pointer URL. */
    val blob_id: String,
)

/** Response body of `GET /discovery/invite/{blob_id_b64url}`.
 *  404 surfaces as `null` in [DiscoveryClient.inviteFetch]
 *  (server-side expiry filter folds expired-blob into the same
 *  status). */
@Serializable
data class InviteFetchResponse(
    val capability: String,
)

/** Body of `POST /discovery/invite/{blob_id_b64url}/consume`
 *  (Phase 1.18d3b mandatory shape). */
@Serializable
data class InviteConsumeRequest(
    val pow: PowProofPayload,
    /** Standard-alphabet base64 of the 32-byte joiner-tag from
     *  [uniffi.lakko_ffi_uniffi.computeJoinerInviteTag]. */
    val joiner_tag: String,
)

/** Response body of `POST /discovery/invite/{blob_id_b64url}/consume`. */
@Serializable
data class InviteConsumeResponse(
    /** Post-increment counter. UI surfaces "N of MAX uses" copy
     *  when the invite has a finite `max_uses`. When
     *  `already_consumed` is `true` the counter was NOT bumped —
     *  the value reflects pre-call state. */
    val used_count: Int,
    /** Echo of the row's `max_uses`. Sentinel mapping preserved:
     *  `0` = single-use, `0xFFFFFFFF` = unlimited, any other N
     *  = N-use ceiling. */
    val max_uses: Long,
    /** `true` when the side-table PK on `(blob_id, joiner_tag)`
     *  already had a row — i.e. this joiner has consumed the
     *  invite before (Phase 1.18d3b). The handler returns 200
     *  with the counter unchanged; the UI surfaces "you've
     *  already accepted this invite" + Decline-only and skips
     *  the ContactRequest send. */
    val already_consumed: Boolean,
)

/** Body of `DELETE /discovery/invite/{blob_id_b64url}` (Phase
 *  1.18d2b). The `signature` field carries proof that the
 *  request came from the same Ed25519 key that signed the
 *  capability. */
@Serializable
data class InviteRevokeRequest(
    /** UNIX millis at which the body was signed. Must be within
     *  `±REVOKE_REPLAY_WINDOW_MS` (10 min) of the server clock
     *  or the request is rejected with 400. */
    val signed_at_ms: Long,
    /** Standard-alphabet base64 of the 64-byte Ed25519 signature
     *  from [uniffi.lakko_ffi_uniffi.signInviteRevocation]. */
    val signature: String,
)

/** Body of `POST /discovery/invite/by-owner` (Phase
 *  1.18d2b-list). The owner signs the canonical bytes
 *  (`OWNER_LIST_DOMAIN_V1` ‖ `owner_account_id` ‖
 *  `signed_at_ms` BE) with their identity Ed25519 key; the
 *  server resolves the verifying key from
 *  `identity_bundles.signature_pubkey` for the given
 *  `account_id` and verifies. The session-token middleware on
 *  the route is a DoS gate, not the ownership proof. */
@Serializable
data class InviteOwnerListRequest(
    /** Standard-alphabet base64 of the owner's 16-byte UUIDv7
     *  `account_id`. */
    val owner_account_id: String,
    /** UNIX millis at which the body was signed. Same ±10 min
     *  replay window as the revoke endpoint. */
    val signed_at_ms: Long,
    /** Standard-alphabet base64 of the 64-byte Ed25519 signature
     *  from [uniffi.lakko_ffi_uniffi.signOwnerListQuery]. */
    val signature: String,
)

/** One entry in the `/discovery/invite/by-owner` response —
 *  wire-shape mirror of the server's `OwnerListEntry`. */
@Serializable
data class InviteOwnerListEntry(
    val blob_id_slug: String,
    val expiry_ms: Long,
    val max_uses: Long,
    val used_count: Long,
    val label: String,
    val bootstrap_referral: Boolean,
    val created_at: String,
)

/** Wire-level response shape from
 *  `POST /discovery/invite/by-owner`. */
@Serializable
data class InviteOwnerListResponse(
    val invites: List<InviteOwnerListEntry>,
)
