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
 * Never overwrites a recorded chat, a name or an account id. Other rows are
 * returned untouched, in their order.
 */
export function adoptOneToOnePeer(
  accepted: readonly AcceptedContact[],
  peer: OneToOnePeer,
): AcceptedContact[] | null {
  const accountIdHex =
    peer.accountIdHex !== undefined && peer.accountIdHex !== null && peer.accountIdHex.length > 0
      ? peer.accountIdHex
      : undefined;
  const at = accepted.findIndex((c) => c.signaturePubkeyB64 === peer.signatureKey);
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
  const needsGroup = existing.groupIdB64 === undefined || existing.groupIdB64.length === 0;
  const needsAccount =
    accountIdHex !== undefined &&
    (existing.accountIdHex === undefined || existing.accountIdHex.length === 0);
  if (!needsGroup && !needsAccount) return null;
  const completed: AcceptedContact = {
    ...existing,
    ...(needsGroup ? { groupIdB64: peer.groupIdB64 } : {}),
    ...(needsAccount ? { accountIdHex } : {}),
  };
  return accepted.map((c, i) => (i === at ? completed : c));
}
