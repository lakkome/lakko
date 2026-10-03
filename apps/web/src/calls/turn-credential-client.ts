// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * TURN-credential client — the web mirror of iOS `TurnCredentialClient.swift`
 * and Android `TurnCredentialClient.kt`.
 *
 * `POST /call/turn-credential` is bearer-authed and mints a short-lived
 * credential in the coturn REST-API shape (`services/auth/src/turn.rs`): the
 * username is `<unix_expiry>:<account_id_hex>` and the password is
 * `base64(HMAC-SHA1(shared_secret, username))`, which the TURN server
 * recomputes rather than looks up.
 *
 * **Fail-soft by contract, and the same contract the natives keep.** Anything
 * other than a well-formed 200 - a 503 because the deployment has no TURN
 * configured, a network failure, a body that does not parse - resolves to
 * `null`, and the call proceeds on STUN alone. A call that cannot reach a
 * relay is a call that may not connect; a call that refuses to start because
 * the relay could not be reached is one that certainly does not. This is an
 * availability choice about ICE candidates and touches nothing about
 * encryption, so it is not a fallback of the kind security invariant #4 bars.
 */
import { sessionFetch } from "../auth/session-fetch";

/** The auth service, as nginx mounts it for the web client. */
export const DEFAULT_TURN_BASE_URL = "/api/auth";

/** Refresh this many seconds before the credential actually expires, so a
 *  call placed at the boundary does not allocate against a dead one. */
const REFRESH_MARGIN_SECS = 60;

interface TurnCredentialResponse {
  readonly urls: unknown;
  readonly username: unknown;
  readonly password: unknown;
  readonly expiry_unix_secs: unknown;
}

/** A credential plus the moment it stops being usable. */
interface CachedCredential {
  readonly server: RTCIceServer;
  readonly expiryUnixSecs: number;
}

/**
 * Parse the wire body into an `RTCIceServer`, or `null` if it is not one.
 *
 * Exported for tests: the shape is a cross-service contract with
 * `services/auth`, and a silent drift here degrades every call to STUN
 * without an error anywhere.
 */
export function parseTurnCredential(body: unknown): CachedCredential | null {
  if (typeof body !== "object" || body === null) return null;
  const b = body as TurnCredentialResponse;
  if (!Array.isArray(b.urls) || b.urls.length === 0) return null;
  const urls = b.urls.filter((u): u is string => typeof u === "string" && u.length > 0);
  if (urls.length === 0) return null;
  if (typeof b.username !== "string" || b.username.length === 0) return null;
  if (typeof b.password !== "string" || b.password.length === 0) return null;
  const expiry = typeof b.expiry_unix_secs === "number" ? b.expiry_unix_secs : 0;
  return {
    server: { urls, username: b.username, credential: b.password },
    expiryUnixSecs: expiry,
  };
}

/**
 * Holds one credential and re-mints it when it is about to expire.
 *
 * One instance per signed-in session. The token is rebound rather than
 * captured, because login, recover and pairing all replace it and a
 * credential minted against a stale token is refused.
 */
export class TurnCredentialClient {
  private sessionToken: string | null = null;
  private cached: CachedCredential | null = null;
  private inFlight: Promise<RTCIceServer | null> | null = null;

  constructor(
    private readonly baseUrl: string = DEFAULT_TURN_BASE_URL,
    private readonly now: () => number = () => Date.now(),
  ) {}

  setSessionToken(token: string | null): void {
    if (token !== this.sessionToken) this.cached = null;
    this.sessionToken = token;
  }

  /**
   * The ICE server to add to this call's configuration, or `null` for
   * STUN-only.
   *
   * Concurrent callers share one request: two calls placed in the same tick
   * must not mint two credentials, and the TURN server counts allocations.
   */
  async fetchOrNull(): Promise<RTCIceServer | null> {
    const token = this.sessionToken;
    if (token === null || token.length === 0) return null;
    const nowSecs = Math.floor(this.now() / 1000);
    const cached = this.cached;
    if (cached !== null && cached.expiryUnixSecs - REFRESH_MARGIN_SECS > nowSecs) {
      return cached.server;
    }
    if (this.inFlight !== null) return this.inFlight;
    this.inFlight = this.mint(token)
      .catch(() => null)
      .finally(() => {
        this.inFlight = null;
      });
    return this.inFlight;
  }

  private async mint(token: string): Promise<RTCIceServer | null> {
    const res = await sessionFetch(`${this.baseUrl}/call/turn-credential`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}` },
    });
    if (!res.ok) {
      // 503 `turn_not_configured` is the expected answer on a deployment
      // without a relay, and is not worth a console line on every call.
      if (res.status !== 503) {
        // eslint-disable-next-line no-console
        console.warn(`turn credential: HTTP ${res.status}`);
      }
      return null;
    }
    const parsed = parseTurnCredential(await res.json());
    if (parsed === null) {
      // eslint-disable-next-line no-console
      console.warn("turn credential: unexpected body shape");
      return null;
    }
    this.cached = parsed;
    return parsed.server;
  }
}
