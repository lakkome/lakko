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
 *   - G-50: the rotation's effects (the DS epoch registration,
 *     the reseal + commit) are recorded durably WITH the N+1
 *     provider snapshot before any of them runs
 *     (`publisher-commit-outbox.ts`); a reload or a failed step
 *     resumes them from that record, never from a fresh fetch of
 *     the server epoch.
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
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { DsClient } from "../ds/client";
import { commitWithNextEpoch, takeSigner, type ProofSigner } from "../ds/access";
import { isCommitConflict } from "../ds/ownership";
import { isBackoffRefusal } from "../chat/sweep-backoff";
import { channelsActions, channelsState } from "../state/channels-state";
import {
  newEntryId,
  runPublisherCommit,
  type PublisherCommitEntry,
  type PublisherCommitOutcome,
  type PublisherCommitPorts,
} from "./publisher-commit-outbox";
import { writeStagedHead } from "./channel-vault-head";

type Identity = InstanceType<Wasm["Identity"]>;
type PublisherGroup = InstanceType<Wasm["PublisherGroup"]>;
type ChannelRotationPolicy = ReturnType<Wasm["channelRotationPolicyDefault"]>;
type ChannelRotationState = ReturnType<Wasm["channelRotationStateNew"]>;

import {
  commitRotation,
  fetchEpoch,
  listSubscriberPage,
  putPublicSeed,
  stageRotation,
  MAX_STAGE_CHUNK_ENTRIES,
  type ChannelEpoch,
  type RotateBatchEntry,
} from "./api";
import { sealEachSkippingInvalid } from "./member-card-rules";

/**
 * Caller-provided handles + persistence callbacks. Kept as
 * one object so the call site cannot forget a field on a
 * `runRotationOnce` invocation.
 */
export interface RotationContext {
  /** Wasm PublisherGroup handle — the publisher's MLS group
   *  for this channel. */
  readonly publisherGroup: PublisherGroup;
  /** The account (lineage) identity: the channel proofs (owner or
   *  co-publisher key), never a group call. */
  readonly identity: Identity;
  /** ADR-0090: the bound DEVICE identity the PublisherGroup lives in; every
   *  group call (stage, merge, seed) takes it. */
  readonly device: Identity;
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

  const run = await runStagedPublisherCommit(ctx, {
    kind: "rotation",
    stage: () => ({ commit: ctx.publisherGroup.stageRotate(ctx.device), welcome: null }),
  });
  if (run.outcome !== "landed") return { rotated: false, newState: state };
  const newState = wasm.channelRotationStateAfterRotation(nowMsBig);
  await ctx.saveState(newState);
  ctx.onRotated?.(run.serverTargetEpoch);
  return { rotated: true, newState, newEpoch: run.serverTargetEpoch };
}

/**
 * ADR-0087 section 15 (phase B), ADR-0090 section 3 — the owner removes a
 * publisher: every leaf of `accountKey` in one STAGED commit, merged when the
 * journal takes it, then the reseal of the new seed to every subscriber; the
 * removed person cannot derive it. Returns the new server epoch, or `null`
 * when the commit did not land (lost to another owner device's commit, or
 * owed: re-sent later).
 */
export async function runPublisherRemoval(ctx: RotationContext, accountKey: Uint8Array): Promise<number | null> {
  const run = await runStagedPublisherCommit(ctx, {
    kind: "removal",
    stage: () => ({ commit: ctx.publisherGroup.stageRemovePublisher(ctx.device, accountKey), welcome: null }),
  });
  if (run.outcome !== "landed") return null;
  const wasm = await loadWasm();
  await ctx.saveState(wasm.channelRotationStateAfterRotation(BigInt(Date.now())));
  ctx.onRotated?.(run.serverTargetEpoch);
  return run.serverTargetEpoch;
}

/** What {@link runStagedPublisherCommit} stages. */
export interface StagedPublisherCommit {
  readonly kind: PublisherCommitEntry["kind"];
  /** Stage the commit on the group (throws a local refusal: nothing is
   *  staged then). */
  readonly stage: () => { readonly commit: Uint8Array; readonly welcome: Uint8Array | null };
  readonly joinerKeyB64?: string | null;
  readonly joinerAccountKeyB64?: string | null;
  readonly replyGrantB64?: string | null;
}

