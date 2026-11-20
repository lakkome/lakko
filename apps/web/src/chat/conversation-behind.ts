// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 1 and 2 - where this device stands in each
 * conversation: in step, suspected of being behind, or confirmed behind.
 *
 * ## What feeds it
 *
 * Every conversation poll (`ChatClient.pollMessages`) reports one read:
 *
 * - whether the read was DRAINED (no row, nothing more, nothing held);
 * - the DS's `latest_epoch` and the group's own epoch;
 * - whether this device owes a commit of its own (an ADR-0040 outbox entry or
 *   a staged sweep awaiting its verdict);
 *
 * and the store adds what it kept between reads: consecutive refused reads
 * (a `401` on a read signed at the current key, clock not skewed), whether a
 * row framed ABOVE the local epoch failed since the last applied row, and
 * whether a publish of this device to the journal was refused. The verdict
 * itself is the core's (`conversationBehindVerdict`, written once in Rust for
 * the three clients); this module only folds observations into it.
 *
 * ## What it keeps
 *
 * Per (account, group): the state and since when (durable, so the waiting
 * note survives a restart), and the gap bookkeeping section 2 asks for: the
 * time of the first row this device skipped, the highest framed epoch among
 * skipped rows, whether any was a commit, and the commit rows the DS last
 * named above this device's epoch (so the device can read again the rows
 * after the commit that brings it back). Counters live in memory only: a
 * restart re-learns them in a few reads.
 *
 * One `localStorage` row per (account, group):
 * `lakko.chat.behind.v1:<account>:<group>`, written through the ADR-0077
 * `Storage.setItem` gate. Dropped with the group (`membership-wipe.ts`), on
 * Forget device (`wipe-account.ts`) and when the device is back in step.
 * Nothing in it leaves the device.
 */

import { createSignal } from "solid-js";

import { boundContactsAccount } from "../contacts/local-store";
import * as wasmModule from "../../wasm/lakko_ffi_wasm";
import type { BehindState as WasmBehindState } from "../../wasm/lakko_ffi_wasm";

const KEY_PREFIX = "lakko.chat.behind.v1:";

/** The three states of section 1. */
export type BehindKind = "in-step" | "suspected" | "confirmed";

/** One state, as the core returns it. */
export interface BehindStateView {
  readonly kind: BehindKind;
  /** When the suspicion began; absent while in step. */
  readonly sinceMs?: number;
  /** C-93: no commit, sweep, proposal, registration or repeat envelope. */
  readonly stopsActing: boolean;
}

export const IN_STEP: BehindStateView = { kind: "in-step", stopsActing: false };

/** One read of a conversation journal, as the verdict takes it. */
export interface BehindObservation {
  readonly ownCommitOwed: boolean;
  readonly drained: boolean;
  readonly latestEpoch: bigint | undefined;
  readonly localEpoch: bigint;
  readonly refusedReads: number;
  readonly failedRowAboveLocal: boolean;
  readonly publishRefused: boolean;
  readonly nowMs: number;
}

/** The fold: the core's `conversationBehindVerdict` (or a test's double). */
export type BehindVerdictFn = (previous: BehindStateView, observation: BehindObservation) => BehindStateView;

/** The part of the wasm surface the verdict adapter needs, structurally. */
export interface BehindVerdictWasm {
  readonly BehindStateKind: { readonly InStep: number; readonly Suspected: number; readonly Confirmed: number };
  readonly BehindState: new (kind: never, sinceMs: bigint) => WasmBehindState;
  conversationBehindVerdict(
    previous: WasmBehindState,
    ownCommitOwed: boolean,
    drained: boolean,
    latestEpoch: bigint | null | undefined,
    localEpoch: bigint,
    refusedReads: number,
    failedRowAboveLocal: boolean,
    publishRefused: boolean,
    nowMs: bigint,
  ): WasmBehindState;
}

function kindToWasm(wasm: BehindVerdictWasm, kind: BehindKind): number {
  if (kind === "suspected") return wasm.BehindStateKind.Suspected;
  if (kind === "confirmed") return wasm.BehindStateKind.Confirmed;
  return wasm.BehindStateKind.InStep;
}

