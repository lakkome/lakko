// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 - the durable rows a device keeps to come back into a
 * conversation it fell behind in, and the rows a group's members keep to
 * reach its owner and admins.
 *
 * - **The re-entry record** (section 3), written BEFORE a request is sent:
 *   the group, the re-entry KeyPackages of the last requests (their private
 *   keys stay in the MLS provider until the join or a newer request), the
 *   epoch named, the gap start, when it was last sent and by which route.
 *   `Group.joinReentry` takes the recorded packages; a Welcome refused as
 *   `groupStillActive` is tried as a re-entry while a record exists, and is
 *   never dropped (section 5).
 * - **The read-from mark**: after a re-entry the poll reads again from the
 *   commit that brought the device back, so rows sent after it and skipped
 *   before the Welcome arrived are applied.
 * - **The gap lines** (UX 2.4): a local line in the chat where messages are
 *   missing. Never sent, never read for others.
 * - **The group's knock key** (section 3, route 3): the owner sends
 *   `(version, public key)` to every member in-group; a member keeps the
 *   newest from the owner or an admin. And the owner's mark of what it sent.
 *
 * - **The re-admitter's owed sends** (section 7): the gap's messages this
 *   device owes a device it brought back, sent once its group stands at the
 *   epoch the replacement created. Public fields only (group, leaf key,
 *   epoch, times).
 * - **The gap looks** (section 7, live walk 2026-11-19): the device that came
 *   back keeps looking for the gap's messages until they arrive or the DS
 *   retention passed. The 32-byte history secret is stored sealed under the
 *   account's device key (`DeviceCipher`), never in the clear.
 *
 * Every row is `localStorage`, per account, written through the ADR-0077
 * `Storage.setItem` gate; dropped with the group (`membership-wipe.ts`) and
 * on Forget device (`wipe-account.ts`). Nothing here is secret: a KeyPackage
 * and a knock public key are public artefacts, and the KeyPackage private
 * keys never leave the MLS provider.
 */

import { createSignal } from "solid-js";

import { boundContactsAccount } from "../contacts/local-store";

/** Bumped on every gap-line change, so an open chat redraws its lines. */
const [gapVersion, setGapVersion] = createSignal(0);
/** Reactive read of the gap lines' version. */
export function gapLinesVersion(): number {
  return gapVersion();
}

const RECORDS_PREFIX = "lakko.chat.reentry.v1:";
const READ_FROM_PREFIX = "lakko.chat.reentry-read-from.v1:";
const GAP_PREFIX = "lakko.chat.reentry-gap.v1:";
const KNOCK_KEY_PREFIX = "lakko.chat.knock-key.v1:";
const KNOCK_KEY_SENT_PREFIX = "lakko.chat.knock-key-sent.v1:";
const OWED_PREFIX = "lakko.chat.reentry-owed.v1:";
const GAP_LOOK_PREFIX = "lakko.chat.reentry-gap-look.v1:";

const ALL_PREFIXES = [
  RECORDS_PREFIX,
  READ_FROM_PREFIX,
  GAP_PREFIX,
  KNOCK_KEY_PREFIX,
  KNOCK_KEY_SENT_PREFIX,
  OWED_PREFIX,
  GAP_LOOK_PREFIX,
];

/** The route a request took (section 3). */
export type ReentryRouteName = "own-device" | "mailbox" | "knock";

/** One durable re-entry record. */
export interface ReentryRecord {
  readonly groupIdB64: string;
  /** Standard base64 KeyPackage wires, oldest first, at most
   *  {@link REENTRY_KEY_PACKAGES_KEPT}. */
  readonly keyPackagesB64: readonly string[];
  /** The `latest_epoch` the newest request named, decimal. */
  readonly epochLeft: string;
  /** When the device first failed to read a row, unix ms. */
  readonly gapFromMs: number;
  /** When the newest request was sent (or about to be), unix ms. */
  readonly lastSentMs: number;
  readonly route: ReentryRouteName;
  /**
   * ADR-0091 live walk 5 (2026-11-20): a request of the current behind
   * episode went out by route `own-device` and the device is still behind.
   * From then on every request of the episode takes the conversation's own
   * route (the core's `reentryRoute`, `ownDeviceUnanswered`). Cleared with
   * the episode ({@link endReentryEpisode}). Absent reads as `false`.
   */
  readonly ownDeviceAsked?: boolean;
}

