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
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import { solvePow } from "../auth/pow";
import type {
  IdentityFetchResponse,
  IdentityUploadRequest,
  IdentityUploadResponse,
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
