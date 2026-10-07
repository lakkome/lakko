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
 * it), and it is the very list Settings > Devices draws, so the banner and
 * the `New` label on a row cannot disagree.
 *
 * Kept per account in `localStorage` (so it survives restarts and each
 * browser dismisses its own): the devices already acknowledged, the devices
 * this device itself let in, and the devices the notice is about with the
 * time this device learned of each (the DeviceGroup carries no join date).
 * Device keys are public keys; nothing here is secret.
 *
 * - The first roster this device ever sees seeds the acknowledged set: a
 *   device never shows the notice about itself, and it treats every device
 *   already in the account when it joined as known.
 * - A device this device let in is acknowledged at once.
 * - `Dismiss`, or the device's removal, ends the notice. Opening Devices
 *   does not.
 *
 * Everything that decides is pure; the storage and the signal are thin.
 */
import { createSignal, type Accessor } from "solid-js";

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
    (p) => inRoster.has(p.key) && !letIn.has(p.key) && p.key !== roster.ownKey,
  );
  const pendingKeys = new Set(pending.map((p) => p.key));
  const added: string[] = [];
  for (const key of keys) {
    if (known.has(key) || pendingKeys.has(key)) continue;
    if (key === roster.ownKey || letIn.has(key)) {
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

/** `Dismiss`: every device the notice is about is acknowledged. */
export function dismissNotice(state: AddedNoticeState): AddedNoticeState {
  return {
    v: 1,
    known: [...new Set([...state.known, ...state.pending.map((p) => p.key)])],
    letIn: state.letIn,
    pending: [],
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
  /** The banner is shown. */
  readonly visible: boolean;
  /** `devices.added_notice.title`, or `.title_many` for two or more. */
  readonly titleKey: "devices.added_notice.title" | "devices.added_notice.title_many";
  /** The most recent join time, unix ms (`null` when hidden). */
  readonly latestAtMs: number | null;
  /** The keys that carry the `New` label in Settings > Devices. */
  readonly newKeys: readonly string[];
}

export function noticeView(state: AddedNoticeState | null): AddedNoticeView {
  const pending = state?.pending ?? [];
  return {
    visible: pending.length > 0,
    titleKey: pending.length > 1 ? "devices.added_notice.title_many" : "devices.added_notice.title",
    latestAtMs: pending.length === 0 ? null : Math.max(...pending.map((p) => p.atMs)),
    newKeys: pending.map((p) => p.key),
  };
}

/** Whether the local notification is raised: only when devices became
 *  pending, the app is running but not in front, and the person already
 *  allowed notifications (never asked for here). */
export function shouldNotify(input: {
  readonly added: number;
  readonly hidden: boolean;
  readonly permission: "granted" | "denied" | "default" | "unsupported";
}): boolean {
  return input.added > 0 && input.hidden && input.permission === "granted";
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

/** Bind the live state to `accountIdB64` (or to nobody). */
export function bindAddedNotice(accountIdB64: string | null): void {
  if (accountIdB64 === null) {
    setCurrent(null);
    return;
  }
  if (current()?.accountIdB64 === accountIdB64) return;
  setCurrent({ accountIdB64, state: loadNoticeState(accountIdB64) });
}

function update(accountIdB64: string, next: AddedNoticeState): void {
  saveNoticeState(accountIdB64, next);
  if (current()?.accountIdB64 === accountIdB64) setCurrent({ accountIdB64, state: next });
}

/** Reactive: what the banner and the Devices list show. */
export const addedNotice: Accessor<AddedNoticeView> = () => noticeView(current()?.state ?? null);

/** Fold a roster of `accountIdB64`; returns how many devices became pending. */
export function recordRoster(accountIdB64: string, roster: RosterObservation, nowMs: number): number {
  const before =
    current()?.accountIdB64 === accountIdB64
      ? (current()?.state ?? null)
      : loadNoticeState(accountIdB64);
  const result = observeRoster(before, roster, nowMs);
  if (result.state !== null) update(accountIdB64, result.state);
  return result.added.length;
}

/** `Dismiss` on the banner. */
export function dismissAddedNotice(accountIdB64: string): void {
  const state = loadNoticeState(accountIdB64);
  if (state !== null) update(accountIdB64, dismissNotice(state));
}

/** This device let the device with signature key `deviceKey` in. */
export function noteDeviceLetIn(accountIdB64: string, deviceKey: Uint8Array): void {
  let hex = "";
  for (const b of deviceKey) hex += b.toString(16).padStart(2, "0");
  update(accountIdB64, letIn(loadNoticeState(accountIdB64), hex));
}
