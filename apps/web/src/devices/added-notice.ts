// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "A device was added to your account" (docs/ux/identity-recovery.md
 * section 17.5): the rule, and where it is kept.
 *
 * The trigger is this account's DeviceGroup roster as this device holds it
 * (`DeviceGroupSyncContext.deviceRoster`) gaining a device this device has
 * not acknowledged. The roster, not the directory's device list: it is the
 * end-to-end authenticated membership (a server cannot hide a device from
 * it), and it is the very list Settings > Devices draws, so the badge and
 * the `New` label on a row cannot disagree.
 *
 * Kept per account in `localStorage` (so it survives restarts): the devices
 * this device treats as known, the devices this device itself let in, and
 * the pending devices with the time this device learned of each (the
 * DeviceGroup carries no join date). Device keys are public keys; nothing
 * here is secret.
 *
 * Synced between the account's devices (amended 2026-10-29): the set of
 * devices whose warning was seen, the `lakko-sync` device-acknowledgement
 * register, held by `DeviceGroupSyncContext`. A device pending here whose
 * key is in that set is not pending; folding the set in moves it to known.
 *
 * - The first roster this device ever sees seeds the known set: a device
 *   never shows the notice about itself, and it treats every device already
 *   in the account when it joined as known.
 * - A device this device let in is known at once (and is NOT acknowledged
 *   for the account: the other devices still warn).
 * - Opening Settings > Devices while something is pending acknowledges
 *   every pending device for the account (`beginDevicesVisit`); the
 *   highlight stays for that visit only. Removing the device also ends it.
 *
 * Everything that decides is pure; the storage and the signal are thin.
 */
import { createSignal, untrack, type Accessor } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";

export interface PendingDevice {
  /** The device's signature public key, hex. */
  readonly key: string;
  /** When this device learned of it, unix ms. */
  readonly atMs: number;
}

export interface AddedNoticeState {
  readonly v: 1;
  /** Devices acknowledged on this device. */
  readonly known: readonly string[];
  /** Devices this device let in (acknowledged whenever they show up). */
  readonly letIn: readonly string[];
  /** Devices the notice is about. */
  readonly pending: readonly PendingDevice[];
}

export interface RosterObservation {
  readonly keys: readonly string[];
  readonly ownKey: string;
  /** Keys that entered through a change made on this device. */
  readonly addedHere: readonly string[];
}

export interface ObserveResult {
  /** `null`: still nothing to keep (no real roster was seen yet). */
  readonly state: AddedNoticeState | null;
  /** Devices that became pending with this observation (a local
   *  notification is due when there are any). */
  readonly added: readonly string[];
}

/**
 * Fold one roster observation into the state. An observation that cannot be
 * a real roster (empty, or without this device) changes nothing: a torn
 * read must never read as "every device left" or "every device is new".
 */
export function observeRoster(
  state: AddedNoticeState | null,
  roster: RosterObservation,
  nowMs: number,
  acknowledged: ReadonlySet<string> = new Set(),
): ObserveResult {
  const keys = [...new Set(roster.keys)];
  if (keys.length === 0 || !keys.includes(roster.ownKey)) {
    return { state, added: [] };
  }
  if (state === null) {
    // The first roster seen on this device: everything in it is known.
    return { state: { v: 1, known: keys, letIn: [], pending: [] }, added: [] };
  }
  const inRoster = new Set(keys);
  const letIn = new Set([...state.letIn, ...roster.addedHere]);
  // A device that left is forgotten, so the same key coming back is news.
  const known = new Set(state.known.filter((k) => inRoster.has(k)));
  const pending = state.pending.filter(
    (p) =>
      inRoster.has(p.key) && !letIn.has(p.key) && p.key !== roster.ownKey && !acknowledged.has(p.key),
  );
  const pendingKeys = new Set(pending.map((p) => p.key));
  const added: string[] = [];
  for (const key of keys) {
    if (known.has(key) || pendingKeys.has(key)) continue;
    // Acknowledged on some device of the account (possibly before this one
    // ever saw it): known, never news.
    if (key === roster.ownKey || letIn.has(key) || acknowledged.has(key)) {
      known.add(key);
      continue;
    }
    pending.push({ key, atMs: nowMs });
    added.push(key);
  }
  return {
    state: {
      v: 1,
      known: [...known],
      // Only the let-in devices still (or not yet) relevant are kept: one
      // that is known and gone is dropped with it.
      letIn: [...letIn].filter((k) => inRoster.has(k) || !state.known.includes(k)).slice(-32),
      pending,
    },
    added,
  };
}

