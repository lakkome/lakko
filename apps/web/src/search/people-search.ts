// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * **Search people** - the network half (`docs/ux/who-can-find-me.md`
 * section 6.1, ADR-0060 "Client contract (S4 / S5)", Search).
 *
 * One search asks, in parallel:
 * - the **channel** namespace for the typed handle (kept from the old
 *   "Look up @name" row: the person does not know which namespace a name
 *   they were given lives in);
 * - the **username** (`/discovery/lookup`, which answers for people in
 *   Everyone only);
 * - **one name term** (`nameQueryTerm` -> `/discovery/name/lookup`), people
 *   in Everyone whose published name carries it;
 * then, with the OPRF outputs of the username and the term, the kept
 * **introductions** (contacts of contacts).
 *
 * Every OPRF round yields two values (`discovery/oprf-pair.ts`): the
 * `lookupKey` is what discovery is asked with and what a username certificate
 * binds; the `output` opens records and never leaves the device.
 *
 * Everything is filtered by the WHOLE query (the network matched one term),
 * a username is shown only when verified, and a result found through a
 * grant is shown only after the DS reach check says the grant still opens
 * the person's mailbox. A check that says no drops the result silently: the
 * searcher must not be able to tell "hidden" from "does not exist".
 *
 * **What a person in Everyone is called.** Both lookups return, beside each
 * account id, the person's PUBLIC RECORD (ADR-0060 section 5), sealed under
 * the OPRF outputs of their name terms and username. It is opened with the
 * OUTPUT of the round that found the hit (never its lookup key) and counts
 * only when it names both the hit's account id and the key the identity
 * bundle serves for that account ({@link acceptPublicRecord}); a record that
 * does not open or fails either check is ignored. A name this device already
 * holds (a contact, a profile from a shared group or channel) wins over the
 * record's. A name hit nothing names is dropped; a username match without a
 * usable record is shown under its username.
 *
 * Every dependency is an argument, so the whole decision is testable.
 */
import { base64Decode } from "../auth/account-id";
import type { OprfPair } from "../discovery/oprf-pair";
import { fingerprintLabel } from "../profile/resolver";
import type { IntroductionProbe, OpenedIntroduction } from "./introductions";
import {
  keyB64Of,
  mergePeople,
  shownUsername,
  toHex,
  type PersonReach,
  type PersonResult,
} from "./people";
import { foldSearch, legOutcomeOf, type LegOutcome, type PeopleSearchState } from "./search-state";

/** What this device knows about a person, by account key (base64). */
export interface KnownPerson {
  /** A name this device holds (rename or profile); `null` for none. */
  readonly name: string | null;
  /** For a contact nothing names: what they are shown by anyway (their
   *  verified username, else their fingerprint). A name they published wins
   *  over it; with none, the contact still shows by it. `null` for anyone
   *  who is not a contact, and for a contact with a held `name`. */
  readonly contactFallbackName: string | null;
  readonly verifiedUsername: string | null;
  readonly isContact: boolean;
  readonly keptGrantWireB64: string | null;
}

/** One lookup hit: an account in Everyone and its public record (standard
 *  bytes; `null` when discovery sent none). */
export interface LookupHit {
  readonly accountId: Uint8Array;
  readonly publicRecord: Uint8Array | null;
}

/** The fields of an opened public record the search reads. */
export interface OpenedPublicRecord {
  readonly identityPk: Uint8Array;
  readonly accountId: Uint8Array;
  readonly displayName: string | null;
}

export interface PeopleSearchDeps {
  /** The canonical username the text could be, or `null` (`looksLikeAName`
   *  + `bareName`). No username or channel question is asked without one. */
  canonicalUsername(typed: string): string | null;
  /** `nameQueryTerm`: the one term looked up, `null` when not searchable. */
  queryTerm(query: string): string | null;
  /** `nameMatchesQuery`: the post-filter over the whole query. */
  nameMatches(displayName: string, query: string): boolean;
  /** The channel namespace; `null` when not a channel. Omitted: not asked. */
  lookupChannel?(handle: string): Promise<string | null>;
  /** One OPRF round over a username (one anonymous token). */
  usernameOprf(username: string): Promise<OprfPair>;
  /** `/discovery/lookup` by the username's LOOKUP KEY: the account id and
   *  its public record, or `null`. */
  accountOfUsernameLookupKey(lookupKey: Uint8Array): Promise<LookupHit | null>;
  /** One OPRF round over a name term (one anonymous token). */
  nameTermOprf(term: string): Promise<OprfPair>;
  /** `/discovery/name/lookup` by the term's LOOKUP KEY: accounts in
   *  Everyone carrying the term, with their public records. */
  accountsOfNameTermLookupKey(lookupKey: Uint8Array): Promise<LookupHit[]>;
  /** `openPublicRecord(record, oprfOutput)`: the opened fields, or `null`
   *  when it does not open. Never throws. */
  openPublicRecord(record: Uint8Array, oprfOutput: Uint8Array): OpenedPublicRecord | null;
  /** The account key the identity bundle publishes, `null` for none. */
  identityKeyOf(accountId: Uint8Array): Promise<Uint8Array | null>;
  /** What this device knows about a person. */
  known(keyB64: string): KnownPerson;
  /** Open the kept introduction records with these probes. */
  openIntroductions(probes: readonly IntroductionProbe[]): Promise<OpenedIntroduction[]>;
  /** `verifyUsernameBinding` against the pinned / fetched key: `true` only
   *  when the certificate binds the username's `lookupKey` (never its OPRF
   *  output) to `accountId`. Never throws. */
  verifyUsername(certificate: Uint8Array, usernameLookupKey: Uint8Array, accountId: Uint8Array): Promise<boolean>;
  /** The DS reach check of the person's account mailbox with a grant. */
  reachCheck(identityPk: Uint8Array, grantWireB64: string): Promise<boolean>;
  /** Standard base64 of a grant wire. */
  encodeGrant(wire: Uint8Array): string;
  readonly ownKeyHex: string | null;
}

