// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 (reworked 2026-10-17) - only one of the inviter's devices answers
 * a request sent through one of the account's invite links: CLAIM, THEN
 * BUILD, on the account's own DeviceGroup journal, the pattern ADR-0073 R.6
 * uses for successor groups (`chat/successor-claim.ts`).
 *
 *   1. a device that decides `accept` seals a claim naming the request,
 *      records the sealed bytes, then publishes them into its DeviceGroup;
 *      it builds nothing yet;
 *   2. it reads its DeviceGroup journal past the claim and asks the core
 *      (`linkClaimOutcome`): the EARLIEST claim for the request from a
 *      device in the current roster wins, and the device whose leaf posted
 *      it builds;
 *   3. only the winner founds the one-to-one, sends the Welcome, records the
 *      contact and counts the use;
 *   4. a device that lost, or holds no claim and sees one from a device in
 *      the roster, keeps the request unshown until the link register says
 *      it was answered, checking again against the current roster at every
 *      pass: if the claim that beat it came from a device since removed,
 *      the next earliest current claim wins (possibly this device's), so a
 *      removed device cannot leave a request unanswered.
 *
 * A device recognises its OWN claim rows by their bytes: it cannot decrypt
 * its own MLS application messages, so it records each sealed claim before
 * the publish and the DeviceGroup receive loop matches every row against
 * those bytes. A crash between the publish and the recording of its id
 * therefore loses nothing: the row is still recognised, its earlier id still
 * wins, and the later duplicate changes nothing.
 *
 * This module holds the pure next-step rule and the per-account record,
 * kept in `localStorage`. iOS (`LinkClaims.swift`) and Android
 * (`LinkClaims.kt`) mirror both.
 *
 * Nothing here is secret: a request key is a link id and a public account
 * key, a seen claim is a message id and a public leaf key, and a sealed
 * claim is ciphertext already handed to the DS.
 */

/** A claim another device of the account posted: its DS message id and the
 *  leaf key (hex) the receiver resolved from the MLS sender at receipt. */
export interface ObservedClaim {
  readonly id: number;
  readonly author: string;
}

/** One account a founded one-to-one's Welcome goes to. */
export interface LinkFoundedTarget {
  readonly accountKeyB64: string;
  readonly isRecipient: boolean;
}

/**
 * The one-to-one this device founded for a request it won, recorded BEFORE
 * any Welcome is posted, so a retry (a failed publish, a restart) delivers
 * the SAME group's Welcome instead of founding a second one.
 */
export interface LinkFounded {
  readonly groupIdB64: string;
  /** The Welcome (base64); emptied once every target was written. */
  readonly welcomeB64: string;
  /** In delivery order: this account's own mailbox first. */
  readonly targets: readonly LinkFoundedTarget[];
  /** The link-founded note was published into the account's DeviceGroup;
   *  no Welcome is written before it. */
  readonly notePosted: boolean;
  readonly delivered: boolean;
}

/** A link-founded note another device of the account posted (ADR-0079):
 *  the one-to-one it founded for a request, and that group's Welcome. */
export interface HeldLinkNote {
  /** The DS message id of the note's row: the earliest per request is kept. */
  readonly id: number;
  readonly groupIdB64: string;
  readonly welcomeB64: string;
}

/** This device's own claims for one request. */
export interface OwnLinkClaims {
  /** Sealed claim messages (hex), recorded BEFORE their publish and not yet
   *  matched to a journal row. */
  readonly wires: readonly string[];
  /** DS message ids of this device's claims on the journal. */
  readonly ids: readonly number[];
  /** Decided: this device answers the request, at this claim. Kept until
   *  the build has landed, so a failed build is retried at the next pass. */
  readonly won: number | null;
  /** Decided lost: every claim of this device is on the journal and known,
   *  so the next passes re-check against the current roster without
   *  reading the journal again. */
  readonly settled: boolean;
  /** The one-to-one founded for the request (won claims only). */
  readonly founded: LinkFounded | null;
  /** The sealed link-founded note (hex), recorded BEFORE its publish and not
   *  yet judged at its journal row. */
  readonly noteWire: string | null;
  /** The note's row counted: sealed at the journal's epoch at its position,
   *  so every device of the account can read it. */
  readonly noteConfirmed: boolean;
}

/** The journal's epochs as this device read them (ADR-0079): persisted
 *  form of the core's `JournalEpochs`, for one DeviceGroup. Epochs and ids
 *  as decimal strings (they are u64). */
export interface StoredJournalEpochs {
  readonly groupIdHex: string;
  readonly base: string;
  readonly commitRowIds: readonly string[];
  readonly commitEpochsAfter: readonly string[];
}

/**
 * The order the Welcome of a founded one-to-one is written in: this
 * account's own mailbox (its other devices) FIRST, the requester's after,
 * and only once the own write succeeded. So whenever the requester holds
 * the Welcome, every own device's mailbox holds it too, and a device that
 * takes the request over finds the one-to-one before it could found a
 * second. Stable within each half. Pure.
 */
export function welcomeOrder<T extends { readonly isRecipient: boolean }>(targets: readonly T[]): T[] {
  return [...targets.filter((t) => !t.isRecipient), ...targets.filter((t) => t.isRecipient)];
}

/** What a device that won a request does before it builds anything. */
export type LinkBuildStep =
  /** A one-to-one was founded for it here already: carry that one on. */
  | "resume"
  /** Another device of the account founded it (its note is held): never
   *  found; write the note's Welcome to the requester. */
  | "forward"
  /** Found the one-to-one. */
  | "found";

/**
 * Before any device builds a one-to-one for a request (the first winner, or
 * a device that took it over because the winner left the roster): its own
 * founding first, else a note from another device, else found. Pure.
 */
export function linkBuildStep(input: { readonly hasFounded: boolean; readonly hasNote: boolean }): LinkBuildStep {
  if (input.hasFounded) return "resume";
  return input.hasNote ? "forward" : "found";
}

/** What the build needs from the device, injected so the order is one
 *  tested function (`runLinkBuild`). */
export interface LinkBuildDeps {
  /** This device's founding for the request, if any. */
  founded(): LinkFounded | null;
  /** The earliest note another device posted for the request, if any. */
  note(): HeldLinkNote | null;
  /** Found the one-to-one locally (ONE commit with every other own device,
   *  the journal claimed, the state persisted); nothing is sent. */
  prepare(): Promise<LinkFounded>;
  /** Record the founding durably; throws when the write fails. */
  record(founded: LinkFounded): void;
  /** Publish the link-founded note into the account's DeviceGroup (once:
   *  a sealed note already recorded is not published again), read the
   *  journal past it, and answer whether its row COUNTS (sealed at the
   *  journal's epoch at its position, so every device can read it). `false`
   *  when it was straddled by a commit (the next pass posts it again) or
   *  its row was not read yet; throws when the publish fails. */
  postNote(founded: LinkFounded): Promise<boolean>;
  /** Make the founded group a held conversation; its id, or `null`. */
  hold(founded: LinkFounded): Promise<string | null>;
  /** Write the Welcome: this account's mailbox first, then the requester's. */
  deliver(founded: LinkFounded): Promise<void>;
  /** Write a note's Welcome to the requester's mailbox (a duplicate is
   *  ignored); the id of the group when this device holds it, else `null`. */
  forward(note: HeldLinkNote): Promise<string | null>;
  /** The contact row, the ack and the use count. */
  finish(groupIdB64: string | null): Promise<void>;
}

/**
 * The build of a won request, in its one order (ADR-0079, 2026-10-17):
 *
 *   - a founding of this device's own is carried on (a failed publish, a
 *     restart): nothing is founded twice;
 *   - else a note from another device means it founded already: its Welcome
 *     is written to the requester and the request answered; never founded;
 *   - else found, record the founding, publish the note, and only after the
 *     note landed counting (its row sealed at the journal's epoch there, so
 *     every device can read it) write the Welcome (own mailbox first).
 *
 * `false` when the group could not be held (the next pass tries again); a
 * throw from any step leaves every record where it was, for the next pass.
 */
export async function runLinkBuild(d: LinkBuildDeps): Promise<boolean> {
  let founded = d.founded();
  const step = linkBuildStep({ hasFounded: founded !== null, hasNote: founded === null && d.note() !== null });
  if (step === "forward") {
    const note = d.note();
    if (note === null) return false;
    await d.finish(await d.forward(note));
    return true;
  }
  if (founded === null) {
    founded = await d.prepare();
    d.record(founded);
  }
  if (!founded.notePosted) {
    // No Welcome on the strength of a note the other devices cannot read.
    if (!(await d.postNote(founded))) return false;
    founded = { ...founded, notePosted: true };
    d.record(founded);
  }
  const held = await d.hold(founded);
  if (held === null) return false;
  if (!founded.delivered) {
    await d.deliver(founded);
    founded = { ...founded, welcomeB64: "", delivered: true };
    d.record(founded);
  }
  await d.finish(held);
  return true;
}

/** What the device does with a request carrying a link answer, now. */
export type LinkRequestStep =
  /** The synced state is not loaded yet, or this device holds no record of
   *  the link: keep it undecided, unshown. */
  | "wait"
  /** Answered (by another device, or the link register says so): remove. */
  | "drop"
  /** The link does not count for the request (ADR-0079 amendment of
   *  2026-10-19): drop it silently and answer the requester with a link
   *  refusal. No card. */
  | "refuse"
  /** Live link: post this device's claim. */
  | "post-claim"
  /** Claim posted: read the journal past it and decide. */
  | "decide"
  /** Lost: decide again against the current roster (no journal read). */
  | "recheck"
  /** Claim won: found the one-to-one (again, after a failure). */
  | "build"
  /** The requester already has a live one-to-one with this account: put them
   *  back on the contact list with that conversation, found nothing, send
   *  nothing (a person removed and invited again; amended 2026-10-24). */
  | "restore";

/** The verdict the fold gives a request's link answer (`null` while the fold
 *  is not loaded). */
export type LinkVerdict = "accept" | "answered" | "dead" | "unknown" | null;

function holdsClaim(own: OwnLinkClaims | null): own is OwnLinkClaims {
  return own !== null && (own.won !== null || own.ids.length > 0 || own.wires.length > 0);
}

/**
 * The next step for one request. Pure.
 *
 * | own claims        | answered elsewhere | verdict          | step         |
 * |-------------------|--------------------|------------------|--------------|
 * | won, not founded, no note held | any   | dead             | `refuse`     |
 * | won               | any                | any other        | `build`      |
 * | any other         | any                | answered         | `drop`       |
 * | lost (settled)    | any                | dead             | `drop`       |
 * | lost (settled)    | any                | other            | `recheck`    |
 * | posted or posting | any                | any              | `decide`     |
 * | none              | yes                | dead             | `drop`       |
 * | none              | yes                | other            | `wait`       |
 * | none              | no                 | not held/unknown | `wait`       |
 * | none              | no                 | dead             | `refuse`     |
 * | none              | no                 | accept           | `post-claim` |
 *
 * Before the rows after the first: a requester this device already holds a
 * live one-to-one with is answered by `restore` whenever the link counts
 * (`accept`) or was already answered for them (`answered`). Every device of
 * the account restores on its own; nothing is founded, so no claim decides
 * between them. A claim this device already won keeps building.
 *
 * "Answered elsewhere" is the core's `linkClaimAnsweredElsewhere`: a claim
 * from a device in the current roster. The verdict judges the link's expiry
 * at the moment the server stamped the request and its revocation now, so a
 * revocation wins until this account has answered (ADR-0079 amendment of
 * 2026-10-19): a winner that has not founded yet, and holds no other
 * device's founding note, refuses instead of building. Once it founded, the
 * answer is given and the build carries on. A device that lost, or sees
 * another device's claim, drops a dead request without a refusal: the
 * claimer answers it. A link this device holds no record of waits: its
 * record may not have reached this device yet, and a refusal cannot be
 * taken back.
 */
export function linkRequestStep(input: {
  readonly own: OwnLinkClaims | null;
  readonly answeredElsewhere: boolean;
  readonly verdict: LinkVerdict;
  /** A link-founded note from another device is held for the request. */
  readonly noteHeld?: boolean;
  /** This device holds a live one-to-one with the requester. */
  readonly liveOneToOne?: boolean;
}): LinkRequestStep {
  if (holdsClaim(input.own) && input.own.won !== null) {
    const answered = input.own.founded !== null || input.noteHeld === true;
    return input.verdict === "dead" && !answered ? "refuse" : "build";
  }
  if (input.liveOneToOne === true && (input.verdict === "accept" || input.verdict === "answered")) {
    return "restore";
  }
  if (input.verdict === "answered") return "drop";
  if (holdsClaim(input.own)) {
    if (!input.own.settled) return "decide";
    return input.verdict === "dead" ? "drop" : "recheck";
  }
  if (input.answeredElsewhere) return input.verdict === "dead" ? "drop" : "wait";
  if (input.verdict === null || input.verdict === "unknown") return "wait";
  if (input.verdict === "dead") return "refuse";
  return "post-claim";
}

/**
 * The claims the core decides over: this device's own (authored by its own
 * leaf key) and the others seen. Pure.
 */
export function claimsToDecide(
  own: OwnLinkClaims | null,
  seen: readonly ObservedClaim[],
  ownKeyHex: string,
): ObservedClaim[] {
  const mine = (own?.ids ?? []).map((id) => ({ id, author: ownKeyHex }));
  return [...mine, ...seen];
}

// ---------- The per-account record ------------------------------

const KEY_PREFIX = "lakko.link-claims.v1.";

/** How many requests' seen claims are kept; past it the oldest go. Chosen,
 *  not measured: a claim matters only while some device still holds the
 *  request, and the register answers a request once its use is counted. */
export const MAX_SEEN_LINK_CLAIMS = 512;

/** How many claims per request are kept (the lowest ids). Chosen, not
 *  measured: one per device, and an account has at most 8 conversation
 *  leaves (ADR-0050). */
export const MAX_CLAIMS_PER_REQUEST = 16;

interface SeenEntry {
  readonly claims: readonly ObservedClaim[];
  readonly at: number;
}

interface Persisted {
  readonly v: 2;
  readonly seen: Record<string, SeenEntry>;
  readonly own: Record<string, OwnLinkClaims>;
  /** The earliest link-founded note per request, by request key. */
  readonly notes: Record<string, HeldLinkNote & { readonly at: number }>;
  /** The journal's epochs (ADR-0079 readability rule). */
  readonly journal: StoredJournalEpochs | null;
}

/** How many link-founded notes are kept; past it the oldest go. Chosen, not
 *  measured: a note matters only while its request can still be taken over,
 *  and each holds a Welcome of a few KiB. */
export const MAX_LINK_NOTES = 64;

type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function defaultStorage(): StorageLike | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

/** Lower-case hex of `link_id ‖ requester_account_key`. */
export function linkRequestKeyHex(linkId: Uint8Array, requesterKey: Uint8Array): string {
  let out = "";
  for (const b of linkId) out += b.toString(16).padStart(2, "0");
  for (const b of requesterKey) out += b.toString(16).padStart(2, "0");
  return out;
}

const isId = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v > 0;
const isHex = (v: unknown): v is string => typeof v === "string" && /^[0-9a-f]+$/.test(v) && v.length % 2 === 0;

function parseClaims(v: unknown): ObservedClaim[] | null {
  if (!Array.isArray(v)) return null;
  const out: ObservedClaim[] = [];
  for (const c of v) {
    const id = (c as { id?: unknown }).id;
    const author = (c as { author?: unknown }).author;
    if (!isId(id) || !isHex(author) || author.length !== 64) return null;
    out.push({ id, author });
  }
  return out;
}

function parseFounded(v: unknown): LinkFounded | null | undefined {
  if (v === null) return null;
  if (typeof v !== "object") return undefined;
  const f = v as {
    groupIdB64?: unknown;
    welcomeB64?: unknown;
    targets?: unknown;
    notePosted?: unknown;
    delivered?: unknown;
  };
  if (typeof f.groupIdB64 !== "string" || typeof f.welcomeB64 !== "string") return undefined;
  if (typeof f.delivered !== "boolean" || typeof f.notePosted !== "boolean" || !Array.isArray(f.targets)) {
    return undefined;
  }
  const targets: LinkFoundedTarget[] = [];
  for (const t of f.targets) {
    const key = (t as { accountKeyB64?: unknown }).accountKeyB64;
    const rec = (t as { isRecipient?: unknown }).isRecipient;
    if (typeof key !== "string" || typeof rec !== "boolean") return undefined;
    targets.push({ accountKeyB64: key, isRecipient: rec });
  }
  return {
    groupIdB64: f.groupIdB64,
    welcomeB64: f.welcomeB64,
    targets,
    notePosted: f.notePosted,
    delivered: f.delivered,
  };
}

function parseOwn(v: unknown): OwnLinkClaims | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as {
    wires?: unknown;
    ids?: unknown;
    won?: unknown;
    settled?: unknown;
    founded?: unknown;
    noteWire?: unknown;
    noteConfirmed?: unknown;
  };
  if (!Array.isArray(o.wires) || !o.wires.every(isHex)) return null;
  if (!Array.isArray(o.ids) || !o.ids.every(isId)) return null;
  if (o.won !== null && !isId(o.won)) return null;
  if (typeof o.settled !== "boolean") return null;
  const founded = parseFounded(o.founded);
  if (founded === undefined) return null;
  if (o.noteWire !== null && !isHex(o.noteWire)) return null;
  if (typeof o.noteConfirmed !== "boolean") return null;
  return {
    wires: o.wires as string[],
    ids: o.ids as number[],
    won: o.won as number | null,
    settled: o.settled,
    founded,
    noteWire: o.noteWire as string | null,
    noteConfirmed: o.noteConfirmed,
  };
}

