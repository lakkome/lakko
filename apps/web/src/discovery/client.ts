// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `DiscoveryClient` — fetch-based client for `lakko-svc-discovery`.
 *
 * Two layers, same shape as `auth/client.ts`:
 *
 *   1. **Low-level methods** mirror the three HTTP endpoints 1:1
 *      (`/discovery/oprf`, `/discovery/register`,
 *      `/discovery/lookup`). Take a typed body, return a typed
 *      response, do nothing else.
 *
 *   2. **High-level `register(accountId, username)` and
 *      `lookup(username)`** orchestrate the full multi-round-trip
 *      flow: PoW → OPRF blind → POST `/discovery/oprf` → finalize
 *      → second PoW → POST `/discovery/{register, lookup}`. Both
 *      orchestrators handle username normalisation internally
 *      (via the WASM `OprfBlinder`); callers pass the raw typed
 *      string.
 *
 * Errors mirror the auth-side surface — `DiscoveryClientArgError` /
 * `DiscoveryHttpError` / `DiscoveryNetworkError` — so the UI can
 * distinguish "username taken" (HTTP 409 from `/register`) and
 * "username unknown" (HTTP 404 from `/lookup`) from network-level
 * failures.
 */
import { loadWasm } from "../wasm";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import { solvePow } from "../auth/pow";
import type {
  IdentityFetchResponse,
  IdentityUploadRequest,
  IdentityUploadResponse,
  InviteConsumeRequest,
  InviteConsumeResponse,
  InviteFetchResponse,
  InviteRevokeRequest,
  InviteUploadRequest,
  InviteUploadResponse,
  KeyPackageFetchResponse,
  KeyPackageUploadRequest,
  KeyPackageUploadResponse,
  LookupRequest,
  LookupResponse,
  OprfRequest,
  OprfResponse,
  RegisterRequest,
  RegisterResponse,
} from "./wire";

/** Default base URL — Vite dev proxy (`/api/discovery` →
 *  `127.0.0.1:8084`). Production nginx routes the same path. */
export const DEFAULT_BASE_URL = "/api/discovery";

/** Result of `DiscoveryClient.register`. */
export interface RegisterResult {
  /** The canonical form actually claimed (after NFKC + lowercase
   *  + trim + leading-`@` strip). Surface back to the UI so the
   *  user knows what handle is now bound to their account. */
  normalisedUsername: string;
}

/** Result of `DiscoveryClient.lookup`. The server returns 404
 *  for unknown usernames; the high-level orchestrator surfaces
 *  that as `account: null` rather than an exception, so a
 *  not-found is visible in the type. */
export interface LookupResult {
  normalisedUsername: string;
  /** Raw 16-byte account_id, or `null` if the username is not
   *  registered. */
  account: Uint8Array | null;
}

/** Result of `DiscoveryClient.consumeInvite` (Phase 1.18d2a +
 *  1.18d3c).
 *
 *  Discriminated so the joiner UI can branch cleanly:
 *    * `kind: "consumed"` — server bumped `used_count` and
 *      accepted (fresh consume by this joiner); proceed with
 *      `sendContactRequestToBundle`.
 *    * `kind: "already-consumed"` — server's PK on
 *      `(blob_id, joiner_tag)` already had a row, so the
 *      counter was NOT bumped. UI surfaces "you've already
 *      accepted this invite" + Decline-only — the joiner has
 *      done this before; re-sending a ContactRequest would
 *      land another envelope in the inviter's stranger inbox
 *      and is bad UX.
 *    * `kind: "exhausted"` — 410 Gone, `max_uses` ceiling hit
 *      by other joiners before this consume could land. UI
 *      shows "this invite has been used up" + Decline-only.
 *    * `kind: "not-found"` — 404; the blob_id was never issued
 *      OR has expired (server folds expired into 404 to deny
 *      status-code probing). UI surfaces "invite expired or
 *      revoked".
 *
 *  Other non-2xx codes still throw `DiscoveryHttpError` —
 *  reserve the discriminated path for the four response shapes
 *  the joiner is supposed to act on. */
