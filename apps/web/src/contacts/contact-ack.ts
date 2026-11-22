// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a verified ContactAck does on the requester's device (base walk
 * 2026-11-04, the rule Android has): it makes NO contact. The request stays
 * waiting in Sent, and the ack only records on it the accepter's keys, which
 * the one-to-one's Welcome is matched by. Joining that one-to-one makes the
 * contact and retires the Sent request (`OneToOneContactAdoption.tsx`,
 * `retireAnsweredSentRequests`). So a contact always has its one-to-one: an
 * ack whose Welcome never joins leaves a waiting request, never a contact
 * that nothing can reach.
 *
 * The name they were found under stands until their own profile arrives
 * (`docs/ux/who-can-find-me.md` section 6.1); it is filed on the ack, and
 * again by the one-to-one rule, whichever comes first.
 *
 * One exception, the silent answer (base scenario 7, seen 2026-11-22): this
 * account removed a person and asked them again. On their side this account
 * is still a contact, so their device answers with the ack alone and founds
 * nothing (`known-contact-request.ts`); the one-to-one both still hold IS the
 * answer, and no Welcome follows. When the ack answers a request this device
 * sent to that key and this device holds a live one-to-one with them, the
 * person is put back with that one-to-one (the removal is forgotten: asking
 * them was a decision to have them back) and the request is retired. When
 * this device holds none (it lost its chat while theirs still stands), the
 * asker founds one, as Message on a contact card does: their device joins it
 * and the duplicate-one-to-one rule makes it the head of the conversation
 * they already had. The answering side answers silently only while it holds
 * a live one-to-one (`MailboxContext.tsx`), so only one side ever founds.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import type { IncomingContactAck } from "./client";
import { foundNameOf, sentRequestsAnsweredBy, type FoundName, type SentRequestRecord } from "./contact-list-model";
import { loadAccepted, restoreRemovedContact, upsertAccepted, type AcceptedContact } from "./local-store";
import { forgetSentIntro } from "./sent-request-intros";
import { forgetSentRequest, loadSentRequests, recordAckOnSentRequest } from "./sent-requests";

/** The session the ack is being applied in. */
export interface ContactAckSession {
  /** Whether the session the row was polled for is still the live one. The
   *  profile store is the live session's. */
  current(): boolean;
  /** `ProfileApi.fileFoundName`. */
  fileFoundName(signatureKeyB64: string, found: FoundName): void;
  /** base64(group id) of the live one-to-one this device holds with the
   *  person holding `signatureKeyB64`, or `null`. Absent: none is held. */
  liveOneToOneWith?(signatureKeyB64: string): string | null;
}

/**
 * Whether a sent record asked the person who signed this ack: the record names
 * that key, or it names the account the ack names and no key at all. A record
 * that names the account but another key is not theirs: an ack names its
 * account id itself, and only the key is verified.
 */
export function sentRecordAskedAcker(
  record: Pick<SentRequestRecord, "recipientAccountIdHex" | "recipientKeyB64">,
  ackKeyB64: string,
  ackAccountIdHex: string,
): boolean {
  if (record.recipientKeyB64 === ackKeyB64) return true;
  return record.recipientKeyB64 === undefined && record.recipientAccountIdHex === ackAccountIdHex;
}

/** The name the person with this key was found under, from the Sent list. */
export function foundNameForKey(accountIdB64: string, signatureKeyB64: string): FoundName | null {
  const record = loadSentRequests(accountIdB64).find((r) => r.recipientKeyB64 === signatureKeyB64);
  return record === undefined ? null : foundNameOf(record, base64Decode(signatureKeyB64));
}

/** What an ack left to the caller. */
export type ContactAckOutcome =
  /** Nothing more: the ack only recorded the accepter's keys (the Welcome of
   *  their one-to-one makes the contact), or it answered nothing of this
   *  device's. */
  | "none"
  /** The silent answer with the one-to-one both still hold: the person is a
   *  contact again with it and the request is retired. */
  | "restored"
  /** The silent answer while this device holds no live one-to-one with them:
   *  no Welcome will come, since their device still holds one. The caller
   *  founds a one-to-one with them (`foundOneToOneWith`) and then calls
   *  {@link adoptAnsweredOneToOne} with it; the request stays under Sent
   *  until then. */
  | "needs-one-to-one";

