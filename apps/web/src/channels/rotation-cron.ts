// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G4-2-rotation-timer — web-side rotation orchestrator.
 *
 * The Rust core (`crates/lakko-channels/src/rotation_policy.rs`)
 * decides _when_ to rotate via a pure
 * `channelShouldRotate(policy, state, nowMs)` function. The
 * orchestrator here decides _how_: it walks the subscriber
 * list, reseals the new content seed for each subscriber via
 * the wasm `sealSubscriberSeedEnvelope` surface, then fans the
 * envelopes out with the ADR-0030 chunked staged-rotation
 * protocol — `POST /rotate/stage` in bounded chunks (at most
 * `MAX_STAGE_CHUNK_ENTRIES` each, resumable + idempotent),
 * followed by one atomic `POST /rotate/commit` that promotes
 * the staged set + bumps the epoch. The commit is the single
 * atomic point (C-42): a subscriber never observes a torn
 * rotation — it sees the old epoch until commit lands, then the
 * new epoch with every staged envelope already live.
 *
 * Two entry points:
 *
 *   - `runRotationOnce(...)` — single-pass; returns the new
 *     rotation state. Use this from a "Rotate now" button.
 *   - `startRotationCron(...)` — periodic poller. Calls
 *     `runRotationOnce` on every tick, persists state via
 *     `saveState`, surfaces errors via `onError`. Returns
 *     a handle whose `.stop()` cancels the timer.
 *
 * The orchestrator is **idempotent under no-op** — if the
 * policy says "no rotation needed", it returns the input
 * state unchanged + does no network round-trip.
 *
 * **Failure semantics:**
 *
 *   - If `listSubscribers` / any `stageRotation` chunk /
 *     `commitRotation` throws, the state is NOT updated (so the
 *     next tick retries). Staging is an idempotent upsert, so a
 *     retry re-stages the already-staged chunks harmlessly
 *     before reaching the ones that were missed — no torn
 *     rotation is observable, because the epoch is only bumped
 *     by the final atomic commit.
 *   - If a single envelope seal fails (e.g. malformed
 *     subscriber pubkey hex), the WHOLE batch is aborted before
 *     any chunk is staged — same atomic posture the server
 *     enforces. Partial fanouts would leave some subscribers on
 *     the old seed + some on the new, which is worse than
 *     retrying.
 *   - If the fanout (stage + commit) succeeds but the local
 *     `saveState` callback throws, the rotation is still
 *     committed server-side; the caller's storage is in a
 *     stale state and the next tick will trigger another
 *     rotation. That's a deliberate trade-off: we cannot
 *     un-commit the server, but we MUST persist the new
 *     state to avoid an infinite rotation loop.
 *
 * The wasm surface is loaded lazily by the caller — this
 * module takes the wasm `PublisherGroup` + `Identity`
 * handles as inputs so it never has to await
 * `loadWasm()` itself. Web app callers already hold both
 * handles after `Channels.tsx` opens or creates the
 * PublisherGroup.
 */

import type { Wasm } from "../wasm";
import { loadWasm } from "../wasm";
import { DsClient } from "../ds/client";
import { registerBeforePublish, takeSigner } from "../ds/access";

type Identity = InstanceType<Wasm["Identity"]>;
type PublisherGroup = InstanceType<Wasm["PublisherGroup"]>;
type ChannelRotationPolicy = ReturnType<Wasm["channelRotationPolicyDefault"]>;
type ChannelRotationState = ReturnType<Wasm["channelRotationStateNew"]>;

import {
  commitRotation,
  fetchEpoch,
  listSubscribers,
  stageRotation,
  MAX_STAGE_CHUNK_ENTRIES,
  type ChannelEpoch,
  type RotateBatchEntry,
} from "./api";

/**
 * Caller-provided handles + persistence callbacks. Kept as
 * one object so the call site cannot forget a field on a
 * `runRotationOnce` invocation.
 */
