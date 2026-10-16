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
    /** base64(EvaluationElement). Feed to `blinder.finalize(evaluated)`, which
     *  returns an `OprfResult { output, lookupKey }`. */
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
    /** base64 of the 64-byte discovery lookup key,
     *  `blinder.finalize(..).lookupKey`. ADR-0060 §5: never the OPRF
     *  output itself, which keys public and introduction records and
     *  stays on the device. The field name is the server's. */
    val oprf_hash: String,
)

/** Response body of `POST /discovery/register`. Server returns
 *  409 on username collision; that surfaces as
 *  [DiscoveryHttpError] with `status = 409`. */
@Serializable
data class RegisterResponse(
    val ok: Boolean,
    /** ADR-0060 §5 — standard base64 of the 153-byte username certificate
     *  binding the username's lookup key to the account. */
    val username_certificate_b64: String? = null,
)

/** Response of `GET /discovery/username-certificate` (ADR-0060 §5). */
@Serializable
data class UsernameCertificateResponse(
    val username_certificate_b64: String,
)

/** Response of `GET /.well-known/lakko-username-cert-key` (ADR-0060 §5). */
@Serializable
data class UsernameCertKeyResponse(
    val alg: String,
    val key_b64url: String,
)

/** Body of `PUT /discovery/visibility` (ADR-0060 §5): enter or leave
 *  Everyone. */
@Serializable
data class VisibilityRequest(
    val public: Boolean,
    /** base64 of each name search term's 64-byte lookup key
     *  (`OprfResult.lookupKey`), never an OPRF output. */
    val name_terms_b64: List<String> = emptyList(),
    /** Standard base64 of the public record (`composePublicRecord`, sealed
     *  under the OPRF outputs of the terms). Required when [public], refused
     *  otherwise, so it is omitted (`null`) outside Everyone. */
    val public_record_b64: String? = null,
)

/**
 * The body of `PUT /discovery/visibility`: in Everyone, the lookup keys and
 * the public record; outside it, `public: false` alone (the server refuses
 * terms or a record there). Pure, so the wire shape is host-testable.
 */
internal fun visibilityRequest(
    public: Boolean,
    nameTermLookupKeys: List<ByteArray>,
    publicRecord: ByteArray?,
): VisibilityRequest = if (public) {
    VisibilityRequest(
        public = true,
        name_terms_b64 = nameTermLookupKeys.map { me.lakko.app.auth.base64Encode(it) },
        public_record_b64 = publicRecord?.let { me.lakko.app.auth.base64Encode(it) },
    )
} else {
    VisibilityRequest(public = false)
}

/** Body of `POST /discovery/name/lookup` (ADR-0060 §5). */
@Serializable
data class NameLookupRequest(
    /** base64 of the term's 64-byte lookup key (`OprfResult.lookupKey`),
     *  never an OPRF output. The field name is the server's. */
    val oprf_hash_b64: String,
)

/** One person in Everyone found by a name term (`services/discovery/src/visibility.rs`
 *  `NameLookupResult`). */
@Serializable
data class NameLookupResult(
    /** Standard base64 of the 16-byte account id. */
    val account_id_b64: String,
    /** Standard base64 of their public record; the searcher opens it with
     *  the OPRF OUTPUT of the term it looked up (`openPublicRecord`). */
    val public_record_b64: String? = null,
)

/** Response of `POST /discovery/name/lookup` (at most 20 results). */
@Serializable
data class NameLookupResponse(
    val results: List<NameLookupResult> = emptyList(),
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
    /** ADR-0060 §5 — standard base64 of the account's public record, opened
     *  with the OPRF output of the username the lookup used. */
    val public_record_b64: String? = null,
)

