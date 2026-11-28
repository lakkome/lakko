// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 sections 3, 4 and 5 — `@name` writes this device still owes, and
 * when each held name was last written.
 *
 * A change of a group's `@name` or of who can join is a write at discovery
 * followed by the in-group envelope. When the write does not land (no
 * network, a server error, a token pool that ran dry), the change is recorded
 * here and retried until it lands; a refusal that will never change (the name
 * is taken, the statement is refused) drops it, and the info screen says so.
 *
 * The same queue carries the two writes nobody asks for:
 * - **renewal**: a handle is held 180 days from its last write; an owner
 *   device renews it with fewer than 90 days left
 *   (`group-handle.ts` {@link handleNeedsRenewal});
 * - **republish**: the owner's knock key moved to a new version, and a public
 *   group's record names the knock key.
 *
 * One sealed row per account (`storage/account-kv.ts`). Pure apart from the
 * storage port; the runner is `GroupHandleRuntime.tsx`.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";
import { HandleWriteError } from "../discovery/handle-lookup";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

/** What a subject is: a group (id base64 of 32 bytes) or a channel (hex). */
export type HandleSubjectKind = "group" | "channel";

/** One owed write. `oldHandle` is the name still held at discovery when a
 *  rename is owed. */
export interface OwedHandle {
  readonly kind: HandleSubjectKind;
  readonly id: string;
  readonly op: "claim" | "update" | "replace";
  readonly handle: string;
  readonly oldHandle: string | null;
  readonly isPublic: boolean;
  readonly sinceMs: number;
  /** Why: what the person asked for, or a write nobody asked for. */
  readonly reason: "change" | "renew" | "republish" | "rekey";
  /** ADR-0087 sections 3 and 5: the account key (base64) whose journal root
   *  the server still holds the name under, after an ADR-0074 rotation. The
   *  `rekey` statement, signed with that root's handle key, runs first. */
  readonly rekeyFrom?: string;
  /** ADR-0101 section 7 item 4: after a retake of an owned channel, the
   *  handle write seed (standard base64, 32 bytes; secret, kept only in this
   *  sealed row) the server still holds the channel's name under, from the
   *  owned-channels list. The `rekey` statement signed with it runs first,
   *  moving the name to the key the new account's root derives. */
  readonly rekeyFromSeed?: string;
}

/** When a held name was last written by this account's device. */
export interface HandleWrite {
  readonly kind: HandleSubjectKind;
  readonly id: string;
  readonly handle: string;
  readonly isPublic: boolean;
  readonly lastWriteMs: number;
  readonly leaseUntilMs: number;
}

/** A change that was dropped for good, shown once on the info screen. */
export interface HandleFailure {
  readonly id: string;
  readonly code: string;
}

const ROW = "handle-owed";

function keyOf(kind: HandleSubjectKind, id: string): string {
  return `${kind}:${id}`;
}

/**
 * Fold a new owed write into the one already owed for the same subject. Pure.
 *
 * - A claim never landed: whatever comes next is still a claim.
 * - A rename never landed: the server still holds the OLD name, so a second
 *   rename starts from it, and a flip keeps the rename owed.
 * - A write nobody asked for never replaces what the person asked for.
 */
export function mergeOwed(existing: OwedHandle | undefined, next: OwedHandle): OwedHandle {
  const merged = mergeOp(existing, next);
  // The server holds the name under the OLDEST key not yet re-keyed from; a
  // claim never reached the server, so it is signed with the new key.
  const rekeyFrom = existing?.rekeyFrom ?? next.rekeyFrom;
  const rekeyFromSeed = existing?.rekeyFromSeed ?? next.rekeyFromSeed;
  const { rekeyFrom: _a, rekeyFromSeed: _b, ...rest } = merged;
  if (merged.op === "claim") return rest;
  return {
    ...rest,
    ...(rekeyFrom !== undefined ? { rekeyFrom } : {}),
    ...(rekeyFromSeed !== undefined ? { rekeyFromSeed } : {}),
  };
}

function mergeOp(existing: OwedHandle | undefined, next: OwedHandle): OwedHandle {
  if (existing === undefined) return next;
  // A re-key alone never replaces a write that also has to happen.
  if (next.reason === "rekey" && existing.reason !== "rekey") return existing;
  if (existing.reason === "change" && next.reason !== "change") return existing;
  if (existing.op === "claim") return { ...next, op: "claim", oldHandle: null, sinceMs: existing.sinceMs };
  if (existing.op === "replace") {
    if (next.op === "replace") return { ...next, oldHandle: existing.oldHandle, sinceMs: existing.sinceMs };
    if (next.handle === existing.handle) return { ...existing, isPublic: next.isPublic };
  }
  return next;
}