export type ConsumeInviteResult =
  | { readonly kind: "consumed"; readonly usedCount: number; readonly maxUses: number }
  | { readonly kind: "already-consumed"; readonly usedCount: number; readonly maxUses: number }
  | { readonly kind: "exhausted" }
  | { readonly kind: "not-found" };

/** Result of `DiscoveryClient.revokeInvite` (Phase 1.18d2b-web).
 *
 *  Discriminated so the owner UI can branch cleanly:
 *    * `kind: "revoked"` — server deleted the row.
 *    * `kind: "not-found-or-not-owner"` — 404. Server folds
 *      "no row" / "signature did not verify" / "stored bytes
 *      failed re-decode" into the same status to deny status-
 *      code probing of the blob_id space; the UI surfaces a
 *      generic "this invite no longer exists or you don't own
 *      it" message.
 *
 *  Body-shape failures (bad base64, signed_at_ms outside the
 *  replay window, …) still throw `DiscoveryHttpError(400, …)`
 *  — they indicate a buggy client, not a probing attacker. */
export type RevokeInviteResult =
  | { readonly kind: "revoked" }
  | { readonly kind: "not-found-or-not-owner" };

/** Programmer error — empty / invalid inputs caught before any
 *  network round-trip. Distinct from HTTP errors so the UI can
 *  surface a form-validation message. */
export class DiscoveryClientArgError extends Error {
  override name = "DiscoveryClientArgError";
}

/** Server returned a non-2xx status. Carries the original status
 *  + endpoint so the UI can branch on (e.g.) 409 = taken,
 *  404 = unknown. */
export class DiscoveryHttpError extends Error {
  override name = "DiscoveryHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly body: string,
  ) {
    super(`discovery ${endpoint} → ${status}: ${body}`);
  }
}

/** Network-level failure (DNS, TLS, no route, …). */
export class DiscoveryNetworkError extends Error {
  override name = "DiscoveryNetworkError";
  constructor(public readonly endpoint: string, cause: unknown) {
    super(`discovery ${endpoint} network error: ${String(cause)}`);
  }
}

export class DiscoveryClient {
  constructor(public readonly baseUrl: string = DEFAULT_BASE_URL) {}

  // -----------------------------------------------------------------
  // Low-level: 1:1 HTTP endpoints
  // -----------------------------------------------------------------

  async oprfEvaluate(req: OprfRequest): Promise<OprfResponse> {
    return this.post("/discovery/oprf", req);
  }

  async register(req: RegisterRequest): Promise<RegisterResponse> {
    return this.post("/discovery/register", req);
  }

  /** `POST /discovery/identity` — upsert the identity bundle. */
  async identityUpload(
    req: IdentityUploadRequest,
  ): Promise<IdentityUploadResponse> {
    return this.post("/discovery/identity", req);
  }

  /** `POST /discovery/keypackage` — upsert the user's MLS
   *  KeyPackage. */
  async keypackageUpload(
    req: KeyPackageUploadRequest,
  ): Promise<KeyPackageUploadResponse> {
    return this.post("/discovery/keypackage", req);
  }

  /** `POST /discovery/invite` — upload a verified
   *  `InviteCapability` blob, get the content-addressed
   *  `blob_id` back (Phase 1.18d1). The high-level
   *  [`uploadInvite`] wrapper drives the PoW solve. */
  async inviteUpload(req: InviteUploadRequest): Promise<InviteUploadResponse> {
    return this.post("/discovery/invite", req);
  }

  /** `POST /discovery/invite/{blob_id}/consume` — atomic
   *  `max_uses` enforcement (Phase 1.18d2a). The high-level
   *  [`consumeInvite`] wrapper drives the PoW solve and folds
   *  the 200 / 404 / 410 status trifecta into a discriminated
   *  [`ConsumeInviteResult`]. */
  async inviteConsume(
    blobIdSlug: string,
    req: InviteConsumeRequest,
  ): Promise<InviteConsumeResponse> {
    return this.post(`/discovery/invite/${blobIdSlug}/consume`, req);
  }

