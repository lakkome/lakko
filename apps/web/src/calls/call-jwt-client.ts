// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Client for the call-admission token (ADR-0078 section 2).
 *
 *   * `mintForCall(callId, role)` — `POST /call/jwt`, paid for by one
 *     anonymous `call-join` token from the pool (`x-lakko-anon-token`),
 *     never by the session. The body names the ROOM, computed here from
 *     the call id (`sfuRoomForCall`, `BLAKE3_derive_key("lakko/sfu/room/v1",
 *     call_id)`), so neither the issuer nor the SFU sees the call id or
 *     learns which account joins. The answer carries the JWT (claims:
 *     room, a random peer id, role, times; no account) and a TURN
 *     credential minted in the same exchange, its username tag random.
 *   * `fetchJwks()` — `GET /.well-known/jwks.json`. Anonymous; cached.
 *   * `verifyAgainstJwks(jwt, jwks)` — defensive self-verify.
 *
 * The request is built by `anonymousFetchInit`: no `Authorization`, no
 * cookies, no referrer (the privacy rule of `tokens/anon-token-pool.ts`).
 *
 * See `docs/security/calls.md` C-15..C-23 for the invariants the underlying
 * primitives preserve.
 */

import type { Wasm } from "../wasm";
import { anonTokens, anonymousFetchInit, type AnonTokenPool } from "../tokens/anon-token-pool";

// ---- Public types --------------------------------------------------

/**
 * Participation role baked into the minted JWT. Mirror of
 * the Rust `CallRole` enum; the wire form is lower-case.
 */
export type CallRole = "participant" | "publisher" | "subscriber";

/** Result of a mint. `claims` is the UNVERIFIED parse of the JWT body (fine
 *  for UI); the SFU does the load-bearing verification. */
export interface MintCallJwtResult {
  readonly jwt: string;
  readonly expiresAtUnixSecs: number;
  readonly claims: ParsedCallJwt;
  /** The TURN credential minted beside the token, or `null` when the
   *  deployment has no relay (the call proceeds on STUN alone). */
  readonly turn: RTCIceServer | null;
}

/** Flat claim set returned by `parseCallJwtUnverified` and
 *  `verifyAgainstJwks`. No account, no call id (ADR-0078 section 2). */
export interface ParsedCallJwt {
  readonly iss: string;
  readonly aud: string;
  /** 64 lower-case hex characters: the room. */
  readonly room: string;
  /** 32 lower-case hex characters: the SFU peer id, random per token. */
  readonly peerId: string;
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

/** Base URL the auth service is mounted at. */
export const DEFAULT_BASE_URL = "/api/auth";

/** A room is 32 bytes rendered as 64 lower-case hex characters. */
const ROOM_PATTERN = /^[0-9a-f]{64}$/;

/** Default JWKS cache TTL per ADR-0008. */
export const DEFAULT_JWKS_TTL_MS = 24 * 60 * 60 * 1000;

interface JwksCacheEntry {
  readonly bytes: Uint8Array;
  readonly fetchedAtMs: number;
}

/** The wasm surface this client uses. */
export type CallJwtWasm = Pick<
  Wasm,
  "parseCallJwtUnverified" | "verifyCallJwtWithJwks" | "sfuRoomForCall"
>;

// ---- Client --------------------------------------------------------

/**
 * Mint + JWKS fetch + self-verify client. Holds no session: the mint spends
 * an anonymous token from `tokens` (the app's pool by default).
 */
export class CallJwtClient {
  private jwksCache: JwksCacheEntry | null = null;

  constructor(
    private readonly wasm: CallJwtWasm,
    public readonly baseUrl: string = DEFAULT_BASE_URL,
    /** Time-to-live for the in-memory JWKS cache; `0` disables caching. */
    public readonly jwksTtlMs: number = DEFAULT_JWKS_TTL_MS,
    private readonly tokens: Pick<AnonTokenPool, "spend"> = anonTokens,
    private readonly fetchImpl: typeof fetch = (input, init) => globalThis.fetch(input, init),
  ) {}

  /** Drop the cached JWKS doc. */
  clearJwksCache(): void {
    this.jwksCache = null;
  }

  /** {@link mint} for the room of the 16-byte `callId`. */
  async mintForCall(callId: Uint8Array, role: CallRole = "participant"): Promise<MintCallJwtResult> {
    let room: string;
    try {
      room = this.wasm.sfuRoomForCall(callId);
    } catch (err) {
      throw new CallJwtArgError(`call id must be 16 bytes: ${String(err)}`);
    }
    return this.mint(room, role);
  }