/** Whether a failed write is worth trying again. Pure. */
export function owedRetryable(status: number | null): boolean {
  // `null`: no answer at all (offline, a token pool that could not refill).
  if (status === null) return true;
  return status === 408 || status === 429 || status >= 500;
}

/** The outcome of one attempt. */
export type OwedOutcome = "done" | "retry" | { readonly dropped: string };

/** Classify a failed write: retried, or dropped for good with a code. Pure. */
export function owedOutcomeOf(err: unknown): OwedOutcome {
  if (err instanceof HandleWriteError) {
    return owedRetryable(err.status) ? "retry" : { dropped: err.code ?? `status-${err.status}` };
  }
  if (err instanceof DiscoveryHttpError) {
    return owedRetryable(err.status) ? "retry" : { dropped: `status-${err.status}` };
  }
  if (err instanceof DiscoveryNetworkError) return "retry";
  // A token pool that could not refill, a knock seed not arrived yet, a
  // failed wasm call: worth trying again.
  return "retry";
}

interface Row {
  readonly owed: readonly OwedHandle[];
  readonly writes: readonly HandleWrite[];
}

function decode(raw: unknown): Row {
  if (raw === null || typeof raw !== "object") return { owed: [], writes: [] };
  const o = raw as { owed?: unknown; writes?: unknown };
  const owed = Array.isArray(o.owed)
    ? o.owed.filter(
        (x): x is OwedHandle =>
          x !== null &&
          typeof x === "object" &&
          typeof (x as OwedHandle).id === "string" &&
          typeof (x as OwedHandle).handle === "string" &&
          ((x as OwedHandle).kind === "group" || (x as OwedHandle).kind === "channel"),
      )
    : [];
  const writes = Array.isArray(o.writes)
    ? o.writes.filter(
        (x): x is HandleWrite =>
          x !== null &&
          typeof x === "object" &&
          typeof (x as HandleWrite).id === "string" &&
          typeof (x as HandleWrite).lastWriteMs === "number",
      )
    : [];
  return { owed, writes };
}

const [owedRows, setOwedRows] = createSignal<readonly OwedHandle[]>([]);
const [writeRows, setWriteRows] = createSignal<readonly HandleWrite[]>([]);
const [failures, setFailures] = createSignal<readonly HandleFailure[]>([]);
let account: string | null = null;
let opened: Promise<void> = Promise.resolve();
let chain: Promise<void> = Promise.resolve();

export function openHandleOwed(accountId: string): Promise<void> {
  if (account === accountId) return opened;
  account = accountId;
  setOwedRows([]);
  setWriteRows([]);
  setFailures([]);
  opened = loadAccountJson(accountId, ROW).then(
    (raw) => {
      if (account !== accountId) return;
      const row = decode(raw);
      setOwedRows(row.owed);
      setWriteRows(row.writes);
    },
    () => undefined,
  );
  return opened;
}

function persist(): Promise<void> {
  const acct = account;
  if (acct === null) return Promise.resolve();
  const body = { v: 1, owed: owedRows(), writes: writeRows() };
  const next = chain.then(() => saveAccountJson(acct, ROW, body));
  chain = next.catch(() => undefined);
  return next;
}

/** The writes still owed. Reactive. */
export function owedHandles(): readonly OwedHandle[] {
  return owedRows();
}

/** The write owed for one subject, if any. Reactive. */
export function owedHandleFor(kind: HandleSubjectKind, id: string): OwedHandle | undefined {
  return owedRows().find((o) => o.kind === kind && o.id === id);
}

/** Record a write as owed (merged with one already owed). Durable before it
 *  resolves. */
export async function recordOwedHandle(o: OwedHandle): Promise<void> {
  await opened;
  const existing = owedRows().find((x) => keyOf(x.kind, x.id) === keyOf(o.kind, o.id));
  const merged = mergeOwed(existing, o);
  setOwedRows([...owedRows().filter((x) => keyOf(x.kind, x.id) !== keyOf(o.kind, o.id)), merged]);
  setFailures(failures().filter((f) => f.id !== o.id));
  await persist();
}