export interface PeopleSearchOutcome {
  readonly people: readonly PersonResult[];
  readonly channel: { readonly idHex: string; readonly handle: string } | null;
  readonly state: PeopleSearchState;
}

/** At most this many usernames of introduction results are verified per
 *  search beyond the typed one: each verification costs an OPRF evaluation,
 *  i.e. one anonymous token. The rest show their name without a username. */
export const MAX_EXTRA_USERNAME_CHECKS = 3;

/**
 * The display name a public record gives a lookup hit, or `null`. The record
 * is opened with the OPRF OUTPUT of the round that found the hit, and counts
 * only when it names the hit's account id AND the key the identity bundle
 * serves for that account: any person in Everyone can seal a record naming
 * anyone, and discovery serves whatever the account published. Never throws.
 */
export function acceptPublicRecord(
  deps: Pick<PeopleSearchDeps, "openPublicRecord">,
  hit: LookupHit,
  oprfOutput: Uint8Array,
  bundleKey: Uint8Array,
): string | null {
  if (hit.publicRecord === null) return null;
  let opened: OpenedPublicRecord | null;
  try {
    opened = deps.openPublicRecord(hit.publicRecord, oprfOutput);
  } catch {
    return null;
  }
  if (opened === null) return null;
  if (toHex(opened.accountId) !== toHex(hit.accountId)) return null;
  if (toHex(opened.identityPk) !== toHex(bundleKey)) return null;
  const name = opened.displayName?.trim() ?? "";
  return name.length === 0 ? null : name;
}

/** A result built from what is known locally, over a network finding. */
function resultFor(
  deps: PeopleSearchDeps,
  keyB64: string,
  keyHex: string,
  accountIdHex: string | null,
  foundReach: "open" | "grant",
  foundName: string | null,
  foundUsername: string | null,
  grantWireB64: string | null,
): PersonResult | null {
  const k = deps.known(keyB64);
  const reach: PersonReach = k.isContact ? "contact" : foundReach;
  // A held name, else the name found (published), else a contact's
  // fingerprint / username stand-in.
  const name = k.name ?? foundName ?? k.contactFallbackName;
  if (name === null) return null;
  const nameIsFallback = k.name === null && foundName === null;
  return {
    keyB64,
    keyHex,
    accountIdHex,
    name,
    ...(nameIsFallback ? { nameIsFallback: true } : {}),
    username: shownUsername(foundUsername ?? k.verifiedUsername, reach),
    reach,
    grantWireB64: reach === "grant" ? grantWireB64 : null,
  };
}

