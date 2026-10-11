// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - the pure half of the reach keys this account holds.
 *
 * A reach key is an Ed25519 key derived from the account seed for
 * `(class, scope, version)`; the account mailbox accepts a write only under a
 * key the owner registered there. This module decides WHICH keys should be
 * registered and turns that into a `PUT /ds/mailbox/{hex}/reach` diff. It holds
 * no key material and does no I/O: the seeds and public keys are derived by
 * the wasm core in `reach-service.ts`, which feeds the public keys back in as
 * hex.
 *
 * The approach is declarative. {@link desiredReachKeys} computes the full set
 * of keys the setting, the groups and channels, the live links and the open
 * requests call for; {@link reachDiff} compares it with the set this device
 * last saw registered. A key that should no longer be registered - a group
 * switched off, a contact key rotated after a removal, a link revoked, a
 * request answered - falls out of the desired set and is removed by the same
 * diff, so narrowing never needs a separate code path.
 */

/** The reach classes, by the names the UI and the persisted book use. The
 *  wasm enum values are the frozen wire bytes (1..7). */
export type ReachClassName = "contact" | "link" | "reply" | "group" | "member" | "channel" | "fof";

/** The wire byte of each class (`lakko_contacts::reach::ReachClass`). */
export const REACH_CLASS_BYTE: Readonly<Record<ReachClassName, number>> = {
  contact: 1,
  link: 2,
  reply: 3,
  group: 4,
  member: 5,
  channel: 6,
  fof: 7,
};

/** The class named by a wire byte, or `null` for an unknown byte. */
export function reachClassName(byte: number): ReachClassName | null {
  for (const [name, value] of Object.entries(REACH_CLASS_BYTE)) {
    if (value === byte) return name as ReachClassName;
  }
  return null;
}

/** Classes whose key is account-wide (an empty scope). */
export function isAccountWideClass(cls: ReachClassName): boolean {
  return cls === "contact" || cls === "fof";
}

/** How long a `reply` key lives: 7 days (ADR-0060 open question 2, chosen). */
export const REPLY_TTL_MS = 7 * 24 * 60 * 60 * 1000;

/** How often the `fof` key rotates on a schedule: 7 days (chosen). */
export const FOF_ROTATION_MS = 7 * 24 * 60 * 60 * 1000;

/** One open request of the owner's (a contact request, a `/c/` cold start, a
 *  `/g/` join request): its random scope and when its key stops working. */
export interface ReplyRecord {
  readonly scopeHex: string;
  readonly expiresAtMs: number;
  /** What the request was, for the log line only. */
  readonly purpose: "contact-request" | "call-join" | "group-join";
  /** Who the request went to (account key hex, or a group / call id hex), so
   *  the answer from them closes exactly this key. */
  readonly peerHex?: string;
}

/** One live `/i/` or `/c/` link: its link id and expiry (`null` = never). */
export interface LinkRecord {
  readonly linkIdHex: string;
  readonly expiresAtMs: number | null;
}

/**
 * Everything this device keeps about its own reach keys. Persisted encrypted
 * per account (`reach-store.ts`).
 */
export interface ReachBook {
  /** `${class}:${scopeHex}` -> the current version. Absent = version 0. */
  readonly versions: Readonly<Record<string, number>>;
  readonly replies: readonly ReplyRecord[];
  readonly links: readonly LinkRecord[];
  /** Public key hex -> expiry (`null` = none) of every key this device last
   *  saw the DS accept. The diff base. */
  readonly registered: Readonly<Record<string, number | null>>;
  /** The Everyone flag last set on the mailbox, `null` = never set. */
  readonly open: boolean | null;
  /** When the `fof` key last rotated on its schedule. */
  readonly fofRotatedAtMs: number;
  /** The stamp of the version counters (the ReachState envelope's
   *  `updatedAtMs`, ADR-0060 section 7): moved forward by every local change
   *  of a version, adopted from a newer state another device of the account
   *  sent. `0` = never changed. */
  readonly stateUpdatedAtMs: number;
  /**
   * ADR-0060 section 1, "Versions after a restore". `null` on a device that
   * started with version state. Set on a device whose first sync found the
   * book EMPTY (a fresh install or a restore): from then on, every versioned
   * `(class, scope)` this device is about to register a key for without a
   * known version is first checked against the keys the DS holds for the
   * mailbox, and continues from the highest version registered there.
   * `checked` lists the `${class}:${scopeHex}` pairs already established that
   * way. A group the device joins later (a re-admission after a restore) is
   * checked when it appears, not only the scopes known at the first sync.
   */
  readonly recovery: { readonly checked: Readonly<Record<string, true>> } | null;
  /**
   * ADR-0066 section 3.5, "Reach": the account key (hex) whose mailbox
   * `registered` and `open` describe. Absent on a book written before it
   * was recorded.
   */
  readonly registeredForKeyHex?: string;
}