/** The core's `reentryRequestsKept()` (2): the requests whose KeyPackage keys
 *  are kept. Pinned in the tests against the core. */
export const REENTRY_KEY_PACKAGES_KEPT = 2;

/** A gap line: messages between `fromMs` and `toMs` can't be shown here. */
export interface GapLine {
  readonly fromMs: number;
  readonly toMs: number;
}

/** A knock key a member holds. */
export interface HeldKnockKey {
  readonly version: number;
  readonly publicKeyB64: string;
}

function account(owner?: string): string | null {
  const a = owner ?? boundContactsAccount();
  return a === null || a.length === 0 ? null : a;
}

function read(key: string): string | null {
  try {
    return globalThis.localStorage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  globalThis.localStorage?.setItem(key, value);
}

function remove(key: string): void {
  try {
    globalThis.localStorage?.removeItem(key);
  } catch {
    /* nothing to remove */
  }
}

function isRecord(raw: unknown): raw is ReentryRecord {
  if (typeof raw !== "object" || raw === null) return false;
  const r = raw as Partial<ReentryRecord>;
  return (
    typeof r.groupIdB64 === "string" &&
    Array.isArray(r.keyPackagesB64) &&
    r.keyPackagesB64.every((s) => typeof s === "string") &&
    typeof r.epochLeft === "string" &&
    /^\d{1,20}$/.test(r.epochLeft) &&
    typeof r.gapFromMs === "number" &&
    typeof r.lastSentMs === "number" &&
    (r.route === "own-device" || r.route === "mailbox" || r.route === "knock") &&
    (r.ownDeviceAsked === undefined || typeof r.ownDeviceAsked === "boolean")
  );
}

function loadRecords(owner?: string): Record<string, ReentryRecord> {
  const a = account(owner);
  if (a === null) return {};
  const raw = read(RECORDS_PREFIX + a);
  if (raw === null) return {};
  try {
    const parsed = JSON.parse(raw) as { v?: unknown; records?: unknown };
    if (parsed.v !== 1 || !Array.isArray(parsed.records)) return {};
    const out: Record<string, ReentryRecord> = {};
    for (const r of parsed.records) if (isRecord(r)) out[r.groupIdB64] = r;
    return out;
  } catch {
    return {};
  }
}

function saveRecords(records: Record<string, ReentryRecord>, owner?: string): void {
  const a = account(owner);
  if (a === null) throw new Error("no account bound");
  const list = Object.values(records);
  if (list.length === 0) {
    remove(RECORDS_PREFIX + a);
    return;
  }
  write(RECORDS_PREFIX + a, JSON.stringify({ v: 1, records: list }));
}

/** The record of `groupIdB64`, if any. */
export function reentryRecordFor(groupIdB64: string, owner?: string): ReentryRecord | undefined {
  return loadRecords(owner)[groupIdB64];
}

/** Every record of the bound account. */
export function reentryRecords(owner?: string): ReentryRecord[] {
  return Object.values(loadRecords(owner));
}

/**
 * Add a newly minted request to the group's record, keeping the newest
 * {@link REENTRY_KEY_PACKAGES_KEPT} KeyPackages. THROWS when it could not be
 * written: the request must not go out (section 3, "durably before
 * sending"). Returns the KeyPackages dropped, whose keys the caller forgets.
 */
export function recordReentryRequest(
  input: {
    readonly groupIdB64: string;
    readonly keyPackageB64: string;
    readonly epochLeft: bigint;
    readonly gapFromMs: number;
    readonly sentAtMs: number;
    readonly route: ReentryRouteName;
  },
  owner?: string,
): string[] {
  const records = loadRecords(owner);
  const prior = records[input.groupIdB64];
  const all = [...(prior?.keyPackagesB64 ?? []), input.keyPackageB64];
  const kept = all.slice(-REENTRY_KEY_PACKAGES_KEPT);
  const dropped = all.slice(0, all.length - kept.length);
  records[input.groupIdB64] = {
    groupIdB64: input.groupIdB64,
    keyPackagesB64: kept,
    epochLeft: input.epochLeft.toString(),
    // The gap starts at the earliest start a request named (live walk 2: a
    // later request may know an earlier start, never a later one).
    gapFromMs: prior === undefined ? input.gapFromMs : Math.min(prior.gapFromMs, input.gapFromMs),
    lastSentMs: input.sentAtMs,
    route: input.route,
    ownDeviceAsked: prior?.ownDeviceAsked === true,
  };
  saveRecords(records, owner);
  return dropped;
}

/**
 * The newest request went out at `sentAtMs`. A request that went by route
 * `own-device` marks the episode's own device as asked. A no-op without a
 * record.
 */
export function markReentryRequestSent(groupIdB64: string, sentAtMs: number, owner?: string): void {
  const records = loadRecords(owner);
  const prior = records[groupIdB64];
  if (prior === undefined) return;
  records[groupIdB64] = {
    ...prior,
    lastSentMs: sentAtMs,
    ownDeviceAsked: prior.ownDeviceAsked === true || prior.route === "own-device",
  };
  try {
    saveRecords(records, owner);
  } catch {
    /* the request is sent again sooner: harmless */
  }
}

/**
 * A request by route `own-device` could not be sent (ADR-0091 walk 5): the
 * own device is unavailable for this episode, as if it had been asked and
 * had not answered. A no-op without a record.
 */
export function markOwnDeviceUnavailable(groupIdB64: string, owner?: string): void {
  const records = loadRecords(owner);
  const prior = records[groupIdB64];
  if (prior === undefined || prior.ownDeviceAsked === true) return;
  records[groupIdB64] = { ...prior, ownDeviceAsked: true };
  try {
    saveRecords(records, owner);
  } catch {
    /* no account: nothing stored */
  }
}

/**
 * The device is back in step without a re-entry (ADR-0091 live walk 5): the
 * behind episode ends, so the next one asks its own device first again. The
 * KeyPackages stay (a Welcome that answers them can still land). A no-op
 * without a record.
 */
export function endReentryEpisode(groupIdB64: string, owner?: string): void {
  const records = loadRecords(owner);
  const prior = records[groupIdB64];
  if (prior === undefined || prior.ownDeviceAsked !== true) return;
  records[groupIdB64] = { ...prior, ownDeviceAsked: false };
  try {
    saveRecords(records, owner);
  } catch {
    /* no account: nothing stored */
  }
}

/** Drop the group's record (the re-entry landed, or the group left). */
export function deleteReentryRecord(groupIdB64: string, owner?: string): void {
  const records = loadRecords(owner);
  if (records[groupIdB64] === undefined) return;
  delete records[groupIdB64];
  try {
    saveRecords(records, owner);
  } catch {
    /* no account: nothing stored */
  }
}

// ---------- read-from mark ------------------------------------------------

/** Read again from after `fromId` on the next poll of `groupIdB64`. */
export function setReentryReadFrom(groupIdB64: string, fromId: number, owner?: string): void {
  const a = account(owner);
  if (a === null || !Number.isSafeInteger(fromId) || fromId < 0) return;
  try {
    write(`${READ_FROM_PREFIX}${a}:${groupIdB64}`, String(fromId));
  } catch {
    /* without the mark, the rows after the commit stay skipped */
  }
}

/** The pending read-from mark, if any. */
export function reentryReadFrom(groupIdB64: string, owner?: string): number | undefined {
  const a = account(owner);
  if (a === null) return undefined;
  const raw = read(`${READ_FROM_PREFIX}${a}:${groupIdB64}`);
  if (raw === null || !/^\d{1,15}$/.test(raw)) return undefined;
  return Number(raw);
}

/** The mark was used. */
export function clearReentryReadFrom(groupIdB64: string, owner?: string): void {
  const a = account(owner);
  if (a !== null) remove(`${READ_FROM_PREFIX}${a}:${groupIdB64}`);
}

// ---------- gap lines ------------------------------------------------------

/** The gap lines of a conversation, oldest first. */
export function gapLines(groupIdB64: string, owner?: string): GapLine[] {
  const a = account(owner);
  if (a === null) return [];
  const raw = read(`${GAP_PREFIX}${a}:${groupIdB64}`);
  if (raw === null) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (l): l is GapLine =>
        typeof l === "object" &&
        l !== null &&
        typeof (l as GapLine).fromMs === "number" &&
        typeof (l as GapLine).toMs === "number" &&
        (l as GapLine).fromMs <= (l as GapLine).toMs,
    );
  } catch {
    return [];
  }
}

