// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channels service routes for comments and reactions under a post
 * (ADR-0087 section 13a, `services/channels/src/feedback_api.rs`).
 *
 * Every write is signed by a key of its own (`x-lakko-channel-write-proof`):
 * a reaction by the key of that one reaction, a comment by the key of that
 * one comment, so the service cannot link two writes of one person. An add
 * also spends one anonymous token (`channel-react` / `channel-comment`); a
 * take-off and an author's delete spend none. A publisher deletes someone
 * else's comment under its publisher proof, as it signs a post.
 *
 * The reads (`GET .../reactions?posts=`, `GET .../comments`) carry no
 * session, no token and no proof.
 */
import { guardedChannelFetch as writeGuardedFetch } from "../moderation/channel-removal";
import { anonTokens, anonymousFetchInit } from "../tokens/anon-token-pool";
import { apiError, noteIfChannelDeleted, sendAsPublisher } from "./api";
import { loadWasm, type Wasm } from "../wasm";

type Identity = InstanceType<Wasm["Identity"]>;

/** The same base the rest of the channels client uses. */
const DEFAULT_BASE: string =
  (import.meta as { env?: Record<string, string | undefined> }).env?.LAKKO_CHANNELS_URL ?? "/api/channels";

/** A refusal or a failure of one of these routes. `status` 0: the request
 *  never got an answer from the service. */
export class FeedbackApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
  ) {
    super(`channel feedback request failed: ${code}`);
    this.name = "FeedbackApiError";
  }
}

function isJson(resp: Response): boolean {
  return (resp.headers.get("content-type") ?? "").toLowerCase().includes("application/json");
}

async function errorOf(resp: Response): Promise<FeedbackApiError> {
  if (!isJson(resp)) return new FeedbackApiError(resp.status, "unreachable");
  try {
    const body = (await resp.json()) as { code?: string; error?: string };
    const code = body.code ?? body.error ?? "unknown";
    // ADR-0087 phase B: a deleted channel answers 410 on every route; this
    // one ends it on the device like any other channels route does.
    noteIfChannelDeleted(resp.status, code, resp.url);
    return new FeedbackApiError(resp.status, code);
  } catch {
    return new FeedbackApiError(resp.status, "unreachable");
  }
}

async function send(url: string, init: RequestInit): Promise<Response> {
  try {
    return await writeGuardedFetch(url, init);
  } catch {
    throw new FeedbackApiError(0, "unreachable");
  }
}

export function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

