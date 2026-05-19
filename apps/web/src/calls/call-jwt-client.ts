// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Client-side HTTP helper for the Phase 3b4 call-admission JWT
 * pipeline.
 *
 *   * `mint(callId, role)`           — `POST /call/jwt` with a
 *                                       bearer session token.
 *                                       Returns the JWT + the
 *                                       parsed claims (parsed
 *                                       UNVERIFIED — fine for
 *                                       UI surfaces; verification
 *                                       happens at the SFU side).
 *   * `fetchJwks()`                  — `GET /.well-known/jwks.json`.
 *                                       Anonymous; cacheable.
 *                                       Caller usually caches the
 *                                       result for 24 h.
 *   * `verifyAgainstJwks(jwt, jwks)` — Defensive self-verify
 *                                       against a fetched JWKS
 *                                       doc. Useful for client
 *                                       UIs that want to display
 *                                       the role / expiry from a
 *                                       cryptographically-validated
 *                                       claim set rather than the
 *                                       unverified parse.
 *
 * The crypto + wire-format primitives live in `lakko-call-jwt`
 * and are surfaced through `wasm-bindgen` as the
 * `parseCallJwtUnverified` + `verifyCallJwtWithJwks` free
 * functions. This module is the **HTTP transport** layer on
 * top of them.
 *
 * The SFU itself (3b1c, Go-side) consumes the JWKS doc + the
 * JWT directly; the client doesn't talk to the SFU through
 * this module. This is exclusively the auth-service-side
 * mint + JWKS fetch.
 *
 * See `docs/security/calls.md` C-15..C-23 for the invariants
 * the underlying primitives preserve.
 */

import type { Wasm } from "../wasm";

// ---- Public types --------------------------------------------------

/**
 * Participation role baked into the minted JWT. Mirror of
 * the Rust `CallRole` enum; the wire form is lower-case.
 */
export type CallRole = "participant" | "publisher" | "subscriber";

/**
 * Result of `mint()`. The `claims` field is the
 * **unverified** parse of the JWT body — fine for UI
 * surfaces. Production callers that need a verifying parse
 * should call `verifyAgainstJwks(jwt, fetchedJwks)` and
 * read the result of THAT call instead.
 */
export interface MintCallJwtResult {
  readonly jwt: string;
  readonly expiresAtUnixSecs: number;
  readonly claims: ParsedCallJwt;
}

/**
 * Flat claim set returned by `parseCallJwtUnverified` and
 * `verifyAgainstJwks`. The shape mirrors the Rust
 * `ParsedCallJwt` wasm-bindgen class but as a plain object
 * so consumers don't carry a wasm handle around.
 */
export interface ParsedCallJwt {
  readonly iss: string;
  readonly aud: string;
  readonly sub: string;
  readonly callId: string;
  readonly role: CallRole;
  readonly iat: number;
  readonly exp: number;
  readonly kid: string;
}

// ---- Errors --------------------------------------------------------

/** Caller misuse — bad arg shape before any I/O happens. */
export class CallJwtArgError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CallJwtArgError";
  }
}

/** Server returned a non-2xx response. */
export class CallJwtHttpError extends Error {
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    body: string,
  ) {
    super(
      `${endpoint} failed with HTTP ${status}${body ? `: ${body}` : ""}`,
    );
    this.name = "CallJwtHttpError";
  }
}

/** Network transport failure — DNS, TLS, abort, etc. */
export class CallJwtNetworkError extends Error {
  constructor(public readonly endpoint: string, public readonly cause: unknown) {
    super(`${endpoint} network error: ${String(cause)}`);
    this.name = "CallJwtNetworkError";
  }
}

/** wasm-bindgen verify path rejected. */
export class CallJwtVerifyError extends Error {
  constructor(public readonly endpoint: string, public readonly cause: unknown) {
    super(`call-JWT verify failed: ${String(cause)}`);
    this.name = "CallJwtVerifyError";
  }
}

// ---- Default config ------------------------------------------------

/** Base URL the auth service is mounted at — mirror of
 *  `AuthClient.DEFAULT_BASE_URL` so the two clients live
 *  on the same origin / prefix. */
export const DEFAULT_BASE_URL = "/api/auth";

/** `call_id` is a 16-byte random value rendered as 32 hex
 *  chars. The server enforces this; we pre-validate to
 *  give a clear caller-side error before round-tripping
 *  through fetch. */
const CALL_ID_PATTERN = /^[0-9a-f]{32}$/;

/** Default JWKS cache TTL per ADR-0008. 24 hours matches
 *  the recommended SFU refresh cadence. */
export const DEFAULT_JWKS_TTL_MS = 24 * 60 * 60 * 1000;

interface JwksCacheEntry {
  readonly bytes: Uint8Array;
  readonly fetchedAtMs: number;
}

// ---- Client --------------------------------------------------------

