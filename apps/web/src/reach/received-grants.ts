// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - the reach grants other people handed this account, filed under
 * the sender's account key resolved at receipt (C-68), plus the committer
 * delegations members posted for adds they proposed.
 *
 * Pure: the store is an immutable value, persisted encrypted by
 * `reach-store.ts` (the `grants` row). A grant is kept as its 37-byte wire in
 * base64 (class, version, seed); decoding it back to a wasm `ReachGrant` is
 * the service's job.
 */
import type { ReachClassName } from "./reach-model";

/** One kept grant. */
export interface HeldGrant {
  /** Standard base64 of the 37-byte grant wire. Secret. */
  readonly wireB64: string;
  readonly version: number;
  readonly receivedAtMs: number;
}

/** The scoped classes a profile or a member card can carry. */
export type ScopedGrantClass = "group" | "member" | "channel";

/**
 * A grant that a newer one of the same class replaced after a ROTATION of
 * the sender's account keys (ADR-0066 section 3.5a rule 6), kept beside it.
 *
 * The re-sent profile can reach this device before it has observed the
 * proven key change. It then holds the NEW grant and still addresses the
 * OLD mailbox, which only the old grant opens. So the old one is kept, with
 * the address it was good for, until the address is known to have moved.
 */
export interface PreviousGrant extends HeldGrant {
  /** The account key hex this device addressed the sender by when the
   *  newer grant arrived. The grant is offered only while the sender still
   *  resolves to this key. */
  readonly addressHex: string;
}

/** Everything kept about one person. */
export interface PersonGrants {
  readonly contact?: HeldGrant;
  /** Scope id hex -> the scoped grants received in that scope. */
  readonly scopes: Readonly<Record<string, Partial<Record<ScopedGrantClass, HeldGrant>>>>;
  /** The grants of before a rotation, per class ({@link PreviousGrant}). */
  readonly previous?: {
    readonly contact?: PreviousGrant;
    readonly scopes?: Readonly<Record<string, Partial<Record<ScopedGrantClass, PreviousGrant>>>>;
  };
}

/** A committer-delegation envelope kept for a pending add. */
export interface HeldDelegations {
  /** Standard base64 of the committer-delegation envelope plaintext. */
  readonly envelopeB64: string;
  readonly receivedAtMs: number;
}

export interface ReceivedGrants {
  /** Account key hex -> grants. */
  readonly people: Readonly<Record<string, PersonGrants>>;
  /** `${senderAccountKeyHex}|${addedAccountKeyHex}` -> the newest envelope. */
  readonly delegations: Readonly<Record<string, HeldDelegations>>;
}

export const EMPTY_RECEIVED_GRANTS: ReceivedGrants = { people: {}, delegations: {} };

/** What an incoming grant does to the stored one of its class. */
export type GrantFiling =
  /** Nothing: a re-send of what is held, a stale replay, or the grant of
   *  before a rotation coming back. */
  | "ignore"
  /** A higher version: the sender re-keyed. The stored one is dead. */
  | "replace"
  /** The same version under another key: the sender's ACCOUNT keys were
   *  rotated (every reach key derives from the account seed, and the version
   *  counters survive a rotation). The incoming grant opens the new mailbox;
   *  the stored one is kept beside it for the mailbox that was left. */
  | "replace-keep-previous";

/** The filing decision. Pure. */
export function grantFiling(
  incoming: HeldGrant,
  stored: HeldGrant | undefined,
  previous: PreviousGrant | undefined,
): GrantFiling {
  if (stored === undefined || incoming.version > stored.version) return "replace";
  if (incoming.version < stored.version) return "ignore";
  if (incoming.wireB64 === stored.wireB64) return "ignore";
  // The grant this one already replaced, replayed (an old profile row read
  // again): it must not put the old key back in front.
  if (previous !== undefined && previous.wireB64 === incoming.wireB64) return "ignore";
  return "replace-keep-previous";
}