function saveGapLines(groupIdB64: string, lines: readonly GapLine[], owner?: string): void {
  const a = account(owner);
  if (a === null) return;
  try {
    if (lines.length === 0) remove(`${GAP_PREFIX}${a}:${groupIdB64}`);
    else write(`${GAP_PREFIX}${a}:${groupIdB64}`, JSON.stringify(lines.slice(-16)));
  } catch {
    /* a lost line is a missing explanation, never a lost message */
  }
  setGapVersion((v) => v + 1);
}

/** Add a gap line (the device came back). */
export function addGapLine(groupIdB64: string, line: GapLine, owner?: string): void {
  if (!Number.isFinite(line.fromMs) || !Number.isFinite(line.toMs) || line.fromMs > line.toMs) return;
  saveGapLines(groupIdB64, [...gapLines(groupIdB64, owner), line], owner);
}

/**
 * Missed messages arrived for the gap that ends at `toMs` (UX 2.4: the line
 * covers only what none of the devices held). `complete`: the sender held
 * everything from the gap's start, so the line goes; otherwise it ends at
 * the earliest message received.
 */
export function narrowGapLine(
  groupIdB64: string,
  toMs: number,
  received: { readonly complete: boolean; readonly earliestMs: number | undefined },
  owner?: string,
): void {
  const lines = gapLines(groupIdB64, owner);
  const next: GapLine[] = [];
  for (const l of lines) {
    if (l.toMs !== toMs) {
      next.push(l);
      continue;
    }
    if (received.complete) continue;
    if (received.earliestMs !== undefined && received.earliestMs < l.toMs) {
      if (received.earliestMs <= l.fromMs) continue;
      next.push({ fromMs: l.fromMs, toMs: received.earliestMs });
    } else {
      next.push(l);
    }
  }
  saveGapLines(groupIdB64, next, owner);
}

