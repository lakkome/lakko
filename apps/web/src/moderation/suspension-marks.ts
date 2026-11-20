// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 step 4, `docs/ux/reporting-and-moderation.md` section 3 - whether
 * ANOTHER person's account is suspended, as this page shows it.
 *
 * A suspension is shown only after the moderation log confirms it: the
 * client fetches the snapshot bucket holding the account
 * (`GET {moderation}/moderation/snapshot/latest/bucket/{prefix}`, anonymous)
 * and the core checks it against the pinned log operator key and the
 * transparency witnesses (`moderationAccountStanding`). Anything that cannot
 * be checked is `unknown`, and `unknown` is shown exactly as `not`: the mark
 * is a warning, not access control.
 *
 * A `suspended` standing is not trusted on the snapshot alone (F2 of the
 * ADR-0099 security pass): the snapshot names a log entry, and the client
 * fetches that entry (`GET {moderation}/moderation/log/entry/{logIndex}?size=
 * {root.log_size}`), the moderator list (`GET /moderation/moderators`) and
 * proves with `moderationProveAccountStanding` /
 * `moderationProveChannelStanding` that a listed moderator signed this
 * suspension under the pinned moderation root key. Unproven, failed, or no
 * root key pinned: `unknown`. The proof is kept with the answer it was made
 * for, until that answer's reuse time.
 *
 * Memory only: the raw answer is kept per bucket prefix until the snapshot's
 * `reuseUntilMs`, one request is in flight per prefix, a failed fetch is not
 * tried again for ten minutes for that prefix. Nothing is written to disk, so
 * when a suspension ends nothing about it is kept. No log line names an
 * account key or a prefix.
 */

import { createSignal } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { anonymousFetchInit } from "../tokens/anon-token-pool";
import { pinnedWitnessKeys, witnessThreshold } from "../transparency/config";
import { loadWasm } from "../wasm";
import {
  MODERATION_API_BASE,
  acceptModeratorList,
  cachedModeratorList,
  decodeRootKey,
  forgetModeratorList,
  pinnedModerationRootKey,
  wasmListVersion,
} from "./report-api";

/** What the log says about one account. `unknown` reads as `not`. */
export type SuspensionStanding = "suspended" | "not" | "unknown";

/** A failed fetch (or an answer that does not check) is not tried again for
 *  this long for that prefix. Chosen. */
export const FAILURE_BACKOFF_MS = 10 * 60_000;

/** An answer is kept at least this long, even when the snapshot's own reuse
 *  time has already passed (the server serves the latest snapshot, which may
 *  be the same one until the next is built), so a list of names does not ask
 *  again on every render. Chosen. */
export const MIN_REUSE_MS = 60_000;

/** The two core functions this module asks (the wasm surface, structurally;
 *  tests pass a fake). */
export interface StandingWasm {
  moderationBucketPrefix(accountKey: Uint8Array): string;
  moderationAccountStanding(
    answerJson: string,
    accountKey: Uint8Array,
    logOperatorKey: Uint8Array,
    witnessKeys: Uint8Array,
    witnessThreshold: number,
    nowMs: bigint,
  ): ModerationStandingRecord;
  /** ADR-0099 step 5 - the bucket of a 16-byte channel id. Absent on a core
   *  built before step 5: every channel is then `unknown`. */
  moderationChannelBucketPrefix?(channelId: Uint8Array): string;
  /** ADR-0099 step 5 - whether the channel is removed (`suspended`). */
  moderationChannelStanding?(
    answerJson: string,
    channelId: Uint8Array,
    logOperatorKey: Uint8Array,
    witnessKeys: Uint8Array,
    witnessThreshold: number,
    nowMs: bigint,
  ): ModerationStandingRecord;
  /** F2 - a moderator signed the account's suspension. Throws
   *  `"<code>: <reason>"` when it cannot be proven. Absent on an older core:
   *  then no suspension is ever proven. */
  moderationProveAccountStanding?(
    answerJson: string,
    entryJson: string,
    moderatorList: Uint8Array,
    moderationRootKey: Uint8Array,
    accountKey: Uint8Array,
    nowMs: bigint,
  ): void;
  /** F2 - a moderator signed the channel's removal. */
  moderationProveChannelStanding?(
    answerJson: string,
    entryJson: string,
    moderatorList: Uint8Array,
    moderationRootKey: Uint8Array,
    channelId: Uint8Array,
    nowMs: bigint,
  ): void;
}