function previousOf(stored: HeldGrant, addressHex: string): PreviousGrant {
  return {
    wireB64: stored.wireB64,
    version: stored.version,
    receivedAtMs: stored.receivedAtMs,
    addressHex,
  };
}

/**
 * File a `contact` grant from `senderHex`.
 *
 * @param addressHex the account key this device addresses the sender by NOW
 *   (the sender's key resolved through the proven-predecessor table); the
 *   sender's own key when omitted. Recorded with a grant kept as the
 *   previous one.
 */
export function fileContactGrant(
  store: ReceivedGrants,
  senderHex: string,
  grant: HeldGrant,
  addressHex: string = senderHex,
): ReceivedGrants {
  const person = store.people[senderHex] ?? { scopes: {} };
  const filing = grantFiling(grant, person.contact, person.previous?.contact);
  if (filing === "ignore") return store;
  const previous = { ...(person.previous ?? {}) };
  if (filing === "replace-keep-previous" && person.contact !== undefined) {
    previous.contact = previousOf(person.contact, addressHex);
  } else {
    delete previous.contact;
  }
  return {
    ...store,
    people: { ...store.people, [senderHex]: withPrevious({ ...person, contact: grant }, previous) },
  };
}

function withPrevious(person: PersonGrants, previous: NonNullable<PersonGrants["previous"]>): PersonGrants {
  const scopes: Record<string, Partial<Record<ScopedGrantClass, PreviousGrant>>> = {};
  for (const [scopeHex, held] of Object.entries(previous.scopes ?? {})) {
    if (Object.keys(held).length > 0) scopes[scopeHex] = held;
  }
  const next: { contact?: PreviousGrant; scopes?: typeof scopes } = {};
  if (previous.contact !== undefined) next.contact = previous.contact;
  if (Object.keys(scopes).length > 0) next.scopes = scopes;
  const { previous: _dropped, ...rest } = person;
  return Object.keys(next).length === 0 ? rest : { ...rest, previous: next };
}

function withoutPreviousScoped(
  person: PersonGrants,
  scopeHex: string,
  classes: readonly ScopedGrantClass[] | null,
): PersonGrants {
  const held = person.previous?.scopes?.[scopeHex];
  if (held === undefined) return person;
  const kept: Partial<Record<ScopedGrantClass, PreviousGrant>> = {};
  if (classes !== null) {
    for (const [cls, g] of Object.entries(held) as [ScopedGrantClass, PreviousGrant][]) {
      if (!classes.includes(cls)) kept[cls] = g;
    }
  }
  return withPrevious(person, {
    ...(person.previous ?? {}),
    scopes: { ...(person.previous?.scopes ?? {}), [scopeHex]: kept },
  });
}

/** File a scoped grant (`group`, `member`, `channel`) from `senderHex`
 *  received in `scopeHex`. */
export function fileScopedGrant(
  store: ReceivedGrants,
  senderHex: string,
  scopeHex: string,
  cls: ScopedGrantClass,
  grant: HeldGrant,
  addressHex: string = senderHex,
): ReceivedGrants {
  const person = store.people[senderHex] ?? { scopes: {} };
  const scope = person.scopes[scopeHex] ?? {};
  const stored = scope[cls];
  const filing = grantFiling(grant, stored, person.previous?.scopes?.[scopeHex]?.[cls]);
  if (filing === "ignore") return store;
  const nextScope = { ...scope, [cls]: grant };
  const previousScope = { ...(person.previous?.scopes?.[scopeHex] ?? {}) };
  if (filing === "replace-keep-previous" && stored !== undefined) {
    previousScope[cls] = previousOf(stored, addressHex);
  } else {
    delete previousScope[cls];
  }
  const next = withPrevious(
    { ...person, scopes: { ...person.scopes, [scopeHex]: nextScope } },
    {
      ...(person.previous ?? {}),
      scopes: { ...(person.previous?.scopes ?? {}), [scopeHex]: previousScope },
    },
  );
  return { ...store, people: { ...store.people, [senderHex]: next } };
}