// ----------------------------------------------------------------------------
// /discovery/channel/{register,lookup}
//   (services/discovery/src/channel_names.rs — G4-5, ADR-0010)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/channel/register`. Binds the lookup key
 *  of an OPRF'd `@channelname` to a 16-byte `channel_id` pseudonym.
 *  Same OPRF ciphersuite + wire shape as [RegisterRequest]
 *  (Ristretto255-SHA512, 64-byte hash) — the `/discovery/oprf`
 *  evaluate endpoint is namespace-agnostic, so the client reuses
 *  `OprfBlinder.blind(handle)` verbatim. First-come-first-served
 *  via a UNIQUE on `oprf_hash`; a repeat of the same hash with a
 *  different channel id is 409 Conflict. */
@Serializable
data class ChannelRegisterRequest(
    /** base64 of the 64-byte discovery lookup key,
     *  `blinder.finalize(..).lookupKey` (ADR-0060 §5, never the OPRF
     *  output). */
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
    /** ADR-0073: base64 of the optional 177-byte key-reset attestation auth
     *  minted in `POST /account/keys/reset/finish`. Required for a key
     *  change that carries no supersession proof; the server verifies it
     *  under the pinned attest key, requires a strictly greater generation
     *  than it stored, and deletes every KeyPackage row of the account in the
     *  same transaction. */
    val key_reset_attest: String? = null,
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
    /** ADR-0073: the key generation the published bundle belongs to (1 =
     *  never reset). A device compares it with the generation of its own
     *  session pair before it uploads anything. */
    val key_generation: Long = 1L,
    /** ADR-0073 R.3: standard base64 of the latest key-reset attestation the
     *  account published, absent when it never reset. Opaque to discovery;
     *  verified locally under the trusted attest key before a proof-less key
     *  change is accepted as a reset. After a later PROVEN rotation it no
     *  longer binds the served key, and that path never reads it. */
    val key_reset_attest: String? = null,
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

/** Body of `POST /discovery/invite` (sealed-link amendment of 2026-09-28):
 *  the SEALED capability plus the fields the ledger needs in clear. Since
 *  ADR-0078 section 10 nothing names the owner or the issuer. */
@Serializable
data class InviteUploadRequest(
    /** Standard base64 of the sealed link (`InviteLinkMint.sealed`). Opaque
     *  to the server. */
    val sealed: String,
    /** Standard base64 of the link's own 32-byte revocation public key
     *  ([me.lakko.app.contacts.OwnLinks.freshRevocationKey]) — the key a
     *  later revoke must verify against. */
    val revocation_pubkey: String,
    /** The capability's `expiry_ms` (`0` = never), for the fetch filter. */
    val expiry_ms: Long,
    /** The capability's `max_uses` sentinel, for the consume ceiling. */
    val max_uses: Long,
)

/** Body of `POST /discovery/call-invite` (sealed-link amendment). */
@Serializable
data class CallInviteUploadRequest(
    val sealed: String,
    val revocation_pubkey: String,
    val expiry_ms: Long,
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
    /** Standard base64 of the sealed link; opened locally under the URL
     *  fragment's secret. */
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

/** Body of `POST /discovery/group-invite` (ADR-0028, G-3; sealed since
 *  2026-09-28). The `GroupInviteCapability` wire format does NOT embed
 *  `max_uses`, so the server reads it from the request. It is
 *  a SOFT anti-spam throttle (`0` = single-use, `0xFFFFFFFF` = unlimited), NOT a
 *  security boundary — the admin approval gate (ADR-0027) is the boundary. */
@Serializable
data class GroupInviteUploadRequest(
    /** Standard base64 of the sealed link (`InviteLinkMint.sealed`). */
    val sealed: String,
    /** Standard base64 of the link's own 32-byte revocation public key. */
    val revocation_pubkey: String,
    /** The capability's `expiry_ms` (`0` = never). */
    val expiry_ms: Long,
    /** Soft consume ceiling. Serialised as a JSON number the server reads as a
     *  `u32`; `0xFFFFFFFF` (= 4294967295) means unlimited. */
    val max_uses: Long,
)

/** Body of `DELETE /discovery/invite/{blob_id_b64url}` (Phase
 *  1.18d2b). The `signature` field carries proof that the
 *  request came from the link's own revocation key (ADR-0078
 *  section 10). */
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