function kindFromWasm(wasm: BehindVerdictWasm, kind: number): BehindKind {
  if (kind === wasm.BehindStateKind.Suspected) return "suspected";
  if (kind === wasm.BehindStateKind.Confirmed) return "confirmed";
  return "in-step";
}

/** Build a core `BehindState` from a view (the caller frees it). */
export function toWasmBehindState(wasm: BehindVerdictWasm, view: BehindStateView): WasmBehindState {
  return new wasm.BehindState(kindToWasm(wasm, view.kind) as never, BigInt(Math.max(0, Math.floor(view.sinceMs ?? 0))));
}

/** Read a core `BehindState` into a view (does not free it). */
export function fromWasmBehindState(wasm: BehindVerdictWasm, state: WasmBehindState): BehindStateView {
  const kind = kindFromWasm(wasm, Number(state.kind));
  if (kind === "in-step") return IN_STEP;
  const since = state.sinceMs;
  return {
    kind,
    ...(since === undefined ? {} : { sinceMs: Number(since) }),
    stopsActing: state.stopsActing,
  };
}

/** The core's verdict as a {@link BehindVerdictFn}. */
export function wasmBehindVerdict(wasm: BehindVerdictWasm): BehindVerdictFn {
  return (previous, o) => {
    const prev = toWasmBehindState(wasm, previous);
    let next: WasmBehindState | undefined;
    try {
      next = wasm.conversationBehindVerdict(
        prev,
        o.ownCommitOwed,
        o.drained,
        o.latestEpoch ?? undefined,
        o.localEpoch,
        o.refusedReads,
        o.failedRowAboveLocal,
        o.publishRefused,
        BigInt(Math.max(0, Math.floor(o.nowMs))),
      );
      return fromWasmBehindState(wasm, next);
    } finally {
      prev.free();
      next?.free();
    }
  };
}

/** What the store keeps per group. */
interface GroupRecord {
  state: BehindStateView;
  refusedReads: number;
  failedAbove: boolean;
  publishRefused: boolean;
  /** The first skipped row's DS time, unix ms. */
  gapFromMs?: number;
  /** The highest framed epoch among skipped rows. */
  highestSkippedEpoch?: bigint;
  skippedCommit: boolean;
  /** The commit rows (epoch -> DS id) the last read named above this
   *  device's epoch. */
  commits: [bigint, number][];
  /** The newest `latest_epoch` a read named. */
  latestEpoch?: bigint;
  /** Live walk 2 (2026-11-19): the newest DS time of a row this device
   *  applied here, or of an own publish the DS accepted. Read progress, not
   *  gap bookkeeping: kept when the device is back in step. */
  lastKnownRowMs?: number;
  /** The local time of the last read of this journal that was not refused
   *  (written at most once a minute). */
  lastReadOkMs?: number;
}

function emptyRecord(): GroupRecord {
  return { state: IN_STEP, refusedReads: 0, failedAbove: false, publishRefused: false, skippedCommit: false, commits: [] };
}

/** Where the durable half of a record goes. */
export interface BehindBackend {
  load(key: string): string | null;
  save(key: string, value: string): void;
  remove(key: string): void;
  keys(): string[];
}

/** `localStorage`, every call guarded. */
export const localStorageBehindBackend: BehindBackend = {
  load(key) {
    try {
      return globalThis.localStorage?.getItem(key) ?? null;
    } catch {
      return null;
    }
  },
  save(key, value) {
    try {
      globalThis.localStorage?.setItem(key, value);
    } catch {
      /* storage unavailable or the session sealed: the state is re-learned */
    }
  },
  remove(key) {
    try {
      globalThis.localStorage?.removeItem(key);
    } catch {
      /* nothing to remove */
    }
  },
  keys() {
    try {
      const store = globalThis.localStorage;
      if (store === undefined) return [];
      const out: string[] = [];
      for (let i = 0; i < store.length; i += 1) {
        const k = store.key(i);
        if (k !== null) out.push(k);
      }
      return out;
    } catch {
      return [];
    }
  },
};