// ---------- the group's knock key (route 3) -------------------------------

/** The knock key this member holds for `groupIdB64`. */
export function heldKnockKey(groupIdB64: string, owner?: string): HeldKnockKey | undefined {
  const a = account(owner);
  if (a === null) return undefined;
  const raw = read(`${KNOCK_KEY_PREFIX}${a}:${groupIdB64}`);
  if (raw === null) return undefined;
  try {
    const parsed = JSON.parse(raw) as Partial<HeldKnockKey>;
    if (typeof parsed.version !== "number" || typeof parsed.publicKeyB64 !== "string") return undefined;
    return { version: parsed.version, publicKeyB64: parsed.publicKeyB64 };
  } catch {
    return undefined;
  }
}

/** Keep a knock key (the caller has checked the sender and the version). */
export function storeKnockKey(groupIdB64: string, key: HeldKnockKey, owner?: string): void {
  const a = account(owner);
  if (a === null) return;
  try {
    write(`${KNOCK_KEY_PREFIX}${a}:${groupIdB64}`, JSON.stringify(key));
  } catch {
    /* re-sent on the next version or growth */
  }
}

/** What the owner last delivered to the group ({@link knockKeySentMarkOf}). */
export function knockKeySentMark(groupIdB64: string, owner?: string): string | undefined {
  const a = account(owner);
  if (a === null) return undefined;
  return read(`${KNOCK_KEY_SENT_PREFIX}${a}:${groupIdB64}`) ?? undefined;
}