const isDecimal = (v: unknown): v is string => typeof v === "string" && /^[0-9]{1,20}$/.test(v);

function parseJournal(v: unknown): StoredJournalEpochs | null {
  if (typeof v !== "object" || v === null) return null;
  const j = v as { groupIdHex?: unknown; base?: unknown; commitRowIds?: unknown; commitEpochsAfter?: unknown };
  if (!isHex(j.groupIdHex) || !isDecimal(j.base)) return null;
  if (!Array.isArray(j.commitRowIds) || !j.commitRowIds.every(isDecimal)) return null;
  if (!Array.isArray(j.commitEpochsAfter) || !j.commitEpochsAfter.every(isDecimal)) return null;
  if (j.commitRowIds.length !== j.commitEpochsAfter.length) return null;
  return {
    groupIdHex: j.groupIdHex,
    base: j.base,
    commitRowIds: j.commitRowIds as string[],
    commitEpochsAfter: j.commitEpochsAfter as string[],
  };
}

function read(accountIdB64: string, storage: StorageLike | null): Persisted {
  const empty: Persisted = { v: 2, seen: {}, own: {}, notes: {}, journal: null };
  if (storage === null) return empty;
  let raw: string | null;
  try {
    raw = storage.getItem(KEY_PREFIX + accountIdB64);
  } catch {
    return empty;
  }
  if (raw === null) return empty;
  try {
    const p = JSON.parse(raw) as { v?: unknown; seen?: unknown; own?: unknown; notes?: unknown; journal?: unknown };
    if (p.v !== 2 || typeof p.seen !== "object" || p.seen === null) return empty;
    const seen: Record<string, SeenEntry> = {};
    for (const [k, e] of Object.entries(p.seen as Record<string, unknown>)) {
      const claims = parseClaims((e as { claims?: unknown }).claims);
      const at = (e as { at?: unknown }).at;
      if (claims !== null && typeof at === "number") seen[k] = { claims, at };
    }
    const own: Record<string, OwnLinkClaims> = {};
    if (typeof p.own === "object" && p.own !== null) {
      for (const [k, e] of Object.entries(p.own as Record<string, unknown>)) {
        const parsed = parseOwn(e);
        if (parsed !== null) own[k] = parsed;
      }
    }
    const notes: Record<string, HeldLinkNote & { readonly at: number }> = {};
    if (typeof p.notes === "object" && p.notes !== null) {
      for (const [k, e] of Object.entries(p.notes as Record<string, unknown>)) {
        const n = e as { id?: unknown; groupIdB64?: unknown; welcomeB64?: unknown; at?: unknown };
        if (isId(n.id) && typeof n.groupIdB64 === "string" && typeof n.welcomeB64 === "string" && typeof n.at === "number") {
          notes[k] = { id: n.id, groupIdB64: n.groupIdB64, welcomeB64: n.welcomeB64, at: n.at };
        }
      }
    }
    return { v: 2, seen, own, notes, journal: parseJournal(p.journal) };
  } catch {
    return empty;
  }
}

