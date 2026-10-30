// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Add members screen of a group's info (`docs/ux/conversation-info.md`
 * section 2.4): who is listed, who may be picked, and how the picks are added.
 *
 * Every decision is here, as plain functions; `AddMembersScreen.tsx` renders
 * them and `routes/Chat.tsx` feeds them the live group.
 *
 * - The list is the person's contacts, minus everyone already in the group and
 *   everyone an add is already waiting for. A contact with no captured account
 *   id stays listed, dimmed and not pickable (`new-group.md` section 3).
 * - Typing filters that list locally, loosely, from the first letter.
 * - From three characters the field also asks the network, the same search the
 *   Chats and Contacts field runs (`search/people-search.ts`); its results are
 *   shown under People, minus members, pending adds, contacts (listed above or
 *   not) and this account. A result with no account id is shown, not pickable.
 * - Add adds the picks one after another, then sweeps once; a pick that fails
 *   is named in one sentence and the others still land.
 */
import type { PersonResult } from "../search/people";

/** One pick, whichever list it came from: what the add path takes. */
export interface AddPick {
  /** The person (account) key, standard base64: the add's signature key. */
  readonly keyB64: string;
  /** The 16-byte account id as hex. */
  readonly accountIdHex: string;
  /** The name a failure names them by. */
  readonly label: string;
  /** For a person found by search through a reach grant: the 134-byte grant
   *  wire (standard base64). Kept as a grant held for them before the add, so
   *  the committer delegations are signed with it (ADR-0060 section 6).
   *  `null` for a contact. Secret. */
  readonly grantWireB64: string | null;
  /** For a person found in Everyone: their 97-byte mailbox binding from the
   *  public record (standard base64), which the no-delegation committer
   *  envelope carries so the Welcome can be written to their open mailbox.
   *  `null` otherwise. */
  readonly openBindingB64: string | null;
}

/** A contact as the list needs one. */
export interface ContactCandidate {
  readonly keyB64: string;
  /** `undefined` for a record from before account ids were captured. */
  readonly accountIdHex: string | undefined;
  readonly name: string;
  /** Their verified username as shown (`@anna`), when known. */
  readonly username: string | null;
}

/** One row of the contact list. */
export interface ContactRow {
  readonly contact: ContactCandidate;
  /** `false`: listed dimmed with the accept-again line, no checkbox. */
  readonly pickable: boolean;
}

/** One row of the People section. */
export interface PersonRow {
  readonly person: PersonResult;
  /** `false`: shown with no checkbox (no account id, or not reachable). */
  readonly pickable: boolean;
}

/** Who is out of both lists, by person key (standard base64). */
export interface Excluded {
  /** The people in the group (account keys). */
  readonly memberKeysB64: ReadonlySet<string>;
  /** Everyone an add is already waiting for: the pending adds of the group
   *  and the adds this device has in flight. */
  readonly pendingKeysB64: ReadonlySet<string>;
  /** This account's own key, when known. */
  readonly ownKeyB64: string | null;
}

/** One add this device proposed (`chat/membership-retry.ts`
 *  `OwnRequestRecord`): the person, and the leaf keys of the devices the
 *  proposals carried. */
export interface OwnAddRecord {
  readonly subjectKeyB64: string;
  readonly deviceKeysB64: readonly string[];
}

/**
 * The people an add is waiting for, by person key. The group's pending set
 * names an add by the LEAF key of the proposed device (ADR-0050), which for a
 * device bound to an account is not the person key the lists compare; this
 * device's own add records map those leaves back to the person. A pending add
 * another member proposed for a bound device stays known by its leaf only.
 */
export function pendingPersonKeys(
  pendingLeafKeysB64: readonly string[],
  ownAdds: readonly OwnAddRecord[],
): Set<string> {
  const out = new Set<string>(pendingLeafKeysB64);
  for (const rec of ownAdds) {
    if (pendingLeafKeysB64.includes(rec.subjectKeyB64) || rec.deviceKeysB64.some((k) => out.has(k))) {
      out.add(rec.subjectKeyB64);
    }
  }
  return out;
}

/** A normaliser for the loose match (`search/people.ts` `Normalize`). */
export type Matches = (text: string, query: string) => boolean;

function excludedKey(keyB64: string, ex: Excluded): boolean {
  return ex.memberKeysB64.has(keyB64) || ex.pendingKeysB64.has(keyB64) || keyB64 === ex.ownKeyB64;
}

/** Whether a contact matches what was typed: by name or by username. An
 *  empty query matches everyone. */