export interface RotationContext {
  /** Wasm PublisherGroup handle — the publisher's MLS group
   *  for this channel. */
  readonly publisherGroup: PublisherGroup;
  /** Wasm Identity handle — the publisher's MLS identity
   *  + signing key. Required by `rotate` and `channelSeed`. */
  readonly identity: Identity;
  /** Hex-encoded 16-byte channel id (used in URLs). */
  readonly channelIdHex: string;
  /** Optional API base override (defaults to the channels
   *  api's `DEFAULT_BASE`). */
  readonly apiBase?: string;
  /** Caller-persistence: invoked synchronously inside
   *  `runRotationOnce` with the new state after a successful
   *  fanout. Implementation should write to IndexedDB /
   *  localStorage / whatever the channel record lives in.
   *  If this throws, the rotation has already been committed
   *  server-side — see "Failure semantics" above. */
  readonly saveState: (next: ChannelRotationState) => void | Promise<void>;
  /** Optional callback after a successful rotation,
   *  carrying the new server-side epoch. UI can refresh
   *  the displayed epoch + reload subscriber envelopes. */
  readonly onRotated?: (newEpoch: number) => void;
}

/** Result of one single-pass rotation attempt. */
export interface RotationOnceResult {
  /** `true` if the policy fired AND the rotation
   *  completed end-to-end (PublisherGroup rotated +
   *  envelopes sealed + server committed atomically). */
  readonly rotated: boolean;
  /** New rotation state to persist. For `rotated:false`
   *  cases this is byte-identical to the input state. */
  readonly newState: ChannelRotationState;
  /** New server-side epoch id after the rotation, when
   *  `rotated:true`. Undefined otherwise. */
  readonly newEpoch?: number;
}

/**
 * Run a single rotation pass. Returns `{ rotated:false }`
 * if the policy says no; otherwise drives the full chain.
 *
 * This does NOT mutate any state on the caller side — the
 * caller MUST persist `result.newState` via
 * `ctx.saveState` (which `startRotationCron` does for you).
 */
