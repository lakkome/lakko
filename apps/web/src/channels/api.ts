// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Typed HTTP client for the Lakko channels backend
 * (`services/channels`). Phase 4 G4-2.
 *
 * Endpoints — see services/channels/src/api.rs for the
 * server-side contract:
 *
 *   POST /v1/channels                       (with the owner key, ADR-0059)
 *   POST /v1/channels/:id/posts             (publish; publisher-signed, ADR-0059 section 9)
 *   GET  /v1/channels/:id/posts?after=&limit=
 *   POST /v1/channels/:id/subscribers       (register; one channel-subscribe token)
 *   GET  /v1/channels/:id/subscribers       (list, for fan-out; owner- or co-publisher-signed)
 *   PUT/DELETE /v1/channels/:id/copublishers/:key (appoint / revoke; owner-signed)
 *   POST /v1/channels/:id/subscribers/:pk/envelope (publisher-signed)
 *   GET  /v1/channels/:id/subscribers/:pk/envelope
 *   POST /v1/channels/:id/rotate            (publisher-signed; also rotate/stage, rotate/commit)
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

import { anonTokens, anonymousFetchInit } from "../tokens/anon-token-pool";
import type { Wasm } from "../wasm";
import { loadWasm } from "../wasm";

type Identity = InstanceType<Wasm["Identity"]>;
type ChannelOwnerSigner = Wasm["ChannelOwnerSigner"]["prototype"];

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

/** The one code the server answers every refused publisher proof with
 *  (`services/channels/src/owner_auth.rs`), whichever step failed. */
export const PUBLISHER_REFUSED = "owner-proof-required";

/** One sentence for the user about a channels error: what happened, not the
 *  wire code. The code stays available on the error for callers that branch. */
export function channelErrorText(err: ChannelApiError): string {
  if (err.code === CHANNELS_UNREACHABLE) {
    return "The channels service is not reachable right now. Try again in a moment.";
  }
  if (err.code === PUBLISHER_REFUSED) {
    return "This device is not allowed to publish to this channel yet. A co-publisher can publish once the owner's device has finished adding it.";
  }
  if (err.detail.length > 0) return err.detail;
  return `The server refused the request (${err.code}).`;
}