/** The write landed, or will never land. */
export async function dropOwedHandle(kind: HandleSubjectKind, id: string, failureCode?: string): Promise<void> {
  await opened;
  setOwedRows(owedRows().filter((x) => keyOf(x.kind, x.id) !== keyOf(kind, id)));
  if (failureCode !== undefined) setFailures([...failures().filter((f) => f.id !== id), { id, code: failureCode }]);
  await persist();
}

/** The re-key of one subject landed: what is still owed runs under the new
 *  key. */
export async function clearOwedRekey(kind: HandleSubjectKind, id: string): Promise<void> {
  await opened;
  setOwedRows(
    owedRows().map((x) => {
      if (keyOf(x.kind, x.id) !== keyOf(kind, id)) return x;
      if (x.rekeyFrom === undefined && x.rekeyFromSeed === undefined) return x;
      const { rekeyFrom: _done, rekeyFromSeed: _doneSeed, ...rest } = x;
      return rest;
    }),
  );
  await persist();
}

/** A change dropped for good, for the info screen. Reactive. */
export function handleFailureFor(id: string): string | null {
  return failures().find((f) => f.id === id)?.code ?? null;
}

export function dismissHandleFailure(id: string): void {
  setFailures(failures().filter((f) => f.id !== id));
}

/** A write landed: when, so the lease is renewed in time. */
export async function noteHandleWrite(w: HandleWrite): Promise<void> {
  await opened;
  setWriteRows([...writeRows().filter((x) => keyOf(x.kind, x.id) !== keyOf(w.kind, w.id)), w]);
  await persist();
}

/** The last write of one subject, if this device made one. */
export function handleWriteFor(kind: HandleSubjectKind, id: string): HandleWrite | undefined {
  return writeRows().find((x) => x.kind === kind && x.id === id);
}

/** Forget a subject entirely (its name was retired). */
export async function forgetHandle(kind: HandleSubjectKind, id: string): Promise<void> {
  await opened;
  setOwedRows(owedRows().filter((x) => keyOf(x.kind, x.id) !== keyOf(kind, id)));
  setWriteRows(writeRows().filter((x) => keyOf(x.kind, x.id) !== keyOf(kind, id)));
  await persist();
}

/**
 * ADR-0087 section 5: the owner's knock key of this group is about to move
 * to a new version. A public group's record names the knock key, so it is
 * owed a republish; recorded durably BEFORE the bump is written, so no crash
 * can leave a bumped key with a record naming the old one. The name is read
 * when the write runs (`handle` empty): a group that is private by then, or
 * holds no name, has nothing to publish.
 */
export async function noteKnockKeyChanged(accountId: string, groupIdB64: string): Promise<void> {
  await notePublicRecordChanged(accountId, "group", groupIdB64);
}

/**
 * `docs/ux/group-and-channel-privacy.md` 4.5.4 — the public record of a group
 * or channel carries its name, description and photo, so the owner's device
 * (the one holding the handle key) owes a republish whenever it applies a
 * change to any of them, whoever made it (an admin's photo arrives as a
 * group-photo envelope). Recorded durably before the change is kept, retried
 * by the handle runtime until it lands. The name is read when the write runs
 * (`handle` empty): a subject that is private by then, or holds no name, has
 * nothing to publish.
 */
export async function notePublicRecordChanged(
  accountId: string,
  kind: HandleSubjectKind,
  id: string,
): Promise<void> {
  await openHandleOwed(accountId);
  if (account !== accountId) throw new Error("signed out");
  await recordOwedHandle({
    kind,
    id,
    op: "update",
    handle: "",
    oldHandle: null,
    isPublic: true,
    sinceMs: Date.now(),
    reason: "republish",
  });
}

/** {@link notePublicRecordChanged} for the account this device has open,
 *  for a caller that holds no account id (the channel metadata path).
 *  Nothing when no account is open. */
export async function notePublicRecordChangedHere(kind: HandleSubjectKind, id: string): Promise<void> {
  await opened;
  const acct = account;
  if (acct === null) return;
  await notePublicRecordChanged(acct, kind, id);
}

registerAccountMemory("handle-owed", (ids) => {
  if (account !== null && ids.includes(account)) {
    account = null;
    setOwedRows([]);
    setWriteRows([]);
    setFailures([]);
  }
});