interface Durable {
  readonly v: 1;
  readonly kind: BehindKind;
  readonly since?: number;
  readonly gapFrom?: number;
  readonly highest?: string;
  readonly commit?: boolean;
  readonly commits?: readonly (readonly [string, number])[];
  readonly latest?: string;
  readonly known?: number;
  readonly readOk?: number;
}

function encodeDurable(r: GroupRecord): string {
  const d: Durable = {
    v: 1,
    kind: r.state.kind,
    ...(r.state.sinceMs === undefined ? {} : { since: r.state.sinceMs }),
    ...(r.gapFromMs === undefined ? {} : { gapFrom: r.gapFromMs }),
    ...(r.highestSkippedEpoch === undefined ? {} : { highest: r.highestSkippedEpoch.toString() }),
    ...(r.skippedCommit ? { commit: true } : {}),
    ...(r.commits.length === 0 ? {} : { commits: r.commits.map(([e, id]) => [e.toString(), id] as const) }),
    ...(r.latestEpoch === undefined ? {} : { latest: r.latestEpoch.toString() }),
    ...(r.lastKnownRowMs === undefined ? {} : { known: r.lastKnownRowMs }),
    ...(r.lastReadOkMs === undefined ? {} : { readOk: r.lastReadOkMs }),
  };
  return JSON.stringify(d);
}

function decodeDurable(raw: string): GroupRecord | null {
  let d: Partial<Durable>;
  try {
    d = JSON.parse(raw) as Partial<Durable>;
  } catch {
    return null;
  }
  if (typeof d !== "object" || d === null || d.v !== 1) return null;
  if (d.kind !== "in-step" && d.kind !== "suspected" && d.kind !== "confirmed") return null;
  const record = emptyRecord();
  if (d.kind !== "in-step") {
    record.state = {
      kind: d.kind,
      ...(typeof d.since === "number" ? { sinceMs: d.since } : {}),
      stopsActing: true,
    };
  }
  if (typeof d.gapFrom === "number" && Number.isFinite(d.gapFrom)) record.gapFromMs = d.gapFrom;
  if (typeof d.highest === "string" && /^\d{1,20}$/.test(d.highest)) record.highestSkippedEpoch = BigInt(d.highest);
  record.skippedCommit = d.commit === true;
  if (typeof d.latest === "string" && /^\d{1,20}$/.test(d.latest)) record.latestEpoch = BigInt(d.latest);
  if (typeof d.known === "number" && Number.isFinite(d.known)) record.lastKnownRowMs = d.known;
  if (typeof d.readOk === "number" && Number.isFinite(d.readOk)) record.lastReadOkMs = d.readOk;
  if (Array.isArray(d.commits)) {
    for (const entry of d.commits) {
      if (
        Array.isArray(entry) &&
        typeof entry[0] === "string" &&
        /^\d{1,20}$/.test(entry[0]) &&
        typeof entry[1] === "number" &&
        Number.isSafeInteger(entry[1])
      ) {
        record.commits.push([BigInt(entry[0]), entry[1]]);
      }
    }
  }
  return record;
}

/** A skipped row, as the poll saw it. */
export interface SkippedRow {
  /** The row's framed epoch, read without decrypting. */
  readonly sealed: bigint | undefined;
  /** The epoch after it (greater for a commit). */
  readonly after: bigint | undefined;
  /** The group's epoch before the row. */
  readonly local: bigint | undefined;
  /** The row's DS time, unix ms (`NaN` when unreadable). */
  readonly atMs: number;
}

/** The gap bookkeeping of one group, read for a request and the gap line. */
export interface GapFacts {
  readonly gapFromMs: number | undefined;
  readonly highestSkippedEpoch: bigint | undefined;
  readonly skippedCommit: boolean;
}

/** How often the last good read is written. */
const READ_OK_WRITE_INTERVAL_MS = 60_000;

