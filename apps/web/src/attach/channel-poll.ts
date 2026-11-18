// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 4 / UX 7.7 — voting in a channel poll: one vote per
 * account through an anonymous credential (`lakko-anon-cred`) and a per-poll
 * tag at the channel service.
 *
 * The credential: the secret `s` comes from the lineage seed, so every device
 * of the account computes the same one and nothing is synced. The first time
 * a period's credential is needed the device asks auth for it (registering
 * the commitment first when auth answers `commitment-not-registered`),
 * checks the answer against the period key it fetched itself (never the key
 * the answer echoes), and keeps it per period in a sealed `account-kv` row.
 *
 * The vote: `PUT /v1/channels/{id}/posts/{post}/votes/{tag}` with a fresh
 * presentation bound to the action, the labels and `at_ms`; `DELETE` to
 * retract. `409 stale-vote` is retried once at `stored_at_ms + 1` while that
 * stays inside the service's window; `400 vote-time-out-of-window` once at
 * the service's clock. The retry rules are pure and tested.
 */
import { sessionFetch } from "../auth/session-fetch";
import { anonymousFetchInit } from "../tokens/anon-token-pool";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import { loadWasm } from "../wasm";

import type { AnonCredentialSecret, AnonIssuerPublicKey } from "../../wasm/lakko_ffi_wasm";

type Wasm = Awaited<ReturnType<typeof loadWasm>>;

const AUTH_BASE = "/api/auth";
const CHANNELS_BASE: string =
  (import.meta as { env?: Record<string, string | undefined> }).env?.LAKKO_CHANNELS_URL ?? "/api/channels";

/** The service's freshness window for `at_ms` (ADR-0090 section 4.3). */
export const VOTE_WINDOW_MS = 120_000;

// ----- pure: the vote message and the retry rule -----------------------

export type VoteAction = "put" | "delete";

/** The canonical message a presentation binds (the core builds the bytes;
 *  this is the TS twin used only where the core is not loaded, and in tests
 *  to pin the layout): `"lakko/channel-poll/vote/v1\0" || action(1) ||
 *  at_ms(8 BE) || label_count(1) || labels(16 each, ascending)`. */
export function voteMessageBytes(action: VoteAction, atMs: number, labels: readonly Uint8Array[]): Uint8Array {
  const domain = new TextEncoder().encode("lakko/channel-poll/vote/v1\0");
  const sorted = action === "delete" ? [] : [...labels].sort(compareBytes);
  const out = new Uint8Array(domain.length + 1 + 8 + 1 + sorted.length * 16);
  out.set(domain, 0);
  let o = domain.length;
  out[o++] = action === "put" ? 0x01 : 0x02;
  const view = new DataView(out.buffer);
  view.setBigUint64(o, BigInt(atMs));
  o += 8;
  out[o++] = sorted.length;
  for (const l of sorted) {
    out.set(l, o);
    o += 16;
  }
  return out;
}

function compareBytes(a: Uint8Array, b: Uint8Array): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    const d = (a[i] ?? 0) - (b[i] ?? 0);
    if (d !== 0) return d;
  }
  return a.length - b.length;
}

/** What the service answered a vote. */
export type VoteAnswer =
  | { readonly kind: "ok" }
  | { readonly kind: "stale"; readonly storedAtMs: number }
  | { readonly kind: "out-of-window"; readonly serverTimeMs: number }
  | { readonly kind: "failed"; readonly code: string };

/** The `at_ms` to try again with, or `null` to give up (one retry only). */
export function nextVoteAtMs(answer: VoteAnswer, attempt: number, nowMs: number): number | null {
  if (attempt >= 1) return null;
  if (answer.kind === "stale") {
    const next = answer.storedAtMs + 1;
    return Math.abs(next - nowMs) <= VOTE_WINDOW_MS ? next : null;
  }
  if (answer.kind === "out-of-window") return answer.serverTimeMs;
  return null;
}