/**
 * A profile that arrived WITHOUT a scoped grant it used to carry withdraws
 * it: the sender narrowed their setting (UX section 8). `present` lists the
 * scoped classes the new profile does carry.
 */
export function withdrawMissingScopedGrants(
  store: ReceivedGrants,
  senderHex: string,
  scopeHex: string,
  present: ReadonlySet<ScopedGrantClass>,
): ReceivedGrants {
  const person = store.people[senderHex];
  const scope = person?.scopes[scopeHex];
  if (person === undefined || scope === undefined) return store;
  const kept: Partial<Record<ScopedGrantClass, HeldGrant>> = {};
  let changed = false;
  for (const [cls, g] of Object.entries(scope) as [ScopedGrantClass, HeldGrant][]) {
    if (present.has(cls)) kept[cls] = g;
    else changed = true;
  }
  if (!changed) return store;
  // A withdrawn class takes its kept previous grant with it.
  const withdrawn = (Object.keys(scope) as ScopedGrantClass[]).filter((cls) => !present.has(cls));
  const next = withoutPreviousScoped(
    { ...person, scopes: { ...person.scopes, [scopeHex]: kept } },
    scopeHex,
    withdrawn,
  );
  return { ...store, people: { ...store.people, [senderHex]: next } };
}

/** Forget every grant received in a scope (this account left the group). */
export function dropScope(store: ReceivedGrants, scopeHex: string): ReceivedGrants {
  let changed = false;
  const people: Record<string, PersonGrants> = {};
  for (const [k, p] of Object.entries(store.people)) {
    if (scopeHex in p.scopes) {
      const scopes = { ...p.scopes };
      delete scopes[scopeHex];
      people[k] = withoutPreviousScoped({ ...p, scopes }, scopeHex, null);
      changed = true;
    } else {
      people[k] = p;
    }
  }
  return changed ? { ...store, people } : store;
}

/** Forget the contact grant of a removed contact. */
export function dropContactGrant(store: ReceivedGrants, senderHex: string): ReceivedGrants {
  const person = store.people[senderHex];
  if (person?.contact === undefined) return store;
  const { contact: _dropped, ...rest } = person;
  const previous = { ...(rest.previous ?? {}) };
  delete previous.contact;
  return { ...store, people: { ...store.people, [senderHex]: withPrevious(rest, previous) } };
}

/** Which classes this account holds for `accountHex` (in `scopeHex` when
 *  given; any scope otherwise). */
export function heldClasses(
  store: ReceivedGrants,
  accountHex: string,
  scopeHex?: string,
): Set<ReachClassName> {
  const out = new Set<ReachClassName>();
  const person = store.people[accountHex];
  if (person === undefined) return out;
  if (person.contact !== undefined) out.add("contact");
  const scopes = scopeHex === undefined ? Object.values(person.scopes) : [person.scopes[scopeHex] ?? {}];
  for (const s of scopes) {
    for (const cls of Object.keys(s) as ScopedGrantClass[]) out.add(cls);
  }
  return out;
}

/**
 * The grant that lets this account write to `accountHex`, preferring the
 * widest: a contact grant; then a `group` / `channel` grant (the person lets
 * these members find them); then, only when `allowMember`, a `member` grant
 * of `scopeHex` (a Welcome for that group - the one write a hidden person's
 * member key is for). `null` when nothing kept authorises the write.
 */
export function bestGrantFor(
  store: ReceivedGrants,
  accountHex: string,
  options: { readonly scopeHex?: string; readonly allowMember?: boolean } = {},
): HeldGrant | null {
  return grantsInOrder(store, accountHex, options)[0] ?? null;
}

/**
 * Every kept grant that may authorise a write to `accountHex`, in the order of
 * {@link bestGrantFor} (which is its first element): the contact grant, then
 * each scope's `group` grant (the given scope first), then each scope's
 * `channel` grant, then - only when `allowMember` - the given scope's
 * `member` grant. A writer refused (`401`) under one moves to the next
 * (ADR-0050 section 2.5a item 7): a key the person rotated away answers 401,
 * another key they handed out may still work.
 */