/**
 * Where the gap starts: the earlier of the first skipped row and the last
 * moment the device knew the conversation (newest applied or published row,
 * else last good read); else the start of the suspicion; `undefined` when
 * nothing is known (the caller falls back to now).
 */
function gapStartOf(r: GroupRecord): number | undefined {
  const known = r.lastKnownRowMs ?? r.lastReadOkMs;
  const candidates = [r.gapFromMs, known].filter((v): v is number => v !== undefined && Number.isFinite(v));
  if (candidates.length > 0) return Math.min(...candidates);
  return r.state.sinceMs;
}

/**
 * UX `conversation-rejoin.md` 2.2 (live walk 2, 2026-11-19): a waiting chat
 * (suspected or confirmed behind) shows no error banner. Its reads are
 * refused by design until the device is back; the waiting note says why.
 */
export function chatErrorsHidden(state: BehindStateView): boolean {
  return state.kind !== "in-step";
}

/** The per-(account, group) state of section 1. */
export class ConversationBehindStore {
  private readonly records = new Map<string, GroupRecord>();
  private readonly loaded = new Set<string>();
  private readonly versionSignal = createSignal(0);

  constructor(
    private readonly verdict: () => BehindVerdictFn | null,
    private readonly backend: BehindBackend = localStorageBehindBackend,
    private readonly account: () => string | null = () => boundContactsAccount(),
    private readonly now: () => number = () => Date.now(),
  ) {}

  /** Reactive: bumped whenever any group's state changes. */
  version(): number {
    return this.versionSignal[0]();
  }

  private bump(): void {
    this.versionSignal[1]((v) => v + 1);
  }

  private keyOf(group: string, owner?: string): string | null {
    const account = owner ?? this.account();
    if (account === null || account.length === 0) return null;
    return `${KEY_PREFIX}${account}:${group}`;
  }

  private record(group: string): GroupRecord | null {
    const key = this.keyOf(group);
    if (key === null) return null;
    let r = this.records.get(key);
    if (r === undefined) {
      const raw = this.loaded.has(key) ? null : this.backend.load(key);
      this.loaded.add(key);
      r = (raw === null ? null : decodeDurable(raw)) ?? emptyRecord();
      this.records.set(key, r);
    }
    return r;
  }

  private persist(group: string, r: GroupRecord): void {
    const key = this.keyOf(group);
    if (key === null) return;
    const empty =
      r.state.kind === "in-step" &&
      r.gapFromMs === undefined &&
      r.highestSkippedEpoch === undefined &&
      !r.skippedCommit &&
      r.commits.length === 0 &&
      r.lastKnownRowMs === undefined &&
      r.lastReadOkMs === undefined;
    if (empty) this.backend.remove(key);
    else this.backend.save(key, encodeDurable(r));
  }

  /** The group's state; in step when unknown. Reactive through
   *  {@link version}. */
  state(group: string): BehindStateView {
    this.version();
    return this.record(group)?.state ?? IN_STEP;
  }

  /** C-93: the device stops acting in this conversation. */
  stopsActing(group: string): boolean {
    return this.state(group).stopsActing;
  }

  /**
   * Live walk 3 (2026-11-20): a send that is not the person's own content
   * (a repeat envelope, typing, a read receipt, a beacon) is held while the
   * device stops acting here, and also while a read of this journal was
   * refused (an unskewed `401`) with no answered read since: two refused
   * reads came before the third that suspects, and the profile republish and
   * a typing signal went out between them and drew `401`s.
   */
  holdsAutomaticSends(group: string): boolean {
    const r = this.record(group);
    if (r === null) return false;
    this.version();
    return r.state.stopsActing || r.refusedReads > 0;
  }

  /** The waiting note is shown (UX 2.2). */
  isConfirmed(group: string): boolean {
    return this.state(group).kind === "confirmed";
  }

