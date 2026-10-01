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
 *   offered to every contact who has not been sent this version yet;
 * - the **bundle** this account relays to its contacts: the records its
 *   contacts offered, re-sent when that set changed.
 *
 * Every OPRF evaluation costs an anonymous token, so the published state is
 * remembered (the encrypted `publication` row) and nothing is re-sent unless
 * an input moved.
 */

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
}

export const EMPTY_PUBLICATION: PublicationState = {
  everyone: null,
  termOutputs: {},
  offer: null,
  offerSentTo: {},
  bundleSentTo: {},
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

/** The conversations the current record has not been sent to yet. */
export function offerTargets(
  state: PublicationState,
  contactsOfContacts: boolean,
  oneToOneGroups: readonly string[],
): string[] {
  const offer = state.offer;
  if (!contactsOfContacts || offer === null) return [];
  return oneToOneGroups.filter((g) => state.offerSentTo[g] !== offer.key);
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
    };
  } catch {
    return EMPTY_PUBLICATION;
  }
}
