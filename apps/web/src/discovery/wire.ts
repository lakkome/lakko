// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * TypeScript-side mirror of the JSON shapes consumed and produced
 * by `lakko-svc-discovery`.
 *
 * The Rust originals live in `services/discovery/src/{oprf,registry}.rs`;
 * we mirror them by hand the same way `apps/web/src/auth/wire.ts`
 * mirrors the auth-service shapes. When a field is added on the
 * server side, mirror it here in the same commit.
 *
 * Conventions (same as auth):
 *   * Every byte string crosses the wire base64-encoded
 *     (standard alphabet, with padding).
 *   * Snake_case JSON field names match the server's serde
 *     defaults; we keep the same on the TS side so the request
 *     bodies pass straight to `JSON.stringify`.
 */

import type { PowProofPayload } from "../auth/wire";

// ----------------------------------------------------------------------------
// /discovery/oprf  (services/discovery/src/oprf.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/oprf`. */
export interface OprfRequest {
  pow: PowProofPayload;
  /** base64(BlindedElement<Ristretto255-SHA512>) — produced by
   *  `OprfBlinder.blind(username).blindedB64`. */
  blinded: string;
}

/** Response body of `POST /discovery/oprf`. */
export interface OprfResponse {
  /** base64(EvaluationElement<Ristretto255-SHA512>). Feed to
   *  `blinder.finalize(evaluated)` to recover the OPRF hash. */
  evaluated: string;
}

// ----------------------------------------------------------------------------
// /discovery/register  (services/discovery/src/registry.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/register`. */
export interface RegisterRequest {
  pow: PowProofPayload;
  /** base64(account_id) — raw 16 bytes, same shape as
   *  `auth/wire.ts`. */
  account_id: string;
  /** base64(VOPRF output) — exactly 64 bytes, the OPRF hash the
   *  client just computed via `blinder.finalize`. */
  oprf_hash: string;
}

/** Response body of `POST /discovery/register`. Returns 409 on
 *  collision; the type is only for the 200-OK path. */
export interface RegisterResponse {
  ok: boolean;
}

// ----------------------------------------------------------------------------
// /discovery/lookup  (services/discovery/src/registry.rs)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/lookup`. */
export interface LookupRequest {
  pow: PowProofPayload;
  /** Same shape as `RegisterRequest.oprf_hash`. */
  oprf_hash: string;
}

/** Response body of `POST /discovery/lookup`. The server returns
 *  404 when the username is not registered — the high-level
 *  client surfaces that as a `null` rather than an error class. */
export interface LookupResponse {
  account_id: string;
}

// ----------------------------------------------------------------------------
// /discovery/identity   (services/discovery/src/identity_bundle.rs — 1.16d3a)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/identity`. */
export interface IdentityUploadRequest {
  pow: PowProofPayload;
  account_id: string;
  /** base64(Ed25519 public key) — 32 bytes after decode. */
  signature_pubkey: string;
  /** base64(X25519 public key)  — 32 bytes after decode. */
  x25519_pubkey: string;
  /** G-4 (ADR-0016): base64 of an optional old-key-signed supersession
   *  proof bridging an AccountIdentity rotation. Omit for a normal upload
   *  — its absence clears any previously stored proof server-side. */
  supersession_proof?: string;
  /** G-30 #2 (ADR-0019): base64 of an optional genesis leaf
   *  self-signature authorising a NEW `(account_key -> pubkey)`
   *  transparency-log binding. Minted via `signLeafGenesis` at the
   *  publish sites that introduce a new key (first-ever signup +
   *  the lost-everything fresh-start mint). The transparency append
   *  refuses a new binding that carries neither this nor a
   *  `supersession_proof`. Omit on a routine same-key re-upload — the
   *  log append is idempotent there, so no authorisation is needed. */
  leaf_auth?: string;
  /** ADR-0036 (G-30 finding 2): base64 of an optional auth-attested
   *  genesis attestation (the 153-byte `GenesisAttestation` minted via
   *  `POST /auth/genesis/attest`). Sent ONLY on a first publish / genesis
   *  (and an ADR-0017 reset) alongside the `leaf_auth` self-signature; a
   *  rotation carries none. Discovery stores it opaque and forwards it
   *  verbatim to the transparency append (invariant #2); a peer verifies
   *  it on a first observation against the pinned auth attest key. */
  genesis_attest?: string;
}