  /** Every group of the bound account that is not in step. */
  behindGroups(): string[] {
    const account = this.account();
    if (account === null) return [];
    const prefix = `${KEY_PREFIX}${account}:`;
    const out = new Set<string>();
    for (const k of this.backend.keys()) if (k.startsWith(prefix)) out.add(k.slice(prefix.length));
    for (const [k, r] of this.records) if (k.startsWith(prefix) && r.state.kind !== "in-step") out.add(k.slice(prefix.length));
    return [...out].filter((g) => this.state(g).kind !== "in-step");
  }

  /**
   * The gap bookkeeping of section 2. `gapFromMs` is where the gap starts
   * (live walk 2, 2026-11-19): the earlier of the first skipped row and the
   * last moment this device knew the conversation (its newest applied or
   * published row, else its last good read). A device behind through refused
   * reads skips no row, so the skipped row alone named nothing.
   */
  gap(group: string): GapFacts {
    const r = this.record(group);
    return {
      gapFromMs: r === null ? undefined : gapStartOf(r),
      highestSkippedEpoch: r?.highestSkippedEpoch,
      skippedCommit: r?.skippedCommit ?? false,
    };
  }

  /** The newest `latest_epoch` a read of this group named. */
  latestEpoch(group: string): bigint | undefined {
    return this.record(group)?.latestEpoch;
  }

  /** The commit rows the last read named above this device's epoch. */
  registeredCommits(group: string): ReadonlyMap<bigint, number> {
    return new Map(this.record(group)?.commits ?? []);
  }

  /** A row was skipped (it failed to apply and was not held). */
  noteSkippedRow(group: string, row: SkippedRow): void {
    const r = this.record(group);
    if (r === null) return;
    let changed = false;
    if (Number.isFinite(row.atMs) && (r.gapFromMs === undefined || row.atMs < r.gapFromMs)) {
      r.gapFromMs = row.atMs;
      changed = true;
    }
    if (row.sealed !== undefined && (r.highestSkippedEpoch === undefined || row.sealed > r.highestSkippedEpoch)) {
      r.highestSkippedEpoch = row.sealed;
      changed = true;
    }
    if (row.sealed !== undefined && row.after !== undefined && row.after > row.sealed && !r.skippedCommit) {
      r.skippedCommit = true;
      changed = true;
    }
    if (row.sealed !== undefined && row.local !== undefined && row.sealed > row.local) r.failedAbove = true;
    if (changed) this.persist(group, r);
  }

  /** A row applied: "since its last applied row" starts again. `atMs` is
   *  the row's DS time (`NaN` when unreadable). */
  noteAppliedRow(group: string, atMs: number = Number.NaN): void {
    const r = this.record(group);
    if (r === null) return;
    r.failedAbove = false;
    this.raiseKnown(group, r, atMs);
  }

  /** An own publish to this journal was accepted at the DS time `atMs`. */
  noteOwnRowAccepted(group: string, atMs: number): void {
    const r = this.record(group);
    if (r !== null) this.raiseKnown(group, r, atMs);
  }

  private raiseKnown(group: string, r: GroupRecord, atMs: number): void {
    if (!Number.isFinite(atMs) || atMs <= 0) return;
    if (r.lastKnownRowMs !== undefined && r.lastKnownRowMs >= atMs) return;
    r.lastKnownRowMs = atMs;
    this.persist(group, r);
  }

  /** A read was not refused: the last good read, written at most once a
   *  minute (an earlier value only starts the gap a little earlier). */
  private noteReadOk(group: string, r: GroupRecord): void {
    const now = this.now();
    if (r.lastReadOkMs !== undefined && now >= r.lastReadOkMs && now - r.lastReadOkMs < READ_OK_WRITE_INTERVAL_MS) return;
    r.lastReadOkMs = now;
    this.persist(group, r);
  }

  /** A publish of this device to the journal was refused `401`, clock not
   *  skewed (section 1, evidence). */
  notePublishRefused(group: string): void {
    const r = this.record(group);
    if (r === null) return;
    // Evidence is weighed by the core at the next read that suspects (a
    // refusal alone moves nothing); kept until the device is back in step.
    r.publishRefused = true;
  }