/** The core's standing record (the step-4 `ModerationStanding`). */
export interface ModerationStandingRecord {
  readonly suspended: boolean;
  readonly untilMs: bigint | undefined;
  readonly reuseUntilMs: bigint;
  /** The log index of the bucket entry, for the proof; `undefined` when the
   *  bucket holds none (or the core predates F2). */
  readonly logIndex?: bigint | undefined;
}

/** What {@link SuspensionMarks.check} answers: the standing, and when it is
 *  worth asking again. */
export interface StandingResult {
  readonly standing: SuspensionStanding;
  /** Wall-clock ms after which a caller holding this result asks again;
   *  `Infinity` when asking again can never change it (nothing pinned). */
  readonly recheckAtMs: number;
}

export interface SuspensionMarksDeps {
  /** The core, loaded. */
  readonly wasm: () => Promise<StandingWasm>;
  /** The bucket answer's body for `prefix`; throws for no answer or any
   *  status but 200. */
  readonly fetchBucket: (prefix: string) => Promise<string>;
  /** The pinned moderation log operator key, or `null`: then no marks and no
   *  requests at all. */
  readonly logKey: Uint8Array | null;
  /** The transparency witness keys (empty: operator-only). */
  readonly witnessKeys: readonly Uint8Array[];
  readonly witnessThreshold: number;
  readonly now: () => number;
  /** The pinned moderator root key, or `null`: then no suspension is ever
   *  proven, so none is shown. */
  readonly rootKey: Uint8Array | null;
  /** `GET /moderation/log/entry/{logIndex}?size={logSize}`'s body; throws for
   *  no answer or any status but 200. */
  readonly fetchEntry: (logIndex: bigint, logSize: bigint) => Promise<string>;
  /** The signed moderator list's bytes, or `null` when there is none; throws
   *  for no answer. The core verifies it under {@link rootKey}. */
  readonly moderatorList: () => Promise<Uint8Array | null>;
}

interface CachedAnswer {
  readonly answer: string;
  readonly keepUntilMs: number;
  /** Proofs made for this answer, by target (`a:`/`c:` + hex): whether the
   *  suspension it shows was signed by a moderator. Dropped with it. */
  readonly proofs: Map<string, Promise<boolean>>;
}