/**
 * ADR-0090 section 3 — one STAGED PublisherGroup commit, end to end, under
 * the caller's per-group MLS lock: read the service epoch (a network read,
 * before the stage), stage, record the entry and the provider carrying the
 * stage durably (fail-closed: a refused record abandons the stage and sends
 * nothing), then the steps (`runPublisherCommit`): the journal decides, the
 * stage is merged on its acceptance and abandoned on a 409, and only a
 * merged commit is resealed. A lost commit leaves the group at N; the caller
 * reads the journal and applies the winner.
 */
export async function runStagedPublisherCommit(
  ctx: PublisherCommitContext,
  input: StagedPublisherCommit,
  extras: Partial<Pick<PublisherCommitPorts, "deliverWelcome" | "appoint">> = {},
): Promise<{ readonly outcome: PublisherCommitOutcome; readonly serverTargetEpoch: number }> {
  const current = await fetchEpoch(ctx.channelIdHex, ctx.apiBase);
  const serverTargetEpoch = current.epoch + 1;
  if (!Number.isInteger(serverTargetEpoch) || serverTargetEpoch < 0) {
    throw new Error(`unexpected server epoch ${current.epoch}; cannot derive new epoch`);
  }
  if (ctx.publisherGroup.hasStagedCommit()) {
    // A stage nothing settled yet (a replay is due): no second one.
    throw new Error("a PublisherGroup commit is still staged; it is settled first");
  }
  const proofEpoch = ctx.publisherGroup.epoch();
  const proofSeed = ctx.publisherGroup.dsAccessSeed(ctx.device);
  let staged: { readonly commit: Uint8Array; readonly welcome: Uint8Array | null };
  try {
    staged = input.stage();
  } catch (err) {
    proofSeed.fill(0);
    throw err;
  }
  const entry: PublisherCommitEntry = {
    id: newEntryId(),
    kind: input.kind,
    channelIdHex: ctx.channelIdHex,
    groupIdHex: hexOf(ctx.publisherGroup.groupId()),
    targetEpoch: (proofEpoch + 1n).toString(),
    serverTargetEpoch,
    commitB64: b64Of(staged.commit),
    welcomeB64: staged.welcome === null ? null : b64Of(staged.welcome),
    joinerKeyB64: input.joinerKeyB64 ?? null,
    replyGrantB64: input.replyGrantB64 ?? null,
    joinerAccountKeyB64: input.joinerAccountKeyB64 ?? null,
    proofEpoch: proofEpoch.toString(),
    proofSeedB64: b64Of(proofSeed),
    journalLost: false,
    done: [],
  };
  proofSeed.fill(0);
  const outcome = await runRecordedPublisherCommit(ctx, entry, extras);
  return { outcome, serverTargetEpoch };
}

/**
 * Record `entry` (a commit already staged on `ctx.publisherGroup`, or an
 * entry it holds) and the provider carrying it durably, then run its steps.
 * A refused record abandons the stage and throws: nothing left the device.
 * The run is marked in flight, so a replay in the same session leaves it.
 */
export async function runRecordedPublisherCommit(
  ctx: PublisherCommitContext,
  entry: PublisherCommitEntry,
  extras: Partial<Pick<PublisherCommitPorts, "deliverWelcome" | "appoint">> = {},
): Promise<PublisherCommitOutcome> {
  try {
    await channelsActions.recordPublisherCommit(entry);
  } catch (err) {
    // Nothing left the device: the stage goes.
    try {
      ctx.publisherGroup.abandonStagedCommit(ctx.device);
    } catch {
      /* nothing staged any more */
    }
    channelsActions.notePublisherGroupChanged();
    throw err;
  }
  inFlightCommits.add(entry.id);
  try {
    return await runPublisherCommit(entry, publisherCommitPorts(ctx, extras));
  } finally {
    inFlightCommits.delete(entry.id);
  }
}