  /**
   * A read was refused `401`. `clockSkewed`: the refusal was the device's
   * clock, which says nothing about the journal. Returns the new state.
   */
  noteRefusedRead(group: string, input: { readonly clockSkewed: boolean; readonly localEpoch: bigint | undefined; readonly ownCommitOwed: boolean }): BehindStateView {
    const r = this.record(group);
    if (r === null) return IN_STEP;
    if (input.clockSkewed || input.localEpoch === undefined) return r.state;
    r.refusedReads += 1;
    return this.fold(group, r, { ownCommitOwed: input.ownCommitOwed, drained: false, latestEpoch: undefined, localEpoch: input.localEpoch });
  }

  /**
   * One successful read. `registeredCommits` is the read's `commits` (kept
   * for the rewind after a re-entry). Returns the new state.
   */
  observeRead(
    group: string,
    input: {
      readonly ownCommitOwed: boolean;
      readonly drained: boolean;
      readonly latestEpoch: bigint | undefined;
      readonly localEpoch: bigint | undefined;
      readonly registeredCommits?: ReadonlyMap<bigint, number> | undefined;
    },
  ): BehindStateView {
    const r = this.record(group);
    if (r === null || input.localEpoch === undefined) return r?.state ?? IN_STEP;
    // Live walk 3 (W3-R6): a refused-read hold ends without the device ever
    // having been behind. What it held is re-sent as on back-in-step.
    const holdEnding = r.refusedReads > 0 && r.state.kind === "in-step";
    r.refusedReads = 0;
    this.noteReadOk(group, r);
    if (input.latestEpoch !== undefined) r.latestEpoch = input.latestEpoch;
    if (input.registeredCommits !== undefined && input.registeredCommits.size > 0) {
      const merged = new Map(r.commits);
      for (const [e, id] of input.registeredCommits) if (e > input.localEpoch) merged.set(e, id);
      const next = [...merged.entries()].filter(([e]) => e > input.localEpoch!).sort((a, b) => (a[0] < b[0] ? -1 : 1)).slice(-64);
      if (next.length !== r.commits.length || next.some(([e, id], i) => r.commits[i]?.[0] !== e || r.commits[i]?.[1] !== id)) {
        r.commits = next;
        if (r.state.kind !== "in-step") this.persist(group, r);
      }
    }
    const next = this.fold(group, r, {
      ownCommitOwed: input.ownCommitOwed,
      drained: input.drained,
      latestEpoch: input.latestEpoch,
      localEpoch: input.localEpoch,
    });
    if (holdEnding && next.kind === "in-step") this.announceBackInStep(group);
    return next;
  }

  private fold(
    group: string,
    r: GroupRecord,
    input: { ownCommitOwed: boolean; drained: boolean; latestEpoch: bigint | undefined; localEpoch: bigint },
  ): BehindStateView {
    const verdict = this.verdict();
    if (verdict === null) return r.state;
    const previous = r.state;
    const next = verdict(previous, {
      ownCommitOwed: input.ownCommitOwed,
      drained: input.drained,
      latestEpoch: input.latestEpoch,
      localEpoch: input.localEpoch,
      refusedReads: r.refusedReads,
      failedRowAboveLocal: r.failedAbove,
      publishRefused: r.publishRefused,
      nowMs: this.now(),
    });
    if (next.kind === "in-step") {
      const hadAnything =
        previous.kind !== "in-step" ||
        r.gapFromMs !== undefined ||
        r.highestSkippedEpoch !== undefined ||
        r.skippedCommit ||
        r.commits.length > 0 ||
        r.publishRefused;
      // Cleared (section 1): the bookkeeping that fed the suspicion goes.
      if (input.drained || previous.kind !== "in-step") {
        delete r.gapFromMs;
        delete r.highestSkippedEpoch;
        r.skippedCommit = false;
        r.commits = [];
        delete r.latestEpoch;
        r.publishRefused = false;
        r.failedAbove = false;
      }
      r.state = IN_STEP;
      if (hadAnything) {
        this.persist(group, r);
        if (previous.kind !== "in-step") {
          // eslint-disable-next-line no-console
          console.info("chat conversation back in step: state=in-step");
          this.bump();
          this.announceBackInStep(group);
        }
      }
      return r.state;
    }
    const changed = next.kind !== previous.kind || next.sinceMs !== previous.sinceMs;
    r.state = next;
    if (changed) {
      this.persist(group, r);
      // eslint-disable-next-line no-console
      console.warn(`chat conversation behind: state=${next.kind} local=${input.localEpoch} latest=${input.latestEpoch ?? "-"}`);
      this.bump();
    }
    return r.state;
  }