/**
 * Fold the account's acknowledged set in: every pending device in it becomes
 * known. Returns the same state object when nothing changed.
 */
export function applyAcknowledged(
  state: AddedNoticeState,
  acknowledged: ReadonlySet<string>,
): AddedNoticeState {
  const moved = state.pending.filter((p) => acknowledged.has(p.key)).map((p) => p.key);
  if (moved.length === 0) return state;
  return {
    v: 1,
    known: [...new Set([...state.known, ...moved])],
    letIn: state.letIn,
    pending: state.pending.filter((p) => !acknowledged.has(p.key)),
  };
}

/** This device let `key` in (a confirmation or a scan run here). */
export function letIn(state: AddedNoticeState | null, key: string): AddedNoticeState {
  const base = state ?? { v: 1 as const, known: [], letIn: [], pending: [] };
  const wasPending = base.pending.some((p) => p.key === key);
  return {
    v: 1,
    known: wasPending ? [...new Set([...base.known, key])] : base.known,
    letIn: [...new Set([...base.letIn, key])].slice(-32),
    pending: base.pending.filter((p) => p.key !== key),
  };
}

export interface AddedNoticeView {
  /** Something is pending: the notification, the Settings badge and the
   *  Devices-row badge are up. */
  readonly visible: boolean;
  /** `devices.added_notice.title`, or `.title_many` for two or more. */
  readonly titleKey: "devices.added_notice.title" | "devices.added_notice.title_many";
  /** The most recent join time, unix ms (`null` when nothing is pending). */
  readonly latestAtMs: number | null;
  /** The pending keys (each carries the `New` label in Settings > Devices). */
  readonly newKeys: readonly string[];
}

/** What is pending: the local pending set minus what the account has
 *  acknowledged. */
export function noticeView(
  state: AddedNoticeState | null,
  acknowledged: ReadonlySet<string> = new Set(),
): AddedNoticeView {
  const pending = (state?.pending ?? []).filter((p) => !acknowledged.has(p.key));
  return viewOf(pending);
}

function viewOf(pending: readonly PendingDevice[]): AddedNoticeView {
  return {
    visible: pending.length > 0,
    titleKey: pending.length > 1 ? "devices.added_notice.title_many" : "devices.added_notice.title",
    latestAtMs: pending.length === 0 ? null : Math.max(...pending.map((p) => p.atMs)),
    newKeys: pending.map((p) => p.key),
  };
}

/** The badge on the Settings destination and on the Devices row. */
export function badgeVisible(view: AddedNoticeView): boolean {
  return view.visible;
}

/**
 * Opening Settings > Devices (17.5 "What counts as seen"). `null` when
 * nothing is pending. Otherwise the keys to acknowledge for the account
 * (every device pending here) and the visit snapshot: what the screen keeps
 * highlighting and explaining until it is left, after the acknowledgement
 * has emptied the pending set.
 */
export function beginDevicesVisit(
  view: AddedNoticeView,
): { readonly toAcknowledge: readonly string[]; readonly visit: AddedNoticeView } | null {
  if (!view.visible) return null;
  return { toAcknowledge: [...view.newKeys], visit: view };
}

/** A visit that saw more devices become pending while it was open. */
export function extendVisit(visit: AddedNoticeView | null, more: AddedNoticeView): AddedNoticeView {
  if (visit === null) return more;
  const keys = [...new Set([...visit.newKeys, ...more.newKeys])];
  const at = [visit.latestAtMs, more.latestAtMs].filter((x): x is number => x !== null);
  return {
    visible: keys.length > 0,
    titleKey: keys.length > 1 ? "devices.added_notice.title_many" : "devices.added_notice.title",
    latestAtMs: at.length === 0 ? null : Math.max(...at),
    newKeys: keys,
  };
}

