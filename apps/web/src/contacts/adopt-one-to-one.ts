// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A person this account has a one-to-one conversation with IS a contact.
 *
 * A contact row used to be written on exactly two events: the viewer
 * pressing Accept, and an incoming ContactAck. The conversation itself
 * arrives by another event (the MLS Welcome, or a second device admitted to
 * the account's conversations under ADR-0050), so any path that delivered the
 * conversation without the acknowledgement left the device holding a named
 * chat with a person and NO contact row for them: an empty Contacts list
 * next to a chat whose info sheet reports `isContact: false` (seen live
 * 2026-09-26). This module closes that gap by the rule in the first line,
 * applied to the accepted list from the live rosters.
 *
 * The rule is pure and lives here so it is one function on three clients
 * (iOS and Android carry the same shape). `AcceptedContact` has no numeric
 * id: it is keyed on `signaturePubkeyB64`, the person's account key, which is
 * also what `peerContactByMemberPubkeys` resolves a 1x1 peer on. A synthetic
 * row therefore needs no id of its own; its key IS the peer's key, so it can
 * never collide with a row Accept or an ack writes for the same person, it
 * simply is that row.
 */
import type { AcceptedContact } from "./local-store";

/** The other person in a one-to-one conversation, as the roster names them. */
export interface OneToOnePeer {
  /** The peer's account key (ADR-0050 person key), base64 — the same value
   *  an accepted row carries as `signaturePubkeyB64`. */
  readonly signatureKey: string;
  /** The peer's 16-byte account id as 32 lower-case hex characters, when a
   *  received profile (ADR-0048 v2) names it; else absent. */
  readonly accountIdHex?: string | null;
  /** base64(group_id) of the one-to-one conversation. */
  readonly groupIdB64: string;
  /** The clock, for `acceptedAtMs` on a row this call creates. */
  readonly nowMs: number;
}

/**
 * Apply the rule to one peer. Returns the new accepted list, or `null` when
 * nothing changes.
 *
 *   * No accepted row has the peer's key: a row is added, with an empty
 *     display name (the resolver names the person from their profile, their
 *     username or the key fingerprint), the conversation's group id, and the
 *     account id when known.
 *   * A row exists but lacks the conversation or the account id: the missing
 *     field is completed and everything else on the row, the name above all,
 *     is kept.
 *   * The row is complete: `null`.
 *
 * Never overwrites a name or an account id. A recorded chat is kept unless
 * `linkHolds` says it is not this person's one-to-one (gone from the device,
 * a group, a one-to-one with someone else; seen live 2026-11-05 as a row
 * linked to a group): then the peer's conversation replaces it. Other rows
 * are returned untouched, in their order.
 */
export function adoptOneToOnePeer(
  accepted: readonly AcceptedContact[],
  peer: OneToOnePeer,
  samePerson: (a: string, b: string) => boolean = (a, b) => a === b,
  linkHolds?: (groupIdB64: string, row: AcceptedContact) => boolean,
): AcceptedContact[] | null {
  const accountIdHex =
    peer.accountIdHex !== undefined && peer.accountIdHex !== null && peer.accountIdHex.length > 0
      ? peer.accountIdHex
      : undefined;
  // ADR-0074 section 3.7: a contact met after a rotation has their row under
  // the key they asked with and the conversation under the key their leaves
  // are bound to. One person, one row.
  const at = accepted.findIndex((c) => samePerson(c.signaturePubkeyB64, peer.signatureKey));
  // ADR-0073 R.5: an older one-to-one group with a contact's OLD keys is
  // still that contact's conversation. The row was re-keyed to the new key
  // at the re-pin and remembers the old one; a row for the old key would be
  // a second contact for one person.
  if (
    at === -1 &&
    accepted.some((c) => (c.previousSignaturePubkeysB64 ?? []).includes(peer.signatureKey))
  ) {
    return null;
  }
  if (at === -1) {
    const row: AcceptedContact = {
      signaturePubkeyB64: peer.signatureKey,
      displayName: "",
      acceptedAtMs: peer.nowMs,
      groupIdB64: peer.groupIdB64,
      ...(accountIdHex !== undefined ? { accountIdHex } : {}),
    };
    return [...accepted, row];
  }
  const existing = accepted[at] as AcceptedContact;
  const aliases = existing.previousSignaturePubkeysB64 ?? [];
  const needsAlias =
    existing.signaturePubkeyB64 !== peer.signatureKey && !aliases.includes(peer.signatureKey);
  const needsGroup =
    existing.groupIdB64 === undefined ||
    existing.groupIdB64.length === 0 ||
    (existing.groupIdB64 !== peer.groupIdB64 &&
      linkHolds !== undefined &&
      !linkHolds(existing.groupIdB64, existing));
  const needsAccount =
    accountIdHex !== undefined &&
    (existing.accountIdHex === undefined || existing.accountIdHex.length === 0);
  if (!needsGroup && !needsAccount && !needsAlias) return null;
  const completed: AcceptedContact = {
    ...existing,
    ...(needsAlias ? { previousSignaturePubkeysB64: [...aliases, peer.signatureKey] } : {}),
    ...(needsGroup ? { groupIdB64: peer.groupIdB64 } : {}),
    ...(needsAccount ? { accountIdHex } : {}),
  };
  return accepted.map((c, i) => (i === at ? completed : c));
}

/**
 * The peer a conversation makes a contact of, or `null`. Only a conversation
 * whose kind is one-to-one (ADR-0062) and whose two people are this account
 * and one other person counts: a GROUP that happens to hold two people is
 * not a one-to-one, and linking a contact to it made Message open the group
 * once it grew (seen live 2026-11-05).
 */
export function adoptablePeerOf(input: {
  readonly isOneToOne: boolean;
  /** The conversation's distinct people, base64 account keys. */
  readonly people: readonly string[];
  readonly ownKeyB64: string | null;
}): string | null {
  if (!input.isOneToOne || input.ownKeyB64 === null || input.people.length !== 2) return null;
  const peers = input.people.filter((k) => k !== input.ownKeyB64);
  return peers.length === 1 ? (peers[0] ?? null) : null;
}