function write(accountIdB64: string, value: Persisted, storage: StorageLike | null): void {
  if (storage === null) throw new Error("link claims: no storage");
  storage.setItem(KEY_PREFIX + accountIdB64, JSON.stringify(value));
}

const NO_OWN: OwnLinkClaims = {
  wires: [],
  ids: [],
  won: null,
  settled: false,
  founded: null,
  noteWire: null,
  noteConfirmed: false,
};

/** One account's claim record. Every write is one synchronous `setItem`, so
 *  a read-decide-write in one JS turn cannot interleave with the receive
 *  loop's write. */
export interface LinkClaimRecord {
  /** The claims other devices of the account posted for the request. */
  seen(keyHex: string): ObservedClaim[];
  /** Record a claim another device posted. Returns whether the record
   *  changed. Throws when the write fails. */
  recordSeen(keyHex: string, messageId: number, authorHex: string, nowMs: number): boolean;
  own(keyHex: string): OwnLinkClaims | null;
  /** Record a sealed claim BEFORE its publish. Throws when the write fails:
   *  a claim must not be posted unrecorded. */
  addOwnWire(keyHex: string, wireHex: string): void;
  /** A journal row carried these sealed bytes at this id, and `counts` is
   *  the readability rule's answer for it (sealed at the journal's epoch at
   *  its position). A claim of this device: filed as its own when it
   *  counts, else dropped (straddled: the next pass claims again). This
   *  device's note: confirmed when it counts, else dropped (posted again).
   *  Returns which it was, or `null`. Throws when the write fails. */
  judgeOwnRow(wireHex: string, messageId: number, counts: boolean): "claim" | "note" | null;
  /** `true` when some sealed claim or note still waits for its row. */
  hasUnmatchedWires(): boolean;
  /** Record the sealed note BEFORE its publish. Throws when the write fails. */
  setNoteWire(keyHex: string, wireHex: string): void;
  /** After a read of the journal past the note: a sealed note no row
   *  carried did not land; drop it (posted again). */
  dropUnlandedNote(keyHex: string): void;
  /** The journal's epochs held for the DeviceGroup `groupIdHex`, or `null`
   *  (none held, or held for another DeviceGroup). */
  journal(groupIdHex: string): StoredJournalEpochs | null;
  /** Keep the journal's epochs. Throws when the write fails. */
  setJournal(journal: StoredJournalEpochs): void;
  /** After a read of the journal past every claim: sealed claims never
   *  matched to a row did not land; drop them. */
  dropUnlanded(keyHex: string): void;
  setWon(keyHex: string, messageId: number): void;
  /** Keep a link-founded note another device posted (the earliest per
   *  request). Returns whether the record changed. Throws when the write
   *  fails. */
  recordNote(keyHex: string, note: HeldLinkNote, nowMs: number): boolean;
  /** The earliest note held for the request, or `null`. */
  note(keyHex: string): HeldLinkNote | null;
  /** Decided lost: re-check later without reading the journal again. */
  setSettled(keyHex: string): void;
  /** Record the founded one-to-one (before any Welcome is posted), or its
   *  delivery. Throws when the write fails: nothing may be posted for a
   *  founding this device would not find again. */
  setFounded(keyHex: string, founded: LinkFounded): void;
  forgetOwn(keyHex: string): void;
}

