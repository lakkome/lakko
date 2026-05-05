// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `DsClient` — fetch-based client for `lakko-svc-ds`.
 *
 * The Delivery Service is a thin append-only relay: every
 * message goes into either a per-group journal
 * (`/ds/group/{group_hex}`) or a per-mailbox journal
 * (`/ds/mailbox/{device_hex}`). Both paths key the journal by a
 * 32-byte pseudonym encoded as **lowercase hex** in the URL.
 *
 * Two endpoints per journal kind:
 *
 *   * `POST /ds/{kind}/{pseudonym_hex}/messages`
 *     Body: raw bytes (`application/octet-stream`),
 *     capped server-side at `MAX_MESSAGE_BYTES`.
 *     Response: `{ id, created_at }` — the assigned monotonic
 *     id is the cursor every reader uses on follow-up GETs.
 *
 *   * `GET /ds/{kind}/{pseudonym_hex}/messages?after=&limit=&wait=`
 *     Response: `{ messages: [{ id, payload_b64, created_at }],
 *                  has_more }`. `wait` is the long-poll budget
 *     (seconds) — 0 returns immediately whatever's there.
 *
 * `DsClient` only exposes the **mailbox** flavour today —
 * `lakko-contacts::ContactRequest` lives in mailboxes; group
 * journals come into play with 1×1 chat (1.16e). The two share
 * the same shape so adding the group flavour later is one
 * method.
 *
 * The DS does NOT require PoW or auth in Phase 1. Production
 * deployments will gate writes on auth-issued tokens (Phase
 * 1.5b+); the client signature stays the same.
 */
import { base64Decode } from "../auth/account-id";

/** Default base URL — Vite dev proxy `/api/ds/*` →
 *  `127.0.0.1:8082`. Prod nginx routes the same path. */
export const DEFAULT_BASE_URL = "/api/ds";

/** Lowercase-hex encode a byte array (no `0x` prefix). The DS
 *  path expects exactly 64 lowercase hex chars for a 32-byte
 *  pseudonym. */
export function pseudonymHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    const v = bytes[i] ?? 0;
    out += v.toString(16).padStart(2, "0");
  }
  return out;
}

/** Result of `DsClient.publishMailbox`. */
export interface PublishResult {
  /** Server-assigned monotonic id. Use as the `after` cursor on
   *  subsequent `fetchMailbox` calls. */
  id: number;
  /** ISO-8601 timestamp from the server. */
  createdAt: string;
}

/** Decoded message from the mailbox cursor. */
export interface MailboxMessage {
  id: number;
  /** Raw payload bytes (decoded from the server's `payload_b64`). */
  payload: Uint8Array;
  createdAt: string;
}

/** Result of `DsClient.fetchMailbox`. */
export interface FetchResult {
  messages: MailboxMessage[];
  /** `true` if there are more rows past the last returned id;
   *  client should call again with `after = lastMessage.id`. */
  hasMore: boolean;
}

/** Programmer error — surfaced before any network call. */
export class DsClientArgError extends Error {
  override name = "DsClientArgError";
}

/** Server returned a non-2xx. */
export class DsHttpError extends Error {
  override name = "DsHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly body: string,
  ) {
    super(`ds ${endpoint} → ${status}: ${body}`);
  }
}

/** Network-level failure. */
export class DsNetworkError extends Error {
  override name = "DsNetworkError";
  constructor(public readonly endpoint: string, cause: unknown) {
    super(`ds ${endpoint} network error: ${String(cause)}`);
  }
}

export class DsClient {
  constructor(public readonly baseUrl: string = DEFAULT_BASE_URL) {}

  // -----------------------------------------------------------------
  // Mailbox: POST publish
  // -----------------------------------------------------------------

  /**
   * `POST /ds/mailbox/{pseudonym_hex}/messages` — publish a raw
   * payload to a mailbox. The body is sent as
   * `application/octet-stream`; the server caps the size at
   * `MAX_MESSAGE_BYTES`.
   */
  async publishMailbox(
    pseudonym: Uint8Array,
    payload: Uint8Array,
  ): Promise<PublishResult> {
    if (pseudonym.length !== 32) {
      throw new DsClientArgError("pseudonym must be 32 bytes");
    }
    if (payload.length === 0) {
      throw new DsClientArgError("payload must be non-empty");
    }

    const endpoint = `/ds/mailbox/${pseudonymHex(pseudonym)}/messages`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/octet-stream" },
        // `.slice()` ensures the buffer is an `ArrayBuffer` (not
        // SharedArrayBuffer) — TS strict mode demands the narrower
        // type for `BodyInit`.
        body: payload.slice(),
      });
    } catch (err) {
      throw new DsNetworkError(endpoint, err);
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DsHttpError(response.status, endpoint, text);
    }
    const json = (await response.json()) as { id: number; created_at: string };
    return { id: json.id, createdAt: json.created_at };
  }

  // -----------------------------------------------------------------
  // Mailbox: GET fetch with cursor
  // -----------------------------------------------------------------

  /**
   * `GET /ds/mailbox/{pseudonym_hex}/messages?after=&limit=&wait=`
   * — pull all messages newer than `after` for the given
   * mailbox.
   *
   * `wait` is the long-poll budget in seconds (0 returns
   * immediately whatever's there; non-zero holds the connection
   * up to that many seconds and returns as soon as at least one
   * row arrives). Server clamps to its own `MAX_WAIT_SECS`.
   */
  async fetchMailbox(
    pseudonym: Uint8Array,
    options: { after?: number; limit?: number; waitSecs?: number } = {},
  ): Promise<FetchResult> {
    if (pseudonym.length !== 32) {
      throw new DsClientArgError("pseudonym must be 32 bytes");
    }

    const params = new URLSearchParams();
    if (options.after !== undefined) {
      params.set("after", String(options.after));
    }
    if (options.limit !== undefined) {
      params.set("limit", String(options.limit));
    }
    if (options.waitSecs !== undefined) {
      params.set("wait", String(options.waitSecs));
    }
    const qs = params.toString();
    const endpoint = `/ds/mailbox/${pseudonymHex(pseudonym)}/messages${
      qs.length === 0 ? "" : `?${qs}`
    }`;
    const url = `${this.baseUrl}${endpoint}`;

    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DsNetworkError(endpoint, err);
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DsHttpError(response.status, endpoint, text);
    }
    const json = (await response.json()) as {
      messages: { id: number; payload_b64: string; created_at: string }[];
      has_more: boolean;
    };
    return {
      messages: json.messages.map((m) => ({
        id: m.id,
        payload: base64Decode(m.payload_b64),
        createdAt: m.created_at,
      })),
      hasMore: json.has_more,
    };
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