/**
 * The system notification, decided after each fold. `post`: devices became
 * pending and the person already allowed notifications (never asked for
 * here), whether the app is in front or not. `withdraw`: nothing is pending
 * any more (whichever device acknowledged it). The title follows the whole
 * pending count, so a second device replaces the first notification.
 */
export function notificationAction(input: {
  readonly added: number;
  readonly pendingBefore: number;
  readonly pendingAfter: number;
  readonly permission: "granted" | "denied" | "default" | "unsupported";
}): "post" | "withdraw" | "none" {
  if (input.pendingAfter === 0) return input.pendingBefore > 0 ? "withdraw" : "none";
  if (input.added > 0 && input.permission === "granted") return "post";
  return "none";
}

/** The fixed notification identifier of an account: a second notice
 *  replaces the first, and withdrawing finds it. */
export function noticeTag(accountIdB64: string): string {
  return `lakko-device-added:${accountIdB64}`;
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

const KEY_PREFIX = "lakko/devices/added-notice/v1/";

function storageKey(accountIdB64: string): string {
  return `${KEY_PREFIX}${accountIdB64}`;
}

/** Parse a stored row; anything of another shape reads as "nothing yet". */
export function parseNoticeState(raw: string | null): AddedNoticeState | null {
  if (raw === null) return null;
  try {
    const v = JSON.parse(raw) as Partial<AddedNoticeState> | null;
    if (v === null || typeof v !== "object" || v.v !== 1) return null;
    const strings = (x: unknown): x is string[] =>
      Array.isArray(x) && x.every((k) => typeof k === "string");
    if (!strings(v.known) || !strings(v.letIn) || !Array.isArray(v.pending)) return null;
    const pending: PendingDevice[] = [];
    for (const p of v.pending as unknown[]) {
      const row = p as Partial<PendingDevice> | null;
      if (
        row === null ||
        typeof row !== "object" ||
        typeof row.key !== "string" ||
        typeof row.atMs !== "number" ||
        !Number.isFinite(row.atMs)
      ) {
        return null;
      }
      pending.push({ key: row.key, atMs: row.atMs });
    }
    return { v: 1, known: v.known, letIn: v.letIn, pending };
  } catch {
    return null;
  }
}

function storage(): Storage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}

export function loadNoticeState(accountIdB64: string): AddedNoticeState | null {
  try {
    return parseNoticeState(storage()?.getItem(storageKey(accountIdB64)) ?? null);
  } catch {
    return null;
  }
}

function saveNoticeState(accountIdB64: string, state: AddedNoticeState): void {
  try {
    storage()?.setItem(storageKey(accountIdB64), JSON.stringify(state));
  } catch {
    // A blocked store: the notice lives for the page only.
  }
}

/** Drop the account's row (Forget device, Delete account, the wipe). */
export function clearAddedNotice(accountIdB64: string): void {
  try {
    storage()?.removeItem(storageKey(accountIdB64));
  } catch {
    // best-effort
  }
  if (current()?.accountIdB64 === accountIdB64) setCurrent(null);
}

// ---------------------------------------------------------------------------
// The live state of the signed-in account
// ---------------------------------------------------------------------------

const [current, setCurrent] = createSignal<{
  readonly accountIdB64: string;
  readonly state: AddedNoticeState | null;
} | null>(null);

/** The account's acknowledged set as the synced state holds it (hex keys). */
const [acknowledgedSet, setAcknowledgedSet] = createSignal<{
  readonly accountIdB64: string;
  readonly keys: ReadonlySet<string>;
} | null>(null);

/** The open visit to Settings > Devices, or `null` when it is not open. */
const [visit, setVisit] = createSignal<AddedNoticeView | null>(null);

