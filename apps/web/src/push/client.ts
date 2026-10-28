// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PushClient` — fetch-based client for `lakko-svc-push-gw`.
 *
 * Device-side endpoints only. A client never asks push-gw to wake anybody:
 * since ADR-0054 a group wake is the `x-lakko-wake` header of the signed DS
 * publish (`ds/wake.ts`), and the DS forwards it to push-gw internally.
 *
 * Since ADR-0078 section 3 push-gw holds no device pseudonym and no token in
 * the clear:
 *
 *   * `POST /push/subscribe` — store (or replace) one sealed subscription:
 *     `{sub_id, push_gid, wake_pk, sealed}`, where `sealed` is this device's
 *     provider tokens sealed under the group's unlock key (wasm
 *     `sealPushSubscription`) and `sub_id` is 16 random bytes this device
 *     keeps per subscription. Re-sending the same `sub_id` replaces the row,
 *     which is how a new provider token reaches push-gw.
 *   * `POST /push/unsubscribe` — drop one row by its `sub_id`.
 *   * `GET /push/wake-key` — the X25519 key every wake is sealed to.
 *
 * ADR-0054 S3: each subscription request spends one anonymous
 * `push-register` token (`../tokens/anon-token-pool.ts`) and carries no
 * session (invariant #5). A `429 anon-token-quota-exhausted` from the pool is
 * silent at the callers: registration is retried later and the app keeps
 * working by polling.
 */
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import {
  anonTokens,
  anonymousFetchInit,
  type AnonTokenPool,
} from "../tokens/anon-token-pool";

/** Default base URL — Vite dev proxy (`/api/push` → `127.0.0.1:8085`).
 *  Production nginx routes the same path. */
export const DEFAULT_BASE_URL = "/api/push";

/** Length of a subscription id (mirror of the Rust `PUSH_SUBSCRIPTION_ID_LEN`). */
export const SUBSCRIPTION_ID_LEN = 16;

/** Length of a `push_gid`, a wake public key and push-gw's wake key. */
export const PUSH_KEY_LEN = 32;

/** Provider names — must match the snake_case names of
 *  `lakko_contacts::push_wake::PushProvider`. */
export type ProviderKind = "apns" | "fcm" | "web_push" | "unified_push";

/** Programmer error — a wrong-length id or key, caught before any request. */
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

/** One sealed subscription as `POST /push/subscribe` takes it. */
export interface SealedSubscription {
  /** 16 random bytes this device keeps for this subscription. */
  readonly subId: Uint8Array;
  /** The group's ADR-0042 `push_gid`. */
  readonly pushGid: Uint8Array;
  /** The group's wake public key (wasm `derivePushWakePublicKey`). */
  readonly wakePk: Uint8Array;
  /** The token list sealed under the group's unlock key. */
  readonly sealed: Uint8Array;
}

function checkLen(bytes: Uint8Array, len: number, name: string): void {
  if (bytes.length !== len) {
    throw new PushClientArgError(`${name} must be ${len} bytes (got ${bytes.length})`);
  }
}

export class PushClient {
  private readonly tokens: AnonTokenPool;

  constructor(
    public readonly baseUrl: string = DEFAULT_BASE_URL,
    options: { readonly tokens?: AnonTokenPool } = {},
  ) {
    this.tokens = options.tokens ?? anonTokens;
  }

  /** Store (or replace) one sealed subscription. */
  async subscribe(sub: SealedSubscription): Promise<void> {
    checkLen(sub.subId, SUBSCRIPTION_ID_LEN, "subId");
    checkLen(sub.pushGid, PUSH_KEY_LEN, "pushGid");
    checkLen(sub.wakePk, PUSH_KEY_LEN, "wakePk");
    if (sub.sealed.length === 0) throw new PushClientArgError("sealed must be non-empty");
    const resp = await this.post<{ ok: boolean }>("/push/subscribe", {
      sub_id: base64UrlEncodeNoPad(sub.subId),
      push_gid: base64UrlEncodeNoPad(sub.pushGid),
      wake_pk: base64UrlEncodeNoPad(sub.wakePk),
      sealed: base64UrlEncodeNoPad(sub.sealed),
    });
    if (!resp.ok) {
      throw new PushHttpError(200, "/push/subscribe", "server reported ok=false");
    }
  }

  /** Drop one subscription by its id. Idempotent. */
  async unsubscribe(subId: Uint8Array): Promise<{ removed: boolean }> {
    checkLen(subId, SUBSCRIPTION_ID_LEN, "subId");
    const resp = await this.post<{ ok: boolean; removed: boolean }>("/push/unsubscribe", {
      sub_id: base64UrlEncodeNoPad(subId),
    });
    if (!resp.ok) {
      throw new PushHttpError(200, "/push/unsubscribe", "server reported ok=false");
    }
    return { removed: resp.removed };
  }

  /** push-gw's X25519 wake key. No token: it is public. */
  async wakeKey(): Promise<Uint8Array> {
    const endpoint = "/push/wake-key";
    let response: Response;
    try {
      response = await fetch(`${this.baseUrl}${endpoint}`, anonymousFetchInit({ method: "GET", headers: {} }));
    } catch (err) {
      throw new PushNetworkError(endpoint, err);
    }
    if (!response.ok) {
      throw new PushHttpError(response.status, endpoint, await safeReadBody(response));
    }
    const body = (await response.json()) as { key?: unknown };
    if (typeof body.key !== "string") {
      throw new PushHttpError(200, endpoint, "no key in the response");
    }
    const key = base64UrlDecodeNoPad(body.key);
    checkLen(key, PUSH_KEY_LEN, "wake key");
    return key;
  }

  /** One subscription request: spends one `push-register` token, carries no
   *  session, retried once with another token on a spent / invalid one. */
  private async post<Res>(endpoint: string, body: unknown): Promise<Res> {
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
      throw new PushHttpError(response.status, endpoint, await safeReadBody(response));
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
