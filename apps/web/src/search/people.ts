// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * People search, the pure half (`docs/ux/who-can-find-me.md` section 6.1,
 * ADR-0058 "Client contract (S4 / S5)", Search).
 *
 * What a result is, how the LOCAL half matches ("any part of any word",
 * letter case and accents never matter), how local and network results merge
 * into ONE list keyed on the person's account key, and the one rule that
 * decides whether a `@username` may be shown beside a name.
 *
 * No wasm, no network, no Solid: the core's normaliser arrives as an
 * argument, so a test can hand it the real one or a stand-in.
 */
import { base64Encode } from "../auth/account-id";

/** Normalise a name or a query for search; `null` when it is not searchable. */
export type Normalize = (input: string) => string | null;

/** Longest input the core normaliser accepts, in bytes. */
const MAX_NAME_INPUT_BYTES = 1024;

/**
 * The same normalisation the core applies (NFKD, marks dropped, lowercase,
 * whitespace collapsed, trimmed), for what the core refuses to normalise
 * because it is SHORTER than a network query may be: a local search runs
 * from the first letter, while the core's `normalizeNameForSearch` answers
 * `undefined` under 2 characters.
 */
export function fallbackNormalize(input: string): string | null {
  if (new TextEncoder().encode(input).length > MAX_NAME_INPUT_BYTES) return null;
  const out = input
    .normalize("NFKD")
    .replace(/\p{M}+/gu, "")
    .toLowerCase()
    .replace(/\s+/gu, " ")
    .trim();
  return out.length === 0 ? null : out;
}

/** The core normaliser, with the short-input fallback beside it. */
export function searchNormalizer(core: (input: string) => string | undefined): Normalize {
  return (input) => {
    const normalised = core(input);
    return normalised === undefined ? fallbackNormalize(input) : normalised;
  };
}

/**
 * The local match: every word of the query is a part of some word of the
 * name. `ann`, `nna` and `weber an` all find `Anna Weber`.
 */
export function matchesLoosely(name: string, query: string, normalize: Normalize): boolean {
  const q = normalize(query);
  if (q === null) return false;
  const n = normalize(name);
  if (n === null) return false;
  const words = n.split(" ");
  return q.split(" ").every((part) => words.some((word) => word.includes(part)));
}

/**
 * How this account may reach a person, strongest first:
 * - `contact`: already a contact - nothing to add;
 * - `open`:    the person is in Everyone, their mailbox is open;
 * - `grant`:   they handed this account a reach key (a group or channel they
 *              let find them, or an introduction's contacts-of-contacts key);
 * - `none`:    they do not let this account find them.
 */
export type PersonReach = "contact" | "open" | "grant" | "none";

const REACH_RANK: Record<PersonReach, number> = { contact: 3, open: 2, grant: 1, none: 0 };

/** One person in the result list. */
export interface PersonResult {
  /** The account key (the person key, ADR-0050), standard base64. */
  readonly keyB64: string;
  /** The same key as lower-case hex: what the grant stores key on. */
  readonly keyHex: string;
  /** The 16-byte auth account id as hex, when known. A request needs it. */
  readonly accountIdHex: string | null;
  /** The name to show. */
  readonly name: string;
  /** A VERIFIED username, already passed through `shownUsername`. */
  readonly username: string | null;
  readonly reach: PersonReach;
  /** For `grant`: the 37-byte grant wire (standard base64) the reach check
   *  and the request are signed with. Secret. */
  readonly grantWireB64: string | null;
}

/**
 * Whether a verified username may be shown (UX section 6.2): a username is a
 * handle for finding someone, so it goes with the permission to use it. A
 * contact's always shows; a hidden person's never does.
 */
export function shownUsername(verified: string | null, reach: PersonReach): string | null {
  if (reach === "none") return null;
  return verified;
}

