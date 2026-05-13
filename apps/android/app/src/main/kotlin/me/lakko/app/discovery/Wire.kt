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