/**
 * Mint + JWKS fetch + self-verify client. Hold one instance
 * per signed-in session; rebind the bearer token on
 * sign-in / sign-out via `setSessionToken`.
 *
 * The class is intentionally HTTP-only — it never reaches
 * for `globalThis.fetch` until you call a method, so it's
 * safe to construct in a non-browser context (SSR, tests,
 * SW bootstrap). The wasm module is also lazily-required
 * — only `parseUnverified()` / `verifyAgainstJwks()` need
 * it.
 */
export class CallJwtClient {
  private sessionToken: string | null = null;
  private jwksCache: JwksCacheEntry | null = null;

  constructor(
    private readonly wasm: Wasm,
    public readonly baseUrl: string = DEFAULT_BASE_URL,
    /** Time-to-live for the in-memory JWKS cache. Defaults
     *  to 24 h (ADR-0008). Passing `0` disables caching —
     *  every call fetches fresh. Negative values are
     *  treated as 0. */
    public readonly jwksTtlMs: number = DEFAULT_JWKS_TTL_MS,
  ) {}

  /** Rebind the bearer token. `null` clears (sign-out). */
  setSessionToken(token: string | null): void {
    this.sessionToken = token;
  }

  /** Drop the cached JWKS doc. Call after a `verifyJwt`
   *  failure that suggests a key rotation has happened
   *  (the convenience `verifyJwt` does this for you on
   *  `no JWK matches header.kid` errors). */
  clearJwksCache(): void {
    this.jwksCache = null;
  }

