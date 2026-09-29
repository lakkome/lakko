// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:Suppress("ConstructorParameterNaming")
// `account_id`, `oprf_hash`, `signature_pubkey`, etc. mirror the
// server's snake_case `serde` defaults verbatim. Same posture as
// `me.lakko.app.auth.Wire` (1.15d2).

package me.lakko.app.discovery

import kotlinx.serialization.Serializable

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
 * No body carries a proof of work any more (ADR-0054): the anonymous
 * routes spend a token in the `x-lakko-anon-token` header, the rest
 * carry the session.
 */

// ----------------------------------------------------------------------------
// /discovery/oprf  (services/discovery/src/oprf.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/oprf`. */
@Serializable
data class OprfRequest(
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
// /discovery/channel/{register,lookup}
//   (services/discovery/src/channel_names.rs — G4-5, ADR-0010)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/channel/register`. Binds an OPRF-
 *  hashed `@channelname` to a 16-byte `channel_id` pseudonym.
 *  Same OPRF ciphersuite + wire shape as [RegisterRequest]
 *  (Ristretto255-SHA512, 64-byte hash) — the `/discovery/oprf`
 *  evaluate endpoint is namespace-agnostic, so the client reuses
 *  `OprfBlinder.blind(handle)` verbatim. First-come-first-served
 *  via a UNIQUE on `oprf_hash`; a repeat of the same hash with a
 *  different channel id is 409 Conflict. */
@Serializable
data class ChannelRegisterRequest(
    /** base64(VOPRF output) — exactly 64 bytes, from
     *  `blinder.finalize`. */
    val oprf_hash: String,
    /** base64(channel_id) — raw 16 bytes (ADR-0010 §"Wire
     *  surfaces"). */
    val channel_id: String,
)

/** Response body of `POST /discovery/channel/register`. Returns
 *  409 on collision; that surfaces as [DiscoveryHttpError] with
 *  `status = 409`. This type only describes the 200-OK path. */
@Serializable
data class ChannelRegisterResponse(
    val ok: Boolean,
)

/** Body of `POST /discovery/channel/lookup`. Resolves an OPRF-
 *  hashed `@channelname` to its `channel_id`. */
@Serializable
data class ChannelLookupRequest(
    /** Same shape as [ChannelRegisterRequest.oprf_hash]. */
    val oprf_hash: String,
)

/** Response body of `POST /discovery/channel/lookup`. The server
 *  returns 404 for an unknown `@channelname` — the high-level
 *  [DiscoveryClient.channelLookupRaw] folds that into a `null`. */
@Serializable
data class ChannelLookupResponse(
    /** base64(channel_id) — raw 16 bytes. */
    val channel_id: String,
)

// ----------------------------------------------------------------------------
// /discovery/identity   (services/discovery/src/identity_bundle.rs — 1.16d3a)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/identity`. */
@Serializable
data class IdentityUploadRequest(
    val account_id: String,
    /** base64(Ed25519 public key) — 32 bytes after decode. */
    val signature_pubkey: String,
    /** base64(X25519 public key) — 32 bytes after decode. */
    val x25519_pubkey: String,
    /** G-4 (ADR-0016): base64 of an optional old-key-signed supersession
     *  proof bridging an AccountIdentity rotation. `null` for a normal
     *  upload — its absence clears any stored proof server-side. */
    val supersession_proof: String? = null,
    /** G-30 #2 (ADR-0019): base64 of an optional client leaf authorisation
     *  (a genesis / fresh-start self-signature over the `(account_key,
     *  pubkey)` binding) so the transparency append can verify the account
     *  authorised a NEW log entry. `null` for a same-key re-publish (the
     *  append is idempotent and needs no authorisation). Opaque to the
     *  server. */
    val leaf_auth: String? = null,
    /** ADR-0036 (G-30 finding 2): base64 of an optional auth-attested
     *  genesis attestation (from `POST /auth/genesis/attest`) minted for a
     *  genesis / fresh-start publish. Rides alongside [leaf_auth]; discovery
     *  forwards it opaquely (invariant #2) to the transparency append, which
     *  prevents first-entry squatting. `null` for a same-key re-publish (or
     *  when the mint was unavailable during rollout — the require-genesis-
     *  attest flag defaults OFF, so the leaf still appends). Preserved across
     *  non-rotation re-uploads server-side via `COALESCE`, like [leaf_auth]. */
    val genesis_attest: String? = null,
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

/** Body of `POST /discovery/keypackage` (ADR-0050: one row per device). */
@Serializable
data class KeyPackageUploadRequest(
    val account_id: String,
    /** base64 of the device leaf's 32-byte Ed25519 signature key. */
    val device_id: String,
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

/** One device's entry in [KeyPackageFetchResponse] (ADR-0050). */
@Serializable
data class DeviceKeyPackagePayload(
    /** base64 of the device leaf's 32-byte signature key. */
    val device_id: String,
    /** base64(TLS-encoded MLS KeyPackage). */
    val keypackage: String,
)

/** Response body of `GET /discovery/keypackage/{accountId_b64url}`: every
 *  device of the account with a published KeyPackage, oldest first
 *  (ADR-0050). 404 surfaces as `null` in [DiscoveryClient.keypackageFetch]. */
@Serializable
data class KeyPackageFetchResponse(
    val keypackages: List<DeviceKeyPackagePayload>,
)

// ----------------------------------------------------------------------------
// /discovery/invite   (services/discovery/src/invite.rs — Phase 1.18d1)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/invite` (sealed-link amendment of 2026-09-28).
 *  The capability is encrypted under the link secret, which never leaves the
 *  device except in the link's fragment; what the server needs travels in
 *  the clear fields beside the sealed blob. */
@Serializable
data class InviteUploadRequest(
    /** Standard-alphabet base64 of the sealed link blob
     *  (`InviteLinkMint.sealed`). Opaque to the server. */
    val sealed: String,
    /** Standard-alphabet base64 of the 32-byte Ed25519 key that signed the
     *  capability (`InviteLinkMint.issuerPubkey`), the key an owner-signed
     *  revoke is verified against. */
    val issuer_pubkey: String,
    /** The capability's `expiry_ms` (`0` = never expires). */
    val expiry_ms: Long,
    /** The capability's `max_uses` sentinel (`0` = single-use,
     *  `0xFFFFFFFF` = unlimited), a JSON number the server reads as `u32`. */
    val max_uses: Long,
    /** The capability's label, for the owner's own list. Empty when none. */
    val label: String,
    /** The capability's `bootstrap_referral` flag, for the owner's own list. */
    val bootstrap_referral: Boolean,
)

/** Body of `POST /discovery/call-invite` and `POST /discovery/group-invite`
 *  (sealed-link amendment of 2026-09-28): the sealed blob plus the clear
 *  fields the server needs. The server stores no `call_id` / `group_id`. */
@Serializable
data class SealedInviteUploadRequest(
    /** Standard-alphabet base64 of the sealed link blob. */
    val sealed: String,
    /** Standard-alphabet base64 of the issuer's 32-byte Ed25519 key. */
    val issuer_pubkey: String,
    /** The capability's `expiry_ms` (`0` = never expires). */
    val expiry_ms: Long,
    /** Consume ceiling (`0` = single-use, `0xFFFFFFFF` = unlimited). For
     *  `/g/` a SOFT anti-spam throttle, not part of the capability. */
    val max_uses: Long,
)

/** Response body of `POST /discovery/invite`. */
@Serializable
data class InviteUploadResponse(
    /** base64url-no-pad of a random 6-byte ledger id, 8
     *  characters (ADR-0028 amendment of 2026-09-27). Fresh on
     *  every upload: the same bytes uploaded twice get two ids.
     *  The client substitutes it into the
     *  `https://<host>/i/<blob_id>` pointer URL. */
    val blob_id: String,
)

/** Response body of `GET /discovery/invite/{blob_id_b64url}`.
 *  404 surfaces as `null` in [DiscoveryClient.inviteFetch]
 *  (server-side expiry filter folds expired-blob into the same
 *  status). */
@Serializable
data class InviteFetchResponse(
    /** Standard-alphabet base64 of the sealed link blob. Opened locally
     *  with the link secret from the URL fragment. */
    val sealed: String,
)

/** Body of `POST /discovery/invite/{blob_id_b64url}/consume`
 *  (Phase 1.18d3b mandatory shape). */
@Serializable
data class InviteConsumeRequest(
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

// ----------------------------------------------------------------------------
// /discovery/group-invite   (services/discovery/src/group_invite.rs — ADR-0028)
// ----------------------------------------------------------------------------

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
