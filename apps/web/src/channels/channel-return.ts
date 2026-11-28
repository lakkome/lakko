// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - a channel comes back to a person who reset their keys.
 *
 * ## What travels
 *
 * A channel return (`composeChannelReturnEnvelope`) in a sealed mailbox row
 * (kind `channel-return`) to the reset person's CONTINUITY mailbox. It carries
 * the channel id, the role the sender says the person had, the OLD member
 * card's account binding (the signature the person's previous account key made
 * over `channel_id ‖ old_subscriber_pk ‖ BLAKE3(profile)`), the newest epoch
 * seeds the sender holds (at most 64) and, optionally, the newest profile it
 * holds for the person. It also carries an INVITATION: for a subscriber the
 * channel's invitation CARD, `composeChannelInviteCard(ledger id, link
 * secret, sealed capability)` as `mintChannelInviteCard` makes it (one use
 * for a private channel, unlimited for a public one); for a publisher the
 * one-use PUBLISHER invitation card the owner's "Add publisher" makes
 * (`mintPublisherInviteLink`), whose request the owner's device approves
 * without asking. The reset device opens either on the device with no
 * network (`openChannelInviteCardWith`) and goes through the ordinary path:
 * Subscribe, or Accept (the publisher request, whose Welcome lands like any
 * approved request).
 *
 * An OWNER does not come back this way (rewritten 2026-11-26): it takes its
 * own channels back from its owned-channels list (`owned/`), with no
 * subscriber or publisher taking part. No device sends an owner return, the
 * core's decoder refuses role 3, and a subscriber's device sends nothing.
 *
 * ## Who sends what ({@link channelReturnsFor})
 *
 * | this device                        | the reset account was           | sends role   |
 * |------------------------------------|---------------------------------|--------------|
 * | owner or publisher of the channel  | a subscriber with a member card | subscriber   |
 * | an owner device                    | a publisher                     | publisher    |
 * | anyone                             | the owner                       | nothing      |
 *
 * Each (channel, reset account, role) is sent once per reset generation; the
 * record of what was sent is kept per account ({@link SentReturns}).
 *
 * ## What the reset device does ({@link channelReturnAction})
 *
 * It decodes the return and verifies the old card binding against its own
 * previous account key (`verifyChannelReturnCardBinding`). Only a verified
 * return is acted on without a prompt. An unverified one that carries an
 * invitation is SHOWN as the ordinary channel invitation card (Subscribe /
 * Accept by the person), never accepted silently and never dropped; one
 * without an invitation has nothing to show and is dropped. Knowing the
 * account id is not enough to subscribe someone to a channel.
 */

/** The roles, as the wire numbers them. */
export type ChannelReturnRoleName = "subscriber" | "publisher";

/** What the reset device does with one decoded return. */
export type ChannelReturnAction =
  /** Subscribe with the carried invitation card, then add the seeds. */
  | "subscribe"
  /** Accept the carried publisher invitation (the ordinary request, which
   *  the owner's device approves), then add the seeds. */
  | "accept-publisher"
  /** Already in the channel in that role: only the seeds are added. */
  | "add-seeds"
  /** Not verified, but it carries an invitation: shown as an ordinary
   *  invitation for the person to decide on. */
  | "show-invitation"
  /** Nothing to do. */
  | "drop";

/**
 * The reset device's rule over one decoded return.
 *
 * | verified | role       | invitation | already in that role | action             |
 * |----------|------------|------------|----------------------|--------------------|
 * | no       | any        | yes        | -                    | show-invitation    |
 * | no       | any        | no         | -                    | drop               |
 * | yes      | subscriber | -          | yes (reading)        | add-seeds          |
 * | yes      | subscriber | yes        | no                   | subscribe          |
 * | yes      | subscriber | no         | no                   | drop               |
 * | yes      | publisher  | -          | yes (publishing)     | add-seeds          |
 * | yes      | publisher  | yes        | no                   | accept-publisher   |
 * | yes      | publisher  | no         | no                   | add-seeds          |
 */
export function channelReturnAction(facts: {
  readonly verified: boolean;
  readonly role: ChannelReturnRoleName;
  readonly hasInvitation: boolean;
  /** Reading the channel (a subscriber return), publishing in it (a
   *  publisher return). */
  readonly alreadyInRole: boolean;
}): ChannelReturnAction {
  if (!facts.verified) return facts.hasInvitation ? "show-invitation" : "drop";
  switch (facts.role) {
    case "subscriber":
      if (facts.alreadyInRole) return "add-seeds";
      return facts.hasInvitation ? "subscribe" : "drop";
    case "publisher":
      return facts.hasInvitation && !facts.alreadyInRole ? "accept-publisher" : "add-seeds";
  }
}

/** One member card this device opened, as the peer side needs it. */
export interface ReturnCard {
  /** Standard base64 of the account identity key the card opens to. */
  readonly identityPkB64: string;
  /** Standard base64 of the 32-byte subscriber key the card was uploaded
   *  under. */
  readonly subscriberPkB64: string;
}

/** One channel this device is in, as the peer side needs it. */
export interface ReturnChannel {
  readonly idHex: string;
  /** This device's role in it. A subscriber's device sends nothing. */
  readonly ownRole: "owner" | "publisher" | "subscriber";
  /** The member cards this device holds for it. */
  readonly cards: readonly ReturnCard[];
  /** Standard base64 of the owner's account key, when this device knows it
   *  (the owner's own card never brings a return). */
  readonly ownerKeyB64: string | null;
  /** Standard base64 of the publishers' account keys (owner excluded). */
  readonly publisherKeysB64: readonly string[];
}

/** One return to send. */
export interface PlannedReturn {
  readonly idHex: string;
  readonly role: ChannelReturnRoleName;
  /** The reset account's account id (standard base64). */
  readonly accountIdB64: string;
  /** The card the return carries the binding of (`null` for a publisher
   *  return, which needs none from the card: the caller then uses the
   *  publisher's own card if it holds one, and sends nothing otherwise). */
  readonly card: ReturnCard | null;
}

/**
 * The returns this device owes, over its channels and the resets it
 * accepted (`superseded`: every retired key -> its account id). A card is
 * matched on its identity key; nothing is sent for this account's own keys.
 * Pure; the caller drops what it already sent ({@link SentReturns}).
 */
export function channelReturnsFor(
  channels: readonly ReturnChannel[],
  superseded: ReadonlyMap<string, string>,
  ownKeysB64: ReadonlySet<string>,
): PlannedReturn[] {
  const out: PlannedReturn[] = [];
  for (const ch of channels) {
    // A reset owner takes its channels back from its own list: nobody
    // sends it a return, and a subscriber's device sends none at all.
    if (ch.ownRole === "subscriber") continue;
    const publishers = new Set(ch.publisherKeysB64);
    for (const card of ch.cards) {
      if (ownKeysB64.has(card.identityPkB64) || card.identityPkB64 === ch.ownerKeyB64) continue;
      const acct = superseded.get(card.identityPkB64);
      if (acct === undefined) continue;
      if (publishers.has(card.identityPkB64)) {
        // A publisher's new devices are admitted by an owner device only.
        if (ch.ownRole === "owner") out.push({ idHex: ch.idHex, role: "publisher", accountIdB64: acct, card });
        continue;
      }
      out.push({ idHex: ch.idHex, role: "subscriber", accountIdB64: acct, card });
    }
  }
  return out;
}

/** The record of what was sent, per signed-in account: `idHex|role|acct`
 *  -> the reset generation it was sent for. */
export type SentReturns = Readonly<Record<string, number>>;

/** The key a sent return is recorded under. */
export function sentReturnKey(r: Pick<PlannedReturn, "idHex" | "role" | "accountIdB64">): string {
  return `${r.idHex}|${r.role}|${r.accountIdB64}`;
}

/** The planned returns not yet sent for the accepted generation of their
 *  account (`generationOf`: the accepted reset's generation, or `null` when
 *  the account has none any more, then nothing is sent). */
export function unsentReturns(
  planned: readonly PlannedReturn[],
  sent: SentReturns,
  generationOf: (accountIdB64: string) => number | null,
): PlannedReturn[] {
  return planned.filter((r) => {
    const gen = generationOf(r.accountIdB64);
    if (gen === null) return false;
    return (sent[sentReturnKey(r)] ?? -1) < gen;
  });
}

/** The newest seeds a return carries: at most 64, strictly ascending by
 *  epoch, each seed 32 bytes. */
export const CHANNEL_RETURN_MAX_SEEDS = 64;

/** The newest `CHANNEL_RETURN_MAX_SEEDS` of the held seeds, ascending. */
export function newestSeeds(
  seeds: readonly { readonly epoch: number; readonly seed: Uint8Array }[],
): { epochs: number[]; seeds: Uint8Array } {
  const unique = new Map<number, Uint8Array>();
  for (const s of seeds) {
    if (s.seed.length === 32 && Number.isInteger(s.epoch) && s.epoch >= 0) unique.set(s.epoch, s.seed);
  }
  const epochs = [...unique.keys()].sort((a, b) => a - b).slice(-CHANNEL_RETURN_MAX_SEEDS);
  const flat = new Uint8Array(epochs.length * 32);
  epochs.forEach((e, i) => flat.set(unique.get(e) as Uint8Array, i * 32));
  return { epochs, seeds: flat };
}

// ----------------------------------------------------------------------------
// The reset side's held returns (localStorage, per account)
// ----------------------------------------------------------------------------

interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** How many returns are held at most (the oldest goes first; chosen). */
export const MAX_HELD_RETURNS = 256;

function heldKey(accountIdB64: string): string {
  return `lakko:channel-returns:${accountIdB64}`;
}

/** The returns read from the continuity mailbox and not handled yet
 *  (standard base64 of each envelope), oldest first. */
export function readHeldReturns(s: KeyValueStorage | undefined, accountIdB64: string): string[] {
  try {
    const raw = s?.getItem(heldKey(accountIdB64));
    if (raw === null || raw === undefined) return [];
    const parsed = JSON.parse(raw) as unknown;
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** Hold one more (a duplicate is held once). Throws when storage refuses,
 *  which holds the mailbox cursor: the row is read again. */
export function holdReturn(s: KeyValueStorage | undefined, accountIdB64: string, envelopeB64: string): void {
  if (s === undefined) throw new Error("no storage for a channel return");
  const held = readHeldReturns(s, accountIdB64).filter((x) => x !== envelopeB64);
  held.push(envelopeB64);
  while (held.length > MAX_HELD_RETURNS) held.shift();
  s.setItem(heldKey(accountIdB64), JSON.stringify(held));
}

/** Drop handled returns. */
export function dropHeldReturns(s: KeyValueStorage | undefined, accountIdB64: string, handled: readonly string[]): void {
  try {
    const done = new Set(handled);
    const left = readHeldReturns(s, accountIdB64).filter((x) => !done.has(x));
    if (left.length === 0) s?.removeItem(heldKey(accountIdB64));
    else s?.setItem(heldKey(accountIdB64), JSON.stringify(left));
  } catch {
    /* handled again next pass; every action is idempotent */
  }
}

/** Wipe. */
export function clearHeldReturns(s: KeyValueStorage | undefined, accountIdB64: string): void {
  try {
    s?.removeItem(heldKey(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}
