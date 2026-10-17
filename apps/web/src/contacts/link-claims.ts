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
  readonly delivered: boolean;
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
  /** A one-to-one was founded for it already: deliver that one's Welcome. */
  | "resume"
  /** The account mailbox could not be read past now: decide next pass. */
  | "wait"
  /** A one-to-one with the requester is held already: answered, build
   *  nothing. */
  | "answered"
  /** Found the one-to-one. */
  | "found";

/**
 * Before any device builds a one-to-one for a request (the first winner,
 * or a device that took it over because the winner left the roster), it
 * reads its account mailbox past now and looks for a one-to-one with the
 * requester. Pure.
 */
export function linkBuildStep(input: {
  readonly hasFounded: boolean;
  readonly mailboxCaughtUp: boolean;
  readonly holdsOneToOne: boolean;
}): LinkBuildStep {
  if (input.hasFounded) return "resume";
  if (!input.mailboxCaughtUp) return "wait";
  return input.holdsOneToOne ? "answered" : "found";
}

/** Whether one of `conversations` is a one-to-one with the account whose
 *  key is `requesterKeyHex` (member account keys, lower-case hex). Pure. */
export function holdsOneToOneWith(
  conversations: readonly { readonly oneToOne: boolean; readonly memberAccountKeysHex: readonly string[] }[],
  requesterKeyHex: string,
): boolean {
  const want = requesterKeyHex.toLowerCase();
  return conversations.some((c) => c.oneToOne && c.memberAccountKeysHex.some((k) => k.toLowerCase() === want));
}

/** What the device does with a request carrying a link answer, now. */
export type LinkRequestStep =
  /** The synced state is not loaded yet: keep it undecided, unshown. */
  | "wait"
  /** Answered (by another device, or the link register says so): remove. */
  | "drop"
  /** The link no longer works: show the card with the note. */
  | "card-dead"
  /** Live link: post this device's claim. */
  | "post-claim"
  /** Claim posted: read the journal past it and decide. */
  | "decide"
  /** Lost: decide again against the current roster (no journal read). */
  | "recheck"
  /** Claim won: found the one-to-one (again, after a failure). */
  | "build";

function holdsClaim(own: OwnLinkClaims | null): own is OwnLinkClaims {
  return own !== null && (own.won !== null || own.ids.length > 0 || own.wires.length > 0);
}

/**
 * The next step for one request. Pure.
 *
 * | own claims        | answered elsewhere | verdict  | step         |
 * |-------------------|--------------------|----------|--------------|
 * | won               | any                | any      | `build`      |
 * | any other         | any                | answered | `drop`       |
 * | lost (settled)    | any                | dead     | `drop`       |
 * | lost (settled)    | any                | other    | `recheck`    |
 * | posted or posting | any                | any      | `decide`     |
 * | none              | yes                | dead     | `drop`       |
 * | none              | yes                | other    | `wait`       |
 * | none              | no                 | not held | `wait`       |
 * | none              | no                 | dead     | `card-dead`  |
 * | none              | no                 | accept   | `post-claim` |
 *
 * "Answered elsewhere" is the core's `linkClaimAnsweredElsewhere`: a claim
 * from a device in the current roster. A won claim builds whatever the
 * verdict says by now: the decision was made. Otherwise a request goes only
 * when the register says it was answered (the winner counted the use), or
 * the link is dead while another device holds the claim that answers it:
 * until then a loser re-checks, and a device without a claim waits, so a
 * claimer that is removed before it builds hands the request on.
 */
export function linkRequestStep(input: {
  readonly own: OwnLinkClaims | null;
  readonly answeredElsewhere: boolean;
  readonly verdict: "accept" | "answered" | "dead" | null;
}): LinkRequestStep {
  if (holdsClaim(input.own) && input.own.won !== null) return "build";
  if (input.verdict === "answered") return "drop";
  if (holdsClaim(input.own)) {
    if (!input.own.settled) return "decide";
    return input.verdict === "dead" ? "drop" : "recheck";
  }
  if (input.answeredElsewhere) return input.verdict === "dead" ? "drop" : "wait";
  if (input.verdict === null) return "wait";
  if (input.verdict === "dead") return "card-dead";
  return "post-claim";
}

/**
 * A card already shown for a request (the link no longer worked here) that
 * a device in the roster claimed: the request is answered, the card goes.
 * Pure.
 */
export function shownCardAnswered(own: OwnLinkClaims | null, answeredElsewhere: boolean): boolean {
  return !holdsClaim(own) && answeredElsewhere;
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

interface JoinedWelcome {
  readonly welcomeB64: string;
  readonly at: number;
}

interface Persisted {
  readonly v: 2;
  readonly seen: Record<string, SeenEntry>;
  readonly own: Record<string, OwnLinkClaims>;
  /** The Welcome each recently joined one-to-one was joined from, by group
   *  id: a device that takes a request over forwards it to the requester. */
  readonly joined: Record<string, JoinedWelcome>;
}

/** How many joined one-to-one Welcomes are kept; past it the oldest go.
 *  Chosen, not measured: a Welcome matters only while a request it answers
 *  can still be taken over. */
export const MAX_JOINED_WELCOMES = 32;

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
  const f = v as { groupIdB64?: unknown; welcomeB64?: unknown; targets?: unknown; delivered?: unknown };
  if (typeof f.groupIdB64 !== "string" || typeof f.welcomeB64 !== "string") return undefined;
  if (typeof f.delivered !== "boolean" || !Array.isArray(f.targets)) return undefined;
  const targets: LinkFoundedTarget[] = [];
  for (const t of f.targets) {
    const key = (t as { accountKeyB64?: unknown }).accountKeyB64;
    const rec = (t as { isRecipient?: unknown }).isRecipient;
    if (typeof key !== "string" || typeof rec !== "boolean") return undefined;
    targets.push({ accountKeyB64: key, isRecipient: rec });
  }
  return { groupIdB64: f.groupIdB64, welcomeB64: f.welcomeB64, targets, delivered: f.delivered };
}

