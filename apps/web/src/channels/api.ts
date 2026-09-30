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
import { t } from "../l10n";

/** Default base URL — `LAKKO_CHANNELS_URL` env var
 *  resolved at build time (Vite `import.meta.env`), falls
 *  back to the same `/api/<svc>` shape every other web
 *  service uses (matches the lakko.me nginx vhost route
 *  `location /api/channels/`). Mobile clients targeting
 *  `api.lakko.me/channels/` set the env var explicitly. */
const DEFAULT_BASE: string =
  (import.meta as { env?: Record<string, string | undefined> }).env
    ?.LAKKO_CHANNELS_URL ?? "/api/channels";

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
  /** The `@handle` (without the at-sign) the channel was
   *  created or found under, when there is one. Display
   *  only; the id stays the wire form. */
  readonly localHandle?: string;
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

/** The code `fetchJson` uses when the reply is not the channels service at
 *  all: an HTML page from a proxy or the SPA fallback, an empty body, a
 *  connection that never produced a JSON envelope. */
export const CHANNELS_UNREACHABLE = "channels-unreachable";

/** The catalog key of the sentence for a channels error code (ADR-0058
 *  section 6: the server's `detail` prose is never shown). */
const CHANNEL_ERROR_KEYS: Readonly<Record<string, string>> = {
  [CHANNELS_UNREACHABLE]: "channels.error.unreachable",
  "unknown-channel": "channels.error.unknown_channel",
  "post-too-large": "channels.error.post_too_large",
  "empty-post": "channels.error.empty_post",
  "envelope-not-ready": "channels.error.not_let_in",
};

/** One sentence for the user about a channels error: what happened, not the
 *  wire code and not the server's detail. The code stays available on the
 *  error for callers that branch. Rendered in the current language. */
export function channelErrorText(err: ChannelApiError): string {
  return t(CHANNEL_ERROR_KEYS[err.code] ?? "channels.error.refused");
}

function unreachable(status: number): ChannelApiError {
  return {
    status,
    code: CHANNELS_UNREACHABLE,
    detail: "channels service not reachable",
  };
}

function isJsonResponse(resp: Response): boolean {
  const ct = resp.headers.get("content-type") ?? "";
  return ct.toLowerCase().includes("application/json");
}

async function parseError(resp: Response): Promise<ChannelApiError> {
  // The server promises a stable {code, detail} JSON envelope on every
  // 4xx / 5xx. Anything else (a proxy's HTML error page, the SPA fallback, a
  // 502 with no body) is the service not being there, and says so.
  if (!isJsonResponse(resp)) return unreachable(resp.status);
  try {
    const body = (await resp.json()) as { code?: string; detail?: string };
    return {
      status: resp.status,
      code: body.code ?? "unknown",
      detail: body.detail ?? resp.statusText,
    };
  } catch {
    return unreachable(resp.status);
  }
}

/** Decode a base64url-no-pad string to raw bytes. The server
 *  emits opaque seed-envelope ciphertext as base64url; the wasm
 *  opener wants the raw bytes. Kept local to the REST layer so
 *  `api.ts` stays free of the wasm-backed `composer` module. */
function base64UrlDecode(b64: string): Uint8Array {
  const std = b64
    .replace(/-/g, "+")
    .replace(/_/g, "/")
    .padEnd(b64.length + ((4 - (b64.length % 4)) % 4), "=");
  const binary = atob(std);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
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
  // A 200 that is not JSON is not the channels service either: a misrouted
  // request answered by the SPA fallback would otherwise surface as a JSON
  // parse error with no hint of what went wrong.
  if (!isJsonResponse(resp)) throw unreachable(resp.status);
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
 *  `envelope-not-ready` which we map to null).
 *
 *  This returns only the HIGHEST-epoch (current) envelope and
 *  carries no epoch tag. Use {@link fetchEnvelopes} to open the
 *  whole retained set into an `epoch -> seed` map (ADR-0031). */
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

/** One retained `(epoch, envelope)` entry from the multi-epoch
 *  retained set (ADR-0031). `envelope` is the raw sealed-HPKE
 *  wire bytes (already base64url-decoded from the server's
 *  `envelope_b64`) ready for the wasm subscriber-seed opener. */
export interface RetainedEnvelope {
  readonly epoch: number;
  readonly envelope: Uint8Array;
}

