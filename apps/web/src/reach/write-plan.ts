// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-53 (maintainer decision 2026-11-21) - which held reach grant writes a
 * Welcome or signs a committer delegation. The rule is the core's
 * (`lakko_contacts::reach_plan`, `planReachWrites` on wasm); this module only
 * feeds it what the device keeps and acts on the answer:
 *
 *   - the grants kept for ONE person ({@link heldEntriesForPerson}): under
 *     every key they have had (ADR-0074 section 3.5a rule 2), and the grants
 *     of before a rotation while their address stands (rule 6);
 *   - split by the mailbox each grant's binding names, verified against the
 *     person's pinned keys (ADR-0078): the core plans one mailbox per call,
 *     and the grants of a mailbox the person left go in their own call;
 *   - `sharedScopes`: the conversations this account and the person are both
 *     current members of, read off the live rosters by the provider the app
 *     installs ({@link installSharedScopes});
 *   - `found`: the grant a network search found the person through and this
 *     account kept for its add (`found` in `received-grants.ts`).
 *
 * What comes back: the candidates, best first, one per distinct key, the
 * mailbox of the person's current key first; and the grants a higher version
 * of the same key replaced, which {@link dropSupersededEntries} deletes.
 *
 * The contact-request and reply-answer orders are not this rule's
 * (`received-grants.ts` `grantsInOrderForPerson`, `contactGrantsForPerson`).
 */
import type { ReachPurpose } from "../../wasm/lakko_ffi_wasm";
import { base64Decode } from "../auth/account-id";
import { journalHex } from "../ds/access";
import { loadWasm, type Wasm } from "../wasm";
import { reachSession } from "./reach-service";
import { personKeysHex } from "../contacts/address-resolver";
import type { HeldGrant, PersonGrants, PreviousGrant, ReceivedGrants, ScopedGrantClass } from "./received-grants";

/** Where a kept grant sits in the store. */
export type GrantSlot =
  | { readonly kind: "contact" }
  | { readonly kind: "found" }
  | { readonly kind: "scoped"; readonly scopeHex: string; readonly cls: ScopedGrantClass };

/** One kept grant, with the place it is filed at. */
export interface HeldEntry {
  /** The account key hex the grant is filed under. */
  readonly personHex: string;
  readonly slot: GrantSlot;
  /** Kept as the grant of before a rotation (rule 6). */
  readonly previous: boolean;
  readonly grant: HeldGrant;
}

const SCOPED_ORDER: readonly ScopedGrantClass[] = ["group", "member", "channel"];

function entriesOf(personHex: string, person: PersonGrants): HeldEntry[] {
  const out: HeldEntry[] = [];
  if (person.contact !== undefined) {
    out.push({ personHex, slot: { kind: "contact" }, previous: false, grant: person.contact });
  }
  for (const [scopeHex, held] of Object.entries(person.scopes)) {
    for (const cls of SCOPED_ORDER) {
      const g = held[cls];
      if (g !== undefined) out.push({ personHex, slot: { kind: "scoped", scopeHex, cls }, previous: false, grant: g });
    }
  }
  if (person.found !== undefined) {
    out.push({ personHex, slot: { kind: "found" }, previous: false, grant: person.found });
  }
  return out;
}

function previousEntriesOf(personHex: string, person: PersonGrants, addressHex: string): HeldEntry[] {
  const previous = person.previous;
  if (previous === undefined) return [];
  const live = (g: PreviousGrant | undefined): g is PreviousGrant => g !== undefined && g.addressHex === addressHex;
  const out: HeldEntry[] = [];
  if (live(previous.contact)) {
    out.push({ personHex, slot: { kind: "contact" }, previous: true, grant: previous.contact });
  }
  for (const [scopeHex, held] of Object.entries(previous.scopes ?? {})) {
    for (const cls of SCOPED_ORDER) {
      const g = held[cls];
      if (live(g)) out.push({ personHex, slot: { kind: "scoped", scopeHex, cls }, previous: true, grant: g });
    }
  }
  return out;
}

/**
 * Every grant kept for the person `keysHex` names (the current key first,
 * then the proven predecessors, as `personKeysHex` gives them): the grants
 * filed under each key, then the grants of before a rotation that are still
 * good for the address `keysHex[0]` (rule 6). Pure. Order is only the order
 * mailboxes are first met in; the core does not depend on it.
 */