/**
 * Apply one ack. The signature is verified, so even a stale ack is a
 * legitimate accept by the holder of `accepterPubkey`. The contact and the
 * sent record are durable and keyed by the captured account: recorded even
 * when the session has ended since.
 *
 * See the module comment for the silent answer.
 */
export function applyContactAck(
  ack: Pick<IncomingContactAck, "accepterPubkey" | "accepterAccountId" | "accepterX25519Pubkey">,
  accountIdB64: string,
  session: ContactAckSession,
): ContactAckOutcome {
  const keyB64 = base64Encode(ack.accepterPubkey);
  const accountIdHex = groupIdHex(ack.accepterAccountId);
  const sent = loadSentRequests(accountIdB64);
  const found = foundNameOf(
    sent.find((r) => r.recipientAccountIdHex === accountIdHex),
    ack.accepterPubkey,
  );
  if (found !== null && session.current()) session.fileFoundName(keyB64, found);
  recordAckOnSentRequest(accountIdB64, accountIdHex, keyB64, base64Encode(ack.accepterX25519Pubkey));
  if (!sent.some((r) => sentRecordAskedAcker(r, keyB64, accountIdHex))) return "none";
  if (session.liveOneToOneWith === undefined) return "none";
  const live = session.liveOneToOneWith(keyB64);
  if (live === null) return "needs-one-to-one";
  return adoptAnsweredOneToOne(accountIdB64, keyB64, accountIdHex, live) ? "restored" : "none";
}

/**
 * The person who answered is a contact with this one-to-one (held, or just
 * founded for them): the removal is forgotten (asking them was a decision to
 * have them back) and the Sent request is retired. The contact first, and
 * only then the Sent record: a search row reads the two and must never find
 * them in neither. Returns whether anything changed.
 */
export function adoptAnsweredOneToOne(
  accountIdB64: string,
  keyB64: string,
  accountIdHex: string,
  groupIdB64: string,
): boolean {
  const restored = restoreRemovedContact(
    { signatureKey: keyB64, accountIdHex, groupIdB64, nowMs: Date.now() },
    accountIdB64,
  );
  const retired = retireAnsweredSentRequests(accountIdB64, keyB64, accountIdHex);
  return restored || retired > 0;
}

/**
 * The one-to-one with `peerKeyB64` is on this device: every Sent request it
 * answers goes (with its kept intro), and what the ack told about the person
 * (their X25519 key, the account id the request went to) is carried onto the
 * contact row when the row lacks it. Returns how many requests were retired.
 */
export function retireAnsweredSentRequests(
  accountIdB64: string,
  peerKeyB64: string,
  peerAccountIdHex: string | null,
): number {
  const answered = sentRequestsAnsweredBy(loadSentRequests(accountIdB64), peerKeyB64, peerAccountIdHex);
  if (answered.length === 0) return 0;
  const contact = loadAccepted(accountIdB64).find((c) => c.signaturePubkeyB64 === peerKeyB64);
  if (contact !== undefined) {
    const x25519 = contact.x25519PubkeyB64 ?? answered.find((r) => r.accepterX25519B64 !== undefined)?.accepterX25519B64;
    const accountIdHex = contact.accountIdHex ?? answered[0]?.recipientAccountIdHex;
    if (x25519 !== contact.x25519PubkeyB64 || accountIdHex !== contact.accountIdHex) {
      const next: AcceptedContact = {
        ...contact,
        ...(x25519 === undefined ? {} : { x25519PubkeyB64: x25519 }),
        ...(accountIdHex === undefined ? {} : { accountIdHex }),
      };
      upsertAccepted(next, accountIdB64);
    }
  }
  for (const r of answered) {
    forgetSentRequest(accountIdB64, r.recipientAccountIdHex);
    void forgetSentIntro(accountIdB64, r.recipientAccountIdHex);
  }
  return answered.length;
}