  /** `DELETE /discovery/invite/{blob_id}` — owner-signed
   *  revocation (Phase 1.18d2b). The high-level
   *  [`revokeInvite`] wrapper signs the canonical bytes via
   *  `wasm.signInviteRevocation` and folds the 204 / 404
   *  response shapes into a discriminated result.
   *
   *  Note: not implemented as a `post()` because the verb is
   *  DELETE and the body shape is distinct enough that
   *  factoring through the existing JSON helper would obscure
   *  more than it shares. */
  async inviteRevoke(blobIdSlug: string, req: InviteRevokeRequest): Promise<void> {
    const endpoint = `/discovery/invite/${blobIdSlug}`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.status === 204) return;
    const text = await safeReadBody(response);
    throw new DiscoveryHttpError(response.status, `${endpoint} (DELETE)`, text);
  }

  /**
   * `GET /discovery/identity/{accountId_b64url}`. Returns the
   * raw bundle on 200, `null` on 404 (typed not-found path),
   * throws on any other non-2xx.
   *
   * The path uses **base64url-without-padding** because axum
   * rejects `+` and `/` in path segments; the rest of the wire
   * format uses standard-alphabet base64.
   */
  async identityFetch(
    accountId: Uint8Array,
  ): Promise<IdentityFetchResponse | null> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    const accountIdB64Url = base64UrlEncodeNoPad(accountId);
    const url = `${this.baseUrl}/discovery/identity/${accountIdB64Url}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/identity (GET)", err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/identity (GET)",
        text,
      );
    }
    return (await response.json()) as IdentityFetchResponse;
  }

  /**
   * `GET /discovery/keypackage/{accountId_b64url}`. Returns the
   * raw KeyPackage shape on 200, `null` on 404, throws on
   * other non-2xx.
   */
  async keypackageFetch(
    accountId: Uint8Array,
  ): Promise<KeyPackageFetchResponse | null> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    const accountIdB64Url = base64UrlEncodeNoPad(accountId);
    const url = `${this.baseUrl}/discovery/keypackage/${accountIdB64Url}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/keypackage (GET)", err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/keypackage (GET)",
        text,
      );
    }
    return (await response.json()) as KeyPackageFetchResponse;
  }

  /**
   * `POST /discovery/lookup`. Returns the raw response on 200,
   * `null` on 404 (the typed not-found path), throws on any
   * other non-2xx.
   */
  async lookupRaw(req: LookupRequest): Promise<LookupResponse | null> {
    const url = `${this.baseUrl}/discovery/lookup`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/lookup", err);
    }
    if (response.status === 404) {
      return null;
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/lookup",
        text,
      );
    }
    return (await response.json()) as LookupResponse;
  }

  // -----------------------------------------------------------------
  // High-level: full register / lookup flows
  // -----------------------------------------------------------------

  /**
   * Bind `@username` to `accountId` server-side.
   *
   * Flow: PoW₁ → OPRF blind → POST `/discovery/oprf` → finalize →
   * PoW₂ → POST `/discovery/register`.
   *
   * Username normalisation is handled inside `OprfBlinder.blind`
   * (the WASM helper); the result is also surfaced back via
   * `RegisterResult.normalisedUsername` so the UI can show what
   * was actually registered.
   *
   * Throws `DiscoveryHttpError` with `status === 409` on
   * username collision (server-side `INSERT IGNORE` semantics —
   * usernames are first-come-first-served and immutable in
   * Phase 1.5 per `services/discovery/src/registry.rs` notes).
   */
  async registerUsername(
    accountId: Uint8Array,
    username: string,
  ): Promise<RegisterResult> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blind(username);
    const normalisedUsername = blinder.normalizedUsername;

    // PoW₁ for /discovery/oprf.
    const challenge1 = await this.fetchChallenge();
    const pow1 = await solvePow(challenge1.seed, challenge1.effort_bits);
    const oprfResp = await this.oprfEvaluate({
      pow: pow1,
      blinded: blinder.blindedB64,
    });

    const oprfHashBytes = blinder.finalize(oprfResp.evaluated);
    const oprfHashB64 = base64Encode(oprfHashBytes);

    // PoW₂ for /discovery/register.
    const challenge2 = await this.fetchChallenge();
    const pow2 = await solvePow(challenge2.seed, challenge2.effort_bits);

    const registerResp = await this.register({
      pow: pow2,
      account_id: base64Encode(accountId),
      oprf_hash: oprfHashB64,
    });
    if (!registerResp.ok) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/register",
        "server reported ok=false",
      );
    }
    return { normalisedUsername };
  }

  /**
   * Upload (or replace) the identity bundle for the given
   * account. Drives one PoW solve + `POST /discovery/identity`.
   *
   * Idempotent — the server upserts via
   * `INSERT … ON DUPLICATE KEY UPDATE`. Calling this on every
   * login with a deterministically-derived Identity (1.16d3b2)
   * is a cheap no-op when the keys haven't changed.
   */
  async uploadIdentityBundle(
    accountId: Uint8Array,
    signaturePubkey: Uint8Array,
    x25519Pubkey: Uint8Array,
  ): Promise<void> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    if (signaturePubkey.length !== 32) {
      throw new DiscoveryClientArgError(
        "signaturePubkey must be 32 bytes (Ed25519)",
      );
    }
    if (x25519Pubkey.length !== 32) {
      throw new DiscoveryClientArgError(
        "x25519Pubkey must be 32 bytes (HPKE init_key)",
      );
    }

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const resp = await this.identityUpload({
      pow,
      account_id: base64Encode(accountId),
      signature_pubkey: base64Encode(signaturePubkey),
      x25519_pubkey: base64Encode(x25519Pubkey),
    });
    if (!resp.ok) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/identity",
        "server reported ok=false",
      );
    }
  }

  /**
   * Upload (or replace) the user's MLS KeyPackage. Drives one
   * PoW solve + `POST /discovery/keypackage`.
   *
   * Unlike the identity bundle, KeyPackages are **not**
   * deterministic — `Identity::create_key_package()` generates
   * fresh randomness on every call (RFC 9420 forward-secrecy
   * property). Calling this on every login refreshes the
   * `init_key` on the server, so a fresh sender starting a new
   * 1×1 group gets bytes the recipient hasn't already used.
   *
   * The Phase-1 directory only stores ONE KeyPackage per
   * account at a time (consume-on-use is 1.16e6+ work), so a
   * sender racing two sessions can in theory seed two groups
   * with the same `init_key`. Documented MVP trade-off.
   */
  async uploadKeyPackage(
    accountId: Uint8Array,
    keyPackageBytes: Uint8Array,
  ): Promise<void> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    if (keyPackageBytes.length === 0) {
      throw new DiscoveryClientArgError("keyPackageBytes must be non-empty");
    }
    // Hard ceiling mirrors `MAX_KEYPACKAGE_BYTES = 2048` on the
    // server. Surfacing here saves a round-trip on a
    // misbehaving caller.
    const MAX = 2048;
    if (keyPackageBytes.length > MAX) {
      throw new DiscoveryClientArgError(
        `keyPackageBytes exceeds the ${MAX}-byte ceiling ` +
          `(got ${keyPackageBytes.length} bytes)`,
      );
    }

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const resp = await this.keypackageUpload({
      pow,
      account_id: base64Encode(accountId),
      keypackage: base64Encode(keyPackageBytes),
    });
    if (!resp.ok) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/keypackage",
        "server reported ok=false",
      );
    }
  }

  /**
   * Resolve `accountId → KeyPackage bytes` (TLS-encoded MLS).
   * Returns `null` when the recipient hasn't published one yet.
   *
   * Convenience wrapper around `keypackageFetch` that decodes
   * the base64 wrapper to `Uint8Array` so the caller can hand
   * the bytes straight to `wasm.Group.addMember(...)`.
   */
  async fetchKeyPackage(
    accountId: Uint8Array,
  ): Promise<Uint8Array | null> {
    const raw = await this.keypackageFetch(accountId);
    if (raw === null) return null;
    return base64Decode(raw.keypackage);
  }

  /**
   * Upload a freshly-composed `InviteCapability` blob and
   * receive the server-assigned content-addressed `blob_id`
   * (raw 32-byte SHA-256). Phase 1.18d1: drives one PoW solve
   * + `POST /discovery/invite`. Idempotent — same bytes yield
   * the same id, and the server upserts.
   *
   * Caller responsibility: feed `capabilityBytes` straight from
   * `wasm.InviteCapability.compose(...)`. The server runs
   * `decode_and_verify` server-side, so a junk upload surfaces
   * as `DiscoveryHttpError(400, "/discovery/invite", …)`.
   *
   * @param capabilityBytes  The 1.18a wire-format bytes.
   * @returns The 32-byte `blob_id`, ready to substitute into a
   *          pointer URL via `composePointerInviteUrl`.
   *
   * @throws `DiscoveryClientArgError` on local input validation,
   *         `DiscoveryHttpError` on server rejection (PoW /
   *         malformed bytes / failed verify),
   *         `DiscoveryNetworkError` on transport failure.
   */
  async uploadInvite(capabilityBytes: Uint8Array): Promise<Uint8Array> {
    if (capabilityBytes.length === 0) {
      throw new DiscoveryClientArgError(
        "capabilityBytes must be non-empty",
      );
    }
    // Mirror the server-side `MAX_CAPABILITY_BYTES = 2048`
    // ceiling so a misbehaving caller fails fast without a
    // round-trip.
    const MAX = 2048;
    if (capabilityBytes.length > MAX) {
      throw new DiscoveryClientArgError(
        `capabilityBytes exceeds the ${MAX}-byte ceiling ` +
          `(got ${capabilityBytes.length} bytes)`,
      );
    }

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const resp = await this.inviteUpload({
      pow,
      capability: base64Encode(capabilityBytes),
    });

    // Server returns base64url-no-pad; decode back to raw 32
    // bytes via the same helper we use for `accountId` slugs.
    const blobId = base64UrlDecodeNoPad(resp.blob_id);
    if (blobId.length !== 32) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/invite",
        `server returned blob_id of ${blobId.length} bytes, expected 32`,
      );
    }
    return blobId;
  }

  /**
   * Fetch a previously-uploaded `InviteCapability` by its
   * content-addressed `blob_id`. Returns `null` on 404 (the
   * server folds expired blobs into the same status — see
   * `services/discovery/src/invite.rs::fetch` for why we don't
   * use 410). Returns the raw capability bytes on 200; the
   * caller drives `wasm.InviteCapability.decodeAndVerify(...)`
   * to validate the signature locally (the server already
   * verified at upload time, but we re-verify so a malicious
   * server cannot serve substituted bytes — and the joiner can
   * cross-check `SHA-256(bytes) === blobId` if they want).
   */
  async fetchInvite(blobId: Uint8Array): Promise<Uint8Array | null> {
    if (blobId.length !== 32) {
      throw new DiscoveryClientArgError("blobId must be 32 bytes");
    }
    const slug = base64UrlEncodeNoPad(blobId);
    const url = `${this.baseUrl}/discovery/invite/${slug}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/invite (GET)", err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/invite (GET)",
        text,
      );
    }
    const raw = (await response.json()) as InviteFetchResponse;
    return base64Decode(raw.capability);
  }

  /**
   * Atomically bump the consume counter for the invite at
   * `blobId` with per-joiner dedup (Phase 1.18d2a + 1.18d3c).
   * Returns a discriminated [`ConsumeInviteResult`] so the
   * joiner UI can branch:
   *
   *   * `"consumed"` — server accepted, bumped the counter,
   *     and recorded this joiner in the side table.
   *     Caller proceeds with `sendContactRequestToBundle(...)`.
   *   * `"already-consumed"` — server's `(blob_id, joiner_tag)`
   *     PK already had a row, so the counter was NOT bumped.
   *     Caller surfaces "you've already accepted this invite"
   *     + Decline-only and skips the ContactRequest send (a
   *     fresh CR would land another envelope in the inviter's
   *     stranger inbox; bad UX).
   *   * `"exhausted"` — 410 Gone, the `max_uses` ceiling was
   *     hit by other joiners before this consume could land.
   *     Caller surfaces "this invite has been used up" and
   *     offers Decline only.
   *   * `"not-found"` — 404, never-issued OR expired (server
   *     folds both into the same status). Caller surfaces
   *     "invite expired or revoked".
   *
   * Other non-2xx codes still throw `DiscoveryHttpError` —
   * the discriminated path is reserved for the four response
   * shapes the joiner is supposed to act on without a developer
   * involved.
   *
   * Drives one PoW solve over `services/discovery`'s
   * `/pow/challenge` (rate-limit on consume; same shape as
   * upload), plus one WASM call to compute the per-joiner
   * dedup tag locally before posting.
   *
   * @param blobId            32-byte content-addressed invite id
   *                          (the same value `uploadInvite`
   *                          returned).
   * @param capabilityBytes   The same wire-format bytes the
   *                          server holds — mandatory for the
   *                          tag's per-blob salt derivation.
   *                          The caller already has them after
   *                          `decodeAndVerify` ran on the
   *                          /i/:slug landing path.
   * @param joinerAccountId   The joiner's 16-byte UUIDv7
   *                          account_id. Never sent to the
   *                          server in plaintext — only the
   *                          opaque HMAC tag derived from it
   *                          crosses the wire.
   */
  async consumeInvite(
    blobId: Uint8Array,
    capabilityBytes: Uint8Array,
    joinerAccountId: Uint8Array,
  ): Promise<ConsumeInviteResult> {
    if (blobId.length !== 32) {
      throw new DiscoveryClientArgError("blobId must be 32 bytes");
    }
    if (joinerAccountId.length !== 16) {
      throw new DiscoveryClientArgError("joinerAccountId must be 16 bytes");
    }

    // Compute the per-joiner dedup tag locally before any
    // network call — keeps the joiner_account_id off the wire
    // and lets a slow PoW solve happen in parallel with the
    // tag derivation in future iterations.
    const wasm = await loadWasm();
    const joinerTag = wasm.computeJoinerInviteTag(
      capabilityBytes,
      joinerAccountId,
    );

    const slug = base64UrlEncodeNoPad(blobId);

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const endpoint = `/discovery/invite/${slug}/consume`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          pow,
          joiner_tag: base64Encode(joinerTag),
        } satisfies InviteConsumeRequest),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }
    if (response.status === 404) return { kind: "not-found" };
    if (response.status === 410) return { kind: "exhausted" };
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(response.status, endpoint, text);
    }
    const raw = (await response.json()) as InviteConsumeResponse;
    return {
      kind: raw.already_consumed ? "already-consumed" : "consumed",
      usedCount: raw.used_count,
      maxUses: raw.max_uses,
    };
  }

  /**
   * Owner-signed revocation of an invite blob (Phase 1.18d2b-web).
   *
   * Driven by the same Ed25519 key that signed the capability:
   * `wasm.signInviteRevocation(seed, blobId, signedAtMs)`
   * builds the canonical bytes (`REVOKE_DOMAIN_V1 || blob_id ||
   * signed_at_ms_be`) and signs. We POST `signed_at_ms +
   * signature` as the DELETE body; server recovers the
   * inviter's pubkey from the stored capability bytes and
   * verifies the signature against the same canonical layout.
   *
   * Replay window: the server enforces `±10 min` against its
   * own clock. The web client uses `Date.now()` directly — a
   * machine with a clock more than ~10 min skewed will see
   * 400-bad-window failures, mirroring the consume path.
   *
   * Returns a discriminated [`RevokeInviteResult`]:
   *   * `"revoked"` on 204
   *   * `"not-found-or-not-owner"` on 404 (server folds "no
   *     row" / "bad signature" / "stored bytes failed
   *     re-decode" into one status to deny status-code
   *     probing)
   *
   * Other non-2xx still throw `DiscoveryHttpError`.
   *
   * @param blobId      Raw 32-byte content-addressed id (the
   *                    same value `uploadInvite` returned).
   * @param signingSeed The owner's 32-byte Ed25519 seed —
   *                    typically `chats.identity().ed25519Seed`.
   */
  async revokeInvite(
    blobId: Uint8Array,
    signingSeed: Uint8Array,
  ): Promise<RevokeInviteResult> {
    if (blobId.length !== 32) {
      throw new DiscoveryClientArgError("blobId must be 32 bytes");
    }
    if (signingSeed.length !== 32) {
      throw new DiscoveryClientArgError("signingSeed must be 32 bytes");
    }

    const wasm = await loadWasm();
    // Bind the timestamp once so we sign the SAME value we
    // post; reading `Date.now()` twice could in principle yield
    // distinct values across the await boundary. The server's
    // ±10-min window absorbs the millisecond difference, but
    // pinning the value here is cheap and removes the doubt.
    const signedAtMs = Date.now();
    const signature = wasm.signInviteRevocation(
      signingSeed,
      blobId,
      BigInt(signedAtMs),
    );
    const slug = base64UrlEncodeNoPad(blobId);
    try {
      await this.inviteRevoke(slug, {
        signed_at_ms: signedAtMs,
        signature: base64Encode(signature),
      });
      return { kind: "revoked" };
    } catch (err) {
      if (err instanceof DiscoveryHttpError && err.status === 404) {
        return { kind: "not-found-or-not-owner" };
      }
      throw err;
    }
  }

  /**
   * Resolve `@username` to its raw 16-byte `account_id`, or
   * `null` when the username is not registered.
   *
   * Same flow as `registerUsername` modulo the second endpoint:
   * PoW₁ → OPRF → PoW₂ → POST `/discovery/lookup`.
   */
  async lookup(username: string): Promise<LookupResult> {
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blind(username);
    const normalisedUsername = blinder.normalizedUsername;

    const challenge1 = await this.fetchChallenge();
    const pow1 = await solvePow(challenge1.seed, challenge1.effort_bits);
    const oprfResp = await this.oprfEvaluate({
      pow: pow1,
      blinded: blinder.blindedB64,
    });

    const oprfHashBytes = blinder.finalize(oprfResp.evaluated);
    const oprfHashB64 = base64Encode(oprfHashBytes);

    const challenge2 = await this.fetchChallenge();
    const pow2 = await solvePow(challenge2.seed, challenge2.effort_bits);

    const lookupResp = await this.lookupRaw({
      pow: pow2,
      oprf_hash: oprfHashB64,
    });
    if (lookupResp === null) {
      return { normalisedUsername, account: null };
    }
    return {
      normalisedUsername,
      account: base64Decode(lookupResp.account_id),
    };
  }

  // -----------------------------------------------------------------
  // Internals
  // -----------------------------------------------------------------

  /**
   * Fetch a fresh PoW challenge from the discovery service.
   * `services/discovery` exposes its own `/pow/challenge` (the
   * shape matches `lakko-svc-auth`'s but the seed lives in a
   * separate DB table). We don't reuse the auth service's
   * challenges — the server-side verifier looks up the seed in
   * its own table.
   */
  private async fetchChallenge(): Promise<{
    seed: string;
    effort_bits: number;
  }> {
    return this.post<undefined, { seed: string; effort_bits: number }>(
      "/pow/challenge",
      undefined,
    );
  }

  private async post<Req, Res>(endpoint: string, body: Req): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers:
          body === undefined ? {} : { "content-type": "application/json" },
        body: body === undefined ? null : JSON.stringify(body),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }

    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(response.status, endpoint, text);
    }
    return (await response.json()) as Res;
  }
}

async function safeReadBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 512 ? `${text.slice(0, 512)}…` : text;
  } catch {
    return "<unreadable body>";
  }
}
