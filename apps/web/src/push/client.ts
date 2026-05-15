// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PushClient` — fetch-based client for `lakko-svc-push-gw`.
 *
 * Two endpoints from the user-device side (the third, `/push/notify`,
 * is driven by the DS fan-out — clients themselves never notify):
 *
 *   * `POST /push/register` — bind a `(device_pseudonym, provider)`
 *     pair to a platform-specific token blob. Idempotent: re-register
 *     overwrites the prior token (clients rotate tokens after APNs
 *     rebuild / FCM token refresh / Web Push subscription change).
 *   * `POST /push/unregister` — drop a token. Called on logout +
 *     forget-device.
 *
 * Plus `/pow/challenge` for the per-call PoW seed (same shape as the
 * auth + discovery PoW path).
 *
 * Errors mirror the auth + discovery client surface:
 *
 *   * [`PushClientArgError`] — local input validation (wrong-length
 *     pseudonym, empty token) caught before any round-trip.
 *   * [`PushHttpError`] — server returned non-2xx; carries the status
 *     + endpoint so the UI can branch on 400 (rejection / PoW fail)
 *     vs 5xx.
 *   * [`PushNetworkError`] — DNS / TLS / transport failure.
 */
import { base64Encode } from "../auth/account-id";
import { solvePow } from "../auth/pow";
import type { PowProofPayload } from "../auth/wire";

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

interface PowChallengeResponse {
  seed: string;
  effort_bits: number;
}

interface RegisterRequest {
  pow: PowProofPayload;
  device_pseudonym: string;
  provider: ProviderKind;
  token: string;
}

interface RegisterResponse {
  ok: boolean;
}

interface UnregisterRequest {
  pow: PowProofPayload;
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
  constructor(public readonly baseUrl: string = DEFAULT_BASE_URL) {}

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

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const req: RegisterRequest = {
      pow,
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
   * Subscribe this device to wakes for `groupPseudonym`. Phase
   * 1.20e: publishers call [`notifyGroup`] after each successful
   * `chat.sendMessage`, and the push-gw fans out to every device
   * with a row here.
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

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const resp = await this.post<
      { pow: PowProofPayload; device_pseudonym: string; group_pseudonym: string },
      { ok: boolean; inserted: boolean }
    >("/push/group/subscribe", {
      pow,
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

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const resp = await this.post<
      { pow: PowProofPayload; device_pseudonym: string; group_pseudonym: string },
      { ok: boolean; removed: boolean }
    >("/push/group/unsubscribe", {
      pow,
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
   * Fan a wake out to every device subscribed to
   * `groupPseudonym`. Phase 1.20e: the publisher's client calls
   * this right after `chat.sendMessage` succeeds. The push-gw
   * looks up the subscription table + dispatches per device via
   * the same provider machinery as `/push/notify`.
   *
   * @param groupPseudonym 32-byte group id (same value DS uses
   *                       as the journal key).
   * @param envelopePointer Optional DS message id; lets the
   *                        woken client jump to the exact row
   *                        instead of polling.
   */
  async notifyGroup(
    groupPseudonym: Uint8Array,
    envelopePointer?: number,
  ): Promise<{ devicesWoken: number }> {
    const groupHex = pseudonymHex(groupPseudonym, "groupPseudonym");

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    interface NotifyGroupReq {
      pow: PowProofPayload;
      group_pseudonym: string;
      envelope_pointer?: number;
    }
    const req: NotifyGroupReq = {
      pow,
      group_pseudonym: groupHex,
    };
    if (envelopePointer !== undefined) {
      req.envelope_pointer = envelopePointer;
    }
    const resp = await this.post<
      NotifyGroupReq,
      { devices_woken: number; deliveries: unknown[] }
    >("/push/notify-group", req);
    return { devicesWoken: resp.devices_woken };
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

    const challenge = await this.fetchChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const req: UnregisterRequest = {
      pow,
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

  private async fetchChallenge(): Promise<PowChallengeResponse> {
    return this.post<undefined, PowChallengeResponse>("/pow/challenge", undefined);
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
      throw new PushNetworkError(endpoint, err);
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