export function contactMatches(c: ContactCandidate, query: string, matches: Matches): boolean {
  const q = query.trim().replace(/^@+/u, "").trim();
  if (q.length === 0) return true;
  if (matches(c.name, q)) return true;
  return c.username !== null && matches(c.username, q);
}

/** The contact list, in the order given, filtered by what was typed. */
export function contactRows(
  contacts: readonly ContactCandidate[],
  query: string,
  excluded: Excluded,
  matches: Matches,
): ContactRow[] {
  const out: ContactRow[] = [];
  for (const c of contacts) {
    if (excludedKey(c.keyB64, excluded)) continue;
    if (!contactMatches(c, query, matches)) continue;
    out.push({ contact: c, pickable: c.accountIdHex !== undefined });
  }
  return out;
}

/** The People section: network results that are not members, not pending,
 *  not this account and not a contact (a contact is in the list above, or is
 *  out of it for a reason that holds here too). */
export function personRows(
  results: readonly PersonResult[],
  contactKeysB64: ReadonlySet<string>,
  excluded: Excluded,
): PersonRow[] {
  const out: PersonRow[] = [];
  const seen = new Set<string>();
  for (const p of results) {
    if (seen.has(p.keyB64)) continue;
    seen.add(p.keyB64);
    if (p.reach === "contact" || contactKeysB64.has(p.keyB64)) continue;
    if (excludedKey(p.keyB64, excluded)) continue;
    out.push({ person: p, pickable: personPickable(p) });
  }
  return out;
}

/** A found person may be picked exactly when the search found a way to reach
 *  them (`who-can-find-me.md` section 12) and named their account. */
export function personPickable(p: PersonResult): boolean {
  return p.accountIdHex !== null && (p.reach === "open" || p.reach === "grant");
}

/** The pick a contact row makes, or `null` when it is not pickable. */
export function pickOfContact(c: ContactCandidate): AddPick | null {
  if (c.accountIdHex === undefined) return null;
  return { keyB64: c.keyB64, accountIdHex: c.accountIdHex, label: c.name, grantWireB64: null, openBindingB64: null };
}

/** The pick a People row makes, or `null` when it is not pickable. */
export function pickOfPerson(p: PersonResult): AddPick | null {
  if (!personPickable(p) || p.accountIdHex === null) return null;
  return {
    keyB64: p.keyB64,
    accountIdHex: p.accountIdHex,
    label: p.name,
    grantWireB64: p.reach === "grant" ? p.grantWireB64 : null,
    openBindingB64: p.reach === "open" ? (p.openBindingB64 ?? null) : null,
  };
}

/** Toggle a pick in an ordered list (the chips keep the order picked). */
export function togglePick(picks: readonly AddPick[], pick: AddPick): AddPick[] {
  return picks.some((p) => p.keyB64 === pick.keyB64)
    ? picks.filter((p) => p.keyB64 !== pick.keyB64)
    : [...picks, pick];
}

/** Picks that are still addable: a person who joined, or whose add started
 *  meanwhile, drops out of the picks. */
export function livePicks(picks: readonly AddPick[], excluded: Excluded): AddPick[] {
  return picks.filter((p) => !excludedKey(p.keyB64, excluded));
}

/** The trailing button: `Add` with zero or one pick, `Add {count}` above. */
export type SubmitLabel = { readonly kind: "single" } | { readonly kind: "count"; readonly count: number };

export function submitLabel(pickCount: number): SubmitLabel {
  return pickCount > 1 ? { kind: "count", count: pickCount } : { kind: "single" };
}

/** What adding the picks needs from the conversation. */
export interface AddPorts {
  /** One add through the conversation's add path, with everything it owes.
   *  Throws when the person could not be added. */
  addOne(pick: AddPick): Promise<void>;
  /** The one sweep after the last add. */
  sweep(): Promise<void>;
}

/**
 * Add `picks` one after another, then sweep once (as the New-group setup
 * does). A failed add is collected and the next one still runs; a failed
 * sweep is not a failed add (the requests are out, the next poll sweeps).
 * Returns the labels of the picks that could not be added, in pick order.
 */
export async function addPicksInTurn(picks: readonly AddPick[], ports: AddPorts): Promise<string[]> {
  const failed: string[] = [];
  for (const pick of picks) {
    try {
      await ports.addOne(pick);
    } catch {
      failed.push(pick.label);
    }
  }
  try {
    await ports.sweep();
  } catch {
    /* the requests are published; a later sweep carries them */
  }
  return failed;
}