  /**
   * `POST /call/jwt` — mints a JWT scoped to
   * `(account_id-from-bearer, call_id, role)`. Returns the
   * raw JWS Compact representation along with the
   * **unverified** parsed claims for UI convenience.
   *
   * Throws [`CallJwtArgError`] if `callId` doesn't match
   * `^[0-9a-f]{32}$` or no bearer token is set.
   * Throws [`CallJwtHttpError`] on a non-2xx response
   * (401 on missing / bad bearer, 400 on the server's
   * own `call_id` validation, 500 on a mint failure).
   * Throws [`CallJwtNetworkError`] on transport errors.
   */
  async mint(callId: string, role: CallRole = "participant"): Promise<MintCallJwtResult> {
    if (!CALL_ID_PATTERN.test(callId)) {
      throw new CallJwtArgError(
        `call_id must be 32 lower-case hex chars (RegExp /^[0-9a-f]{32}$/), got ${callId.length} chars`,
      );
    }
    if (this.sessionToken === null) {
      throw new CallJwtArgError(
        "session token is not bound — call setSessionToken before mint()",
      );
    }
    const endpoint = "/call/jwt";
    let response: Response;
    try {
      response = await fetch(`${this.baseUrl}${endpoint}`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${this.sessionToken}`,
          "content-type": "application/json",
        },
        body: JSON.stringify({ call_id: callId, role }),
      });
    } catch (err) {
      throw new CallJwtNetworkError(endpoint, err);
    }
    if (!response.ok) {
      const body = await safeReadBody(response);
      throw new CallJwtHttpError(response.status, endpoint, body);
    }
    const json = (await response.json()) as {
      jwt: string;
      expires_at_unix_secs: number;
    };
    return {
      jwt: json.jwt,
      expiresAtUnixSecs: json.expires_at_unix_secs,
      claims: this.parseUnverified(json.jwt),
    };
  }

  /**
   * `GET /.well-known/jwks.json` — fetches the auth
   * service's published JWK Set. The returned bytes are
   * the raw JSON document; pass them straight into
   * `verifyAgainstJwks(jwt, jwksBytes)` or hold them.
   *
   * Anonymous endpoint — no bearer token required.
   *
   * **Caches in-memory for `jwksTtlMs` ms (default 24 h
   * per ADR-0008).** Pass `forceRefresh = true` to bypass
   * the cache; on a `kid` mismatch during verify the
   * convenience `verifyJwt` does this for you. Direct
   * cache invalidation also via `clearJwksCache()`.
   */
  async fetchJwks(forceRefresh: boolean = false): Promise<Uint8Array> {
    if (!forceRefresh && this.jwksCache !== null) {
      const ageMs = Date.now() - this.jwksCache.fetchedAtMs;
      if (ageMs < this.jwksTtlMs) {
        return this.jwksCache.bytes;
      }
    }
    const endpoint = "/.well-known/jwks.json";
    let response: Response;
    try {
      response = await fetch(`${this.baseUrl}${endpoint}`, {
        method: "GET",
        headers: { accept: "application/json" },
      });
    } catch (err) {
      throw new CallJwtNetworkError(endpoint, err);
    }
    if (!response.ok) {
      const body = await safeReadBody(response);
      throw new CallJwtHttpError(response.status, endpoint, body);
    }
    const buf = await response.arrayBuffer();
    const bytes = new Uint8Array(buf);
    this.jwksCache = { bytes, fetchedAtMs: Date.now() };
    return bytes;
  }

  /**
   * Parse the JWT WITHOUT verifying the signature.
   * Useful for UI surfaces that want to read the role /
   * expiry off a JWT the caller already has. The result
   * should NOT drive any authorisation decision; the SFU
   * does the load-bearing verification.
   *
   * Wraps the `parseCallJwtUnverified` wasm-bindgen free
   * function.
   *
   * @throws CallJwtVerifyError on a malformed JWT
   */
  parseUnverified(jwt: string): ParsedCallJwt {
    try {
      const parsed = this.wasm.parseCallJwtUnverified(jwt);
      return flattenParsed(parsed);
    } catch (err) {
      throw new CallJwtVerifyError("parseCallJwtUnverified", err);
    }
  }

  /**
   * Verify a JWT against a cached JWKS doc. Returns the
   * cryptographically-validated claim set. `nowUnixSecs`
   * defaults to `Math.floor(Date.now() / 1000)`; tests
   * inject a fixed value to drive the `exp` check.
   *
   * Wraps the `verifyCallJwtWithJwks` wasm-bindgen free
   * function. This is a defensive self-verify path — the
   * SFU does the same check authoritatively.
   *
   * @throws CallJwtVerifyError on any verification failure
   *         (malformed JWT, signature mismatch, expired,
   *         unknown kid, JWK shape mismatch, …)
   */
  verifyAgainstJwks(
    jwt: string,
    jwksDocBytes: Uint8Array,
    nowUnixSecs: number = Math.floor(Date.now() / 1000),
  ): ParsedCallJwt {
    try {
      const parsed = this.wasm.verifyCallJwtWithJwks(
        jwksDocBytes,
        jwt,
        BigInt(nowUnixSecs),
      );
      return flattenParsed(parsed);
    } catch (err) {
      throw new CallJwtVerifyError("verifyCallJwtWithJwks", err);
    }
  }

  /**
   * Convenience: fetch JWKS (cache-aware) + verify in one
   * call. On a `no JWK matches header.kid` failure (the
   * canonical key-rotation signal), invalidates the
   * cache, refetches once, retries the verify. Any second
   * failure surfaces verbatim.
   *
   * Use this when the caller has a JWT and just wants the
   * validated claims back; the cache + rotation handling
   * are invisible.
   */
  async verifyJwt(
    jwt: string,
    nowUnixSecs: number = Math.floor(Date.now() / 1000),
  ): Promise<ParsedCallJwt> {
    const cached = await this.fetchJwks();
    try {
      return this.verifyAgainstJwks(jwt, cached, nowUnixSecs);
    } catch (firstErr) {
      if (!isKidMismatch(firstErr)) {
        throw firstErr;
      }
      // Rotation case — force a fresh JWKS pull, retry once.
      this.clearJwksCache();
      const fresh = await this.fetchJwks(true);
      return this.verifyAgainstJwks(jwt, fresh, nowUnixSecs);
    }
  }
}

/** Discriminate the `no JWK matches header.kid` shape from
 *  unrelated verify failures. The wasm-bindgen surface
 *  wraps the Rust error message verbatim into a JsError;
 *  we string-match on the same substring `lakko-call-jwt`
 *  emits. */
function isKidMismatch(err: unknown): boolean {
  const msg = String((err as { message?: string })?.message ?? err);
  return msg.includes("no JWK matches header.kid") || msg.includes("kid");
}

// ---- Helpers -------------------------------------------------------

interface WasmParsedCallJwt {
  readonly iss: string;
  readonly aud: string;
  readonly sub: string;
  readonly callId: string;
  readonly role: string;
  readonly iat: bigint;
  readonly exp: bigint;
  readonly kid: string;
}

function flattenParsed(parsed: WasmParsedCallJwt): ParsedCallJwt {
  const role = parsed.role;
  if (role !== "participant" && role !== "publisher" && role !== "subscriber") {
    throw new CallJwtVerifyError(
      "flattenParsed",
      `unknown role discriminator: ${role}`,
    );
  }
  return {
    iss: parsed.iss,
    aud: parsed.aud,
    sub: parsed.sub,
    callId: parsed.callId,
    role,
    iat: Number(parsed.iat),
    exp: Number(parsed.exp),
    kid: parsed.kid,
  };
}

async function safeReadBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 512 ? `${text.slice(0, 512)}…` : text;
  } catch {
    return "<unreadable body>";
  }
}

// ---- JWT id helper -------------------------------------------------

/**
 * Mint a 16-byte cryptographically-random `call_id` and
 * render it as 32 lower-case hex chars — the shape the
 * server expects in `POST /call/jwt`.
 *
 * Uses `crypto.getRandomValues` directly; the random
 * primitive does NOT round-trip through wasm so the call
 * is synchronous + cheap.
 *
 * Browser-only — throws in environments without
 * `globalThis.crypto`.
 */
export function mintCallId(): string {
  if (typeof globalThis.crypto?.getRandomValues !== "function") {
    throw new CallJwtArgError(
      "globalThis.crypto.getRandomValues is unavailable — mintCallId requires a Web Crypto-capable environment",
    );
  }
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  let out = "";
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, "0");
  }
  return out;
}
