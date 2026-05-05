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