export function grantsInOrder(
  store: ReceivedGrants,
  accountHex: string,
  options: { readonly scopeHex?: string; readonly allowMember?: boolean } = {},
): HeldGrant[] {
  const person = store.people[accountHex];
  if (person === undefined) return [];
  const out: HeldGrant[] = [];
  if (person.contact !== undefined) out.push(person.contact);
  const scopeIds = Object.keys(person.scopes);
  const ordered =
    options.scopeHex !== undefined
      ? [options.scopeHex, ...scopeIds.filter((s) => s !== options.scopeHex)]
      : scopeIds;
  for (const s of ordered) {
    const scope = person.scopes[s];
    if (scope?.group !== undefined) out.push(scope.group);
  }
  for (const s of ordered) {
    const scope = person.scopes[s];
    if (scope?.channel !== undefined) out.push(scope.channel);
  }
  if (options.allowMember === true && options.scopeHex !== undefined) {
    const member = person.scopes[options.scopeHex]?.member;
    if (member !== undefined) out.push(member);
  }
  return out;
}

/**
 * ADR-0066 section 3.5a rule 2 - the grants held for one PERSON, over every
 * key they have had (`keysHex`: the current key first, then the proven
 * predecessors, as `personKeysHex` gives them). A grant filed under a key
 * that later became a proven predecessor is still found; the grants filed
 * under the current key come first, since those are the ones issued for the
 * mailbox being addressed.
 */
export function grantsInOrderForPerson(
  store: ReceivedGrants,
  keysHex: readonly string[],
  options: { readonly scopeHex?: string; readonly allowMember?: boolean } = {},
): HeldGrant[] {
  const out: HeldGrant[] = [];
  const seen = new Set<string>();
  for (const hex of keysHex) {
    for (const g of grantsInOrder(store, hex, options)) {
      if (seen.has(g.wireB64)) continue;
      seen.add(g.wireB64);
      out.push(g);
    }
  }
  // Rule 6: the grants of before a rotation come LAST, and only while the
  // person is still addressed by the key they were good for. Once the proven
  // change is observed the address moves (`keysHex[0]` changes) and they are
  // no longer offered.
  const addressHex = keysHex[0];
  if (addressHex !== undefined) {
    for (const hex of keysHex) {
      for (const g of previousGrantsInOrder(store, hex, addressHex, options)) {
        if (seen.has(g.wireB64)) continue;
        seen.add(g.wireB64);
        out.push(g);
      }
    }
  }
  return out;
}

/** The kept previous grants filed under `accountHex` that are still good for
 *  `addressHex`, in the class order of {@link grantsInOrder}. */
export function previousGrantsInOrder(
  store: ReceivedGrants,
  accountHex: string,
  addressHex: string,
  options: { readonly scopeHex?: string; readonly allowMember?: boolean } = {},
): HeldGrant[] {
  const previous = store.people[accountHex]?.previous;
  if (previous === undefined) return [];
  const out: HeldGrant[] = [];
  const live = (g: PreviousGrant | undefined): g is PreviousGrant =>
    g !== undefined && g.addressHex === addressHex;
  if (live(previous.contact)) out.push(previous.contact);
  const scopes = previous.scopes ?? {};
  const scopeIds = Object.keys(scopes);
  const ordered =
    options.scopeHex !== undefined
      ? [options.scopeHex, ...scopeIds.filter((s) => s !== options.scopeHex)]
      : scopeIds;
  for (const s of ordered) {
    const g = scopes[s]?.group;
    if (live(g)) out.push(g);
  }
  for (const s of ordered) {
    const g = scopes[s]?.channel;
    if (live(g)) out.push(g);
  }
  if (options.allowMember === true && options.scopeHex !== undefined) {
    const g = scopes[options.scopeHex]?.member;
    if (live(g)) out.push(g);
  }
  return out;
}