export async function runRotationOnce(
  ctx: RotationContext,
  policy: ChannelRotationPolicy,
  state: ChannelRotationState,
  nowMs: number = Date.now(),
): Promise<RotationOnceResult> {
  const nowMsBig = BigInt(Math.max(0, Math.floor(nowMs)));
  const wasm = await loadWasm();
  if (!wasm.channelShouldRotate(policy, state, nowMsBig)) {
    return { rotated: false, newState: state };
  }

  // 1. Advance the local MLS epoch + derive the new seed
  //    BEFORE talking to the server. If the server-side
  //    POST /rotate fails later, we have already rotated
  //    the local PublisherGroup — that's fine because the
  //    next tick retries from the new local state, and the
  //    server's atomic-rotation guarantee means no partial
  //    landings are observable.
  // ADR-0053 — the rotation is a self-update commit, so the PublisherGroup's
  // DS access key moves with it. Take the old epoch's key before, register the
  // new one proven under it after. Best effort, unlike a published commit:
  // the rotation commit is not published and cannot be rolled back here, so a
  // refused registration is logged, and the journal's read tier plus the
  // next claim on open cover the gap.
  const signerN = takeSigner(() => ctx.publisherGroup.dsAccessSigner(ctx.identity));
  ctx.publisherGroup.rotate(ctx.identity);
  {
    const signerN1 = takeSigner(() => ctx.publisherGroup.dsAccessSigner(ctx.identity));
    try {
      await registerBeforePublish(
        new DsClient(),
        ctx.publisherGroup.groupId(),
        signerN,
        signerN1,
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ds: PublisherGroup epoch registration after rotation failed", err);
    } finally {
      signerN?.free();
      signerN1?.free();
    }
  }
  const newSeed = ctx.publisherGroup.channelSeed(ctx.identity);
  const channelIdBytes = ctx.publisherGroup.channelId();

  // 2. The new server epoch the envelopes will bind to.
  //    `fetchEpoch` returns the CURRENT server epoch; the
  //    rotate endpoint will bump to current+1.
  const current = await fetchEpoch(ctx.channelIdHex, ctx.apiBase);
  const newServerEpoch = current.epoch + 1;
  if (!Number.isInteger(newServerEpoch) || newServerEpoch < 0) {
    throw new Error(
      `unexpected server epoch ${current.epoch}; cannot derive new epoch`,
    );
  }

  // 3. Walk the subscriber list + seal new envelopes.
  const subscribers = await listSubscribers(ctx.channelIdHex, ctx.identity, ctx.apiBase);
  const batch: RotateBatchEntry[] = [];
  for (const subscriberHex of subscribers.subscribers) {
    const subscriberBytes = hexToBytes(subscriberHex);
    if (subscriberBytes === null) {
      // Malformed subscriber hex from the server — refuse to
      // post a partial batch. Throwing here aborts the
      // rotation; state is NOT updated (the next tick will
      // retry once the server's subscriber list is sane).
      throw new Error(
        `subscriber pubkey hex is not valid: ${subscriberHex}`,
      );
    }
    const envelope = wasm.sealSubscriberSeedEnvelope(
      subscriberBytes,
      channelIdBytes,
      newServerEpoch,
      newSeed,
    );
    batch.push({
      subscriber_pubkey_hex: subscriberHex,
      envelope_b64: base64Encode(envelope),
    });
  }

  // 4. Fan out with the ADR-0030 chunked staged-rotation
  //    protocol: stage the sealed envelopes in bounded chunks,
  //    then run one atomic commit that promotes the staged set
  //    + bumps the epoch. An empty batch stages nothing and
  //    commits directly — still bumps the epoch, covering the
  //    "no subscribers yet" case where the publisher keeps
  //    rotating on schedule before the audience grows. A
  //    stage/commit failure throws, so `saveState` below never
  //    runs and the local rotation state is not advanced (the
  //    next tick retries; staging is idempotent so re-staging
  //    the already-staged chunks is safe).
  const epoch = await stageAndCommitRotation(
    ctx.channelIdHex,
    newServerEpoch,
    batch,
    ctx.identity,
    ctx.apiBase,
  );

  // 5. Stamp the new state + persist.
  const newState = wasm.channelRotationStateAfterRotation(nowMsBig);
  await ctx.saveState(newState);
  ctx.onRotated?.(epoch.epoch);

  return { rotated: true, newState, newEpoch: epoch.epoch };
}

/**
 * ADR-0030 chunked fanout: stage the sealed envelope batch in
 * bounded chunks (at most `MAX_STAGE_CHUNK_ENTRIES` each), then
 * run one atomic commit that promotes the staged set + bumps the
 * epoch. Returns the new epoch.
 *
 * Sequencing is deliberate:
 *
 *   - Chunks are staged SEQUENTIALLY. Staging is an idempotent
 *     upsert keyed on `(channel, target_epoch, subscriber)`, so
 *     a chunk failure throws and the caller does NOT advance its
 *     local rotation state — the next tick re-runs and re-stages
 *     the already-staged chunks harmlessly before reaching the
 *     ones that were missed (resumability).
 *   - Commit runs only after every chunk has staged. It is the
 *     single atomic point (C-42): subscribers see the old epoch
 *     until it lands, then the new epoch with every staged
 *     envelope already live — no torn rotation is observable,
 *     because the epoch is bumped nowhere but in the commit.
 *
 * An empty batch stages nothing and commits directly — this
 * still bumps the epoch (the "no subscribers yet" case where the
 * publisher keeps rotating on schedule before the audience
 * grows), matching the old single-shot `rotateChannel([])`.
 */
export async function stageAndCommitRotation(
  channelIdHex: string,
  targetEpoch: number,
  batch: ReadonlyArray<RotateBatchEntry>,
  identity: Identity,
  apiBase?: string,
): Promise<ChannelEpoch> {
  for (let i = 0; i < batch.length; i += MAX_STAGE_CHUNK_ENTRIES) {
    const chunk = batch.slice(i, i + MAX_STAGE_CHUNK_ENTRIES);
    // Sequential await — a chunk failure aborts here and the
    // caller's `saveState` never runs, so no partial rotation is
    // committed (commit below is never reached).
    await stageRotation(channelIdHex, targetEpoch, chunk, identity, apiBase);
  }
  return commitRotation(channelIdHex, targetEpoch, identity, apiBase);
}

/** Handle returned by `startRotationCron` — call `.stop()`
 *  to cancel the periodic timer. Safe to call multiple
 *  times (idempotent). */
export interface RotationCronHandle {
  stop(): void;
}

/** Options for `startRotationCron`. */
export interface RotationCronOptions {
  /** How often to poll the policy + (maybe) drive a
   *  rotation. Default 5 minutes — small enough that a
   *  24-hour interval rotation lands within ~5 min of the
   *  scheduled moment, large enough that an idle browser
   *  tab doesn't burn CPU on the policy check. */
  readonly pollIntervalMs?: number;
  /** Caller-supplied state reader. Invoked on every poll.
   *  Lets the cron pick up state changes written by other
   *  paths (e.g. the `Rotate now` button, or a
   *  post-counter bump from `composePost`). */
  readonly loadState: () => ChannelRotationState | Promise<ChannelRotationState>;
  /** Rotation policy. Static for the lifetime of the cron
   *  — if the user changes the cadence, stop + restart. */
  readonly policy: ChannelRotationPolicy;
  /** Per-rotation context — same shape as
   *  `runRotationOnce` takes. */
  readonly context: RotationContext;
  /** Optional error sink. The cron swallows thrown errors
   *  so a transient network failure does not kill the
   *  timer; this hook lets the UI surface them. */
  readonly onError?: (err: unknown) => void;
}

/**
 * Start a periodic rotation poller. Returns a handle whose
 * `.stop()` cancels the underlying `setInterval`. Safe to
 * leave running for the lifetime of a channel screen — one
 * `runRotationOnce` per `pollIntervalMs` is a cheap
 * heartbeat (zero network calls when the policy says no).
 */
export function startRotationCron(
  opts: RotationCronOptions,
): RotationCronHandle {
  const interval = Math.max(1_000, opts.pollIntervalMs ?? 5 * 60 * 1000);

  let stopped = false;

  const tick = async () => {
    if (stopped) return;
    try {
      const state = await Promise.resolve(opts.loadState());
      await runRotationOnce(opts.context, opts.policy, state, Date.now());
    } catch (err) {
      opts.onError?.(err);
    }
  };

  // Fire immediately on start so a tab that opens past the
  // rotation deadline catches up rather than waiting one
  // full `pollIntervalMs`. Errors swallowed identically.
  void tick();
  const handle = setInterval(() => {
    void tick();
  }, interval);

  return {
    stop() {
      if (stopped) return;
      stopped = true;
      clearInterval(handle);
    },
  };
}

// ---------------------------------------------------------
// helpers
// ---------------------------------------------------------

/** Parse a lowercase / mixed-case hex string into bytes.
 *  Returns `null` on any malformed input (odd length,
 *  non-hex char) — caller decides what to do. */
function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length === 0 || hex.length % 2 !== 0) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; ++i) {
    const hi = hexNibble(hex.charCodeAt(2 * i));
    const lo = hexNibble(hex.charCodeAt(2 * i + 1));
    if (hi < 0 || lo < 0) return null;
    out[i] = (hi << 4) | lo;
  }
  return out;
}

function hexNibble(code: number): number {
  if (code >= 48 && code <= 57) return code - 48;
  if (code >= 97 && code <= 102) return 10 + code - 97;
  if (code >= 65 && code <= 70) return 10 + code - 65;
  return -1;
}

/** Standard base64 encoder for `Uint8Array`. Browsers
 *  expose `btoa(String.fromCharCode(...))` but that
 *  trips up on byte arrays longer than the call-stack
 *  limit; chunking keeps it stable for the 80-byte
 *  envelope (which is well under the limit anyway). */
function base64Encode(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; ++i) {
    const b = bytes[i];
    if (b === undefined) continue;
    s += String.fromCharCode(b);
  }
  return btoa(s);
}
