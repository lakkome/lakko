// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PushClient` — fetch-based client for `lakko-svc-push-gw`.
 *
 * Device-side endpoints only. A client never asks push-gw to wake anybody:
 * since ADR-0054 a group wake is the `x-lakko-wake` header of the signed DS
 * publish (`ds/wake.ts`), and the DS forwards it to push-gw internally.
 *
 *   * `POST /push/register` — bind a `(device_pseudonym, provider)`
 *     pair to a platform-specific token blob. Idempotent: re-register
 *     overwrites the prior token (clients rotate tokens after APNs
 *     rebuild / FCM token refresh / Web Push subscription change).
 *   * `POST /push/unregister` — drop a token. Called on logout +
 *     forget-device.
 *   * `POST /push/group/{subscribe,unsubscribe}` — (un)bind this device to
 *     a group's `push_gid`.
 *
 * ADR-0054 S3: each of the four routes spends one anonymous `push-register`
 * token (`../tokens/anon-token-pool.ts`) and carries no session - the device
 * pseudonym must never be linkable to an account (invariant #5). No proof of
 * work any more. A `429 anon-token-quota-exhausted` from the pool is silent
 * at the callers: registration is retried at the next launch and the app
 * keeps working by polling.
 *
 * Errors mirror the auth + discovery client surface:
 *
 *   * [`PushClientArgError`] — local input validation (wrong-length
 *     pseudonym, empty token) caught before any round-trip.
 *   * [`PushHttpError`] — server returned non-2xx; carries the status
 *     + endpoint so the UI can branch on 400 (rejection)
 *     vs 5xx.
 *   * [`PushNetworkError`] — DNS / TLS / transport failure.
 */
import { base64Encode } from "../auth/account-id";
import {
  anonTokens,
  anonymousFetchInit,
  type AnonTokenPool,
} from "../tokens/anon-token-pool";

/** Default base URL — Vite dev proxy
 *  (`/api/push` → `127.0.0.1:8085`). Production nginx routes the
 *  same path. */
export const DEFAULT_BASE_URL = "/api/push";

/** Raw device pseudonym length — 32 bytes, 64 lowercase hex chars
 *  on the wire (mirror of the `services/push-gw` registry
 *  `DEVICE_LEN` constant). */
export const DEVICE_PSEUDONYM_LEN = 32;

/** Provider names — must match the snake_case `serde` rename on
 *  `ProviderKind` in `services/push-gw/src/provider.rs`. */
export type ProviderKind = "apns" | "fcm" | "web_push" | "unified_push";

/** Programmer error — wrong-length pseudonym, empty token, etc. */
export class PushClientArgError extends Error {
  override name = "PushClientArgError";
}

/** Server returned a non-2xx status. */
export class PushHttpError extends Error {
  override name = "PushHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly body: string,
  ) {
    super(`push ${endpoint} → ${status}: ${body}`);
  }
}

/** Network-level failure (DNS, TLS, transport). */
export class PushNetworkError extends Error {
  override name = "PushNetworkError";
  constructor(public readonly endpoint: string, cause: unknown) {
    super(`push ${endpoint} network error: ${String(cause)}`);
  }
}

interface RegisterRequest {
  device_pseudonym: string;
  provider: ProviderKind;
  token: string;
}

interface RegisterResponse {
  ok: boolean;
}

interface UnregisterRequest {
  device_pseudonym: string;
  provider: ProviderKind;
}

interface UnregisterResponse {
  ok: boolean;
  removed: boolean;
}

/** Convert a 32-byte buffer to its lowercase-hex form. */
function devicePseudonymHex(bytes: Uint8Array): string {
  return pseudonymHex(bytes, "devicePseudonym");
}

/** Generic 32-byte → lowercase-hex with a typed error message
 *  carrying the named-arg the validation failed for. Used by
 *  both the device + group pseudonym paths (1.20e). */
function pseudonymHex(bytes: Uint8Array, name: string): string {
  if (bytes.length !== DEVICE_PSEUDONYM_LEN) {
    throw new PushClientArgError(
      `${name} must be ${DEVICE_PSEUDONYM_LEN} bytes (got ${bytes.length})`,
    );
  }
  let out = "";
  for (const b of bytes) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}

export class PushClient {
  private readonly tokens: AnonTokenPool;

  constructor(
    public readonly baseUrl: string = DEFAULT_BASE_URL,
    options: { readonly tokens?: AnonTokenPool } = {},
  ) {
    this.tokens = options.tokens ?? anonTokens;
  }