  /** The device is back (the re-entry landed): in step, bookkeeping gone.
   *  It applied no row since joining, so the join is its last good read. */
  clear(group: string): void {
    const r = this.record(group);
    if (r === null) return;
    const was = r.state.kind;
    const key = this.keyOf(group);
    const fresh = emptyRecord();
    fresh.lastReadOkMs = this.now();
    if (key !== null) this.records.set(key, fresh);
    this.persist(group, fresh);
    if (was !== "in-step") {
      this.bump();
      this.announceBackInStep(group);
    }
  }

  private readonly backInStepListeners = new Set<(group: string) => void>();

  /**
   * ADR-0091 live walk 3 — called when a conversation this device was behind
   * in is back in step (a read cleared it, or the re-entry landed), and when
   * a read is answered after refused reads that never made it behind
   * (W3-R6). What was
   * held meanwhile and is owed (a handle stored first, the growth-time state)
   * is re-sent from here. Returns the unsubscribe.
   */
  onBackInStep(listener: (group: string) => void): () => void {
    this.backInStepListeners.add(listener);
    return () => this.backInStepListeners.delete(listener);
  }

  private announceBackInStep(group: string): void {
    for (const l of this.backInStepListeners) {
      try {
        l(group);
      } catch {
        /* a listener's failure is its own */
      }
    }
  }

  /** The group left this device (`membership-wipe.ts`). */
  forgetGroup(group: string, owner?: string): void {
    const key = this.keyOf(group, owner);
    if (key === null) return;
    this.records.delete(key);
    this.loaded.add(key);
    this.backend.remove(key);
    this.bump();
  }

  /** Forget device: every row of the account. */
  forgetAccount(accountIdB64: string): void {
    const prefix = `${KEY_PREFIX}${accountIdB64}:`;
    for (const k of [...this.records.keys()]) if (k.startsWith(prefix)) this.records.delete(k);
    for (const k of this.backend.keys()) if (k.startsWith(prefix)) this.backend.remove(k);
    this.bump();
  }

  /** Tests. */
  resetForTests(): void {
    this.records.clear();
    this.loaded.clear();
  }
}

/**
 * The core's verdict over the module namespace, guarded: before the wasm
 * module is initialised a call throws, and the state is then kept as it was
 * (the next read folds again).
 */
const moduleVerdict: BehindVerdictFn = (() => {
  const verdict = wasmBehindVerdict(wasmModule as unknown as BehindVerdictWasm);
  return (previous: BehindStateView, observation: BehindObservation): BehindStateView => {
    try {
      return verdict(previous, observation);
    } catch {
      return previous;
    }
  };
})();

/** The module verdict; a test may install a double. */
let installedVerdict: BehindVerdictFn | null = moduleVerdict;

/** Install the core verdict (`wasmBehindVerdict(await loadWasm())`). */
export function installBehindVerdict(verdict: BehindVerdictFn | null): void {
  installedVerdict = verdict;
}

/** The app's instance. */
export const conversationBehind = new ConversationBehindStore(() => installedVerdict);

/** Parse a DS `created_at` into unix ms (`NaN` when unreadable). */
export function rowTimeMs(createdAt: string | undefined): number {
  if (createdAt === undefined) return Number.NaN;
  const v = Date.parse(createdAt);
  return Number.isFinite(v) ? v : Number.NaN;
}

/** Back to the core's verdict over the module namespace. */
export function restoreModuleBehindVerdict(): void {
  installedVerdict = moduleVerdict;
}
