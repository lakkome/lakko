// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Typed HTTP client for the Lakko channels backend
 * (`services/channels`). Phase 4 G4-2.
 *
 * Endpoints — see services/channels/src/api.rs for the
 * server-side contract:
 *
 *   POST /v1/channels
 *   POST /v1/channels/:id/posts             (publish)
 *   GET  /v1/channels/:id/posts?after=&limit=
 *   POST /v1/channels/:id/subscribers       (register)
 *   GET  /v1/channels/:id/subscribers       (list, for fan-out)
 *   POST /v1/channels/:id/subscribers/:pk/envelope
 *   GET  /v1/channels/:id/subscribers/:pk/envelope
 *   POST /v1/channels/:id/rotate
 *   GET  /v1/channels/:id/epoch
 *
 * The web client treats every response body as JSON and
 * the on-wire `payload_b64` / `envelope_b64` fields as
 * opaque base64url — same opacity contract the server
 * enforces. Real ChannelPost composition + signature
 * verification + AEAD open happens on the
 * `lakko-channels` Rust crate via WASM bindings (follow-
 * up sub-sprint G4-2-crypto).
 */

/** Default base URL — `LAKKO_CHANNELS_URL` env var
 *  resolved at build time (Vite `import.meta.env`), falls
 *  back to a same-origin path so the dev server proxy
 *  can forward `/channels/*` upstream. */
const DEFAULT_BASE: string =
  (import.meta as { env?: Record<string, string | undefined> }).env
    ?.LAKKO_CHANNELS_URL ?? "/channels";

/** A registered channel as the web UI sees it. The
 *  `services/channels` MVP only carries the channel id;
 *  display name + description are client-side metadata
 *  the user stores locally (encrypted-at-rest under
 *  device-group key) and are not propagated through the
 *  server. */
export interface ChannelHandle {
  /** 32-char lowercase hex, the wire form. */
  readonly id: string;
  /** Locally-stored display name (user input, never sent
   *  to the server). */
  readonly localName: string;
  /** Locally-stored description / about. Empty string if
   *  the user did not enter one. */
  readonly localDescription: string;
  /** Unix milliseconds — when the user added this channel
   *  to their local list. */
  readonly addedAtMs: number;
}

/** One post in the cursor-paginated fetch response. */
export interface ChannelPostView {
  /** Server-assigned monotone cursor. */
  readonly cursor: number;
  /** Server wall-clock at insert time, unix ms. */
  readonly serverTsMs: number;
  /** Opaque payload, base64url-encoded. Decoding +
   *  signature verify + AEAD open happens client-side
   *  via the Rust core (follow-up sub-sprint). */
  readonly payloadB64: string;
}

/** Server's view of the channel's current epoch. */
export interface ChannelEpoch {
  readonly epoch: number;
}

/** Subscriber registry list. */
export interface ChannelSubscribers {
  /** Pubkey hex strings. */
  readonly subscribers: ReadonlyArray<string>;
}

/** Error envelope mirrored from `services/channels::api::ErrorResponse`. */
export interface ChannelApiError {
  readonly status: number;
  readonly code: string;
  readonly detail: string;
}

function isChannelApiError(x: unknown): x is ChannelApiError {
  return (
    typeof x === "object" &&
    x !== null &&
    "status" in x &&
    "code" in x &&
    "detail" in x
  );
}

/** Re-export the type guard so handlers can branch on
 *  stable error codes. */
export const apiError = { is: isChannelApiError };

async function parseError(resp: Response): Promise<ChannelApiError> {
  // The server promises a stable {code, detail} JSON envelope
  // on every 4xx / 5xx. Fall back to a stub envelope if the
  // body is unparseable (would only happen on a server bug or
  // a proxy injecting HTML).
  try {
    const body = (await resp.json()) as { code?: string; detail?: string };
    return {
      status: resp.status,
      code: body.code ?? "unknown",
      detail: body.detail ?? resp.statusText,
    };
  } catch {
    return {
      status: resp.status,
      code: "unknown",
      detail: resp.statusText,
    };
  }
}

async function fetchJson<T>(
  url: string,
  init: RequestInit = {},
): Promise<T> {
  const resp = await fetch(url, init);
  if (!resp.ok) {
    throw await parseError(resp);
  }
  // 204 No Content carries no body — return a sentinel that
  // callers asserting `void` ignore.
  if (resp.status === 204) {
    return undefined as T;
  }
  return (await resp.json()) as T;
}

/** Create a fresh channel. Idempotent on the server side
 *  if the channel id is already registered. */
export async function createChannel(
  channelIdHex: string,
  base: string = DEFAULT_BASE,
): Promise<{ readonly channelId: string }> {
  const body = await fetchJson<{ channel_id: string }>(`${base}/v1/channels`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ channel_id: channelIdHex }),
  });
  return { channelId: body.channel_id };
}

/** Publish an opaque ChannelPost. The body MUST be the
 *  output of `lakko_channels::ChannelPost::to_bytes()` on
 *  the Rust side — the server stores it byte-identical. */