/** What binding a book to an account key does with its registrations. */
export type ReachRebind =
  /** They were made under the bound key: nothing changes. */
  | "keep"
  /** Nothing says which key they were made under: they are the bound key's,
   *  and the book now says so. */
  | "stamp"
  /** They were made under ANOTHER key of this account, hence on another
   *  mailbox: forgotten here, and everything is registered again under the
   *  bound key. The old mailbox is left as it is. */
  | "move";

/** The decision of {@link bindReachBook}. Pure. */
export function reachRebind(boundKeyHex: string, registeredForKeyHex: string | undefined): ReachRebind {
  if (registeredForKeyHex === boundKeyHex) return "keep";
  return registeredForKeyHex === undefined ? "stamp" : "move";
}

/**
 * The book as the session of `boundKeyHex` holds it (ADR-0066 section 3.5,
 * "Reach"; the rule iOS's `ReachCenter.rekey` and Android's `rekeyDecision`
 * apply). A reach key and the mailbox it is registered on both derive from
 * the account seed, so after a rotation the recorded registrations describe
 * the OLD mailbox: they are dropped (with the `open` flag), and the next
 * sync registers every wanted key on the new one. The version counters, the
 * links and the open requests are kept. Returns the same object when nothing
 * changes.
 */
export function bindReachBook(book: ReachBook, boundKeyHex: string): ReachBook {
  switch (reachRebind(boundKeyHex, book.registeredForKeyHex)) {
    case "keep":
      return book;
    case "stamp":
      return { ...book, registeredForKeyHex: boundKeyHex };
    case "move":
      return { ...book, registered: {}, open: null, registeredForKeyHex: boundKeyHex };
  }
}

export const EMPTY_REACH_BOOK: ReachBook = {
  versions: {},
  replies: [],
  links: [],
  registered: {},
  open: null,
  fofRotatedAtMs: 0,
  stateUpdatedAtMs: 0,
  recovery: null,
};

function versionKey(cls: ReachClassName, scopeHex: string): string {
  return `${cls}:${scopeHex}`;
}

/** The current version of `(cls, scope)`. */
export function versionOf(book: ReachBook, cls: ReachClassName, scopeHex: string): number {
  return book.versions[versionKey(cls, scopeHex)] ?? 0;
}

/** Bump `(cls, scope)` to the next version: the old key stops being wanted,
 *  so the next diff removes it and registers the new one. */
export function bumpVersion(book: ReachBook, cls: ReachClassName, scopeHex: string): ReachBook {
  const key = versionKey(cls, scopeHex);
  const next = (book.versions[key] ?? 0) + 1;
  return { ...book, versions: { ...book.versions, [key]: next } };
}

/** Set `(cls, scope)` to an explicit version (a channel key follows the
 *  channel's epoch). Never moves a version backwards. */
export function raiseVersion(
  book: ReachBook,
  cls: ReachClassName,
  scopeHex: string,
  version: number,
): ReachBook {
  const key = versionKey(cls, scopeHex);
  if ((book.versions[key] ?? 0) >= version) return book;
  return { ...book, versions: { ...book.versions, [key]: version } };
}

/** Record a new open request. */
export function addReply(book: ReachBook, reply: ReplyRecord): ReachBook {
  if (book.replies.some((r) => r.scopeHex === reply.scopeHex)) return book;
  return { ...book, replies: [...book.replies, reply] };
}

/** Forget a request: answered, joined, or given up. Its key leaves the
 *  desired set. */
export function dropReply(book: ReachBook, scopeHex: string): ReachBook {
  return { ...book, replies: book.replies.filter((r) => r.scopeHex !== scopeHex) };
}