/** Read one service answer into a {@link VoteAnswer}. */
export async function voteAnswerOf(resp: Response): Promise<VoteAnswer> {
  if (resp.ok) return { kind: "ok" };
  let body: { code?: string; stored_at_ms?: number; server_time_ms?: number } = {};
  try {
    body = (await resp.json()) as typeof body;
  } catch {
    /* not JSON */
  }
  const code = body.code ?? `http-${resp.status}`;
  if (resp.status === 409 && code === "stale-vote" && typeof body.stored_at_ms === "number") {
    return { kind: "stale", storedAtMs: body.stored_at_ms };
  }
  if (resp.status === 400 && code === "vote-time-out-of-window" && typeof body.server_time_ms === "number") {
    return { kind: "out-of-window", serverTimeMs: body.server_time_ms };
  }
  return { kind: "failed", code };
}

// ----- the credential -------------------------------------------------

export class ChannelVoteError extends Error {
  constructor(readonly code: string) {
    super(`channel vote failed: ${code}`);
    this.name = "ChannelVoteError";
  }
}

function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromB64url(s: string): Uint8Array {
  const std = s.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(std.padEnd(std.length + ((4 - (std.length % 4)) % 4), "="));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function hex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The period's issuer key, fetched from auth (anonymous). */
async function issuerKey(wasm: Wasm, period: bigint): Promise<AnonIssuerPublicKey> {
  const resp = await fetch(`${AUTH_BASE}/auth/anon-credential/key/${period}`, anonymousFetchInit({ method: "GET", headers: {} }));
  if (!resp.ok) throw new ChannelVoteError(`key-${resp.status}`);
  const body = (await resp.json()) as { period?: number; public_key_b64?: string };
  if (body.public_key_b64 === undefined) throw new ChannelVoteError("key-malformed");
  return wasm.AnonIssuerPublicKey.decode(period, fromB64url(body.public_key_b64));
}

async function bearerJson(method: string, path: string, body: unknown): Promise<Response> {
  return sessionFetch(`${AUTH_BASE}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** This period's credential, from the store or newly issued. */
export async function credentialFor(
  accountId: string,
  secret: AnonCredentialSecret,
  nowMs: number,
): Promise<{ credential: Uint8Array; key: AnonIssuerPublicKey }> {
  const wasm = await loadWasm();
  let period = wasm.anonCredPeriodAt(BigInt(Math.floor(nowMs / 1000)));
  const row = `anon-credential:${period}`;
  const stored = (await loadAccountJson(accountId, row)) as { c?: string } | null;
  if (typeof stored?.c === "string") {
    return { credential: fromB64url(stored.c), key: await issuerKey(wasm, period) };
  }
  for (let attempt = 0; attempt < 3; attempt++) {
    const key = await issuerKey(wasm, period);
    const request = secret.issuanceRequest(key);
    const resp = await bearerJson("POST", "/auth/anon-credential", { request_b64: b64url(request) });
    if (resp.ok) {
      const body = (await resp.json()) as { response_b64?: string };
      if (body.response_b64 === undefined) throw new ChannelVoteError("issuance-malformed");
      // Checked against the key fetched above, never the one the answer echoes.
      const credential = secret.finalizeIssuance(key, fromB64url(body.response_b64));
      await saveAccountJson(accountId, `anon-credential:${period}`, { c: b64url(credential) });
      return { credential, key };
    }
    key.free();
    let code = "";
    let current: number | undefined;
    try {
      const body = (await resp.json()) as { error?: string; code?: string; current_period?: number };
      code = body.error ?? body.code ?? "";
      current = body.current_period;
    } catch {
      /* not JSON */
    }
    if (resp.status === 409 && code === "commitment-not-registered") {
      const reg = await bearerJson("PUT", "/auth/anon-credential/commitment", {
        registration_b64: b64url(secret.registration()),
      });
      if (!reg.ok && reg.status !== 409) throw new ChannelVoteError(`commitment-${reg.status}`);
      continue;
    }
    if (resp.status === 400 && code === "period-not-issuable" && typeof current === "number") {
      period = BigInt(current);
      continue;
    }
    throw new ChannelVoteError(code.length > 0 ? code : `issuance-${resp.status}`);
  }
  throw new ChannelVoteError("issuance-retries");
}

/** The message a presentation binds: the core's canonical bytes. */
function voteMessage(wasm: Wasm, action: VoteAction, atMs: number, labels: readonly Uint8Array[]): Uint8Array {
  const flat = new Uint8Array(labels.length * 16);
  labels.forEach((l, i) => flat.set(l, i * 16));
  return wasm.channelPollVoteMessage(action, BigInt(atMs), flat);
}

/**
 * Cast (`labels` non-empty) or retract (`labels` empty) a vote in a channel
 * poll. Throws {@link ChannelVoteError} when it could not be sent.
 */
export async function sendChannelVote(args: {
  readonly accountId: string;
  readonly lineageSeed32: Uint8Array;
  readonly channelIdHex: string;
  readonly postCursor: number;
  readonly labels: readonly Uint8Array[];
  readonly now?: () => number;
}): Promise<void> {
  const wasm = await loadWasm();
  const now = args.now ?? Date.now;
  const secret = wasm.AnonCredentialSecret.fromLineageSeed(args.lineageSeed32);
  try {
    const { credential, key } = await credentialFor(args.accountId, secret, now());
    try {
      const action: VoteAction = args.labels.length === 0 ? "delete" : "put";
      const channelId = Uint8Array.from(args.channelIdHex.match(/../g) ?? [], (h) => Number.parseInt(h, 16));
      let atMs = now();
      for (let attempt = 0; ; attempt++) {
        const message = voteMessage(wasm, action, atMs, args.labels);
        const presentation = secret.present(credential, key, channelId, BigInt(args.postCursor), message);
        let resp: Response;
        try {
          const url = `${CHANNELS_BASE}/v1/channels/${args.channelIdHex}/posts/${args.postCursor}/votes/${hex(presentation.tag)}`;
          const body =
            action === "put"
              ? {
                  presentation_b64: b64url(presentation.bytes),
                  labels_hex: [...args.labels].map(hex).sort(),
                  at_ms: atMs,
                }
              : { presentation_b64: b64url(presentation.bytes), at_ms: atMs };
          resp = await fetch(
            url,
            anonymousFetchInit({
              method: action === "put" ? "PUT" : "DELETE",
              headers: { "content-type": "application/json" },
              body: JSON.stringify(body),
            }),
          );
        } catch {
          throw new ChannelVoteError("unreachable");
        } finally {
          presentation.free();
        }
        const answer = await voteAnswerOf(resp);
        if (answer.kind === "ok") return;
        const next = nextVoteAtMs(answer, attempt, now());
        if (next === null) throw new ChannelVoteError(answer.kind === "failed" ? answer.code : answer.kind);
        atMs = next;
      }
    } finally {
      key.free();
    }
  } finally {
    secret.free();
  }
}

/** The 16-byte option labels of a channel poll post (the reaction key of the
 *  post's epoch). */
export function channelPollOptionLabels(
  wasm: Wasm,
  reactionKey: Parameters<Wasm["channelPollOptionLabels"]>[0],
  postCursor: number,
  optionCount: number,
): Uint8Array[] {
  const flat = wasm.channelPollOptionLabels(reactionKey, BigInt(postCursor), optionCount);
  const out: Uint8Array[] = [];
  for (let i = 0; i + 16 <= flat.length; i += 16) out.push(flat.slice(i, i + 16));
  return out;
}

/** Counts per option from the service's per-label counts (`votes.labels`). */
export function optionCountsOf(
  optionLabelsHex: readonly string[],
  labelCounts: ReadonlyMap<string, number>,
): number[] {
  return optionLabelsHex.map((h) => labelCounts.get(h.toLowerCase()) ?? 0);
}