export function linkClaimRecord(
  accountIdB64: string,
  storage: StorageLike | null = defaultStorage(),
): LinkClaimRecord {
  const updateOwn = (keyHex: string, f: (cur: OwnLinkClaims) => OwnLinkClaims): void => {
    const cur = read(accountIdB64, storage);
    write(accountIdB64, { ...cur, own: { ...cur.own, [keyHex]: f(cur.own[keyHex] ?? NO_OWN) } }, storage);
  };
  return {
    seen(keyHex) {
      return [...(read(accountIdB64, storage).seen[keyHex]?.claims ?? [])];
    },
    recordSeen(keyHex, messageId, authorHex, nowMs) {
      const cur = read(accountIdB64, storage);
      const held = cur.seen[keyHex]?.claims ?? [];
      if (held.some((c) => c.id === messageId)) return false;
      const claims = [...held, { id: messageId, author: authorHex }]
        .sort((a, b) => a.id - b.id)
        .slice(0, MAX_CLAIMS_PER_REQUEST);
      if (!claims.some((c) => c.id === messageId)) return false;
      const seen: Record<string, SeenEntry> = { ...cur.seen, [keyHex]: { claims, at: nowMs } };
      const keys = Object.keys(seen);
      if (keys.length > MAX_SEEN_LINK_CLAIMS) {
        keys.sort((a, b) => (seen[a]?.at ?? 0) - (seen[b]?.at ?? 0));
        for (const k of keys.slice(0, keys.length - MAX_SEEN_LINK_CLAIMS)) delete seen[k];
      }
      write(accountIdB64, { ...cur, seen }, storage);
      return true;
    },
    own(keyHex) {
      return read(accountIdB64, storage).own[keyHex] ?? null;
    },
    addOwnWire(keyHex, wireHex) {
      updateOwn(keyHex, (o) => ({ ...o, wires: [...o.wires, wireHex] }));
    },
    judgeOwnRow(wireHex, messageId, counts) {
      const cur = read(accountIdB64, storage);
      for (const [k, o] of Object.entries(cur.own)) {
        if (o.wires.includes(wireHex)) {
          const wires = o.wires.filter((w) => w !== wireHex);
          const ids =
            !counts || o.ids.includes(messageId) ? o.ids : [...o.ids, messageId].sort((a, b) => a - b);
          write(accountIdB64, { ...cur, own: { ...cur.own, [k]: { ...o, wires, ids } } }, storage);
          return "claim";
        }
        if (o.noteWire === wireHex) {
          const next = { ...o, noteWire: null, noteConfirmed: o.noteConfirmed || counts };
          write(accountIdB64, { ...cur, own: { ...cur.own, [k]: next } }, storage);
          return "note";
        }
      }
      return null;
    },
    hasUnmatchedWires() {
      return Object.values(read(accountIdB64, storage).own).some((o) => o.wires.length > 0 || o.noteWire !== null);
    },
    setNoteWire(keyHex, wireHex) {
      updateOwn(keyHex, (o) => ({ ...o, noteWire: wireHex }));
    },
    dropUnlandedNote(keyHex) {
      if ((read(accountIdB64, storage).own[keyHex]?.noteWire ?? null) === null) return;
      updateOwn(keyHex, (o) => ({ ...o, noteWire: null }));
    },
    journal(groupIdHex) {
      const j = read(accountIdB64, storage).journal;
      return j !== null && j.groupIdHex === groupIdHex ? j : null;
    },
    setJournal(journal) {
      const cur = read(accountIdB64, storage);
      write(accountIdB64, { ...cur, journal }, storage);
    },
    dropUnlanded(keyHex) {
      if ((read(accountIdB64, storage).own[keyHex]?.wires.length ?? 0) === 0) return;
      updateOwn(keyHex, (o) => ({ ...o, wires: [] }));
    },
    setWon(keyHex, messageId) {
      updateOwn(keyHex, (o) => ({ ...o, won: messageId }));
    },
    recordNote(keyHex, note, nowMs) {
      const cur = read(accountIdB64, storage);
      const held = cur.notes[keyHex];
      if (held !== undefined && held.id <= note.id) return false;
      const notes = { ...cur.notes, [keyHex]: { ...note, at: nowMs } };
      const keys = Object.keys(notes);
      if (keys.length > MAX_LINK_NOTES) {
        keys.sort((x, y) => (notes[x]?.at ?? 0) - (notes[y]?.at ?? 0));
        for (const k of keys.slice(0, keys.length - MAX_LINK_NOTES)) delete notes[k];
      }
      write(accountIdB64, { ...cur, notes }, storage);
      return true;
    },
    note(keyHex) {
      const n = read(accountIdB64, storage).notes[keyHex];
      return n === undefined ? null : { id: n.id, groupIdB64: n.groupIdB64, welcomeB64: n.welcomeB64 };
    },
    setSettled(keyHex) {
      updateOwn(keyHex, (o) => ({ ...o, settled: true }));
    },
    setFounded(keyHex, founded) {
      updateOwn(keyHex, (o) => ({ ...o, founded }));
    },
    forgetOwn(keyHex) {
      const cur = read(accountIdB64, storage);
      if (cur.own[keyHex] === undefined) return;
      const own = { ...cur.own };
      delete own[keyHex];
      try {
        write(accountIdB64, { ...cur, own }, storage);
      } catch {
        // The request is already gone; a kept record builds nothing without
        // its request.
      }
    },
  };
}

/** Forget device: drop the account's claim record. */
export function clearLinkClaims(accountIdB64: string, storage: StorageLike | null = defaultStorage()): void {
  if (storage === null) return;
  try {
    storage.removeItem(KEY_PREFIX + accountIdB64);
  } catch {
    // best-effort, like the rest of the forget-device wipe
  }
}