/** Response body of `POST /discovery/identity`. */
export interface IdentityUploadResponse {
  ok: boolean;
}

/** Response body of `GET /discovery/identity/{acct_b64url}`.
 *  The server returns 404 when no bundle exists for the account
 *  — the high-level client surfaces that as a `null`. */
export interface IdentityFetchResponse {
  signature_pubkey: string;
  x25519_pubkey: string;
  /** G-4 (ADR-0016): base64 of the stored supersession proof when the
   *  account rotated recently; absent otherwise. The peer verifies it
   *  locally — the server passes the bytes through opaquely. */
  supersession_proof?: string;
}

// ----------------------------------------------------------------------------
// /discovery/keypackage   (services/discovery/src/keypackage.rs — 1.16e1)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/keypackage`. */
export interface KeyPackageUploadRequest {
  pow: PowProofPayload;
  account_id: string;
  /** base64(TLS-encoded MLS KeyPackage). Server caps decoded
   *  size at 2048 bytes — mirrors `MAX_KEYPACKAGE_BYTES` in the
   *  Rust handler. */
  keypackage: string;
}

/** Response body of `POST /discovery/keypackage`. */
export interface KeyPackageUploadResponse {
  ok: boolean;
}

/** Response body of `GET /discovery/keypackage/{acct_b64url}`.
 *  404 surfaces as `null` in the high-level client. */
export interface KeyPackageFetchResponse {
  keypackage: string;
}

// ----------------------------------------------------------------------------
// /discovery/invite   (services/discovery/src/invite.rs — Phase 1.18d1)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/invite`. The server runs
 *  `lakko_contacts::invite::decode_and_verify` over the decoded
 *  bytes and rejects junk uploads at ingress, so the only way a
 *  POST succeeds is if `capability` is a real, signature-valid
 *  invite the client just composed. */
export interface InviteUploadRequest {
  pow: PowProofPayload;
  /** Standard-alphabet base64 of `wasm.InviteCapability.compose(...)`
   *  output — same wire format as 1.18a / 1.18b / 1.18c. Server
   *  caps decoded size at 2048 bytes. */
  capability: string;
}

/** Response body of `POST /discovery/invite`. */
export interface InviteUploadResponse {
  /** base64url-no-pad of `SHA-256(capability_bytes)` — same
   *  bytes ⇒ same id, retried upload is a no-op (server-side
   *  upsert via `INSERT ... ON DUPLICATE KEY UPDATE`). The
   *  client substitutes this directly into the
   *  `https://<host>/i/<blob_id>` pointer URL. */
  blob_id: string;
}

/** Response body of `GET /discovery/invite/{blob_id_b64url}`.
 *  404 surfaces as `null` in the high-level client (server-side
 *  expiry filter folds expired-blob into the same status — see
 *  `services/discovery/src/invite.rs::fetch` for why we don't
 *  use 410). */
export interface InviteFetchResponse {
  capability: string;
}

/** Body of `POST /discovery/invite/{blob_id_b64url}/consume`
 *  (Phase 1.18d3b mandatory shape). `joiner_tag` is the
 *  server-blind 32-byte HMAC the joiner computes locally via
 *  `wasm.computeJoinerInviteTag(capability_bytes,
 *  joiner_account_id)`; the server stores it keyed by
 *  `(blob_id, joiner_tag)` to dedup repeats. */
export interface InviteConsumeRequest {
  pow: PowProofPayload;
  /** Standard-alphabet base64 of the 32-byte joiner-tag. */
  joiner_tag: string;
}