// The live state is what the next roster fold starts from and saves: a state
// held from before Forget device (or put back by a fold in flight across it)
// would be saved into the wiped account's store at its next sign-in on this
// page, and `bindAddedNotice` keeps a state already bound to the account.
registerAccountMemory(
  "device-added-notice",
  (ids) => {
    const c = current();
    if (c !== null && ids.includes(c.accountIdB64)) setCurrent(null);
    const a = acknowledgedSet();
    if (a !== null && ids.includes(a.accountIdB64)) setAcknowledgedSet(null);
    setVisit(null);
  },
  { signOut: true },
);

/** Bind the live state to `accountIdB64` (or to nobody). */
export function bindAddedNotice(accountIdB64: string | null): void {
  if (accountIdB64 === null) {
    setCurrent(null);
    setAcknowledgedSet(null);
    setVisit(null);
    return;
  }
  if (untrack(current)?.accountIdB64 === accountIdB64) return;
  setCurrent({ accountIdB64, state: loadNoticeState(accountIdB64) });
  setVisit(null);
}

function update(accountIdB64: string, next: AddedNoticeState): void {
  saveNoticeState(accountIdB64, next);
  if (untrack(current)?.accountIdB64 === accountIdB64) setCurrent({ accountIdB64, state: next });
}

function acknowledgedOf(accountIdB64: string | undefined): ReadonlySet<string> {
  const a = acknowledgedSet();
  return a !== null && a.accountIdB64 === accountIdB64 ? a.keys : new Set();
}

/** Reactive: what is pending (the badges and the notification). */
export const addedNotice: Accessor<AddedNoticeView> = () => {
  const c = current();
  return noticeView(c?.state ?? null, acknowledgedOf(c?.accountIdB64));
};

/** Reactive: the open visit's snapshot, or `null` outside Settings > Devices. */
export const devicesVisit: Accessor<AddedNoticeView | null> = visit;

/** Reactive: the keys Settings > Devices highlights (the visit's snapshot,
 *  or what is pending when no visit snapshot covers it). */
export function highlightedDeviceKeys(): ReadonlySet<string> {
  return new Set([...(visit()?.newKeys ?? []), ...addedNotice().newKeys]);
}

/** Settings > Devices opened (or a device became pending while it is open).
 *  Returns the keys to acknowledge for the account; the caller writes and
 *  sends them through the synced state. */
export function enterDevicesVisit(): readonly string[] {
  const begun = beginDevicesVisit(untrack(addedNotice));
  if (begun === null) return [];
  setVisit(extendVisit(untrack(visit), begun.visit));
  return begun.toAcknowledge;
}

/** Settings > Devices left: the highlight and the explanation go. */
export function leaveDevicesVisit(): void {
  setVisit(null);
}

/** Fold a roster of `accountIdB64`; returns how many devices became pending. */
export function recordRoster(accountIdB64: string, roster: RosterObservation, nowMs: number): number {
  // Read untracked: the watcher calls this from an effect, and a tracked read
  // of the state it then writes re-ran that effect without end.
  const held = untrack(current);
  const before = held?.accountIdB64 === accountIdB64 ? held.state : loadNoticeState(accountIdB64);
  const acked = untrack(() => acknowledgedOf(accountIdB64));
  const result = observeRoster(before, roster, nowMs, acked);
  if (result.state !== null) update(accountIdB64, result.state);
  return result.added.length;
}

/** The synced acknowledged set of `accountIdB64` changed (hex keys). */
export function recordAcknowledged(accountIdB64: string, keys: readonly string[]): void {
  const set = new Set(keys);
  setAcknowledgedSet({ accountIdB64, keys: set });
  const held = untrack(current);
  const before = held?.accountIdB64 === accountIdB64 ? held.state : loadNoticeState(accountIdB64);
  if (before === null) return;
  const next = applyAcknowledged(before, set);
  if (next !== before) update(accountIdB64, next);
}

/** This device let the device with signature key `deviceKey` in. */
export function noteDeviceLetIn(accountIdB64: string, deviceKey: Uint8Array): void {
  let hex = "";
  for (const b of deviceKey) hex += b.toString(16).padStart(2, "0");
  update(accountIdB64, letIn(loadNoticeState(accountIdB64), hex));
}
