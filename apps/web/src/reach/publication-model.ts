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
  /** The Everyone publication in effect (`key` = `everyoneKey` of the inputs
   *  it was built from): `null` = never published. */
  readonly everyone: { readonly public: boolean; readonly key: string | null } | null;
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

/** The inputs the Everyone publication (name terms + public record) is
 *  built from, as one key. */
export function everyoneKey(
  name: string | null,
  username: string | null,
  usernameCertificateB64: string | null,
): string {
  return JSON.stringify([name ?? "", username ?? "", usernameCertificateB64 ?? ""]);
}

export function everyoneAction(
  held: PublicationState["everyone"],
  everyone: boolean,
  key: string,
): EveryoneAction {
  if (everyone) {
    if (held !== null && held.public && held.key === key) return { kind: "none" };
    return { kind: "publish" };
  }
  // Leaving Everyone (or never in it): withdraw only what was published.
  if (held !== null && held.public) return { kind: "withdraw" };
  return { kind: "none" };
}

/** The inputs an introduction record is built from, as one key. */
export function offerKey(fofVersion: number, name: string | null, username: string | null): string {
  return JSON.stringify([fofVersion, name ?? "", username ?? ""]);
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

/** The conversations the relayed bundle (of `digest`) must go to. */
export function bundleTargets(
  state: PublicationState,
  digest: string,
  oneToOneGroups: readonly string[],
): string[] {
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