/** The `root.log_size` of a bucket answer, or `null`. Pure. */
export function answerLogSize(answer: string): bigint | null {
  try {
    const parsed = JSON.parse(answer) as { root?: { log_size?: unknown } };
    const size = parsed.root?.log_size;
    if (typeof size !== "number" || !Number.isSafeInteger(size) || size < 0) return null;
    return BigInt(size);
  } catch {
    return null;
  }
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The code part of a core error (`"<code>: <reason>"`), for a log line that
 *  names no identifier. */
function errorCode(err: unknown): string {
  const message = err instanceof Error ? err.message : typeof err === "string" ? err : "";
  const code = message.split(":")[0]?.trim() ?? "";
  return /^[a-z-]{1,40}$/.test(code) ? code : "error";
}

function flatten(keys: readonly Uint8Array[]): Uint8Array {
  const out = new Uint8Array(keys.length * 32);
  keys.forEach((k, i) => out.set(k, i * 32));
  return out;
}

type ProveFn = (
  wasm: StandingWasm,
  answer: string,
  entry: string,
  list: Uint8Array,
  rootKey: Uint8Array,
  nowMs: bigint,
) => void;

/** The standing service: one per page (see {@link suspensionMarks}). */
export class SuspensionMarks {
  private readonly answers = new Map<string, CachedAnswer>();
  private readonly inFlight = new Map<string, Promise<CachedAnswer | null>>();
  private readonly failedUntil = new Map<string, number>();
  private readonly witnessFlat: Uint8Array;

  constructor(private readonly deps: SuspensionMarksDeps) {
    this.witnessFlat = flatten(deps.witnessKeys.filter((k) => k.length === 32));
  }

  /** Whether this build pins a log key at all. */
  get enabled(): boolean {
    return this.deps.logKey !== null;
  }

  /** The standing of `accountKey`. Never throws. */
  async standing(accountKey: Uint8Array): Promise<SuspensionStanding> {
    return (await this.check(accountKey)).standing;
  }

  /** The standing of `accountKey` and when to ask again. Never throws. */
  async check(accountKey: Uint8Array): Promise<StandingResult> {
    return this.checkWith(
      (wasm) => wasm.moderationBucketPrefix(accountKey),
      (wasm, answer, logKey, nowMs) =>
        wasm.moderationAccountStanding(answer, accountKey, logKey, this.witnessFlat, this.deps.witnessThreshold, nowMs),
      `a:${hexOf(accountKey)}`,
      (wasm, answer, entry, list, rootKey, nowMs) => {
        if (wasm.moderationProveAccountStanding === undefined) throw new Error("no proof in this core");
        wasm.moderationProveAccountStanding(answer, entry, list, rootKey, accountKey, nowMs);
      },
    );
  }

  /**
   * ADR-0099 step 5 - whether the channel `channelId` (16 bytes) is removed
   * (`suspended`), and when to ask again. The bucket answers are the same
   * cache as the accounts': one request per prefix whatever it names. Never
   * throws.
   */
  async checkChannel(channelId: Uint8Array): Promise<StandingResult> {
    return this.checkWith(
      (wasm) => {
        if (wasm.moderationChannelBucketPrefix === undefined) throw new Error("no channel standing in this core");
        return wasm.moderationChannelBucketPrefix(channelId);
      },
      (wasm, answer, logKey, nowMs) => {
        if (wasm.moderationChannelStanding === undefined) throw new Error("no channel standing in this core");
        return wasm.moderationChannelStanding(
          answer,
          channelId,
          logKey,
          this.witnessFlat,
          this.deps.witnessThreshold,
          nowMs,
        );
      },
      `c:${hexOf(channelId)}`,
      (wasm, answer, entry, list, rootKey, nowMs) => {
        if (wasm.moderationProveChannelStanding === undefined) throw new Error("no proof in this core");
        wasm.moderationProveChannelStanding(answer, entry, list, rootKey, channelId, nowMs);
      },
    );
  }

  private async checkWith(
    prefixOf: (wasm: StandingWasm) => string,
    verify: (wasm: StandingWasm, answer: string, logKey: Uint8Array, nowMs: bigint) => ModerationStandingRecord,
    targetTag: string,
    prove: ProveFn,
  ): Promise<StandingResult> {
    const logKey = this.deps.logKey;
    if (logKey === null) return { standing: "unknown", recheckAtMs: Number.POSITIVE_INFINITY };
    let wasm: StandingWasm;
    let prefix: string;
    try {
      wasm = await this.deps.wasm();
      prefix = prefixOf(wasm);
    } catch {
      return { standing: "unknown", recheckAtMs: Number.POSITIVE_INFINITY };
    }
    const bound = (answer: string, nowMs: bigint): ModerationStandingRecord => verify(wasm, answer, logKey, nowMs);
    const cached = await this.answerFor(prefix, bound);
    const now = this.deps.now();
    if (cached === null) {
      return { standing: "unknown", recheckAtMs: (this.failedUntil.get(prefix) ?? now) };
    }
    let s: ModerationStandingRecord;
    try {
      s = bound(cached.answer, BigInt(Math.floor(now)));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.info("[moderation] standing unavailable", errorCode(err));
      return { standing: "unknown", recheckAtMs: cached.keepUntilMs };
    }
    if (!s.suspended) return { standing: "not", recheckAtMs: cached.keepUntilMs };
    let recheckAtMs = cached.keepUntilMs;
    if (s.untilMs !== undefined) recheckAtMs = Math.min(recheckAtMs, Number(s.untilMs));
    // A suspension is acted on only once a moderator's signature is proven.
    const proven = await this.proofFor(cached, targetTag, s.logIndex, wasm, prove);
    return { standing: proven ? "suspended" : "unknown", recheckAtMs };
  }

  /** Whether the suspension `cached` shows for `targetTag` is proven; one
   *  proof per target per answer, kept with the answer. Never throws. */
  private proofFor(
    cached: CachedAnswer,
    targetTag: string,
    logIndex: bigint | undefined,
    wasm: StandingWasm,
    prove: ProveFn,
  ): Promise<boolean> {
    const held = cached.proofs.get(targetTag);
    if (held !== undefined) return held;
    const attempt = this.runProof(cached.answer, logIndex, wasm, prove);
    cached.proofs.set(targetTag, attempt);
    return attempt;
  }

  private async runProof(
    answer: string,
    logIndex: bigint | undefined,
    wasm: StandingWasm,
    prove: ProveFn,
  ): Promise<boolean> {
    const rootKey = this.deps.rootKey;
    if (rootKey === null || logIndex === undefined) return false;
    const logSize = answerLogSize(answer);
    if (logSize === null) return false;
    try {
      const [entry, list] = await Promise.all([this.deps.fetchEntry(logIndex, logSize), this.deps.moderatorList()]);
      if (list === null) return false;
      prove(wasm, answer, entry, list, rootKey, BigInt(Math.floor(this.deps.now())));
      return true;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.info("[moderation] standing not proven", errorCode(err));
      return false;
    }
  }

  /** The answer for `prefix`: cached, the request already out, or a new
   *  one. `null` while the prefix is backing off after a failure. */
  private async answerFor(
    prefix: string,
    verify: (answer: string, nowMs: bigint) => ModerationStandingRecord,
  ): Promise<CachedAnswer | null> {
    const now = this.deps.now();
    const held = this.answers.get(prefix);
    if (held !== undefined && now < held.keepUntilMs) return held;
    const out = this.inFlight.get(prefix);
    if (out !== undefined) return out;
    const failed = this.failedUntil.get(prefix);
    if (failed !== undefined && now < failed) return null;
    const request = this.fetchAnswer(prefix, verify).finally(() => {
      this.inFlight.delete(prefix);
    });
    this.inFlight.set(prefix, request);
    return request;
  }

  private async fetchAnswer(
    prefix: string,
    verify: (answer: string, nowMs: bigint) => ModerationStandingRecord,
  ): Promise<CachedAnswer | null> {
    try {
      const answer = await this.deps.fetchBucket(prefix);
      // The answer is checked once here, which also yields its reuse time;
      // an answer that does not check is a failure, not a cache entry.
      const now = this.deps.now();
      const s = verify(answer, BigInt(Math.floor(now)));
      const keepUntilMs = Math.max(Number(s.reuseUntilMs), now + MIN_REUSE_MS);
      const entry: CachedAnswer = { answer, keepUntilMs, proofs: new Map() };
      this.answers.set(prefix, entry);
      this.failedUntil.delete(prefix);
      return entry;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.info("[moderation] snapshot unavailable", errorCode(err));
      this.answers.delete(prefix);
      this.failedUntil.set(prefix, this.deps.now() + FAILURE_BACKOFF_MS);
      return null;
    }
  }

  /** Forget everything held (sign-out). */
  clear(): void {
    this.answers.clear();
    this.failedUntil.clear();
  }
}

// ----------------------------------------------------------------------------
// The page's service and its reactive reads
// ----------------------------------------------------------------------------

/** The pinned moderation log operator key (build time, base64url, 32 bytes),
 *  or `null`: then nothing is marked and nothing is asked. */
export function pinnedModerationLogKey(): Uint8Array | null {
  const raw = (import.meta as { env?: Record<string, string | undefined> }).env?.VITE_LAKKO_MODERATION_LOG_KEY;
  return decodeRootKey(raw);
}

/** `GET {moderation}/moderation/snapshot/latest/bucket/{prefix}`, anonymous. */
export async function fetchSnapshotBucket(prefix: string, base: string = MODERATION_API_BASE): Promise<string> {
  if (!/^[0-9a-f]{4}$/.test(prefix)) throw new Error("bad prefix");
  const response = await fetch(
    `${base}/moderation/snapshot/latest/bucket/${prefix}`,
    anonymousFetchInit({ method: "GET", headers: {} }),
  );
  if (!response.ok) throw new Error(`bucket answered ${response.status}`);
  return response.text();
}

/** `GET {moderation}/moderation/log/entry/{logIndex}?size={logSize}`,
 *  anonymous. */
export async function fetchLogEntry(
  logIndex: bigint,
  logSize: bigint,
  base: string = MODERATION_API_BASE,
): Promise<string> {
  if (logIndex < 0n || logSize <= logIndex) throw new Error("bad index");
  const response = await fetch(
    `${base}/moderation/log/entry/${logIndex.toString()}?size=${logSize.toString()}`,
    anonymousFetchInit({ method: "GET", headers: {} }),
  );
  if (!response.ok) throw new Error(`entry answered ${response.status}`);
  return response.text();
}

/**
 * The held moderator list, refused (`null`) when its version is below the
 * highest this device has seen (a server cannot hand an older list, in which
 * a since-revoked moderator is still listed); a newer one raises that floor.
 * `null` too when the list does not verify under the pinned root key.
 */
async function floorCheckedModeratorList(): Promise<Uint8Array | null> {
  const rootKey = pinnedModerationRootKey();
  if (rootKey === null) return null;
  const list = await cachedModeratorList();
  if (list === null) return null;
  return acceptModeratorList(list, rootKey, wasmListVersion(await loadWasm()));
}

let service: SuspensionMarks | null = null;

/** The page's one service, built from the build's pins. */
export function suspensionMarks(): SuspensionMarks {
  if (service === null) {
    service = new SuspensionMarks({
      wasm: async () => (await loadWasm()) as unknown as StandingWasm,
      fetchBucket: (prefix) => fetchSnapshotBucket(prefix),
      logKey: pinnedModerationLogKey(),
      witnessKeys: pinnedWitnessKeys(),
      witnessThreshold: witnessThreshold(),
      now: () => Date.now(),
      rootKey: pinnedModerationRootKey(),
      fetchEntry: (logIndex, logSize) => fetchLogEntry(logIndex, logSize),
      moderatorList: () => floorCheckedModeratorList(),
    });
  }
  return service;
}

/** Tests: replace the page's service. */
export function installSuspensionMarks(next: SuspensionMarks | null): void {
  service = next;
  results.clear();
  asking.clear();
  setVersion((v) => v + 1);
}

const [version, setVersion] = createSignal(0);
const results = new Map<string, StandingResult>();
const asking = new Set<string>();
/** This account's own keys (standard base64: the current account key and a
 *  conversation predecessor): never marked. */
let ownKeysB64: () => readonly string[] = () => [];

/** Install how this page reads its own account keys (standard base64). */
export function installOwnAccountKeys(provider: () => readonly string[]): void {
  ownKeysB64 = provider;
}

function isOwn(keyB64: string): boolean {
  try {
    return ownKeysB64().includes(keyB64);
  } catch {
    return false;
  }
}

function keyBytes(keyB64: string): Uint8Array | null {
  try {
    const k = base64Decode(keyB64);
    return k.length === 32 ? k : null;
  } catch {
    return null;
  }
}

function ask(keyB64: string, key: Uint8Array): void {
  if (asking.has(keyB64)) return;
  asking.add(keyB64);
  // Never a signal write inside the read that asked.
  queueMicrotask(() => {
    void suspensionMarks()
      .check(key)
      .then((r) => {
        const before = results.get(keyB64)?.standing;
        results.set(keyB64, r);
        asking.delete(keyB64);
        if (before !== r.standing) setVersion((v) => v + 1);
      });
  });
}

/**
 * Reactive: whether the account `keyB64` (standard base64, the person key) is
 * shown as suspended. Asks the log the first time and whenever the held
 * answer is due again; `false` until a verified `suspended` is in. This
 * device's own account is never marked.
 */
export function isSuspendedKeyB64(keyB64: string | null | undefined): boolean {
  version();
  if (keyB64 === null || keyB64 === undefined || keyB64.length === 0) return false;
  if (isOwn(keyB64)) return false;
  const service = suspensionMarks();
  if (!service.enabled) return false;
  const held = results.get(keyB64);
  if (held === undefined || Date.now() >= held.recheckAtMs) {
    const key = keyBytes(keyB64);
    if (key === null) return false;
    ask(keyB64, key);
  }
  // A held mark stays until the answer asked again replaces it.
  return held?.standing === "suspended";
}

/**
 * The standing of `keyB64` awaited for at most `boundMs` (a contact request's
 * arrival). `unknown` on timeout, for this device's own account, and for a
 * key that is not one.
 */
export async function standingWithin(keyB64: string, boundMs: number): Promise<SuspensionStanding> {
  if (isOwn(keyB64)) return "unknown";
  const key = keyBytes(keyB64);
  if (key === null) return "unknown";
  const service = suspensionMarks();
  if (!service.enabled) return "unknown";
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<SuspensionStanding>((resolve) => {
    timer = setTimeout(() => resolve("unknown"), boundMs);
  });
  try {
    return await Promise.race([service.standing(key), timeout]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/**
 * The standings of several accounts at once (one batch of contact requests),
 * all asked together under ONE bound: a slow moderation server holds the
 * batch `boundMs` at most, never `boundMs` per request. A key not answered
 * in time, this device's own and a key that is not one read `unknown`.
 */
export async function standingsWithin(
  keysB64: readonly string[],
  boundMs: number,
): Promise<ReadonlyMap<string, SuspensionStanding>> {
  const out = new Map<string, SuspensionStanding>();
  const unique = [...new Set(keysB64)];
  for (const k of unique) out.set(k, "unknown");
  const service = suspensionMarks();
  if (!service.enabled || unique.length === 0) return out;
  const asks: Promise<void>[] = [];
  for (const k of unique) {
    if (isOwn(k)) continue;
    const key = keyBytes(k);
    if (key === null) continue;
    asks.push(
      service.standing(key).then((standing) => {
        out.set(k, standing);
      }),
    );
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, boundMs);
  });
  try {
    await Promise.race([Promise.all(asks), timeout]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
  // A late answer must not change what the batch already decided.
  return new Map(out);
}

/**
 * The accounts among `keysB64` the moderation log confirms are suspended,
 * all asked at once under one bound ({@link standingsWithin}). A timeout or
 * an unchecked standing is not in the set: such a request proceeds.
 */
export async function suspendedAmong(keysB64: readonly string[], boundMs: number): Promise<ReadonlySet<string>> {
  const standings = await standingsWithin(keysB64, boundMs);
  return new Set([...standings].filter(([, standing]) => standing === "suspended").map(([k]) => k));
}

/** Sign-out: nothing held. */
export function clearSuspensionMarks(): void {
  service?.clear();
  forgetModeratorList();
  results.clear();
  asking.clear();
  setVersion((v) => v + 1);
}