function parseOwn(v: unknown): OwnLinkClaims | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as { wires?: unknown; ids?: unknown; won?: unknown; settled?: unknown; founded?: unknown };
  if (!Array.isArray(o.wires) || !o.wires.every(isHex)) return null;
  if (!Array.isArray(o.ids) || !o.ids.every(isId)) return null;
  if (o.won !== null && !isId(o.won)) return null;
  if (typeof o.settled !== "boolean") return null;
  const founded = parseFounded(o.founded);
  if (founded === undefined) return null;
  return {
    wires: o.wires as string[],
    ids: o.ids as number[],
    won: o.won as number | null,
    settled: o.settled,
    founded,
  };
}

function read(accountIdB64: string, storage: StorageLike | null): Persisted {
  const empty: Persisted = { v: 2, seen: {}, own: {}, joined: {} };
  if (storage === null) return empty;
  let raw: string | null;
  try {
    raw = storage.getItem(KEY_PREFIX + accountIdB64);
  } catch {
    return empty;
  }
  if (raw === null) return empty;
  try {
    const p = JSON.parse(raw) as { v?: unknown; seen?: unknown; own?: unknown; joined?: unknown };
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
    const joined: Record<string, JoinedWelcome> = {};
    if (typeof p.joined === "object" && p.joined !== null) {
      for (const [k, e] of Object.entries(p.joined as Record<string, unknown>)) {
        const w = (e as { welcomeB64?: unknown }).welcomeB64;
        const at = (e as { at?: unknown }).at;
        if (typeof w === "string" && typeof at === "number") joined[k] = { welcomeB64: w, at };
      }
    }
    return { v: 2, seen, own, joined };
  } catch {
    return empty;
  }
}

function write(accountIdB64: string, value: Persisted, storage: StorageLike | null): void {
  if (storage === null) throw new Error("link claims: no storage");
  storage.setItem(KEY_PREFIX + accountIdB64, JSON.stringify(value));
}

const NO_OWN: OwnLinkClaims = { wires: [], ids: [], won: null, settled: false, founded: null };

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
  /** A journal row (or a publish answer) carried these sealed bytes at this
   *  id: if they are one of this device's recorded claims, file the id as
   *  its own. Returns whether they were. Throws when the write fails. */
  recordOwnClaimRow(wireHex: string, messageId: number): boolean;
  /** `true` when some sealed claim still waits for its row. */
  hasUnmatchedWires(): boolean;
  /** After a read of the journal past every claim: sealed claims never
   *  matched to a row did not land; drop them. */
  dropUnlanded(keyHex: string): void;
  setWon(keyHex: string, messageId: number): void;
  /** Keep the Welcome a one-to-one was joined from (bounded). */
  recordJoinedWelcome(groupIdB64: string, welcomeB64: string, nowMs: number): void;
  /** The Welcome a held one-to-one was joined from, or `null`. */
  joinedWelcome(groupIdB64: string): string | null;
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
    recordOwnClaimRow(wireHex, messageId) {
      const cur = read(accountIdB64, storage);
      for (const [k, o] of Object.entries(cur.own)) {
        if (!o.wires.includes(wireHex)) continue;
        const ids = o.ids.includes(messageId) ? o.ids : [...o.ids, messageId].sort((a, b) => a - b);
        write(
          accountIdB64,
          { ...cur, own: { ...cur.own, [k]: { ...o, wires: o.wires.filter((w) => w !== wireHex), ids } } },
          storage,
        );
        return true;
      }
      return false;
    },
    hasUnmatchedWires() {
      return Object.values(read(accountIdB64, storage).own).some((o) => o.wires.length > 0);
    },
    dropUnlanded(keyHex) {
      if ((read(accountIdB64, storage).own[keyHex]?.wires.length ?? 0) === 0) return;
      updateOwn(keyHex, (o) => ({ ...o, wires: [] }));
    },
    setWon(keyHex, messageId) {
      updateOwn(keyHex, (o) => ({ ...o, won: messageId }));
    },
    recordJoinedWelcome(groupIdB64, welcomeB64, nowMs) {
      const cur = read(accountIdB64, storage);
      const joined: Record<string, JoinedWelcome> = { ...cur.joined, [groupIdB64]: { welcomeB64, at: nowMs } };
      const keys = Object.keys(joined);
      if (keys.length > MAX_JOINED_WELCOMES) {
        keys.sort((a, b) => (joined[a]?.at ?? 0) - (joined[b]?.at ?? 0));
        for (const k of keys.slice(0, keys.length - MAX_JOINED_WELCOMES)) delete joined[k];
      }
      write(accountIdB64, { ...cur, joined }, storage);
    },
    joinedWelcome(groupIdB64) {
      return read(accountIdB64, storage).joined[groupIdB64]?.welcomeB64 ?? null;
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