/** The owner sent the knock key. */
export function setKnockKeySentMark(groupIdB64: string, mark: string, owner?: string): void {
  const a = account(owner);
  if (a === null) return;
  try {
    write(`${KNOCK_KEY_SENT_PREFIX}${a}:${groupIdB64}`, mark);
  } catch {
    /* sent again next time */
  }
}

/** Forget the owner's sent mark, so the key goes again at the next poll. */
export function clearKnockKeySentMark(groupIdB64: string, owner?: string): void {
  const a = account(owner);
  if (a !== null) remove(`${KNOCK_KEY_SENT_PREFIX}${a}:${groupIdB64}`);
}

/**
 * The mark of a delivered knock key: its version and the members it reached
 * (a short fingerprint of each leaf signature key, standard base64).
 */
export function knockKeySentMarkOf(version: number, memberKeysB64: readonly string[]): string {
  return `${version}|${[...new Set(memberKeysB64.map(knockKeyMemberFingerprint))].sort().join(",")}`;
}

/** The first 12 base64 characters (9 bytes) of a member key. */
function knockKeyMemberFingerprint(keyB64: string): string {
  return keyB64.slice(0, 12);
}

/**
 * Whether the owner sends the knock key now (section 3: "re-sent on a version
 * change and on growth"): no delivery yet, a new version, or a current member
 * the last delivery did not reach. Judged against the members of the last
 * DELIVERED send, not a growth one poll saw (live walk 2, 2026-11-19: a
 * growth seen while the send was skipped stayed unserved), so a remove and an
 * add in one commit also count. A shrink sends nothing.
 */
export function knockKeySendDue(sentMark: string | undefined, version: number, memberKeysB64: readonly string[]): boolean {
  if (sentMark === undefined) return true;
  const bar = sentMark.indexOf("|");
  if (bar < 0) return true;
  const sentVersion = Number(sentMark.slice(0, bar));
  if (!Number.isFinite(sentVersion) || sentVersion !== version) return true;
  const reached = new Set(sentMark.slice(bar + 1).split(",").filter((f) => f.length > 0));
  return memberKeysB64.some((k) => !reached.has(knockKeyMemberFingerprint(k)));
}

// ---------- the re-admitter's owed sends (section 7) -----------------------

/** One owed send, as stored. */
export interface StoredOwedGapSend {
  readonly groupIdB64: string;
  /** The returning leaf's signature key, standard base64. */
  readonly leafKeyB64: string;
  /** The epoch the replacement creates, decimal. */
  readonly epochAfter: string;
  readonly gapFromMs: number;
  readonly notedAtMs: number;
}

function isOwed(raw: unknown): raw is StoredOwedGapSend {
  if (typeof raw !== "object" || raw === null) return false;
  const r = raw as Partial<StoredOwedGapSend>;
  return (
    typeof r.groupIdB64 === "string" &&
    typeof r.leafKeyB64 === "string" &&
    typeof r.epochAfter === "string" &&
    /^\d{1,20}$/.test(r.epochAfter) &&
    typeof r.gapFromMs === "number" &&
    typeof r.notedAtMs === "number"
  );
}

/** Every owed send of the bound account. */
export function loadOwedGapSends(owner?: string): StoredOwedGapSend[] {
  const a = account(owner);
  if (a === null) return [];
  const raw = read(OWED_PREFIX + a);
  if (raw === null) return [];
  try {
    const parsed = JSON.parse(raw) as { v?: unknown; owed?: unknown };
    if (parsed.v !== 1 || !Array.isArray(parsed.owed)) return [];
    return parsed.owed.filter(isOwed);
  } catch {
    return [];
  }
}

/** Replace the owed sends. Returns `false` when they could not be written. */
export function saveOwedGapSends(list: readonly StoredOwedGapSend[], owner?: string): boolean {
  const a = account(owner);
  if (a === null) return false;
  try {
    if (list.length === 0) remove(OWED_PREFIX + a);
    else write(OWED_PREFIX + a, JSON.stringify({ v: 1, owed: list }));
    return true;
  } catch {
    return false;
  }
}