/** The best grant held for a person ({@link grantsInOrderForPerson}). */
export function bestGrantForPerson(
  store: ReceivedGrants,
  keysHex: readonly string[],
  options: { readonly scopeHex?: string; readonly allowMember?: boolean } = {},
): HeldGrant | null {
  return grantsInOrderForPerson(store, keysHex, options)[0] ?? null;
}

/** {@link heldClasses} over every key of one person. */
export function heldClassesForPerson(
  store: ReceivedGrants,
  keysHex: readonly string[],
  scopeHex?: string,
): Set<ReachClassName> {
  const out = new Set<ReachClassName>();
  for (const hex of keysHex) for (const cls of heldClasses(store, hex, scopeHex)) out.add(cls);
  return out;
}

/** Every kept committer-delegation envelope for adds of one person, under
 *  any key they have had, newest first. */
export function delegationsForPerson(
  store: ReceivedGrants,
  keysHex: readonly string[],
): HeldDelegations[] {
  const out: HeldDelegations[] = [];
  const seen = new Set<string>();
  for (const hex of keysHex) {
    for (const d of delegationsFor(store, hex)) {
      if (seen.has(d.envelopeB64)) continue;
      seen.add(d.envelopeB64);
      out.push(d);
    }
  }
  return out.sort((a, b) => b.receivedAtMs - a.receivedAtMs);
}

/** Whether this account can find (and so offer "Add contact" for) a person:
 *  it holds a grant other than a bare `member` key. UX section 6.2. */
export function canFind(store: ReceivedGrants, accountHex: string): boolean {
  return bestGrantFor(store, accountHex) !== null;
}

/** Keep the newest committer-delegation envelope per (sender, added). */
export function fileDelegations(
  store: ReceivedGrants,
  senderHex: string,
  addedHex: string,
  held: HeldDelegations,
): ReceivedGrants {
  const key = `${senderHex}|${addedHex}`;
  const prev = store.delegations[key];
  if (prev !== undefined && prev.receivedAtMs > held.receivedAtMs) return store;
  return { ...store, delegations: { ...store.delegations, [key]: held } };
}

/** Every kept envelope for adds of `addedHex`, newest first. */
export function delegationsFor(store: ReceivedGrants, addedHex: string): HeldDelegations[] {
  return Object.entries(store.delegations)
    .filter(([k]) => k.endsWith(`|${addedHex}`))
    .map(([, v]) => v)
    .sort((a, b) => b.receivedAtMs - a.receivedAtMs);
}

/** Drop delegation envelopes older than `maxAgeMs` (a delegation lives at
 *  most 24 hours). */
export function pruneDelegations(
  store: ReceivedGrants,
  nowMs: number,
  maxAgeMs: number,
): ReceivedGrants {
  const kept: Record<string, HeldDelegations> = {};
  let changed = false;
  for (const [k, v] of Object.entries(store.delegations)) {
    if (nowMs - v.receivedAtMs <= maxAgeMs) kept[k] = v;
    else changed = true;
  }
  return changed ? { ...store, delegations: kept } : store;
}

export function encodeReceivedGrants(store: ReceivedGrants): string {
  return JSON.stringify({ v: 1, ...store });
}

export function decodeReceivedGrants(json: string | null | undefined): ReceivedGrants {
  if (json === null || json === undefined) return EMPTY_RECEIVED_GRANTS;
  try {
    const raw = JSON.parse(json) as Partial<ReceivedGrants> & { v?: number };
    if (raw.v !== 1) return EMPTY_RECEIVED_GRANTS;
    return {
      people: typeof raw.people === "object" && raw.people !== null ? raw.people : {},
      delegations:
        typeof raw.delegations === "object" && raw.delegations !== null ? raw.delegations : {},
    };
  } catch {
    return EMPTY_RECEIVED_GRANTS;
  }
}