/** Fetch the whole retained seed-envelope set for `pubkey`
 *  (ADR-0031, up to N=4 most-recent epochs), ascending by epoch.
 *  The catching-up subscriber opens each entry under ITS epoch
 *  into the per-channel `epoch -> seed` map, then decrypts each
 *  post with the seed matching the post's own `epoch_id`.
 *
 *  An empty retained set (subscriber registered, nothing
 *  delivered yet) is a valid `[]`. A 404 `unknown-subscriber`
 *  (pubkey not registered) surfaces as a thrown `ChannelApiError`
 *  — same posture the singular `fetchEnvelope` gives on that
 *  error (only `envelope-not-ready` is mapped to a soft null). */
export async function fetchEnvelopes(
  channelIdHex: string,
  subscriberPubkeyHex: string,
  base: string = DEFAULT_BASE,
): Promise<ReadonlyArray<RetainedEnvelope>> {
  const url = `${base}/v1/channels/${channelIdHex}/subscribers/${subscriberPubkeyHex}/envelopes`;
  const body = await fetchJson<{
    envelopes: ReadonlyArray<{ epoch: number; envelope_b64: string }>;
  }>(url);
  // The server already sorts ascending by epoch; decode each
  // opaque base64url envelope to the raw bytes the wasm opener
  // wants. The opacity contract is unchanged — these are sealed
  // HPKE ciphertexts the client never inspects beyond opening.
  return body.envelopes.map((e) => ({
    epoch: e.epoch,
    envelope: base64UrlDecode(e.envelope_b64),
  }));
}

/** Deliver an HPKE-sealed seed envelope to a registered
 *  subscriber. Called by the publisher side of the UI for
 *  each subscriber after a fan-out (initial join or
 *  scheduled rotation). The server takes the 80 raw bytes
 *  as the request body — same opacity contract the publish
 *  endpoint uses.
 *
 *  `epoch` (ADR-0031) — when the publisher knows the concrete
 *  epoch it sealed this envelope for, it passes it so the server
 *  files the envelope under that epoch (and prunes to the last N).
 *  Absent → the server files at the channel's current epoch. A
 *  future epoch is refused server-side with 409
 *  `envelope-epoch-in-future`.
 *
 *  Throws `ChannelApiError`:
 *  - `unknown-subscriber` (404) — pubkey is not registered
 *  - `bad-envelope-len`   (400) — wrong wire length
 *  - `bad-channel-id`     (400) — wrong channel-id shape
 *  - `bad-subscriber-id`  (400) — wrong pubkey-hex shape
 *  - `envelope-epoch-in-future` (409) — epoch > current epoch */
export async function deliverEnvelope(
  channelIdHex: string,
  subscriberPubkeyHex: string,
  envelope: Uint8Array,
  base: string = DEFAULT_BASE,
  epoch?: number,
): Promise<void> {
  const query = epoch !== undefined ? `?epoch=${epoch}` : "";
  const url = `${base}/v1/channels/${channelIdHex}/subscribers/${subscriberPubkeyHex}/envelope${query}`;
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

/** G4-2-rotation-timer — one entry in the rotation batch the
 *  publisher posts to `POST /v1/channels/:id/rotate`. */
export interface RotateBatchEntry {
  /** Hex-encoded subscriber public key. */
  readonly subscriber_pubkey_hex: string;
  /** Base64-encoded `EXPECTED_ENVELOPE_LEN`-byte sealed
   *  HPKE envelope carrying the new content seed under the
   *  subscriber's HPKE public key. */
  readonly envelope_b64: string;
}

/** Atomic rotation fanout. The publisher walks
 *  `listSubscribers(...)`, reseals the new MLS-exporter
 *  seed once per subscriber via the wasm `sealSeed`
 *  surface, then posts the batch here. The server validates
 *  every entry's length + registration before committing —
 *  either the whole batch lands or none does (per the
 *  atomic-rotation invariant in G4-4). On success the
 *  response carries the new epoch id, which the caller
 *  records as part of the rotation state.
 *
 *  Throws `ChannelApiError`:
 *  - `unknown-subscriber` (404) — one of the pubkeys is
 *    not registered (batch aborted; no envelopes landed)
 *  - `bad-envelope-len`   (400) — wrong wire length
 *  - `bad-channel-id`     (400) — wrong channel-id shape
 *  - `bad-subscriber-id`  (400) — wrong pubkey-hex shape */
export async function rotateChannel(
  channelIdHex: string,
  envelopes: ReadonlyArray<RotateBatchEntry>,
  base: string = DEFAULT_BASE,
): Promise<ChannelEpoch> {
  const url = `${base}/v1/channels/${channelIdHex}/rotate`;
  const resp = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ envelopes }),
  });
  if (!resp.ok) {
    throw await parseError(resp);
  }
  return (await resp.json()) as ChannelEpoch;
}