// ---------- the gap looks (section 7) --------------------------------------

/** One gap look, as stored: the secret sealed under the device key. */
export interface StoredGapLook {
  /** AES-GCM IV, standard base64. */
  readonly ivB64: string;
  /** The sealed 32-byte history secret, standard base64. */
  readonly sealedSecretB64: string;
  readonly gapFromMs: number;
  /** When the device came back: the gap line's end. */
  readonly gapToMs: number;
  /** When it last looked (0: never). */
  readonly lastLookMs: number;
  /** The route of the request the device came back by (live walk 3): a
   *  bundled row that names no author takes its direction from it. */
  readonly route?: ReentryRouteName;
}

function isGapLook(raw: unknown): raw is StoredGapLook {
  if (typeof raw !== "object" || raw === null) return false;
  const r = raw as Partial<StoredGapLook>;
  return (
    typeof r.ivB64 === "string" &&
    typeof r.sealedSecretB64 === "string" &&
    typeof r.gapFromMs === "number" &&
    typeof r.gapToMs === "number" &&
    typeof r.lastLookMs === "number" &&
    (r.route === undefined || r.route === "own-device" || r.route === "mailbox" || r.route === "knock")
  );
}

/** The gap look of `groupIdB64`, if any. */
export function gapLookFor(groupIdB64: string, owner?: string): StoredGapLook | undefined {
  const a = account(owner);
  if (a === null) return undefined;
  const raw = read(`${GAP_LOOK_PREFIX}${a}:${groupIdB64}`);
  if (raw === null) return undefined;
  try {
    const parsed = JSON.parse(raw) as unknown;
    return isGapLook(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

/** Keep a gap look. Returns `false` when it could not be written. */
export function saveGapLook(groupIdB64: string, look: StoredGapLook, owner?: string): boolean {
  const a = account(owner);
  if (a === null) return false;
  try {
    write(`${GAP_LOOK_PREFIX}${a}:${groupIdB64}`, JSON.stringify(look));
    return true;
  } catch {
    return false;
  }
}

/** The look ended (the messages arrived, or the retention passed). */
export function deleteGapLook(groupIdB64: string, owner?: string): void {
  const a = account(owner);
  if (a !== null) remove(`${GAP_LOOK_PREFIX}${a}:${groupIdB64}`);
}

/** The groups the bound account has a gap look for. */
export function gapLookGroups(owner?: string): string[] {
  const a = account(owner);
  if (a === null) return [];
  const prefix = `${GAP_LOOK_PREFIX}${a}:`;
  const out: string[] = [];
  try {
    const store = globalThis.localStorage;
    if (store === undefined) return [];
    for (let i = 0; i < store.length; i += 1) {
      const k = store.key(i);
      if (k !== null && k.startsWith(prefix)) out.push(k.slice(prefix.length));
    }
  } catch {
    return [];
  }
  return out;
}

// ---------- wipes ---------------------------------------------------------

/** The group left this device. */
export function clearReentryRowsForGroup(groupIdB64: string, owner?: string): void {
  const a = account(owner);
  if (a === null) return;
  deleteReentryRecord(groupIdB64, a);
  for (const p of [READ_FROM_PREFIX, GAP_PREFIX, KNOCK_KEY_PREFIX, KNOCK_KEY_SENT_PREFIX, GAP_LOOK_PREFIX]) {
    remove(`${p}${a}:${groupIdB64}`);
  }
  const owed = loadOwedGapSends(a);
  const kept = owed.filter((o) => o.groupIdB64 !== groupIdB64);
  if (kept.length !== owed.length) saveOwedGapSends(kept, a);
}

/** Forget device: every row of the account. */
export function clearReentryRowsForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i += 1) {
      const k = store.key(i);
      if (k === null) continue;
      if (ALL_PREFIXES.some((p) => k === `${p}${accountIdB64}` || k.startsWith(`${p}${accountIdB64}:`))) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* nothing to clear */
  }
}