export function heldEntriesForPerson(store: ReceivedGrants, keysHex: readonly string[]): HeldEntry[] {
  const out: HeldEntry[] = [];
  for (const hex of keysHex) {
    const person = store.people[hex];
    if (person !== undefined) out.push(...entriesOf(hex, person));
  }
  const addressHex = keysHex[0];
  if (addressHex !== undefined) {
    for (const hex of keysHex) {
      const person = store.people[hex];
      if (person !== undefined) out.push(...previousEntriesOf(hex, person, addressHex));
    }
  }
  return out;
}

/** The scope bytes of a slot: the group or channel id for a scoped grant,
 *  empty otherwise. A scope that is not hex (never in practice) is given as
 *  its text, so the call stays well formed. */
function scopeBytesOf(slot: GrantSlot): Uint8Array {
  if (slot.kind !== "scoped") return new Uint8Array(0);
  return scopeBytes(slot.scopeHex);
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function scopeBytes(scopeHex: string): Uint8Array {
  if (/^(?:[0-9a-f]{2})*$/i.test(scopeHex)) return hexBytes(scopeHex).slice(0, 64);
  return new TextEncoder().encode(scopeHex).slice(0, 64);
}

/** One mailbox's plan. */
export interface MailboxReachPlan {
  /** The mailbox address the candidates' bindings name, hex. */
  readonly addressHex: string;
  /** Best first; one per distinct key. */
  readonly candidates: readonly HeldEntry[];
}

/** The plan for one person, over every mailbox a kept grant names. */
export interface PersonReachPlan {
  /** The mailbox the first kept grant names first (the person's current key
   *  is read first). */
  readonly mailboxes: readonly MailboxReachPlan[];
  /** Every mailbox's candidates in that order, each wire once. */
  readonly candidates: readonly HeldEntry[];
  /** The grants a higher version of the same key replaced. Delete them. */
  readonly superseded: readonly HeldEntry[];
}

/** What a plan is for. `welcome`: a Welcome this device writes; `delegation`:
 *  a committer delegation this device signs. */
export type ReachWritePurpose = "welcome" | "delegation";

function purposeOf(wasm: Wasm, purpose: ReachWritePurpose): ReachPurpose {
  return purpose === "welcome" ? wasm.ReachPurpose.Welcome : wasm.ReachPurpose.Delegation;
}

interface Classified {
  readonly entry: HeldEntry;
  readonly cls: number;
  readonly version: number;
}

/**
 * The address a kept grant's binding names, verified against one of
 * `personKeys`, with its class and version as the wire carries them; `null`
 * for a grant that does not decode or whose binding verifies against none.
 */
function classify(wasm: Wasm, entry: HeldEntry, personKeys: readonly Uint8Array[]): (Classified & { addressHex: string }) | null {
  let grant;
  try {
    grant = wasm.decodeReachGrant(base64Decode(entry.grant.wireB64));
  } catch {
    return null;
  }
  try {
    for (const key of personKeys) {
      try {
        const mailboxPk = grant.mailboxPkVerified(key);
        return {
          entry,
          cls: grant.class as number,
          version: grant.version,
          addressHex: journalHex(wasm.mailboxAddress(mailboxPk)),
        };
      } catch {
        /* not this key */
      }
    }
    return null;
  } finally {
    try {
      grant.free();
    } catch {
      /* already freed */
    }
  }
}

/** Log a refused plan input. No identifier is logged. */
function notePlanRefused(err: unknown): void {
  const code = (err as { reachError?: unknown } | null)?.reachError;
  // eslint-disable-next-line no-console
  console.warn(`G-53: a reach write plan was refused (${typeof code === "string" ? code : "unknown"})`);
}

/**
 * Plan the writes to one person (the core's rule, one call per mailbox).
 *
 * @param keysHex the person's keys, current first (`personKeysHex`).
 * @param personKeys the same keys as bytes, the ones a binding is verified
 *   against (`personKeysOf` in `mailbox-write.ts`).
 * @param ctx `targetScopeHex`: the group the write is for; `sharedScopeHexes`:
 *   the conversations this account and the person are both current members
 *   of. Never throws.
 */
export async function planPersonReach(
  store: ReceivedGrants,
  keysHex: readonly string[],
  personKeys: readonly Uint8Array[],
  purpose: ReachWritePurpose,
  ctx: { readonly targetScopeHex?: string; readonly sharedScopeHexes: readonly string[] },
): Promise<PersonReachPlan> {
  const entries = heldEntriesForPerson(store, keysHex);
  if (entries.length === 0) return { mailboxes: [], candidates: [], superseded: [] };
  const wasm = await loadWasm();
  const byMailbox = new Map<string, Classified[]>();
  for (const entry of entries) {
    const c = classify(wasm, entry, personKeys);
    if (c === null) continue;
    const list = byMailbox.get(c.addressHex) ?? [];
    list.push(c);
    byMailbox.set(c.addressHex, list);
  }
  const mailboxes: MailboxReachPlan[] = [];
  const candidates: HeldEntry[] = [];
  const superseded: HeldEntry[] = [];
  const offered = new Set<string>();
  for (const [addressHex, items] of byMailbox) {
    const found = items.findIndex((it) => it.entry.slot.kind === "found");
    const held = items.map((it, i) => new wasm.HeldReach(it.cls, scopeBytesOf(it.entry.slot), it.version, i));
    let plan;
    try {
      plan = wasm.planReachWrites(
        held,
        purposeOf(wasm, purpose),
        ctx.targetScopeHex === undefined ? undefined : scopeBytes(ctx.targetScopeHex),
        ctx.sharedScopeHexes.map(scopeBytes),
        found < 0 ? undefined : found,
      );
    } catch (err) {
      notePlanRefused(err);
      continue;
    }
    try {
      const mine: HeldEntry[] = [];
      for (const i of plan.candidates) {
        const entry = items[i]?.entry;
        if (entry === undefined) continue;
        mine.push(entry);
        if (offered.has(entry.grant.wireB64)) continue;
        offered.add(entry.grant.wireB64);
        candidates.push(entry);
      }
      for (const i of plan.superseded) {
        const entry = items[i]?.entry;
        if (entry !== undefined) superseded.push(entry);
      }
      mailboxes.push({ addressHex, candidates: mine });
    } finally {
      plan.free();
    }
  }
  return { mailboxes, candidates, superseded };
}

/**
 * Delete `entries` from the store: each one only where it is filed, and only
 * while that place still holds the same wire (a grant that arrived since is
 * left alone). Pure; returns `store` itself when nothing changed.
 */
export function dropSupersededEntries(store: ReceivedGrants, entries: readonly HeldEntry[]): ReceivedGrants {
  let people = store.people;
  let changed = false;
  for (const e of entries) {
    const person = people[e.personHex];
    if (person === undefined) continue;
    const next = withoutEntry(person, e);
    if (next === person) continue;
    people = { ...people, [e.personHex]: next };
    changed = true;
  }
  return changed ? { ...store, people } : store;
}

function withoutEntry(person: PersonGrants, e: HeldEntry): PersonGrants {
  const wire = e.grant.wireB64;
  if (!e.previous) {
    if (e.slot.kind === "contact") {
      if (person.contact?.wireB64 !== wire) return person;
      const { contact: _gone, ...rest } = person;
      return rest;
    }
    if (e.slot.kind === "found") {
      if (person.found?.wireB64 !== wire) return person;
      const { found: _gone, ...rest } = person;
      return rest;
    }
    const { scopeHex, cls } = e.slot;
    const scope = person.scopes[scopeHex];
    if (scope?.[cls]?.wireB64 !== wire) return person;
    const kept = { ...scope };
    delete kept[cls];
    const scopes = { ...person.scopes };
    if (Object.keys(kept).length === 0) delete scopes[scopeHex];
    else scopes[scopeHex] = kept;
    return { ...person, scopes };
  }
  const previous = person.previous;
  if (previous === undefined) return person;
  if (e.slot.kind === "contact") {
    if (previous.contact?.wireB64 !== wire) return person;
    const { contact: _gone, ...restPrev } = previous;
    return withPreviousOrNone(person, restPrev);
  }
  if (e.slot.kind !== "scoped") return person;
  const { scopeHex, cls } = e.slot;
  const scope = previous.scopes?.[scopeHex];
  if (scope?.[cls]?.wireB64 !== wire) return person;
  const kept = { ...scope };
  delete kept[cls];
  const scopes = { ...(previous.scopes ?? {}) };
  if (Object.keys(kept).length === 0) delete scopes[scopeHex];
  else scopes[scopeHex] = kept;
  const nextPrev: { contact?: PreviousGrant; scopes?: typeof scopes } = {};
  if (previous.contact !== undefined) nextPrev.contact = previous.contact;
  if (Object.keys(scopes).length > 0) nextPrev.scopes = scopes;
  return withPreviousOrNone(person, nextPrev);
}

function withPreviousOrNone(person: PersonGrants, previous: NonNullable<PersonGrants["previous"]>): PersonGrants {
  const { previous: _old, ...rest } = person;
  const empty = previous.contact === undefined && (previous.scopes === undefined || Object.keys(previous.scopes).length === 0);
  return empty ? rest : { ...rest, previous };
}

// ----------------------------------------------------------------------------
// The shared scopes, from the live rosters
// ----------------------------------------------------------------------------

/** The conversation ids (hex) this account and the person `keysHex` names
 *  are both current members of. Installed by the runtime that holds the
 *  registry. */
export type SharedScopes = (keysHex: readonly string[]) => readonly string[];

let installedSharedScopes: SharedScopes | null = null;

/** Install (or, with `null`, remove) the shared-scope reader. */
export function installSharedScopes(reader: SharedScopes | null): void {
  installedSharedScopes = reader;
}

/** The shared scopes of the person `keysHex` names; none when no reader is
 *  installed or it fails. */
export function sharedScopesOf(keysHex: readonly string[]): string[] {
  try {
    return [...(installedSharedScopes?.(keysHex) ?? [])];
  } catch {
    return [];
  }
}

// ----------------------------------------------------------------------------
// Acting on a plan
// ----------------------------------------------------------------------------

/** Delete a plan's superseded grants from the session's store. Never throws. */
export async function forgetSuperseded(superseded: readonly HeldEntry[]): Promise<void> {
  if (superseded.length === 0) return;
  const session = reachSession();
  if (session === null) return;
  try {
    await session.updateReceivedGrants((g) => dropSupersededEntries(g, superseded));
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("G-53: superseded reach grants not deleted", err instanceof Error ? err.name : "");
  }
}

/**
 * A person left the conversation `scopeHex` (a roster shrink): for every
 * person this device keeps a grant from that conversation, delete the grants
 * a higher version of the same key replaced (the core decides; a grant at
 * its key's highest version stays, wherever it was filed). Never throws.
 */
export async function pruneGrantsOfScope(scopeHex: string): Promise<void> {
  const session = reachSession();
  if (session === null) return;
  try {
    const store = session.receivedGrants();
    const seen = new Set<string>();
    const superseded: HeldEntry[] = [];
    for (const [personHex, person] of Object.entries(store.people)) {
      if (!(scopeHex in person.scopes) && !(scopeHex in (person.previous?.scopes ?? {}))) continue;
      const keysHex = personKeysHex(personHex);
      const head = keysHex[0] ?? personHex;
      if (seen.has(head)) continue;
      seen.add(head);
      const keys = keysHex.map(hexBytes);
      const plan = await planPersonReach(store, keysHex, keys, "welcome", {
        sharedScopeHexes: sharedScopesOf(keysHex),
      });
      superseded.push(...plan.superseded);
    }
    await forgetSuperseded(superseded);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("G-53: reach grants of a shrunk conversation not pruned", err instanceof Error ? err.name : "");
  }
}

/**
 * Whether the unsigned write to the person's open mailbox may be tried now:
 * the record is open and every signed candidate tried was refused (`401`),
 * which includes trying none (the core's rule).
 */
export async function openMailboxWriteAllowed(recordOpen: boolean, candidates: number, refused: number): Promise<boolean> {
  const wasm = await loadWasm();
  return wasm.reachOpenMailboxWriteAllowed(recordOpen, candidates, refused);
}