  /**
   * Bind a push token to `(devicePseudonym, provider)`. Idempotent:
   * re-registering with a fresh token overwrites the prior bytes,
   * which is the canonical pattern for APNs / FCM / Web Push token
   * rotation.
   *
   * @param devicePseudonym  32-byte stable per-device handle. Derive
   *                         once per device and persist locally; see
   *                         `apps/web/src/push/device-id.ts`.
   * @param provider         Which delivery path this token is for.
   * @param tokenBytes       Provider-opaque token blob. For Web Push,
   *                         this is the JSON-serialised
   *                         `PushSubscription` (endpoint + p256dh +
   *                         auth keys) encoded as UTF-8.
   */
  async register(
    devicePseudonym: Uint8Array,
    provider: ProviderKind,
    tokenBytes: Uint8Array,
  ): Promise<void> {
    if (tokenBytes.length === 0) {
      throw new PushClientArgError("tokenBytes must be non-empty");
    }
    const deviceHex = devicePseudonymHex(devicePseudonym);

    const req: RegisterRequest = {
      device_pseudonym: deviceHex,
      provider,
      token: base64Encode(tokenBytes),
    };
    const resp = await this.post<RegisterRequest, RegisterResponse>(
      "/push/register",
      req,
    );
    if (!resp.ok) {
      throw new PushHttpError(200, "/push/register", "server reported ok=false");
    }
  }

  /**
   * Subscribe this device to wakes for `groupPseudonym` (the group's
   * ADR-0042 `push_gid`). A publisher's DS publish carries an
   * `x-lakko-wake` header (ADR-0054); the DS forwards it and push-gw fans
   * out to every device with a row here.
   *
   * Idempotent — re-subscribing the same `(device, group)` pair
   * returns `inserted: false` and is a no-op.
   */
  async subscribeGroup(
    devicePseudonym: Uint8Array,
    groupPseudonym: Uint8Array,
  ): Promise<{ inserted: boolean }> {
    const deviceHex = devicePseudonymHex(devicePseudonym);
    const groupHex = pseudonymHex(groupPseudonym, "groupPseudonym");

    const resp = await this.post<
      { device_pseudonym: string; group_pseudonym: string },
      { ok: boolean; inserted: boolean }
    >("/push/group/subscribe", {
      device_pseudonym: deviceHex,
      group_pseudonym: groupHex,
    });
    if (!resp.ok) {
      throw new PushHttpError(
        200,
        "/push/group/subscribe",
        "server reported ok=false",
      );
    }
    return { inserted: resp.inserted };
  }

  /** Drop a `(device, group)` subscription. Idempotent. */
  async unsubscribeGroup(
    devicePseudonym: Uint8Array,
    groupPseudonym: Uint8Array,
  ): Promise<{ removed: boolean }> {
    const deviceHex = devicePseudonymHex(devicePseudonym);
    const groupHex = pseudonymHex(groupPseudonym, "groupPseudonym");

    const resp = await this.post<
      { device_pseudonym: string; group_pseudonym: string },
      { ok: boolean; removed: boolean }
    >("/push/group/unsubscribe", {
      device_pseudonym: deviceHex,
      group_pseudonym: groupHex,
    });
    if (!resp.ok) {
      throw new PushHttpError(
        200,
        "/push/group/unsubscribe",
        "server reported ok=false",
      );
    }
    return { removed: resp.removed };
  }

  /**
   * Drop a previously-registered token. Idempotent: the server
   * returns `removed: false` if the row was already gone (clients
   * can call this on logout without checking the registry first).
   */
  async unregister(
    devicePseudonym: Uint8Array,
    provider: ProviderKind,
  ): Promise<{ removed: boolean }> {
    const deviceHex = devicePseudonymHex(devicePseudonym);

    const req: UnregisterRequest = {
      device_pseudonym: deviceHex,
      provider,
    };
    const resp = await this.post<UnregisterRequest, UnregisterResponse>(
      "/push/unregister",
      req,
    );
    if (!resp.ok) {
      throw new PushHttpError(200, "/push/unregister", "server reported ok=false");
    }
    return { removed: resp.removed };
  }

  /** One registration request: spends one `push-register` token, carries
   *  no session, retried once with another token on a spent / invalid one. */
  private async post<Req, Res>(endpoint: string, body: Req): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    const json = JSON.stringify(body);
    let response: Response;
    try {
      response = await this.tokens.spend("push-register", (headerName, headerValue) =>
        fetch(
          url,
          anonymousFetchInit({
            method: "POST",
            headers: { "content-type": "application/json", [headerName]: headerValue },
            body: json,
          }),
        ),
      );
    } catch (err) {
      if (err instanceof TypeError) throw new PushNetworkError(endpoint, err);
      throw err;
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new PushHttpError(response.status, endpoint, text);
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