/** Record a live link. */
export function addLink(book: ReachBook, link: LinkRecord): ReachBook {
  const rest = book.links.filter((l) => l.linkIdHex !== link.linkIdHex);
  return { ...book, links: [...rest, link] };
}

/** Forget a link (revoked). */
export function dropLink(book: ReachBook, linkIdHex: string): ReachBook {
  return { ...book, links: book.links.filter((l) => l.linkIdHex !== linkIdHex) };
}

/** Forget every request that went to `peerHex`: they answered. */
export function dropRepliesTo(book: ReachBook, peerHex: string): ReachBook {
  const replies = book.replies.filter((r) => r.peerHex !== peerHex);
  return replies.length === book.replies.length ? book : { ...book, replies };
}

/** Drop requests and links whose keys have expired: the DS no longer honours
 *  them, and they must not be re-registered. */
export function pruneExpired(book: ReachBook, nowMs: number): ReachBook {
  const replies = book.replies.filter((r) => r.expiresAtMs > nowMs);
  const links = book.links.filter((l) => l.expiresAtMs === null || l.expiresAtMs > nowMs);
  if (replies.length === book.replies.length && links.length === book.links.length) return book;
  return { ...book, replies, links };
}

/** One key the owner should have registered. */
export interface WantedReachKey {
  readonly cls: ReachClassName;
  readonly scopeHex: string;
  readonly version: number;
  readonly expiresAtMs: number | null;
}

/** What the setting and the account's current scopes allow. */
export interface ReachInputs {
  /** Hex ids of every conversation group this account is in (1:1 included). */
  readonly groupIdsHex: readonly string[];
  /** Hex ids and current epochs of every channel this account is in. */
  readonly channels: readonly { readonly idHex: string; readonly epoch: number }[];
  /** Whether the setting lets the members of this group find the owner. */
  readonly allowsGroup: (groupIdHex: string) => boolean;
  /** Whether the setting lets the members of this channel find the owner. */
  readonly allowsChannel: (channelIdHex: string) => boolean;
  /** Whether contacts of contacts may find the owner (2.1 on). */
  readonly allowsContactsOfContacts: boolean;
  /** Everyone. */
  readonly everyone: boolean;
  readonly nowMs: number;
}

/**
 * The full set of keys that should be registered on the account mailbox:
 *
 * - `contact`, always (contacts can always write);
 * - `member` per group, always (ADR-0060 section 1: re-admission Welcomes);
 * - `group` per group the setting allows;
 * - `channel` per channel the setting allows, at the channel's epoch;
 * - `fof` while contacts of contacts are allowed;
 * - `link` per live link, `reply` per open request, each with its expiry.
 */
export function desiredReachKeys(book: ReachBook, inputs: ReachInputs): WantedReachKey[] {
  const out: WantedReachKey[] = [];
  out.push({ cls: "contact", scopeHex: "", version: versionOf(book, "contact", ""), expiresAtMs: null });
  for (const g of inputs.groupIdsHex) {
    out.push({ cls: "member", scopeHex: g, version: versionOf(book, "member", g), expiresAtMs: null });
    if (inputs.allowsGroup(g)) {
      out.push({ cls: "group", scopeHex: g, version: versionOf(book, "group", g), expiresAtMs: null });
    }
  }
  for (const c of inputs.channels) {
    if (inputs.allowsChannel(c.idHex)) {
      const version = Math.max(c.epoch, versionOf(book, "channel", c.idHex));
      out.push({ cls: "channel", scopeHex: c.idHex, version, expiresAtMs: null });
    }
  }
  if (inputs.allowsContactsOfContacts) {
    out.push({ cls: "fof", scopeHex: "", version: versionOf(book, "fof", ""), expiresAtMs: null });
  }
  for (const l of book.links) {
    if (l.expiresAtMs !== null && l.expiresAtMs <= inputs.nowMs) continue;
    out.push({ cls: "link", scopeHex: l.linkIdHex, version: versionOf(book, "link", l.linkIdHex), expiresAtMs: l.expiresAtMs });
  }
  for (const r of book.replies) {
    if (r.expiresAtMs <= inputs.nowMs) continue;
    out.push({ cls: "reply", scopeHex: r.scopeHex, version: 0, expiresAtMs: r.expiresAtMs });
  }
  return out;
}

