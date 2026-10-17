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

interface Persisted {
  readonly v: 2;
  readonly seen: Record<string, SeenEntry>;
  readonly own: Record<string, OwnLinkClaims>;
}

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

function parseOwn(v: unknown): OwnLinkClaims | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as { wires?: unknown; ids?: unknown; won?: unknown; settled?: unknown };
  if (!Array.isArray(o.wires) || !o.wires.every(isHex)) return null;
  if (!Array.isArray(o.ids) || !o.ids.every(isId)) return null;
  if (o.won !== null && !isId(o.won)) return null;
  if (typeof o.settled !== "boolean") return null;
  return { wires: o.wires as string[], ids: o.ids as number[], won: o.won as number | null, settled: o.settled };
}

function read(accountIdB64: string, storage: StorageLike | null): Persisted {
  const empty: Persisted = { v: 2, seen: {}, own: {} };
  if (storage === null) return empty;
  let raw: string | null;
  try {
    raw = storage.getItem(KEY_PREFIX + accountIdB64);
  } catch {
    return empty;
  }
  if (raw === null) return empty;
  try {
    const p = JSON.parse(raw) as { v?: unknown; seen?: unknown; own?: unknown };
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
    return { v: 2, seen, own };
  } catch {
    return empty;
  }
}

function write(accountIdB64: string, value: Persisted, storage: StorageLike | null): void {
  if (storage === null) throw new Error("link claims: no storage");
  storage.setItem(KEY_PREFIX + accountIdB64, JSON.stringify(value));
}

const NO_OWN: OwnLinkClaims = { wires: [], ids: [], won: null, settled: false };

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
  /** Decided lost: re-check later without reading the journal again. */
  setSettled(keyHex: string): void;
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
    setSettled(keyHex) {
      updateOwn(keyHex, (o) => ({ ...o, settled: true }));
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