/** The recorded commits a live run is driving now; a replay leaves them. */
const inFlightCommits = new Set<string>();

/** Whether entry `id` is being run live right now. */
export function commitInFlight(id: string): boolean {
  return inFlightCommits.has(id);
}

/** What a PublisherGroup commit's steps run against. */
export interface PublisherCommitContext {
  readonly publisherGroup: PublisherGroup;
  /** The account (lineage) identity: channel proofs. */
  readonly identity: Identity;
  /** ADR-0090: the device identity the group lives in. */
  readonly device: Identity;
  readonly channelIdHex: string;
  readonly apiBase?: string;
  readonly ds?: DsClient;
}

/**
 * G-50 — the ports a recorded PublisherGroup commit runs through. The
 * approve-only steps (the journal publish, the Welcome, the appointment)
 * come from the co-publisher module as `extras`; a rotation has none of
 * them. Recording goes to the channel log (`channelsActions`).
 */
export function publisherCommitPorts(
  ctx: PublisherCommitContext,
  extras: Partial<Pick<PublisherCommitPorts, "deliverWelcome" | "appoint">> = {},
): PublisherCommitPorts {
  const ds = ctx.ds ?? new DsClient();
  return {
    async commitToJournal(commit, proof) {
      // ADR-0090: the N+1 key is the STAGED commit's (`epochKey` of the
      // commit statement). A replay after the merge already landed has no
      // stage left: the group's current key is N+1 then.
      const next =
        takeSigner(() => {
          const staged = ctx.publisherGroup.dsStagedAccessSigner(ctx.device);
          if (staged === undefined) throw new Error("no staged commit");
          return staged;
        }) ?? takeSigner(() => ctx.publisherGroup.dsAccessSigner(ctx.device));
      // ADR-0081 - proven under the recorded key of the epoch the commit
      // left, live and on a replay alike. An entry without one (none is
      // written any more) can complete only an identical re-send, which the
      // DS recognises under any retained key.
      let proofSigner: ProofSigner | null = null;
      if (proof !== null) {
        const wasm = await loadWasm();
        proofSigner = takeSigner(() => wasm.DsProofSigner.forGroup(proof.epoch, proof.seed));
      }
      try {
        // ADR-0057 — registered with the channel owner's root, with the row.
        await commitWithNextEpoch(ds, ctx.publisherGroup.groupId(), next, proofSigner ?? next, commit);
      } finally {
        next?.free();
        proofSigner?.free();
      }
    },
    classifyCommitFailure: (err) => (isCommitConflict(err) || isBackoffRefusal(err) ? "lost" : "unknown"),
    async merge() {
      if (!ctx.publisherGroup.hasStagedCommit()) return false;
      ctx.publisherGroup.mergeStagedCommit(ctx.device);
      // The merged N+1 on disk before any of its effects leaves.
      await channelsActions.persistPublisherGroupDurable();
      return true;
    },
    async abandon() {
      if (ctx.publisherGroup.hasStagedCommit()) ctx.publisherGroup.abandonStagedCommit(ctx.device);
      await channelsActions.persistPublisherGroupDurable().catch(() => {
        // The stage is gone in memory; the replay drops it from disk too.
        channelsActions.notePublisherGroupChanged();
      });
    },
    deliverWelcome:
      extras.deliverWelcome ??
      (async () => {
        throw new Error("publisher commit: no Welcome for this kind");
      }),
    async serverEpoch() {
      return (await fetchEpoch(ctx.channelIdHex, ctx.apiBase)).epoch;
    },
    async reseal(targetEpoch) {
      await resealToSubscribers(ctx.publisherGroup, ctx.device, ctx.identity, ctx.channelIdHex, targetEpoch, ctx.apiBase);
    },
    appoint: extras.appoint ?? (async () => undefined),
    async handover(accountKey) {
      await resetHandoverFor(ctx.channelIdHex, accountKey);
    },
    save: (e) => channelsActions.savePublisherCommit(e),
    finish: (e) => channelsActions.finishPublisherCommit(e.id),
    // ADR-0064 section 7 (amendment of 2026-11-14): an owner device saves the
    // epoch the staged commit moves to before the commit leaves.
    writeVaultHead: () => writeStagedHead(ctx.publisherGroup, ctx.device, ctx.identity),
  };
}