function unreachable(status: number): ChannelApiError {
  return {
    status,
    code: CHANNELS_UNREACHABLE,
    detail: "The channels service is not reachable right now. Try again in a moment.",
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

/** The channel owner signer of `channelIdHex` for the account identity
 *  (ADR-0059 section 1). Only the account that created the channel derives
 *  the key the server registered for it. */
async function ownerSigner(
  accountIdentity: Identity,
  channelIdHex: string,
): Promise<ChannelOwnerSigner> {
  const wasm = await loadWasm();
  return wasm.channelOwnerSigner(accountIdentity, hexToBytesStrict(channelIdHex));
}

function hexToBytesStrict(hex: string): Uint8Array {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) {
    throw new Error("channel id is not hex");
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesToHexLower(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

// ---------------------------------------------------------------------------
// ADR-0059 section 9 - publisher-signed writes
// ---------------------------------------------------------------------------

/** What this account is to a channel it publishes to: the owner (it created
 *  the channel and derives the key registered for it) or a co-publisher the
 *  owner appointed. */
export type PublisherRole = "owner" | "copublisher";

/** The role per (account, channel), keyed on the owner verifying key the
 *  account derives for the channel - unique to that pair, so a second account
 *  in the same tab never reuses another's answer. In memory only: nothing
 *  persisted says which role a device holds, so it is learned once per
 *  session (from creation, a co-publisher join, or the first signed request)
 *  and a large stage chunk is never uploaded twice to find out. */
const roleByOwnerKey = new Map<string, PublisherRole>();

async function roleKey(accountIdentity: Identity, channelIdHex: string): Promise<string> {
  const signer = await ownerSigner(accountIdentity, channelIdHex);
  try {
    return bytesToHexLower(signer.verifyingKey());
  } finally {
    signer.free();
  }
}

/** Record the role this account holds for `channelIdHex` (on creation, or
 *  once a co-publisher join completed), so the next signed request goes out
 *  once under the right key. */
export async function rememberPublisherRole(
  channelIdHex: string,
  accountIdentity: Identity,
  role: PublisherRole,
): Promise<void> {
  roleByOwnerKey.set(await roleKey(accountIdentity, channelIdHex), role);
}

/** The cached role, or `undefined` when this session has not learned it. */
export async function cachedPublisherRole(
  channelIdHex: string,
  accountIdentity: Identity,
): Promise<PublisherRole | undefined> {
  return roleByOwnerKey.get(await roleKey(accountIdentity, channelIdHex));
}

/** Drop every cached role (tests; sign-out). */
export function forgetPublisherRoles(): void {
  roleByOwnerKey.clear();
}

/** The proof header for one request under `role`. `path` is the service path
 *  (never the `/api/channels` prefix nginx strips), `query` the exact query
 *  string sent without `?`, `body` the exact bytes sent. */
async function publisherProofHeader(
  role: PublisherRole,
  accountIdentity: Identity,
  channelIdHex: string,
  method: string,
  path: string,
  query: string,
  body: Uint8Array,
): Promise<Record<string, string>> {
  const wasm = await loadWasm();
  if (role === "owner") {
    const signer = await ownerSigner(accountIdentity, channelIdHex);
    try {
      return { [wasm.channelOwnerProofHeaderName()]: signer.sign(method, path, query, body) };
    } finally {
      signer.free();
    }
  }
  const signer = wasm.channelCoPublisherSigner(accountIdentity, hexToBytesStrict(channelIdHex));
  try {
    return { [wasm.channelCoPublisherProofHeaderName()]: signer.sign(method, path, query, body) };
  } finally {
    signer.free();
  }
}

/** A request body as the exact bytes signed: a fresh `ArrayBuffer` copy, so
 *  the type narrows from `ArrayBufferLike` and what `fetch` sends is byte for
 *  byte what the proof covers. */
function exactBody(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

/** Send one publisher-signed request (ADR-0059 section 9) and return the
 *  successful response; a refusal throws the `ChannelApiError`.
 *
 *  With the role known, the request is signed once under it; a 401 then
 *  forgets the role, so the next call re-learns it, and surfaces. With the
 *  role unknown: the owner proof first, and on a 401 the co-publisher proof;
 *  whichever is accepted is remembered. */
async function sendAsPublisher(
  accountIdentity: Identity,
  channelIdHex: string,
  req: {
    readonly method: string;
    readonly path: string;
    readonly query: string;
    readonly body: Uint8Array;
    readonly contentType?: string;
  },
  base: string,
): Promise<Response> {
  const key = await roleKey(accountIdentity, channelIdHex);
  const url = `${base}${req.path}${req.query.length > 0 ? `?${req.query}` : ""}`;
  const attempt = async (role: PublisherRole): Promise<Response> => {
    const proof = await publisherProofHeader(
      role,
      accountIdentity,
      channelIdHex,
      req.method,
      req.path,
      req.query,
      req.body,
    );
    const headers: Record<string, string> = { ...proof };
    if (req.contentType !== undefined) headers["content-type"] = req.contentType;
    const init: RequestInit = { method: req.method, headers };
    if (req.method !== "GET" && req.method !== "HEAD") init.body = exactBody(req.body);
    return fetch(url, init);
  };

  const known = roleByOwnerKey.get(key);
  if (known !== undefined) {
    const resp = await attempt(known);
    if (resp.ok) return resp;
    if (resp.status === 401) roleByOwnerKey.delete(key);
    throw await parseError(resp);
  }
  const asOwner = await attempt("owner");
  if (asOwner.ok) {
    roleByOwnerKey.set(key, "owner");
    return asOwner;
  }
  if (asOwner.status !== 401) throw await parseError(asOwner);
  const asCopublisher = await attempt("copublisher");
  if (asCopublisher.ok) {
    roleByOwnerKey.set(key, "copublisher");
    return asCopublisher;
  }
  throw await parseError(asCopublisher);
}

/** Parse a successful publisher response as JSON (same rules as
 *  `fetchJson`). */
async function jsonOf<T>(resp: Response): Promise<T> {
  if (resp.status === 204) return undefined as T;
  if (!isJsonResponse(resp)) throw unreachable(resp.status);
  return (await resp.json()) as T;
}

const utf8 = new TextEncoder();

/** Create a fresh channel under the owner key the account identity derives
 *  for it. Idempotent on the server side for the same key; another key for
 *  an existing id is refused with 409 `channel-exists`. */
export async function createChannel(
  channelIdHex: string,
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<{ readonly channelId: string }> {
  const signer = await ownerSigner(accountIdentity, channelIdHex);
  const ownerPubkeyHex = bytesToHexLower(signer.verifyingKey());
  signer.free();
  const body = await fetchJson<{ channel_id: string }>(`${base}/v1/channels`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ channel_id: channelIdHex, owner_pubkey_hex: ownerPubkeyHex }),
  });
  roleByOwnerKey.set(ownerPubkeyHex, "owner");
  return { channelId: body.channel_id };
}

/** Publish an opaque ChannelPost. The body MUST be the
 *  output of `lakko_channels::ChannelPost::to_bytes()` on
 *  the Rust side — the server stores it byte-identical.
 *  Signed with this account's publisher role for the
 *  channel (ADR-0059 section 9) over exactly those bytes. */
export async function publishPost(
  channelIdHex: string,
  payload: Uint8Array,
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<ChannelPostView> {
  const resp = await sendAsPublisher(
    accountIdentity,
    channelIdHex,
    {
      method: "POST",
      path: `/v1/channels/${channelIdHex}/posts`,
      query: "",
      body: payload,
      contentType: "application/octet-stream",
    },
    base,
  );
  const body = await jsonOf<{
    cursor: number;
    server_ts_ms: number;
    payload_b64: string;
  }>(resp);
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

/** Register a subscriber's X25519 pubkey. Spends one `channel-subscribe`
 *  anonymous token (ADR-0059 section 4); the request carries no session. */
export async function subscribe(
  channelIdHex: string,
  subscriberPubkeyHex: string,
  base: string = DEFAULT_BASE,
): Promise<{ readonly subscriberId: string }> {
  const url = `${base}/v1/channels/${channelIdHex}/subscribers`;
  const json = JSON.stringify({ subscriber_pubkey_hex: subscriberPubkeyHex });
  let resp: Response;
  try {
    resp = await anonTokens.spend("channel-subscribe", (headerName, headerValue) =>
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
    // A token failure (quota, disabled, unavailable) is the caller's to
    // word; a fetch rejection is the network.
    if (err instanceof TypeError) throw unreachable(0);
    throw err;
  }
  if (!resp.ok) throw await parseError(resp);
  if (!isJsonResponse(resp)) throw unreachable(resp.status);
  const body = (await resp.json()) as { subscriber_id: string };
  return { subscriberId: body.subscriber_id };
}

/** List subscribers — used by a publisher to fan out HPKE seed envelopes.
 *  Signed (ADR-0059 sections 4 and 9) with the role this account holds for
 *  the channel: the owner key when it created it, its co-publisher key (which
 *  the owner appointed after approving it) otherwise. With the role not yet
 *  learned this session, the owner proof goes first and a 401 is retried
 *  once with the co-publisher proof; the accepted role is remembered for
 *  every later signed request. A co-publisher the owner has not appointed
 *  yet still gets the 401. */
export async function listSubscribers(
  channelIdHex: string,
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<ChannelSubscribers> {
  const resp = await sendAsPublisher(
    accountIdentity,
    channelIdHex,
    { method: "GET", path: `/v1/channels/${channelIdHex}/subscribers`, query: "", body: new Uint8Array() },
    base,
  );
  return jsonOf<ChannelSubscribers>(resp);
}

/** Appoint (`PUT`) or revoke (`DELETE`) a co-publisher key for a channel
 *  (owner-signed, ADR-0059 section 1). Idempotent both ways; 409
 *  `too-many-copublishers` past the cap. `copublisherKey` must come from a
 *  binding verified against the joiner key the owner approved, never from
 *  the server. */
async function setCoPublisherAppointment(
  method: "PUT" | "DELETE",
  channelIdHex: string,
  accountIdentity: Identity,
  copublisherKey: Uint8Array,
  base: string,
): Promise<void> {
  const wasm = await loadWasm();
  const path = wasm.channelCoPublisherPath(hexToBytesStrict(channelIdHex), copublisherKey);
  const signer = await ownerSigner(accountIdentity, channelIdHex);
  const proof = signer.sign(method, path, "", new Uint8Array());
  signer.free();
  await fetchJson<void>(`${base}${path}`, {
    method,
    headers: { [wasm.channelOwnerProofHeaderName()]: proof },
  });
}

/** Appoint a co-publisher key (owner only). */
export async function appointCoPublisher(
  channelIdHex: string,
  accountIdentity: Identity,
  copublisherKey: Uint8Array,
  base: string = DEFAULT_BASE,
): Promise<void> {
  await setCoPublisherAppointment("PUT", channelIdHex, accountIdentity, copublisherKey, base);
}

/** Revoke a co-publisher key (owner only). */
export async function revokeCoPublisher(
  channelIdHex: string,
  accountIdentity: Identity,
  copublisherKey: Uint8Array,
  base: string = DEFAULT_BASE,
): Promise<void> {
  await setCoPublisherAppointment("DELETE", channelIdHex, accountIdentity, copublisherKey, base);
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
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
  epoch?: number,
): Promise<void> {
  // Octet-stream body so the server reads raw `Bytes` —
  // matches `services/channels::api::deliver_envelope`. The
  // query is part of what the proof covers, so it is built
  // once and both signed and sent.
  await sendAsPublisher(
    accountIdentity,
    channelIdHex,
    {
      method: "POST",
      path: `/v1/channels/${channelIdHex}/subscribers/${subscriberPubkeyHex}/envelope`,
      query: epoch !== undefined ? `epoch=${epoch}` : "",
      body: envelope,
      contentType: "application/octet-stream",
    },
    base,
  );
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
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<ChannelEpoch> {
  const resp = await sendAsPublisher(
    accountIdentity,
    channelIdHex,
    {
      method: "POST",
      path: `/v1/channels/${channelIdHex}/rotate`,
      query: "",
      body: utf8.encode(JSON.stringify({ envelopes })),
      contentType: "application/json",
    },
    base,
  );
  return jsonOf<ChannelEpoch>(resp);
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
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<StageResult> {
  const resp = await sendAsPublisher(
    accountIdentity,
    channelIdHex,
    {
      method: "POST",
      path: `/v1/channels/${channelIdHex}/rotate/stage`,
      query: "",
      body: utf8.encode(JSON.stringify({ target_epoch: targetEpoch, envelopes })),
      contentType: "application/json",
    },
    base,
  );
  const body = await jsonOf<{ target_epoch: number; staged: number }>(resp);
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
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<ChannelEpoch> {
  const resp = await sendAsPublisher(
    accountIdentity,
    channelIdHex,
    {
      method: "POST",
      path: `/v1/channels/${channelIdHex}/rotate/commit`,
      query: "",
      body: utf8.encode(JSON.stringify({ target_epoch: targetEpoch })),
      contentType: "application/json",
    },
    base,
  );
  return jsonOf<ChannelEpoch>(resp);
}

// ---------------------------------------------------------------------------
// ADR-0059 — the directory (Discover)
// ---------------------------------------------------------------------------

/** One directory item as the server sends it (the listing bytes are the
 *  owner's; decode them with the shared codec, never trust the server's
 *  channel id over the listing's). */
export interface DirectoryItem {
  readonly channel_id: string;
  readonly listing_b64: string;
  /** A band: `<10`, `10+`, `100+`, `1k+`, `10k+`, `100k+`. */
  readonly subscribers: string;
}

/** A directory read: no cookie, no referrer, no session, and cacheable (the
 *  server answers every caller the same, ADR-0059 section 5). */
const CACHEABLE_ANONYMOUS: RequestInit = { credentials: "omit", referrerPolicy: "no-referrer" };

/** Whether this account holds the OWNER key of `channelIdHex` (not a
 *  co-publisher's): an owner-signed read of the subscriber list is accepted
 *  only under the key registered at creation. `null` when the server could
 *  not answer (network, a 5xx). */
export async function probeChannelOwner(
  channelIdHex: string,
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<boolean | null> {
  const key = await roleKey(accountIdentity, channelIdHex);
  const known = roleByOwnerKey.get(key);
  if (known !== undefined) return known === "owner";
  const path = `/v1/channels/${channelIdHex}/subscribers`;
  const wasm = await loadWasm();
  const signer = await ownerSigner(accountIdentity, channelIdHex);
  const proof = signer.sign("GET", path, "", new Uint8Array());
  signer.free();
  try {
    await fetchJson<ChannelSubscribers>(`${base}${path}`, {
      headers: { [wasm.channelOwnerProofHeaderName()]: proof },
    });
    roleByOwnerKey.set(key, "owner");
    return true;
  } catch (err) {
    if (isChannelApiError(err) && err.status === 401) return false;
    return null;
  }
}

/** Store or replace the channel's listing (owner-signed). `listing` is the
 *  canonical `composeChannelListing` bytes. */
export async function putListing(
  channelIdHex: string,
  accountIdentity: Identity,
  listing: Uint8Array,
  base: string = DEFAULT_BASE,
): Promise<void> {
  const path = `/v1/channels/${channelIdHex}/listing`;
  const wasm = await loadWasm();
  const signer = await ownerSigner(accountIdentity, channelIdHex);
  const proof = signer.sign("PUT", path, "", listing);
  signer.free();
  await fetchJson<void>(`${base}${path}`, {
    method: "PUT",
    headers: {
      "content-type": "application/octet-stream",
      [wasm.channelOwnerProofHeaderName()]: proof,
    },
    body: new Blob(
      [listing.buffer.slice(listing.byteOffset, listing.byteOffset + listing.byteLength) as ArrayBuffer],
      { type: "application/octet-stream" },
    ),
  });
}

/** Withdraw the listing (owner-signed, idempotent). */
export async function deleteListing(
  channelIdHex: string,
  accountIdentity: Identity,
  base: string = DEFAULT_BASE,
): Promise<void> {
  const path = `/v1/channels/${channelIdHex}/listing`;
  const wasm = await loadWasm();
  const signer = await ownerSigner(accountIdentity, channelIdHex);
  const proof = signer.sign("DELETE", path, "", new Uint8Array());
  signer.free();
  await fetchJson<void>(`${base}${path}`, {
    method: "DELETE",
    headers: { [wasm.channelOwnerProofHeaderName()]: proof },
  });
}

/** One channel's listing, or `null` when it is not listed (404). Tells the
 *  server someone looked at this channel, as any card view does. */
export async function fetchListing(
  channelIdHex: string,
  base: string = DEFAULT_BASE,
): Promise<DirectoryItem | null> {
  try {
    return await fetchJson<DirectoryItem>(`${base}/v1/directory/listing/${channelIdHex}`, CACHEABLE_ANONYMOUS);
  } catch (err) {
    if (isChannelApiError(err) && err.status === 404) return null;
    throw err;
  }
}

/** The top listings of one (topic, language) bucket. No session, no token. */
export async function fetchBucket(
  topic: number,
  language: string,
  base: string = DEFAULT_BASE,
): Promise<readonly DirectoryItem[]> {
  const body = await fetchJson<{ items: DirectoryItem[] }>(
    `${base}/v1/directory/bucket/${topic}/${encodeURIComponent(language)}`,
    CACHEABLE_ANONYMOUS,
  );
  return Array.isArray(body.items) ? body.items : [];
}

/** Search the directory (one `directory-search` token; the query is sent). */
export async function searchDirectory(
  query: string,
  base: string = DEFAULT_BASE,
): Promise<readonly DirectoryItem[]> {
  const url = `${base}/v1/directory/search?${new URLSearchParams({ q: query }).toString()}`;
  let resp: Response;
  try {
    resp = await anonTokens.spend("directory-search", (headerName, headerValue) =>
      fetch(url, anonymousFetchInit({ method: "GET", headers: { [headerName]: headerValue } })),
    );
  } catch (err) {
    if (err instanceof TypeError) throw unreachable(0);
    throw err;
  }
  if (!resp.ok) throw await parseError(resp);
  if (!isJsonResponse(resp)) throw unreachable(resp.status);
  const body = (await resp.json()) as { items?: DirectoryItem[] };
  return Array.isArray(body.items) ? body.items : [];
}