/** A key with its public half, as the diff consumes it. */
export interface ResolvedReachKey {
  readonly publicKeyHex: string;
  readonly expiresAtMs: number | null;
}

/** The change that brings the registered set to the desired one. */
export interface ReachDiff {
  readonly add: readonly ResolvedReachKey[];
  readonly remove: readonly string[];
  /** `undefined` when the flag already matches. */
  readonly open: boolean | undefined;
}

/**
 * Compare the desired keys with what the DS last accepted. A key whose expiry
 * changed is re-added (the DS replaces the expiry). `open` is sent only when
 * it differs from the last value set.
 */
export function reachDiff(
  desired: readonly ResolvedReachKey[],
  registered: Readonly<Record<string, number | null>>,
  everyone: boolean,
  lastOpen: boolean | null,
): ReachDiff {
  const want = new Map<string, number | null>();
  for (const d of desired) want.set(d.publicKeyHex, d.expiresAtMs);
  const add: ResolvedReachKey[] = [];
  for (const [pk, exp] of want) {
    if (!(pk in registered) || registered[pk] !== exp) add.push({ publicKeyHex: pk, expiresAtMs: exp });
  }
  const remove = Object.keys(registered).filter((pk) => !want.has(pk));
  return { add, remove, open: lastOpen === everyone ? undefined : everyone };
}

/** `true` when the diff changes nothing. */
export function isEmptyDiff(diff: ReachDiff): boolean {
  return diff.add.length === 0 && diff.remove.length === 0 && diff.open === undefined;
}

/** The book after the DS accepted `diff`. */
export function applyAcceptedDiff(book: ReachBook, diff: ReachDiff): ReachBook {
  const registered: Record<string, number | null> = { ...book.registered };
  for (const pk of diff.remove) delete registered[pk];
  for (const a of diff.add) registered[a.publicKeyHex] = a.expiresAtMs;
  return { ...book, registered, open: diff.open === undefined ? book.open : diff.open };
}

/** Whether the `fof` key is due for its scheduled rotation. */
export function fofRotationDue(book: ReachBook, nowMs: number): boolean {
  return nowMs - book.fofRotatedAtMs >= FOF_ROTATION_MS;
}

/** The book with the `fof` key rotated now. */
export function rotateFof(book: ReachBook, nowMs: number): ReachBook {
  return { ...bumpVersion(book, "fof", ""), fofRotatedAtMs: nowMs };
}

// ----------------------------------------------------------------------------
// The version counters shared by the account's devices (ADR-0060 section 7)
// ----------------------------------------------------------------------------

/** One version counter as the ReachState envelope carries it. */
export interface ReachStateEntryModel {
  readonly cls: ReachClassName;
  /** Hex scope id; `""` for the account-wide classes. */
  readonly scopeHex: string;
  readonly version: number;
}

/** Longest scope id a reach key is derived over, as hex (64 bytes). */
const MAX_SCOPE_HEX_LEN = 128;

function shapeFits(cls: ReachClassName, scopeHex: string): boolean {
  if (cls === "reply") return false;
  if (isAccountWideClass(cls)) return scopeHex === "";
  return scopeHex.length > 0 && scopeHex.length <= MAX_SCOPE_HEX_LEN && scopeHex.length % 2 === 0;
}

/** Every version counter the book holds: the bumped ones, and any a merged
 *  state set (an explicit 0 included, so the book composes back to exactly
 *  the state it adopted; an absent counter is version 0 on every device).
 *  `reply` keys have no version. In the core's canonical order, so the
 *  envelope is the same bytes for the same book. */
export function reachStateEntries(book: ReachBook): ReachStateEntryModel[] {
  const out: ReachStateEntryModel[] = [];
  for (const [key, version] of Object.entries(book.versions)) {
    const at = key.indexOf(":");
    if (at < 0 || !Number.isInteger(version) || version < 0) continue;
    const cls = key.slice(0, at) as ReachClassName;
    const scopeHex = key.slice(at + 1);
    if (!(cls in REACH_CLASS_BYTE) || !shapeFits(cls, scopeHex)) continue;
    out.push({ cls, scopeHex, version });
  }
  // The core's canonical order: class byte, then scope bytes (lowercase hex
  // compares like the bytes it spells).
  out.sort((a, b) =>
    a.cls === b.cls
      ? a.scopeHex < b.scopeHex
        ? -1
        : a.scopeHex > b.scopeHex
          ? 1
          : 0
      : REACH_CLASS_BYTE[a.cls] - REACH_CLASS_BYTE[b.cls],
  );
  return out;
}