  /**
   * `POST /call/jwt` for `room` (64 lower-case hex), paid for by one
   * `call-join` token. Throws {@link CallJwtArgError} on a malformed room,
   * the pool's errors when no token can be had, {@link CallJwtHttpError} on
   * any other non-2xx (`429 room-admission-limit` included), and
   * {@link CallJwtNetworkError} on transport errors.
   */
  async mint(room: string, role: CallRole = "participant"): Promise<MintCallJwtResult> {
    if (!ROOM_PATTERN.test(room)) {
      throw new CallJwtArgError(`room must be 64 lower-case hex chars, got ${room.length} chars`);
    }
    const endpoint = "/call/jwt";
    let response: Response;
    try {
      response = await this.tokens.spend("call-join", (headerName, headerValue) =>
        this.fetchImpl(
          `${this.baseUrl}${endpoint}`,
          anonymousFetchInit({
            method: "POST",
            headers: { [headerName]: headerValue, "content-type": "application/json" },
            body: JSON.stringify({ room, role }),
          }),
        ),
      );
    } catch (err) {
      if (err instanceof TypeError) throw new CallJwtNetworkError(endpoint, err);
      throw err;
    }
    if (!response.ok) {
      const body = await safeReadBody(response);
      throw new CallJwtHttpError(response.status, endpoint, body);
    }
    const json = (await response.json()) as {
      jwt: string;
      expires_at_unix_secs: number;
      turn?: unknown;
    };
    return {
      jwt: json.jwt,
      expiresAtUnixSecs: json.expires_at_unix_secs,
      claims: this.parseUnverified(json.jwt),
      turn: parseTurnCredential(json.turn),
    };
  }

  /** `GET /.well-known/jwks.json`, cached for `jwksTtlMs`. */
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
      response = await this.fetchImpl(`${this.baseUrl}${endpoint}`, {
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
    const bytes = new Uint8Array(await response.arrayBuffer());
    this.jwksCache = { bytes, fetchedAtMs: Date.now() };
    return bytes;
  }

  /** Parse the JWT WITHOUT verifying the signature (UI only). */
  parseUnverified(jwt: string): ParsedCallJwt {
    try {
      return flattenParsed(this.wasm.parseCallJwtUnverified(jwt));
    } catch (err) {
      throw new CallJwtVerifyError("parseCallJwtUnverified", err);
    }
  }

  /** Verify a JWT against a fetched JWKS doc. */
  verifyAgainstJwks(
    jwt: string,
    jwksDocBytes: Uint8Array,
    nowUnixSecs: number = Math.floor(Date.now() / 1000),
  ): ParsedCallJwt {
    try {
      return flattenParsed(this.wasm.verifyCallJwtWithJwks(jwksDocBytes, jwt, BigInt(nowUnixSecs)));
    } catch (err) {
      throw new CallJwtVerifyError("verifyCallJwtWithJwks", err);
    }
  }

  /** Fetch JWKS (cache-aware) + verify, refetching once on a kid mismatch. */
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
      this.clearJwksCache();
      const fresh = await this.fetchJwks(true);
      return this.verifyAgainstJwks(jwt, fresh, nowUnixSecs);
    }
  }
}

function isKidMismatch(err: unknown): boolean {
  const msg = String((err as { message?: string })?.message ?? err);
  return msg.includes("no JWK matches header.kid") || msg.includes("kid");
}

// ---- Helpers -------------------------------------------------------

interface WasmParsedCallJwt {
  readonly iss: string;
  readonly aud: string;
  readonly room: string;
  readonly peerId: string;
  readonly role: string;
  readonly iat: bigint;
  readonly exp: bigint;
  readonly kid: string;
}

function flattenParsed(parsed: WasmParsedCallJwt): ParsedCallJwt {
  const role = parsed.role;
  if (role !== "participant" && role !== "publisher" && role !== "subscriber") {
    throw new CallJwtVerifyError("flattenParsed", `unknown role discriminator: ${role}`);
  }
  return {
    iss: parsed.iss,
    aud: parsed.aud,
    room: parsed.room,
    peerId: parsed.peerId,
    role,
    iat: Number(parsed.iat),
    exp: Number(parsed.exp),
    kid: parsed.kid,
  };
}

/**
 * The `turn` field of a mint answer as an `RTCIceServer`, or `null` when it
 * is absent or not one (a deployment without a relay answers `null`; a body
 * that does not parse degrades the call to STUN, never fails it - an
 * availability choice about ICE candidates, not an E2EE fallback).
 *
 * Exported for tests: the shape is a contract with `services/auth/turn.rs`.
 */
export function parseTurnCredential(body: unknown): RTCIceServer | null {
  if (typeof body !== "object" || body === null) return null;
  const b = body as { urls?: unknown; username?: unknown; password?: unknown };
  if (!Array.isArray(b.urls)) return null;
  const urls = b.urls.filter((u): u is string => typeof u === "string" && u.length > 0);
  if (urls.length === 0) return null;
  if (typeof b.username !== "string" || b.username.length === 0) return null;
  if (typeof b.password !== "string" || b.password.length === 0) return null;
  return { urls, username: b.username, credential: b.password };
}

/**
 * A TURN provider for a call that mints its credential through the same
 * anonymous exchange (a one-to-one call, which has no SFU token of its own).
 * Resolves `null` on any failure: the call proceeds on STUN alone.
 */
export function turnProviderForCall(
  client: Pick<CallJwtClient, "mintForCall">,
  callId: Uint8Array,
): () => Promise<RTCIceServer | null> {
  let minted: Promise<RTCIceServer | null> | null = null;
  return () => {
    minted ??= client
      .mintForCall(callId, "participant")
      .then((r) => r.turn)
      .catch(() => null);
    return minted;
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

// ---- Call id helper ------------------------------------------------

/**
 * Mint a 16-byte cryptographically-random `call_id` and render it as 32
 * lower-case hex chars.
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