/**
 * ADR-0090 section 6 — new devices of `accountKey` joined: the hand-over
 * records of that account are cleared, so the owner's publisher pass sends
 * the read key, the admission seeds and the newest epoch seeds again (a
 * receiver keeps a seed or key only for what it lacks, so a repeat changes
 * nothing for the devices that held them). The owner's own account is
 * `ownHandover` of the access record.
 */
async function resetHandoverFor(channelIdHex: string, accountKey: Uint8Array): Promise<void> {
  const keyB64 = base64Encode(accountKey);
  await channelsActions.updateAccessDurable(channelIdHex, (r) => {
    const own = r.ownHandover ?? null;
    const isOwn = own !== null && own.accountKeyB64 === keyB64;
    return {
      ...r,
      ownHandover: isOwn ? { ...own, sentVersions: [], sentSeedIds: [], sentEpochs: [], readKeySent: false } : own,
      publisherKeys: r.publisherKeys.map((p) =>
        p.accountKeyB64 === keyB64 ? { ...p, sentVersions: [], sentSeedIds: [], sentEpochs: [], readKeySent: false } : p,
      ),
    };
  });
}

/**
 * Reseal the PublisherGroup's CURRENT seed for the channels-service epoch
 * `targetEpoch`, and commit it there with the ADR-0030 chunked fanout.
 *
 * ADR-0087 sections 6 and 7 decide who gets what:
 *
 * - a PUBLIC channel files one public seed envelope for the epoch, sealed
 *   under its read key, BEFORE the commit (the service takes the next epoch);
 *   no subscriber list is read for the readers who came by the read key.
 * - per-key envelopes go only to the rows this device let in
 *   (`admittedThroughSeq` of the channel's access record): every admitted row
 *   of a private channel, and the rows of the time a now-public channel was
 *   private. A row above it is the admission pass's to judge first.
 * - a public channel whose read key this device does not hold (a co-publisher
 *   that never learned it) reseals per key to every row instead.
 *
 * A subscriber key the core refuses to seal to (a pre-ADR-0060 X25519 key)
 * is skipped; any other seal failure aborts the whole batch.
 */
export async function resealToSubscribers(
  publisherGroup: PublisherGroup,
  device: Identity,
  identity: Identity,
  channelIdHex: string,
  targetEpoch: number,
  apiBase?: string,
  onProgress?: (done: number, total: number) => void,
): Promise<ChannelEpoch> {
  const wasm = await loadWasm();
  const newSeed = publisherGroup.channelSeed(device);
  const channelIdBytes = publisherGroup.channelId();
  const plan = resealPlan(channelIdHex);
  if (plan.readKey !== null) {
    const envelope = wasm.sealPublicSeedEnvelope(plan.readKey, channelIdBytes, targetEpoch, newSeed);
    await putPublicSeed(channelIdHex, targetEpoch, envelope, identity, apiBase);
  }
  const keys: string[] = [];
  if (plan.perKeyThroughSeq === "all" || plan.perKeyThroughSeq > 0) {
    let after = 0;
    for (;;) {
      const page = await listSubscriberPage(channelIdHex, identity, after, undefined, apiBase);
      let past = false;
      for (const e of page.entries) {
        if (plan.perKeyThroughSeq !== "all" && e.seq > plan.perKeyThroughSeq) {
          past = true;
          break;
        }
        keys.push(e.subscriberId);
      }
      if (past || page.entries.length === 0 || page.nextAfter <= after) break;
      after = page.nextAfter;
    }
  }
  // 2026-11-13: the account's own reader key (its other devices read with
  // it) gets every new seed, above the mark too, like this device's own key
  // is never removed.
  const resealed = withOwnReaderKey(keys, plan.ownReaderKeyHex);
  const bytesByHex = new Map<string, Uint8Array>();
  for (const subscriberHex of resealed) {
    const subscriberBytes = hexToBytes(subscriberHex);
    if (subscriberBytes === null) {
      // Malformed subscriber hex from the server: refuse to post a partial
      // batch (the step stays owed and is retried).
      throw new Error("subscriber pubkey hex is not valid");
    }
    bytesByHex.set(subscriberHex, subscriberBytes);
  }
  onProgress?.(0, bytesByHex.size);
  // ADR-0060: a subscriber key is Ed25519 now. A key registered before the
  // flag day (an X25519 key) may not be a valid Ed25519 point, and the core
  // refuses to seal to it; that subscriber could not open the envelope
  // anyway, so it is skipped rather than aborting the whole rotation.
  const { sealed, skipped } = sealEachSkippingInvalid([...bytesByHex.keys()], (hex) =>
    wasm.sealSubscriberSeedEnvelope(bytesByHex.get(hex) ?? new Uint8Array(0), channelIdBytes, targetEpoch, newSeed),
  );
  if (skipped.length > 0) {
    // eslint-disable-next-line no-console
    console.warn(`[channels] reseal skipped ${skipped.length} subscriber key(s) the core refused`);
  }
  const batch: RotateBatchEntry[] = sealed.map((s) => ({
    subscriber_id: s.subscriberHex,
    // The service takes base64url without padding.
    envelope_b64: base64UrlEncodeNoPad(s.value),
  }));
  const committed = await stageAndCommitRotation(channelIdHex, targetEpoch, batch, identity, apiBase);
  onProgress?.(batch.length, batch.length);
  return committed;
}