/** Response body of `POST /discovery/invite/{blob_id_b64url}/consume`. */
export interface InviteConsumeResponse {
  /** Post-increment counter. UI surfaces "1 of 10 uses" copy
   *  when the invite has a finite `max_uses`. When
   *  `already_consumed` is `true` the counter was NOT bumped —
   *  the value reflects pre-call state. */
  used_count: number;
  /** Echo of the row's `max_uses` — sentinel mapping is preserved
   *  (`0` for single-use, `0xFFFFFFFF` for unlimited, any other
   *  N for an N-use ceiling). The web client compares it to
   *  `used_count` to decide between "consumed successfully" and
   *  "this was the last use" copy. */
  max_uses: number;
  /** `true` when the side-table PK on `(blob_id, joiner_tag)`
   *  already had a row — i.e. this joiner has consumed the
   *  invite before (Phase 1.18d3b). The handler returns 200
   *  with the counter unchanged; the UI surfaces "you've
   *  already accepted this invite" + Decline-only and skips
   *  the ContactRequest send. */
  already_consumed: boolean;
}

/** Body of `POST /discovery/group-invite` (ADR-0028, G-3). Mirror of
 *  [`InviteUploadRequest`] but carries an extra `max_uses` field: a
 *  `GroupInviteCapability` does NOT embed `max_uses` in its signed wire (it
 *  conveys only `group_id` + `issuer_pubkey` + `expiry_ms`), so the uploader
 *  supplies the SOFT consume ceiling out-of-band. It is an anti-spam throttle,
 *  not an access control — the security boundary is the admin approval gate
 *  (ADR-0027). The response reuses [`InviteUploadResponse`] (`blob_id`). */
export interface GroupInviteUploadRequest {
  pow: PowProofPayload;
  /** Standard-alphabet base64 of `wasm.composeGroupInviteCapability(...)`
   *  output. Server runs `decode_and_verify` at ingress + caps decoded size at
   *  512 bytes. */
  capability: string;
  /** SOFT consume ceiling — `0` = single-use, `0xFFFFFFFF` = unlimited, any
   *  other N = at most N consumes. NOT part of the signed capability. */
  max_uses: number;
}

/** Body of `DELETE /discovery/invite/{blob_id_b64url}` (Phase
 *  1.18d2b). The `signature` field carries proof that the
 *  request came from the same Ed25519 key that signed the
 *  capability — server recovers `owner_signature_pubkey` from
 *  the stored `capability_bytes` and verifies against the
 *  canonical bytes built from `blob_id` + `signed_at_ms`.
 *
 *  `signed_at_ms` MUST be within `±REVOKE_REPLAY_WINDOW_MS`
 *  (10 min, mirroring `lakko_contacts::invite`) of the server
 *  clock or the request is rejected with 400 Bad Request. */
export interface InviteRevokeRequest {
  /** UNIX millis at which the body was signed. JSON `number`
   *  is safe up to 2^53; UNIX millis is ~1.7e12, well within
   *  the safe range. */
  signed_at_ms: number;
  /** Standard-alphabet base64 of the 64-byte Ed25519 signature
   *  from `wasm.signInviteRevocation(...)`. */
  signature: string;
}

/** Body of `POST /discovery/invite/by-owner` (Phase
 *  1.18d2b-list). The owner signs the canonical bytes
 *  (`OWNER_LIST_DOMAIN_V1` ‖ `owner_account_id` ‖
 *  `signed_at_ms` BE) with their identity Ed25519 key; the
 *  server resolves the verifying key from
 *  `identity_bundles.signature_pubkey` and verifies. The
 *  session-token middleware on the route is a DoS gate, not
 *  the ownership proof.
 *
 *  `signed_at_ms` MUST be within `±REVOKE_REPLAY_WINDOW_MS`
 *  (10 min, re-used from the revoke flow) of the server clock
 *  or the request is rejected with 400 Bad Request. */