/**
 * The book holding a reach state exactly as the core's `mergeReachState`
 * returned it: every carried counter set to the state's value and the
 * state's stamp adopted as is. The merge rule itself (maximum per counter,
 * who re-stamps, who re-sends) is the core's, never re-derived here; a
 * counter the state does not carry stays as it was.
 */
export function adoptReachState(
  book: ReachBook,
  entries: readonly ReachStateEntryModel[],
  updatedAtMs: number,
): ReachBook {
  const versions: Record<string, number> = { ...book.versions };
  for (const e of entries) {
    if (!shapeFits(e.cls, e.scopeHex)) continue;
    versions[versionKey(e.cls, e.scopeHex)] = e.version;
  }
  return { ...book, versions, stateUpdatedAtMs: updatedAtMs };
}

// ----------------------------------------------------------------------------
// Versions after a restore (ADR-0060 section 1, amended 2026-10-01)
// ----------------------------------------------------------------------------

/**
 * How many versions of an account-wide key (`contact`, `fof`) a restored
 * device derives when it looks for the one registered on the DS: 0..=1024.
 * `fof` rotates on a 7-day schedule ({@link FOF_ROTATION_MS}), so 1024
 * versions cover about 19 years of weekly rotations; `contact` moves only on
 * a contact removal. One derivation cost about 0.4 ms in the debug wasm build
 * under node (measured once, 5000 derivations, this repo on 2026-10-01), so
 * the two account-wide scans together cost under a second, once per restore.
 * A version above the bound is not found and the class starts at 0, which
 * could re-register a removed version: the bound is the stated limit.
 */
export const REACH_RECOVERY_ACCOUNT_WIDE_VERSIONS = 1024;

/**
 * How many versions of a per-group key (`group`, `member`) are derived, and
 * how far past a channel's known epoch a `channel` key is looked for: 256.
 * A group's keys move only when that group shrinks (one bump per removal), so
 * 256 removals from one group; the cost is two scans per group, about 0.2 s
 * each in the same measurement, paid once per group on a restored device.
 */
export const REACH_RECOVERY_SCOPED_VERSIONS = 256;

/** `true` when this device holds no version state at all: nothing bumped,
 *  nothing adopted, nothing ever registered. */
export function bookHoldsNoVersionState(book: ReachBook): boolean {
  return (
    Object.keys(book.versions).length === 0 &&
    Object.keys(book.registered).length === 0 &&
    book.stateUpdatedAtMs === 0
  );
}

/** One `(class, scope)` to look up, with the versions to derive. */
export interface VersionCheck {
  readonly cls: ReachClassName;
  readonly scopeHex: string;
  /** First version derived (inclusive). */
  readonly from: number;
  /** Last version derived (inclusive). */
  readonly to: number;
}

/**
 * The `(class, scope)` pairs a recovering device must check before it
 * registers anything: every versioned class it could register for its scopes
 * (contact and fof with the empty scope, group and member per group, channel
 * per channel), minus the ones whose version is already established (in
 * `versions`, or checked). Empty when the book is not recovering.
 */
export function versionChecksDue(
  book: ReachBook,
  scopes: { readonly groupIdsHex: readonly string[]; readonly channels: readonly { readonly idHex: string; readonly epoch: number }[] },
): VersionCheck[] {
  const recovery = book.recovery;
  if (recovery === null) return [];
  const out: VersionCheck[] = [];
  const due = (cls: ReachClassName, scopeHex: string, from: number, to: number): void => {
    const key = versionKey(cls, scopeHex);
    if (key in book.versions || recovery.checked[key] === true) return;
    out.push({ cls, scopeHex, from, to });
  };
  due("contact", "", 0, REACH_RECOVERY_ACCOUNT_WIDE_VERSIONS);
  due("fof", "", 0, REACH_RECOVERY_ACCOUNT_WIDE_VERSIONS);
  for (const g of scopes.groupIdsHex) {
    due("group", g, 0, REACH_RECOVERY_SCOPED_VERSIONS);
    due("member", g, 0, REACH_RECOVERY_SCOPED_VERSIONS);
  }
  for (const c of scopes.channels) {
    // A channel key's version is the channel epoch it was registered at, and
    // the wanted version is never below the known epoch, so a version below
    // it can never be re-registered; only the window above it is looked at.
    const from = Math.max(0, Math.trunc(c.epoch));
    due("channel", c.idHex, from, from + REACH_RECOVERY_SCOPED_VERSIONS);
  }
  return out;
}