/** ADR-0030 — maximum entries a single `POST /rotate/stage`
 *  chunk may carry. Kept in lock-step with the server's
 *  `services/channels::api::MAX_STAGE_CHUNK_ENTRIES`; a chunk
 *  over this is refused server-side with 413 `chunk-too-large`,
 *  so the publisher splits its fanout into more chunks. */
export const MAX_STAGE_CHUNK_ENTRIES = 5000;

/** Response for `POST /v1/channels/:id/rotate/stage` — mirrors
 *  `services/channels::api::StageResponse`. */
export interface StageResult {
  /** Echoed target epoch these staged envelopes become live
   *  at once the publisher commits. */
  readonly targetEpoch: number;
  /** Number of envelopes this chunk accepted into the staging
   *  area. Lets the publisher track fanout progress. */
  readonly staged: number;
}

/** ADR-0030 prepare phase — stage a bounded, resumable,
 *  idempotent chunk of the next epoch's envelopes WITHOUT
 *  bumping the epoch or serving them to subscribers. The
 *  publisher splits its full fanout into chunks of at most
 *  [`MAX_STAGE_CHUNK_ENTRIES`] and calls this once per chunk;
 *  re-sending a chunk is an idempotent upsert (keyed on
 *  `(channel, target_epoch, subscriber)`), so a client that
 *  dropped mid-fanout simply re-sends the missing chunks.
 *
 *  `targetEpoch` MUST equal `current_epoch + 1` — the server
 *  refuses a stale / replayed target so a rotation cannot roll
 *  the epoch back or skip.
 *
 *  Throws `ChannelApiError`:
 *  - `chunk-too-large`    (413) — chunk over MAX_STAGE_CHUNK_ENTRIES
 *  - `stale-target-epoch` (409) — targetEpoch != current + 1
 *  - `unknown-subscriber` (409) — a pubkey is not registered
 *  - `bad-envelope-len`   (400) — wrong wire length
 *  - `unknown-channel`    (404) — no such channel
 *  - `bad-channel-id`     (400) — wrong channel-id shape */
export async function stageRotation(
  channelIdHex: string,
  targetEpoch: number,
  envelopes: ReadonlyArray<RotateBatchEntry>,
  base: string = DEFAULT_BASE,
): Promise<StageResult> {
  const url = `${base}/v1/channels/${channelIdHex}/rotate/stage`;
  const resp = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ target_epoch: targetEpoch, envelopes }),
  });
  if (!resp.ok) {
    throw await parseError(resp);
  }
  const body = (await resp.json()) as { target_epoch: number; staged: number };
  return { targetEpoch: body.target_epoch, staged: body.staged };
}

/** ADR-0030 commit phase — the single atomic point. Once every
 *  chunk is staged via [`stageRotation`], promote the staged
 *  envelopes for `targetEpoch` into the live set + bump the
 *  epoch in one server-side transaction (C-42: subscribers see
 *  the old epoch until this runs, then the new epoch with every
 *  staged envelope already live). Returns the new epoch.
 *
 *  Throws `ChannelApiError`:
 *  - `stale-target-epoch` (409) — targetEpoch != current + 1
 *  - `unknown-channel`    (404) — no such channel
 *  - `bad-channel-id`     (400) — wrong channel-id shape */
export async function commitRotation(
  channelIdHex: string,
  targetEpoch: number,
  base: string = DEFAULT_BASE,
): Promise<ChannelEpoch> {
  const url = `${base}/v1/channels/${channelIdHex}/rotate/commit`;
  const resp = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ target_epoch: targetEpoch }),
  });
  if (!resp.ok) {
    throw await parseError(resp);
  }
  return (await resp.json()) as ChannelEpoch;
}