export function bytesOfHex(hex: string): Uint8Array {
  const out = new Uint8Array(Math.floor(hex.length / 2));
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function b64urlOf(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function bytesOfB64url(b64: string): Uint8Array {
  const std = b64.replace(/-/g, "+").replace(/_/g, "/");
  const padded = std.padEnd(std.length + ((4 - (std.length % 4)) % 4), "=");
  const bin = atob(padded);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

// ---- reactions ----------------------------------------------------------

/** Add one reaction row. Spends one `channel-react` token; a token failure
 *  (quota, pool empty, service disabled) is thrown as it came, so the caller
 *  can keep the reaction waiting. Resolves `true` when the row is new. */
export async function addReactionRow(
  channelIdHex: string,
  postCursor: number,
  writerKeyHex: string,
  labelHex: string,
  proof: string,
  base: string = DEFAULT_BASE,
): Promise<boolean> {
  const wasm = await loadWasm();
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/reactions`;
  const body = JSON.stringify({ writer_key_hex: writerKeyHex, label_hex: labelHex });
  let resp: Response;
  try {
    resp = await anonTokens.spend("channel-react", (headerName, headerValue) =>
      writeGuardedFetch(
        url,
        anonymousFetchInit({
          method: "POST",
          headers: {
            "content-type": "application/json",
            [wasm.channelWriteProofHeaderName()]: proof,
            [headerName]: headerValue,
          },
          body,
        }),
      ),
    );
  } catch (err) {
    if (err instanceof TypeError) throw new FeedbackApiError(0, "unreachable");
    throw err;
  }
  if (!resp.ok) throw await errorOf(resp);
  return resp.status === 201;
}

/** Take one reaction row off (signed by the same key that added it). */
export async function deleteReactionRow(
  channelIdHex: string,
  postCursor: number,
  writerKeyHex: string,
  labelHex: string,
  proof: string,
  base: string = DEFAULT_BASE,
): Promise<void> {
  const wasm = await loadWasm();
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/reactions/${writerKeyHex}/${labelHex}`;
  const resp = await send(url, { method: "DELETE", headers: { [wasm.channelWriteProofHeaderName()]: proof } });
  if (!resp.ok && resp.status !== 404) throw await errorOf(resp);
}

/** One post's counts: label hex -> count, and its comment count (the
 *  service's maintained counter; `undefined` from a service that predates
 *  it). */
export interface PostLabelCounts {
  readonly cursor: number;
  readonly labels: ReadonlyMap<string, number>;
  readonly comments: number | undefined;
  /** ADR-0092 section 4 — a channel poll's distinct voters and votes per
   *  opaque option label; `undefined` on a post nobody voted on. */
  readonly votes?: { readonly voters: number; readonly labels: ReadonlyMap<string, number> };
}

/** The counts of up to 50 posts in one call. */
export async function fetchReactionCounts(
  channelIdHex: string,
  cursors: readonly number[],
  base: string = DEFAULT_BASE,
): Promise<readonly PostLabelCounts[]> {
  if (cursors.length === 0) return [];
  const url = `${base}/v1/channels/${channelIdHex}/reactions?posts=${cursors.join(",")}`;
  const resp = await send(url, { method: "GET" });
  if (!resp.ok) throw await errorOf(resp);
  if (!isJson(resp)) throw new FeedbackApiError(resp.status, "unreachable");
  const body = (await resp.json()) as {
    posts?: {
      cursor: number;
      labels: { label_hex: string; count: number }[];
      comments?: number;
      votes?: { voters?: number; labels?: { label_hex: string; count: number }[] };
    }[];
  };
  return (body.posts ?? []).map((p) => ({
    cursor: Number(p.cursor),
    labels: labelMap(p.labels),
    comments: typeof p.comments === "number" && Number.isFinite(p.comments) && p.comments >= 0 ? p.comments : undefined,
    ...(p.votes === undefined || p.votes === null
      ? {}
      : { votes: { voters: Math.max(0, Number(p.votes.voters ?? 0)), labels: labelMap(p.votes.labels) } }),
  }));
}

function labelMap(labels: readonly { label_hex: string; count: number }[] | undefined): Map<string, number> {
  return new Map((labels ?? []).map((l) => [l.label_hex.toLowerCase(), Number(l.count)] as const));
}

// ---- reactions on comments (phase B) ------------------------------------

/** Add one reaction row on a comment. Spends one `channel-react` token, as a
 *  post reaction does; `404 unknown-comment` when the comment is gone.
 *  Resolves `true` when the row is new. */
export async function addCommentReactionRow(
  channelIdHex: string,
  postCursor: number,
  commentCursor: number,
  writerKeyHex: string,
  labelHex: string,
  proof: string,
  base: string = DEFAULT_BASE,
): Promise<boolean> {
  const wasm = await loadWasm();
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/comments/${commentCursor}/reactions`;
  const body = JSON.stringify({ writer_key_hex: writerKeyHex, label_hex: labelHex });
  let resp: Response;
  try {
    resp = await anonTokens.spend("channel-react", (headerName, headerValue) =>
      writeGuardedFetch(
        url,
        anonymousFetchInit({
          method: "POST",
          headers: {
            "content-type": "application/json",
            [wasm.channelWriteProofHeaderName()]: proof,
            [headerName]: headerValue,
          },
          body,
        }),
      ),
    );
  } catch (err) {
    if (err instanceof TypeError) throw new FeedbackApiError(0, "unreachable");
    throw err;
  }
  if (!resp.ok) throw await errorOf(resp);
  return resp.status === 201;
}

/** Take one reaction off a comment (signed by the key that added it). */
export async function deleteCommentReactionRow(
  channelIdHex: string,
  postCursor: number,
  commentCursor: number,
  writerKeyHex: string,
  labelHex: string,
  proof: string,
  base: string = DEFAULT_BASE,
): Promise<void> {
  const wasm = await loadWasm();
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/comments/${commentCursor}/reactions/${writerKeyHex}/${labelHex}`;
  const resp = await send(url, { method: "DELETE", headers: { [wasm.channelWriteProofHeaderName()]: proof } });
  if (!resp.ok && resp.status !== 404) throw await errorOf(resp);
}

/** One comment's counts: label hex -> count. */
export interface CommentLabelCounts {
  readonly cursor: number;
  readonly labels: ReadonlyMap<string, number>;
}

/** The reaction counts of up to 50 comments under one post, in one call. */
export async function fetchCommentReactionCounts(
  channelIdHex: string,
  postCursor: number,
  commentCursors: readonly number[],
  base: string = DEFAULT_BASE,
): Promise<readonly CommentLabelCounts[]> {
  if (commentCursors.length === 0) return [];
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/comment-reactions?comments=${commentCursors.join(",")}`;
  const resp = await send(url, { method: "GET" });
  if (!resp.ok) throw await errorOf(resp);
  if (!isJson(resp)) throw new FeedbackApiError(resp.status, "unreachable");
  const body = (await resp.json()) as {
    comments?: { cursor: number; labels: { label_hex: string; count: number }[] }[];
  };
  return (body.comments ?? []).map((c) => ({ cursor: Number(c.cursor), labels: labelMap(c.labels) }));
}

// ---- comments -----------------------------------------------------------

/** One comment row as the service serves it. */
export interface CommentRow {
  readonly cursor: number;
  readonly writerKeyHex: string;
  readonly sealed: Uint8Array;
  readonly serverTsMs: number;
}

/** Post one comment. Spends one `channel-comment` token. Returns its cursor. */
export async function addCommentRow(
  channelIdHex: string,
  postCursor: number,
  writerKeyHex: string,
  sealed: Uint8Array,
  proof: string,
  base: string = DEFAULT_BASE,
): Promise<number> {
  const wasm = await loadWasm();
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/comments`;
  const body = JSON.stringify({ writer_key_hex: writerKeyHex, sealed_b64: b64urlOf(sealed) });
  let resp: Response;
  try {
    resp = await anonTokens.spend("channel-comment", (headerName, headerValue) =>
      writeGuardedFetch(
        url,
        anonymousFetchInit({
          method: "POST",
          headers: {
            "content-type": "application/json",
            [wasm.channelWriteProofHeaderName()]: proof,
            [headerName]: headerValue,
          },
          body,
        }),
      ),
    );
  } catch (err) {
    if (err instanceof TypeError) throw new FeedbackApiError(0, "unreachable");
    throw err;
  }
  if (!resp.ok) throw await errorOf(resp);
  const json = (await resp.json()) as { cursor: number };
  return Number(json.cursor);
}

/** A page of comments after `after`, oldest first (20 at most). */
export async function listCommentRows(
  channelIdHex: string,
  postCursor: number,
  after: number,
  limit?: number,
  base: string = DEFAULT_BASE,
): Promise<{ readonly comments: readonly CommentRow[]; readonly nextAfter: number }> {
  const q = new URLSearchParams({ after: String(after) });
  if (limit !== undefined) q.set("limit", String(limit));
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/comments?${q.toString()}`;
  const resp = await send(url, { method: "GET" });
  if (!resp.ok) throw await errorOf(resp);
  if (!isJson(resp)) throw new FeedbackApiError(resp.status, "unreachable");
  const body = (await resp.json()) as {
    comments?: { cursor: number; writer_key_hex: string; sealed_b64: string; server_ts_ms: number }[];
    next_after?: number;
  };
  const comments: CommentRow[] = [];
  for (const c of body.comments ?? []) {
    try {
      comments.push({
        cursor: Number(c.cursor),
        writerKeyHex: c.writer_key_hex.toLowerCase(),
        sealed: bytesOfB64url(c.sealed_b64),
        serverTsMs: Number(c.server_ts_ms),
      });
    } catch {
      // A row that is not base64 is dropped, never shown.
    }
  }
  return { comments, nextAfter: Number(body.next_after ?? after) };
}

/** The author deletes its own comment (signed by that comment's key). */
export async function deleteCommentAsAuthor(
  channelIdHex: string,
  postCursor: number,
  commentCursor: number,
  proof: string,
  base: string = DEFAULT_BASE,
): Promise<void> {
  const wasm = await loadWasm();
  const url = `${base}/v1/channels/${channelIdHex}/posts/${postCursor}/comments/${commentCursor}`;
  const resp = await send(url, { method: "DELETE", headers: { [wasm.channelWriteProofHeaderName()]: proof } });
  if (!resp.ok && resp.status !== 404) throw await errorOf(resp);
}

/** A publisher deletes a comment under its publisher proof, signed the way
 *  `api.ts` signs every publisher request (the owner key, a moved owner key
 *  after a rotation, or the co-publisher key). A comment already gone counts
 *  as deleted. */
export async function deleteCommentAsPublisher(
  identity: Identity,
  channelIdHex: string,
  postCursor: number,
  commentCursor: number,
  base: string = DEFAULT_BASE,
): Promise<void> {
  const path = `/v1/channels/${channelIdHex}/posts/${postCursor}/comments/${commentCursor}`;
  try {
    await sendAsPublisher(identity, channelIdHex, { method: "DELETE", path, query: "", body: new Uint8Array() }, base);
  } catch (err) {
    if (apiError.is(err)) {
      if (err.status === 404) return;
      throw new FeedbackApiError(err.status, err.code);
    }
    throw new FeedbackApiError(0, "unreachable");
  }
}