/**
 * Apply what the DS holds to the due checks: for each, the HIGHEST version
 * whose public key is registered becomes the current version, and every
 * version found registered is recorded as registered (with the DS's expiry),
 * so the next diff keeps the current key and removes any lower one still
 * there. A pair with no registered key starts at 0. Every check is marked
 * checked. Nothing lower than a registered version is ever wanted again.
 */
export function applyVersionChecks(
  book: ReachBook,
  checks: readonly VersionCheck[],
  dsKeys: ReadonlyMap<string, number | null>,
  publicKeyHex: (cls: ReachClassName, scopeHex: string, version: number) => string,
): { readonly book: ReachBook; readonly raised: boolean } {
  if (book.recovery === null || checks.length === 0) return { book, raised: false };
  const versions: Record<string, number> = { ...book.versions };
  const registered: Record<string, number | null> = { ...book.registered };
  const checked: Record<string, true> = { ...book.recovery.checked };
  let raised = false;
  for (const c of checks) {
    let highest: number | null = null;
    for (let v = c.from; v <= c.to; v++) {
      const pk = publicKeyHex(c.cls, c.scopeHex, v);
      if (!dsKeys.has(pk)) continue;
      registered[pk] = dsKeys.get(pk) ?? null;
      highest = v;
    }
    const key = versionKey(c.cls, c.scopeHex);
    if (highest !== null && highest > 0) {
      versions[key] = highest;
      raised = true;
    }
    checked[key] = true;
  }
  return { book: { ...book, versions, registered, recovery: { checked } }, raised };
}

function decodeRecovery(raw: unknown): ReachBook["recovery"] {
  if (raw === null || typeof raw !== "object") return null;
  const checkedRaw = (raw as { checked?: unknown }).checked;
  const checked: Record<string, true> = {};
  if (checkedRaw !== null && typeof checkedRaw === "object") {
    for (const [k, v] of Object.entries(checkedRaw as Record<string, unknown>)) {
      if (v === true) checked[k] = true;
    }
  }
  return { checked };
}

// ----------------------------------------------------------------------------
// Persistence form
// ----------------------------------------------------------------------------

/** JSON form of the book. */
export function encodeReachBook(book: ReachBook): string {
  return JSON.stringify({ v: 1, ...book });
}

/** Parse the persisted book; an unreadable row is an empty book (the next
 *  sync re-registers every wanted key, which the DS treats idempotently). */
export function decodeReachBook(json: string | null | undefined): ReachBook {
  if (json === null || json === undefined) return EMPTY_REACH_BOOK;
  try {
    const raw = JSON.parse(json) as Partial<ReachBook> & { v?: number };
    if (raw.v !== 1) return EMPTY_REACH_BOOK;
    return {
      versions: typeof raw.versions === "object" && raw.versions !== null ? raw.versions : {},
      replies: Array.isArray(raw.replies) ? raw.replies : [],
      links: Array.isArray(raw.links) ? raw.links : [],
      registered: typeof raw.registered === "object" && raw.registered !== null ? raw.registered : {},
      open: typeof raw.open === "boolean" ? raw.open : null,
      fofRotatedAtMs: typeof raw.fofRotatedAtMs === "number" ? raw.fofRotatedAtMs : 0,
      stateUpdatedAtMs: typeof raw.stateUpdatedAtMs === "number" ? raw.stateUpdatedAtMs : 0,
      recovery: decodeRecovery(raw.recovery),
      ...(typeof raw.registeredForKeyHex === "string"
        ? { registeredForKeyHex: raw.registeredForKeyHex }
        : {}),
    };
  } catch {
    return EMPTY_REACH_BOOK;
  }
}
