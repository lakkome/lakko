// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 sections 4 and 5 - the pure decisions behind what this account
 * publishes for others to find it by:
 *
 * - the **Everyone** publication at discovery (the username's `public` flag,
 *   the lookup key of each name search term, and the public record sealed
 *   under the terms' OPRF outputs), sent on entering Everyone and whenever the
 *   name, the username or its certificate changed, withdrawn on leaving;
 * - the **introduction record** (contacts of contacts), rebuilt when 2.1 is
 *   on and the `fof` key version, the name or the username changed, and
 *   offered to every contact the setting allows who has not been sent this
 *   version yet;
 * - the **bundle** this account relays to its contacts: the records its
 *   contacts offered, re-sent when that set changed;
 * - the **ask**: an offer and a bundle go out once per conversation, so a
 *   device that missed them holds nothing and nothing would ever come. It
 *   sends the profile request (ADR-0048) into that one-to-one, and the other
 *   person's device, seeing it, forgets the conversation in its sent records,
 *   so its offer and bundle go out again.
 *
 * Every OPRF evaluation costs an anonymous token, so the published state is
 * remembered (the encrypted `publication` row) and nothing is re-sent unless
 * an input moved.
 */
import type { ProfileRequestRule } from "../profile/profile-request";

/** What this device last published, persisted per account. */
export interface PublicationState {
  /** The Everyone publication this device last sent (`key` = `everyoneKey` of
   *  the inputs it was built from; `settingMs` = the stamp of the "who can find
   *  me" setting it was sent for, as a decimal string): `null` = this device
   *  never sent one. */
  readonly everyone: {
    readonly public: boolean;
    readonly key: string | null;
    readonly settingMs?: string | null;
  } | null;
  /** The OPRF OUTPUT of each term already evaluated (term -> base64). Never
   *  sent anywhere: the lookup key discovery gets is derived from it with
   *  `discoveryLookupKey`, and records are sealed under it (ADR-0060 §5). */
  readonly termOutputs: Readonly<Record<string, string>>;
  /** The current introduction record and the inputs it was built from. */
  readonly offer: { readonly key: string; readonly recordB64: string } | null;
  /** Conversation (group id base64) -> the offer key last sent there. */
  readonly offerSentTo: Readonly<Record<string, string>>;
  /** Conversation -> the bundle digest last relayed there. */
  readonly bundleSentTo: Readonly<Record<string, string>>;
  /** Conversation -> when an ask last made this device send again (ms). */
  readonly introductionsResentAt: Readonly<Record<string, number>>;
}

export const EMPTY_PUBLICATION: PublicationState = {
  everyone: null,
  termOutputs: {},
  offer: null,
  offerSentTo: {},
  bundleSentTo: {},
  introductionsResentAt: {},
};

/** The Everyone step: what to send to discovery, if anything. */
export type EveryoneAction =
  | { readonly kind: "none" }
  | { readonly kind: "publish" }
  | { readonly kind: "withdraw" };

/**
 * The inputs the Everyone publication (name terms + public record) is built
 * from, as one key. The account key is one of them: a finder keeps a record
 * only when it names the key the directory serves (`search/people-search.ts`),
 * so after a key rotation the record already at discovery finds nobody.
 */
export function everyoneKey(
  name: string | null,
  username: string | null,
  usernameCertificateB64: string | null,
  accountKeyHex: string,
): string {
  return JSON.stringify([name ?? "", username ?? "", usernameCertificateB64 ?? "", accountKeyHex]);
}

/** The state with its Everyone note made stale: whatever was published stands
 *  as published, and no key matches it, so the next pass sends it again. */
export function staleEveryoneNote(state: PublicationState): PublicationState {
  if (state.everyone === null || state.everyone.key === null) return state;
  return { ...state, everyone: { ...state.everyone, key: null } };
}

/**
 * What the Everyone step sends. `settingMs` is the stamp of the setting in
 * force (decimal string).
 *
 * Outside Everyone the withdraw is sent unless THIS device already withdrew
 * for this very setting. It does not matter whether this device ever
 * published: the record at discovery is the account's, and another device of
 * the account (one that is offline, lost or removed) or this device before its
 * note was lost may have published it. The setting's stamp changes on every
 * choice made on any device, so each device withdraws once per choice that
 * leaves Everyone, not on every pass.
 */
export function everyoneAction(
  held: PublicationState["everyone"],
  everyone: boolean,
  key: string,
  settingMs: string,
): EveryoneAction {
  if (everyone) {
    if (held !== null && held.public && held.key === key) return { kind: "none" };
    return { kind: "publish" };
  }
  if (held !== null && !held.public && (held.settingMs ?? null) === settingMs) return { kind: "none" };
  return { kind: "withdraw" };
}

/** The inputs an introduction record is built from, as one key. The mailbox
 *  binding (ADR-0078 phase 1, standard base64) is one of them, as on iOS: a
 *  record composed before the binding existed (payload v3, 2026-10-15) has a
 *  key without it and is rebuilt, where it used to be offered forever while
 *  no client could open it (`coc-discovery-findings.md` item 5). */
export function offerKey(
  fofVersion: number,
  name: string | null,
  username: string | null,
  mailboxBindingB64: string,
): string {
  return JSON.stringify([fofVersion, name ?? "", username ?? "", mailboxBindingB64]);
}

/** Whether the held record must be rebuilt. `false` when 2.1 is off. */
export function offerNeedsRebuild(
  held: PublicationState["offer"],
  contactsOfContacts: boolean,
  key: string,
): boolean {
  return contactsOfContacts && (held === null || held.key !== key);
}

/** A contact as the offer step reads it. */
export interface OfferContact {
  /** Their account key (hex). */
  readonly keyHex: string;
  readonly accountIdHex: string | null;
}

/**
 * Whether the record may go to the other person of a one-to-one
 * (`docs/ux/who-can-find-me.md` sections 1 and 4.1): someone the list holds,
 * under any key they have had, whom the setting allows. A person the viewer
 * removed keeps the conversation and never the record. `allows` is the core's
 * `allowsContact`, asked with `null` for a contact with no known account id.
 */
export function mayOfferTo(
  peerKeysHex: readonly string[],
  contacts: readonly OfferContact[],
  allows: (accountIdHex: string | null) => boolean,
): boolean {
  const contact = contacts.find((c) => peerKeysHex.includes(c.keyHex));
  return contact !== undefined && allows(contact.accountIdHex);
}

/**
 * The conversations the current record has not been sent to yet, and may be
 * (`allowed`). One passed over is not recorded, so a contact switched on later
 * gets the record at the next pass with no new key.
 */
export function offerTargets(
  state: PublicationState,
  contactsOfContacts: boolean,
  oneToOneGroups: readonly string[],
  allowed: (groupIdB64: string) => boolean,
): string[] {
  const offer = state.offer;
  if (!contactsOfContacts || offer === null) return [];
  return oneToOneGroups.filter((g) => state.offerSentTo[g] !== offer.key && allowed(g));
}

/** A stable digest of a set of record wires (order-independent). */
export function recordsDigest(recordsB64: readonly string[]): string {
  return JSON.stringify([...recordsB64].sort());
}

/** The digest of a relayed set with no record in it. */
export const EMPTY_RECORDS_DIGEST = recordsDigest([]);

/**
 * The conversations the relayed bundle (of `digest`) must go to. A set that
 * became empty (the last contact who offered a record was removed) goes, as a
 * bundle of padding only, to the conversations that were sent a bundle
 * before: it replaces theirs, so the removed person's record stops being
 * relayed. A conversation never sent a bundle gets no empty one.
 */
export function bundleTargets(
  state: PublicationState,
  digest: string,
  oneToOneGroups: readonly string[],
): string[] {
  if (digest === EMPTY_RECORDS_DIGEST) {
    return oneToOneGroups.filter((g) => state.bundleSentTo[g] !== undefined && state.bundleSentTo[g] !== digest);
  }
  return oneToOneGroups.filter((g) => state.bundleSentTo[g] !== digest);
}

/** Fewest milliseconds between two re-sends into one conversation that an ask
 *  caused. Chosen, not measured (ADR-0060 section 4). */
export const INTRODUCTIONS_RESEND_MIN_MS = 60 * 60 * 1000;

/** The kept introductions, as far as the ask reads them: account key hex ->
 *  what that person offered or relayed. */
export interface HeldIntroductions {
  readonly offers: Readonly<Record<string, unknown>>;
  readonly bundles: Readonly<Record<string, unknown>>;
}

/** Whether an offer or a bundle is kept from a person, under any of their
 *  keys. */
export function holdsIntroductionsFrom(held: HeldIntroductions, keysHex: readonly string[]): boolean {
  return keysHex.some((k) => k in held.offers || k in held.bundles);
}

/** The other person of a conversation, or `null` when it does not hold
 *  exactly this account and one other. */
export function oneToOnePeerHex(peopleHex: readonly string[], ownHex: string): string | null {
  const others = new Set(peopleHex.filter((k) => k !== ownHex));
  if (others.size !== 1) return null;
  const [peer] = others;
  return peer ?? null;
}

/** Fewest milliseconds between two asks into a conversation whose other
 *  person's contact grant this device holds. Chosen, not measured. */
export const INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS = 7 * 24 * 60 * 60 * 1000;

/**
 * Whether to ask for the introductions of a conversation: a one-to-one that
 * holds neither an offer nor a bundle from the other person. With no contact
 * grant held, how often is the core's profile-request rule, asked as for one
 * unnamed member, so at most once a day. A contact grant says this device
 * has heard from them, not that it received their records (every profile
 * carries the grant), so it still asks: once, then at most weekly.
 */
export function shouldAskForIntroductions(
  facts: {
    readonly oneToOne: boolean;
    readonly holdsRecords: boolean;
    readonly holdsContactGrant: boolean;
    readonly lastAskedAtMs: number | undefined;
    readonly nowMs: number;
  },
  rule: ProfileRequestRule,
): boolean {
  if (!facts.oneToOne || facts.holdsRecords) return false;
  if (facts.holdsContactGrant) {
    return (
      facts.lastAskedAtMs === undefined ||
      facts.nowMs - facts.lastAskedAtMs >= INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS
    );
  }
  const last =
    facts.lastAskedAtMs === undefined ? undefined : BigInt(Math.max(0, Math.trunc(facts.lastAskedAtMs)));
  return rule(false, 1, last, BigInt(Math.max(0, Math.trunc(facts.nowMs))));
}

/** One conversation the ask step looks at. */
export interface AskConversation {
  readonly groupIdB64: string;
  /** The other person (account key hex); `null`: not a one-to-one. */
  readonly peerHex: string | null;
}

/** Send the ask into every conversation {@link shouldAskForIntroductions}
 *  says yes for. The time is recorded only after the send went out, so a
 *  failed send is tried again at the next run. Never throws. */
export async function askForMissingIntroductions(deps: {
  readonly conversations: readonly AskConversation[];
  readonly held: HeldIntroductions;
  keysOfPerson(peerHex: string): readonly string[];
  holdsContactGrant(keysHex: readonly string[]): boolean;
  lastAskedAt(groupIdB64: string): number | undefined;
  recordAsked(groupIdB64: string, atMs: number): void;
  send(groupIdB64: string): Promise<void>;
  rule: ProfileRequestRule;
  now(): number;
}): Promise<void> {
  for (const { groupIdB64, peerHex } of deps.conversations) {
    const keys = peerHex === null ? [] : deps.keysOfPerson(peerHex);
    const ask = shouldAskForIntroductions(
      {
        oneToOne: peerHex !== null,
        holdsRecords: holdsIntroductionsFrom(deps.held, keys),
        holdsContactGrant: deps.holdsContactGrant(keys),
        lastAskedAtMs: deps.lastAskedAt(groupIdB64),
        nowMs: deps.now(),
      },
      deps.rule,
    );
    if (!ask) continue;
    try {
      await deps.send(groupIdB64);
      deps.recordAsked(groupIdB64, deps.now());
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[reach] introductions ask not sent", err instanceof Error ? err.message : "");
    }
  }
}

/** Whether a profile request makes this device send its offer and bundle
 *  again: only in a one-to-one, only from the other person, once an hour. */
export function askResetsRecords(facts: {
  readonly oneToOne: boolean;
  readonly fromOtherParticipant: boolean;
  readonly lastResetAtMs: number | undefined;
  readonly nowMs: number;
}): boolean {
  if (!facts.oneToOne || !facts.fromOtherParticipant) return false;
  return facts.lastResetAtMs === undefined || facts.nowMs - facts.lastResetAtMs >= INTRODUCTIONS_RESEND_MIN_MS;
}

/** Forget one conversation in the sent records, and when that was. */
export function forgetSentRecords(
  state: PublicationState,
  groupIdB64: string,
  nowMs: number,
): PublicationState {
  const { [groupIdB64]: _offer, ...offerSentTo } = state.offerSentTo;
  const { [groupIdB64]: _bundle, ...bundleSentTo } = state.bundleSentTo;
  return {
    ...state,
    offerSentTo,
    bundleSentTo,
    introductionsResentAt: { ...state.introductionsResentAt, [groupIdB64]: nowMs },
  };
}

/** The terms still to evaluate (not in the cache). */
export function termsToEvaluate(state: PublicationState, terms: readonly string[]): string[] {
  return [...new Set(terms)].filter((t) => !(t in state.termOutputs));
}

/** One term to evaluate: its cache key and the OPRF round (base64 output). */
export interface TermRound {
  readonly key: string;
  readonly evaluate: () => Promise<string>;
}

/** How long a term waits once after a rate limit: a long name has more terms
 *  than the server's burst, and the bucket refills about one per second.
 *  Chosen, not measured. */
export const TERM_RATE_LIMIT_WAIT_MS = 8000;

/**
 * Evaluate the terms not yet kept, keeping each output as soon as it is known.
 *
 * Each evaluation costs an anonymous token, so a failure must not lose the
 * ones already paid for: a build that failed part way resumes where it stopped.
 * Keeping them only at the end spent a token per term on every retry, and the
 * retries ran through the account's daily quota without ever finishing.
 */
export async function evaluateMissingTerms(
  state: PublicationState,
  terms: readonly TermRound[],
  deps: {
    readonly keep: (termOutputs: Readonly<Record<string, string>>) => Promise<void>;
    readonly isRateLimited: (err: unknown) => boolean;
    readonly wait: (ms: number) => Promise<void>;
  },
): Promise<void> {
  const missing = new Set(termsToEvaluate(state, terms.map((t) => t.key)));
  let kept = state.termOutputs;
  for (const term of terms) {
    if (!missing.delete(term.key)) continue;
    let output: string;
    try {
      output = await term.evaluate();
    } catch (err) {
      if (!deps.isRateLimited(err)) throw err;
      await deps.wait(TERM_RATE_LIMIT_WAIT_MS);
      output = await term.evaluate();
    }
    kept = { ...kept, [term.key]: output };
    await deps.keep(kept);
  }
}

/** One own term for a batch round: its cache key and what to blind. */
export interface BatchTerm {
  readonly key: string;
  readonly kind: "term" | "username";
  readonly value: string;
}

/**
 * Evaluate the terms not yet kept in ONE batch round (`POST
 * /discovery/oprf/batch`, one register token per 66 terms), asked once more
 * after a rate limit, and keep every output the moment the batch answers.
 * The per-term {@link evaluateMissingTerms} spent a lookup token per word and
 * ran into the server's burst on a long name.
 */
export async function evaluateMissingTermsInBatch(
  state: PublicationState,
  terms: readonly BatchTerm[],
  deps: {
    /** The base64 outputs of `missing`, in order. */
    readonly evaluateBatch: (missing: readonly BatchTerm[]) => Promise<string[]>;
    readonly keep: (termOutputs: Readonly<Record<string, string>>) => Promise<void>;
    readonly isRateLimited: (err: unknown) => boolean;
    readonly wait: (ms: number) => Promise<void>;
  },
): Promise<Readonly<Record<string, string>>> {
  const wanted = new Set(termsToEvaluate(state, terms.map((t) => t.key)));
  const missing: BatchTerm[] = [];
  for (const t of terms) {
    if (wanted.delete(t.key)) missing.push(t);
  }
  if (missing.length === 0) return state.termOutputs;
  let outputs: string[];
  try {
    outputs = await deps.evaluateBatch(missing);
  } catch (err) {
    if (!deps.isRateLimited(err)) throw err;
    await deps.wait(TERM_RATE_LIMIT_WAIT_MS);
    outputs = await deps.evaluateBatch(missing);
  }
  if (outputs.length !== missing.length) throw new Error("an OPRF batch answered fewer terms than asked");
  const kept: Record<string, string> = { ...state.termOutputs };
  missing.forEach((t, i) => {
    kept[t.key] = outputs[i] ?? "";
  });
  await deps.keep(kept);
  return kept;
}

/** Version of the persisted row. 2: `termOutputs` and the Everyone `key`
 *  (ADR-0060 section 5: discovery holds lookup keys, never OPRF outputs). A
 *  row of any other version reads as empty, so a device that published under
 *  version 1 publishes again. */
const PUBLICATION_VERSION = 2;

export function encodePublication(state: PublicationState): string {
  return JSON.stringify({ v: PUBLICATION_VERSION, ...state });
}

export function decodePublication(json: string | null): PublicationState {
  if (json === null) return EMPTY_PUBLICATION;
  try {
    const raw = JSON.parse(json) as Partial<PublicationState> & { v?: number };
    if (raw.v !== PUBLICATION_VERSION) return EMPTY_PUBLICATION;
    return {
      everyone: raw.everyone ?? null,
      termOutputs: raw.termOutputs ?? {},
      offer: raw.offer ?? null,
      offerSentTo: raw.offerSentTo ?? {},
      bundleSentTo: raw.bundleSentTo ?? {},
      introductionsResentAt: raw.introductionsResentAt ?? {},
    };
  } catch {
    return EMPTY_PUBLICATION;
  }
}