/** Run one search. Never throws. */
export async function searchPeople(query: string, deps: PeopleSearchDeps): Promise<PeopleSearchOutcome> {
  const legs: LegOutcome[] = [];
  const username = deps.canonicalUsername(query);
  const term = deps.queryTerm(query);
  const probes: IntroductionProbe[] = [];

  // ---- the channel namespace ------------------------------------------
  const channelLeg = (async (): Promise<{ idHex: string; handle: string } | null> => {
    if (username === null || deps.lookupChannel === undefined) return null;
    try {
      const idHex = await deps.lookupChannel(username);
      legs.push("answered");
      return idHex === null ? null : { idHex, handle: username };
    } catch (err) {
      legs.push(legOutcomeOf(err));
      return null;
    }
  })();

  // ---- the username ---------------------------------------------------
  // Written by the username question, read after all questions settled.
  const rounds: { username: OprfPair | null } = { username: null };
  const usernameLeg = (async (): Promise<PersonResult[]> => {
    if (username === null) return [];
    try {
      const round = await deps.usernameOprf(username);
      rounds.username = round;
      probes.push({ oprfOutput: round.output, via: "username" });
      const hit = await deps.accountOfUsernameLookupKey(round.lookupKey);
      if (hit === null) {
        legs.push("answered");
        return [];
      }
      const accountIdHex = toHex(hit.accountId);
      // The key comes from the identity bundle discovery serves for the
      // account, never from a profile's CLAIMED account id: any member of a
      // shared group could claim someone else's.
      const key = await identityKeyB64(deps, hit.accountId);
      legs.push("answered");
      if (key === null) return [];
      // The name the person published, when their record names this key.
      const published = acceptPublicRecord(deps, hit, round.output, key.bytes);
      // Discovery itself resolved this username to this account: that is
      // the binding a certificate would prove, so the username shows.
      const r = resultFor(
        deps,
        key.b64,
        key.hex,
        accountIdHex,
        "open",
        published ?? `@${username}`,
        username,
        null,
      );
      return r === null ? [] : [r];
    } catch (err) {
      legs.push(legOutcomeOf(err));
      return [];
    }
  })();

  // ---- one name term --------------------------------------------------
  const nameLeg = (async (): Promise<PersonResult[]> => {
    if (term === null) return [];
    try {
      const round = await deps.nameTermOprf(term);
      probes.push({ oprfOutput: round.output, via: "term" });
      const hits = await deps.accountsOfNameTermLookupKey(round.lookupKey);
      const out: PersonResult[] = [];
      for (const hit of hits) {
        const accountIdHex = toHex(hit.accountId);
        const key = await identityKeyB64(deps, hit.accountId);
        if (key === null) continue;
        // A name this device holds, else the one the person published
        // (module docs); an unnamed hit is dropped.
        const published = acceptPublicRecord(deps, hit, round.output, key.bytes);
        const name = deps.known(key.b64).name ?? published;
        if (name === null || !deps.nameMatches(name, query)) continue;
        const r = resultFor(deps, key.b64, key.hex, accountIdHex, "open", published, null, null);
        if (r !== null) out.push(r);
      }
      legs.push("answered");
      return out;
    } catch (err) {
      legs.push(legOutcomeOf(err));
      return [];
    }
  })();

  const [channel, byUsername, byName] = await Promise.all([channelLeg, usernameLeg, nameLeg]);

  // ---- contacts of contacts ------------------------------------------
  const byIntroduction: PersonResult[] = [];
  let introFailed = false;
  if (probes.length > 0) {
    let opened: OpenedIntroduction[] = [];
    try {
      opened = await deps.openIntroductions(probes);
    } catch {
      opened = [];
    }
    let extraChecks = 0;
    for (const intro of opened) {
      const matches =
        intro.via === "username"
          ? username !== null && intro.username === username
          : intro.displayName !== null && deps.nameMatches(intro.displayName, query);
      if (!matches) continue;
      // The record is the introduced person's own claim: its account id must
      // be the account the identity bundle publishes this key for, or a
      // record could pair its author's key with someone else's account id -
      // and that account's username certificate.
      let bound: boolean;
      try {
        const published = await deps.identityKeyOf(intro.accountId);
        bound = published !== null && toHex(published) === toHex(intro.identityPk);
      } catch {
        introFailed = true;
        continue;
      }
      if (!bound) continue;
      let verified: string | null = null;
      if (intro.username !== null && intro.usernameCertificate !== null) {
        let lookupKey: Uint8Array | null = null;
        if (intro.username === username && rounds.username !== null) {
          lookupKey = rounds.username.lookupKey;
        } else if (extraChecks < MAX_EXTRA_USERNAME_CHECKS) {
          extraChecks += 1;
          try {
            lookupKey = (await deps.usernameOprf(intro.username)).lookupKey;
          } catch {
            lookupKey = null;
          }
        }
        if (
          lookupKey !== null &&
          (await deps.verifyUsername(intro.usernameCertificate, lookupKey, intro.accountId))
        ) {
          verified = intro.username;
        }
      }
      const keyB64 = keyB64Of(intro.identityPk);
      const name = intro.displayName ?? (verified !== null ? `@${verified}` : fingerprintLabel(intro.identityPk));
      const r = resultFor(
        deps,
        keyB64,
        toHex(intro.identityPk),
        toHex(intro.accountId),
        "grant",
        name,
        verified,
        deps.encodeGrant(intro.fofWire),
      );
      if (r !== null) byIntroduction.push(r);
    }
  }

  // ---- one list, then the reach check --------------------------------
  const merged = mergePeople([byUsername, byName, byIntroduction], deps.ownKeyHex);
  const people: PersonResult[] = [];
  let checkFailed = false;
  for (const p of merged) {
    if (p.reach !== "grant") {
      people.push(p);
      continue;
    }
    if (p.grantWireB64 === null) continue;
    try {
      const pk = base64Decode(p.keyB64);
      if (await deps.reachCheck(pk, p.grantWireB64)) people.push(p);
    } catch {
      checkFailed = true;
    }
  }
  if (checkFailed || introFailed) legs.push("failed");
  const found = people.length + (channel === null ? 0 : 1);
  return { people, channel, state: foldSearch(found, legs) };
}

async function identityKeyB64(
  deps: PeopleSearchDeps,
  accountId: Uint8Array,
): Promise<{ b64: string; hex: string; bytes: Uint8Array } | null> {
  const key = await deps.identityKeyOf(accountId);
  if (key === null || key.length !== 32) return null;
  return { b64: keyB64Of(key), hex: toHex(key), bytes: key };
}