/** What a reseal of `channelIdHex` sends: the read key a public seed is
 *  sealed under (or `null`), and the rows that get a per-key envelope. */
export function resealPlan(channelIdHex: string): {
  readonly readKey: Uint8Array | null;
  readonly perKeyThroughSeq: number | "all";
  /** The account's own reader key this device registered (2026-11-13), or
   *  `null`: resealed to whatever the mark says. */
  readonly ownReaderKeyHex: string | null;
} {
  const own = channelsActions.accessOf(channelIdHex).readerKeyRegisteredHex ?? null;
  return { ...resealRows(channelIdHex), ownReaderKeyHex: own };
}

/** The per-key rows and the public read key of a reseal. */
function resealRows(channelIdHex: string): {
  readonly readKey: Uint8Array | null;
  readonly perKeyThroughSeq: number | "all";
} {
  const access = channelsActions.accessOf(channelIdHex);
  const meta = channelsState.metaByChannel().get(channelIdHex);
  const visibility =
    access.visibility ?? (meta?.listed === undefined ? null : meta.listed ? "public" : "private");
  // Only a device that set the flag and runs the ticket check knows which
  // rows it let in. A private channel never reseals to every row, even on a
  // device holding no admission seed: that device let nobody in, so it
  // reseals to the readers at or below its mark only, never to a row with
  // no valid ticket a server could have added. Every row only for a channel
  // of unknown type (from before phase B).
  const checks =
    visibility === "private" ||
    (access.visibility !== null && (access.visibility === "public" || access.admissionSeeds.length > 0));
  const perKeyThroughSeq: number | "all" = checks ? access.admittedThroughSeq : "all";
  if (visibility !== "public") return { readKey: null, perKeyThroughSeq };
  const readKey = access.readKeyB64 === null ? null : b64ToBytesStd(access.readKeyB64);
  if (readKey === null || readKey.length !== 32) return { readKey: null, perKeyThroughSeq: "all" };
  return { readKey, perKeyThroughSeq };
}

/** The per-key reseal set with the account's own reader key added once
 *  (2026-11-13). Pure. */
export function withOwnReaderKey(keys: readonly string[], ownReaderKeyHex: string | null): string[] {
  if (ownReaderKeyHex === null) return [...keys];
  const own = ownReaderKeyHex.toLowerCase();
  return keys.some((k) => k.toLowerCase() === own) ? [...keys] : [...keys, own];
}

function b64ToBytesStd(b64: string): Uint8Array | null {
  try {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
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

/** Standard base64 of `bytes` (the entry's encoding). */
function b64Of(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 1) bin += String.fromCharCode(bytes[i] ?? 0);
  return btoa(bin);
}