export async function publishPost(
  channelIdHex: string,
  payload: Uint8Array,
  base: string = DEFAULT_BASE,
): Promise<ChannelPostView> {
  // Wrap the typed array in a Blob — fetch's `body` field
  // accepts `BodyInit`, which includes `Blob` but not
  // `Uint8Array<ArrayBufferLike>` directly on every TS
  // lib config. Same byte content reaches the network
  // either way.
  const body = await fetchJson<{
    cursor: number;
    server_ts_ms: number;
    payload_b64: string;
  }>(`${base}/v1/channels/${channelIdHex}/posts`, {
    method: "POST",
    headers: { "content-type": "application/octet-stream" },
    // Copy through a fresh ArrayBuffer slice so the TS
    // type narrows from `ArrayBufferLike` (which permits
    // `SharedArrayBuffer`) to the plain `ArrayBuffer`
    // the Blob constructor expects.
    body: new Blob(
      [payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength) as ArrayBuffer],
      { type: "application/octet-stream" },
    ),
  });
  return {
    cursor: body.cursor,
    serverTsMs: body.server_ts_ms,
    payloadB64: body.payload_b64,
  };
}

/** Cursor-paginated fetch. `after = 0` returns the
 *  channel from genesis. */
export async function fetchPosts(
  channelIdHex: string,
  after: number,
  limit?: number,
  base: string = DEFAULT_BASE,
): Promise<{
  readonly posts: ReadonlyArray<ChannelPostView>;
  readonly nextAfter: number;
}> {
  const q = new URLSearchParams({ after: String(after) });
  if (limit !== undefined) q.set("limit", String(limit));
  const body = await fetchJson<{
    posts: ReadonlyArray<{
      cursor: number;
      server_ts_ms: number;
      payload_b64: string;
    }>;
    next_after: number;
  }>(`${base}/v1/channels/${channelIdHex}/posts?${q.toString()}`);
  return {
    posts: body.posts.map((p) => ({
      cursor: p.cursor,
      serverTsMs: p.server_ts_ms,
      payloadB64: p.payload_b64,
    })),
    nextAfter: body.next_after,
  };
}

/** Read the server's current epoch for a channel.
 *  Genesis = 0; first rotation bumps to 1. */
export async function fetchEpoch(
  channelIdHex: string,
  base: string = DEFAULT_BASE,
): Promise<ChannelEpoch> {
  return fetchJson<ChannelEpoch>(`${base}/v1/channels/${channelIdHex}/epoch`);
}

/** Register a subscriber's X25519 pubkey. */
export async function subscribe(
  channelIdHex: string,
  subscriberPubkeyHex: string,
  base: string = DEFAULT_BASE,
): Promise<{ readonly subscriberId: string }> {
  const body = await fetchJson<{ subscriber_id: string }>(
    `${base}/v1/channels/${channelIdHex}/subscribers`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ subscriber_pubkey_hex: subscriberPubkeyHex }),
    },
  );
  return { subscriberId: body.subscriber_id };
}

/** List subscribers — used by the publisher to fan out
 *  HPKE seed envelopes. */
export async function listSubscribers(
  channelIdHex: string,
  base: string = DEFAULT_BASE,
): Promise<ChannelSubscribers> {
  return fetchJson<ChannelSubscribers>(
    `${base}/v1/channels/${channelIdHex}/subscribers`,
  );
}

/** Fetch the latest seed envelope delivered to `pubkey`.
 *  Returns `null` if the subscriber is registered but no
 *  envelope has been delivered yet (server returns 404
 *  `envelope-not-ready` which we map to null). */
export async function fetchEnvelope(
  channelIdHex: string,
  subscriberPubkeyHex: string,
  base: string = DEFAULT_BASE,
): Promise<string | null> {
  const url = `${base}/v1/channels/${channelIdHex}/subscribers/${subscriberPubkeyHex}/envelope`;
  try {
    const body = await fetchJson<{ envelope_b64: string }>(url);
    return body.envelope_b64;
  } catch (err) {
    if (isChannelApiError(err) && err.code === "envelope-not-ready") {
      return null;
    }
    throw err;
  }
}

/** Deliver an HPKE-sealed seed envelope to a registered
 *  subscriber. Called by the publisher side of the UI for
 *  each subscriber after a fan-out (initial join or
 *  scheduled rotation). The server takes the 80 raw bytes
 *  as the request body — same opacity contract the publish
 *  endpoint uses.
 *
 *  Throws `ChannelApiError`:
 *  - `unknown-subscriber` (404) — pubkey is not registered
 *  - `bad-envelope-len`   (400) — wrong wire length
 *  - `bad-channel-id`     (400) — wrong channel-id shape
 *  - `bad-subscriber-id`  (400) — wrong pubkey-hex shape */
export async function deliverEnvelope(
  channelIdHex: string,
  subscriberPubkeyHex: string,
  envelope: Uint8Array,
  base: string = DEFAULT_BASE,
): Promise<void> {
  const url = `${base}/v1/channels/${channelIdHex}/subscribers/${subscriberPubkeyHex}/envelope`;
  // Octet-stream body so the server reads raw `Bytes` —
  // matches `services/channels::api::deliver_envelope`. Same
  // buffer-slice pattern as `publishPost` to narrow
  // `ArrayBufferLike` (which permits `SharedArrayBuffer`)
  // down to the plain `ArrayBuffer` the Blob constructor
  // expects.
  const resp = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/octet-stream" },
    body: new Blob(
      [
        envelope.buffer.slice(
          envelope.byteOffset,
          envelope.byteOffset + envelope.byteLength,
        ) as ArrayBuffer,
      ],
      { type: "application/octet-stream" },
    ),
  });
  if (!resp.ok) {
    throw await parseError(resp);
  }
}