/** Whether the row offers **Add contact**. */
export function canAddContact(person: PersonResult): boolean {
  return (person.reach === "open" || person.reach === "grant") && person.accountIdHex !== null;
}

/** A person this device already knows: a contact, or a member of one of its
 *  groups or channels whose profile it holds. */
export interface LocalCandidate {
  readonly keyB64: string;
  readonly keyHex: string;
  /** The resolved name (a local rename, their profile name, or their
   *  username); `null` when this device holds no name for them. */
  readonly name: string | null;
  /** Their username, when verified. */
  readonly verifiedUsername: string | null;
  readonly isContact: boolean;
  readonly accountIdHex: string | null;
  /** The widest grant kept for them (`received-grants.ts` `bestGrantFor`),
   *  or `null`. */
  readonly keptGrantWireB64: string | null;
}

/** A local candidate as a result, with its reach worked out. */
export function localResult(c: LocalCandidate, name: string): PersonResult {
  const reach: PersonReach = c.isContact ? "contact" : c.keptGrantWireB64 !== null ? "grant" : "none";
  return {
    keyB64: c.keyB64,
    keyHex: c.keyHex,
    accountIdHex: c.accountIdHex,
    name,
    username: shownUsername(c.verifiedUsername, reach),
    reach,
    grantWireB64: reach === "grant" ? c.keptGrantWireB64 : null,
  };
}

/**
 * The local half: the candidates whose name - or whose verified username,
 * for someone this account may find - matches the query loosely. A
 * candidate with no name is never listed (there is nothing to show).
 */
export function localPeople(
  candidates: readonly LocalCandidate[],
  query: string,
  normalize: Normalize,
  ownKeyHex: string | null,
): PersonResult[] {
  if (normalize(query) === null) return [];
  const bare = query.trim().replace(/^@+/, "");
  const seen = new Set<string>();
  const out: PersonResult[] = [];
  for (const c of candidates) {
    if (c.keyHex === ownKeyHex || seen.has(c.keyHex) || c.name === null) continue;
    const result = localResult(c, c.name);
    const byName = matchesLoosely(c.name, query, normalize);
    const byUsername =
      result.username !== null && bare.length > 0 && matchesLoosely(result.username, bare, normalize);
    if (!byName && !byUsername) continue;
    seen.add(c.keyHex);
    out.push(result);
  }
  return out;
}

/**
 * Merge result lists into one, keyed on the account key: the strongest reach
 * wins, the first list's name is kept (the local one, which respects a
 * rename), and a username is kept from whichever list verified one - then
 * gated again by the merged reach. The own account never appears.
 */
export function mergePeople(
  lists: readonly (readonly PersonResult[])[],
  ownKeyHex: string | null,
): PersonResult[] {
  const byKey = new Map<string, PersonResult>();
  const order: string[] = [];
  for (const list of lists) {
    for (const p of list) {
      if (p.keyHex === ownKeyHex) continue;
      const held = byKey.get(p.keyHex);
      if (held === undefined) {
        byKey.set(p.keyHex, p);
        order.push(p.keyHex);
        continue;
      }
      const stronger = REACH_RANK[p.reach] > REACH_RANK[held.reach] ? p : held;
      const reach = stronger.reach;
      byKey.set(p.keyHex, {
        keyB64: held.keyB64,
        keyHex: held.keyHex,
        accountIdHex: held.accountIdHex ?? p.accountIdHex,
        name: held.name,
        username: shownUsername(held.username ?? p.username, reach),
        reach,
        grantWireB64:
          reach === "grant" ? (stronger.grantWireB64 ?? held.grantWireB64 ?? p.grantWireB64) : null,
      });
    }
  }
  return order.map((k) => byKey.get(k)!);
}

/** Lower-case hex of a byte string. */
export function toHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  return out;
}

/** Standard base64 of a key, the form the profile store keys on. */
export function keyB64Of(key: Uint8Array): string {
  return base64Encode(key);
}