export interface InviteOwnerListRequest {
  /** Standard-alphabet base64 of the owner's 16-byte UUIDv7
   *  `account_id` (matches `IdentitySecrets.accountId`). */
  owner_account_id: string;
  /** UNIX millis at which the body was signed. */
  signed_at_ms: number;
  /** Standard-alphabet base64 of the 64-byte Ed25519 signature
   *  from `wasm.signOwnerListQuery(...)`. */
  signature: string;
}

/** One entry in the `/discovery/invite/by-owner` response —
 *  mirrors `OwnerListEntry` on the server side. The shape
 *  matches the wire-name (snake_case) the server emits; the
 *  TS client surfaces a normalised camelCase view to the UI
 *  via the `ListInvitesResult` type below. */
export interface InviteOwnerListEntry {
  /** URL-safe base64 (no padding) of the 32-byte blob_id —
   *  the same `/i/<slug>` segment that appears in the share
   *  URL the owner originally minted. */
  blob_id_slug: string;
  /** UNIX millis the capability expires at. `0` (the
   *  `NEVER_EXPIRES` sentinel) means it never expires. */
  expiry_ms: number;
  /** Wire-format `max_uses` sentinel: `0` = SINGLE_USE,
   *  `0xFFFFFFFF` = unlimited, any other N = N consumes
   *  allowed. */
  max_uses: number;
  /** Server-counted consumes so far. */
  used_count: number;
  /** Owner-supplied free-text label, empty string when not
   *  set. */
  label: string;
  /** Mirrors `InviteCapability.bootstrap_referral` — `true`
   *  flags the link as a signup-deep-link that auto-Accepts
   *  on the joiner side (Phase 1.9b4). */
  bootstrap_referral: boolean;
  /** RFC 3339 timestamp recording when the server first
   *  accepted the upload. */
  created_at: string;
}

/** Wire-level response shape from
 *  `POST /discovery/invite/by-owner`. */
export interface InviteOwnerListResponse {
  invites: InviteOwnerListEntry[];
}

// ----------------------------------------------------------------------------
// /discovery/channel/{register,lookup}
//   (services/discovery/src/channel_names.rs — G4-5, ADR-0010)
// ----------------------------------------------------------------------------

/** Body of `POST /discovery/channel/register`. Binds an OPRF-
 *  hashed `@channelname` to a 16-byte `channel_id` pseudonym.
 *  Same OPRF ciphersuite + wire shape as `RegisterRequest`
 *  (Ristretto255-SHA512, 64-byte hash) — the `/discovery/oprf`
 *  evaluate endpoint is namespace-agnostic, so the client reuses
 *  `OprfBlinder.blind(handle)` verbatim. First-come-first-served
 *  via a UNIQUE on `oprf_hash`; a repeat of the same hash with a
 *  different channel id is 409 Conflict. */
export interface ChannelRegisterRequest {
  pow: PowProofPayload;
  /** base64(VOPRF output) — exactly 64 bytes, from
   *  `blinder.finalize`. */
  oprf_hash: string;
  /** base64(channel_id) — raw 16 bytes (ADR-0010 §"Wire
   *  surfaces"). */
  channel_id: string;
}

/** Response body of `POST /discovery/channel/register`. Returns
 *  409 on collision; the type is only for the 200-OK path. */
export interface ChannelRegisterResponse {
  ok: boolean;
}

/** Body of `POST /discovery/channel/lookup`. Resolves an OPRF-
 *  hashed `@channelname` to its `channel_id`. */
export interface ChannelLookupRequest {
  pow: PowProofPayload;
  /** Same shape as `ChannelRegisterRequest.oprf_hash`. */
  oprf_hash: string;
}

/** Response body of `POST /discovery/channel/lookup`. The server
 *  returns 404 for an unknown `@channelname` — the high-level
 *  client surfaces that as a `null` rather than an error class. */
export interface ChannelLookupResponse {
  /** base64(channel_id) — raw 16 bytes. */
  channel_id: string;
}
