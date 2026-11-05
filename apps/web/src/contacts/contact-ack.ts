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
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import type { IncomingContactAck } from "./client";
import { foundNameOf, sentRequestsAnsweredBy, type FoundName } from "./contact-list-model";
import { loadAccepted, upsertAccepted, type AcceptedContact } from "./local-store";
import { forgetSentIntro } from "./sent-request-intros";
import { forgetSentRequest, loadSentRequests, recordAckOnSentRequest } from "./sent-requests";

/** The session the ack is being applied in. */
export interface ContactAckSession {
  /** Whether the session the row was polled for is still the live one. The
   *  profile store is the live session's. */
  current(): boolean;
  /** `ProfileApi.fileFoundName`. */
  fileFoundName(signatureKeyB64: string, found: FoundName): void;
}

/** The name the person with this key was found under, from the Sent list. */
export function foundNameForKey(accountIdB64: string, signatureKeyB64: string): FoundName | null {
  const record = loadSentRequests(accountIdB64).find((r) => r.recipientKeyB64 === signatureKeyB64);
  return record === undefined ? null : foundNameOf(record, base64Decode(signatureKeyB64));
}

/**
 * Apply one ack. The signature is verified, so even a stale ack is a
 * legitimate accept by the holder of `accepterPubkey`. The contact and the
 * sent record are durable and keyed by the captured account: recorded even
 * when the session has ended since.
 */
export function applyContactAck(
  ack: Pick<IncomingContactAck, "accepterPubkey" | "accepterAccountId" | "accepterX25519Pubkey">,
  accountIdB64: string,
  session: ContactAckSession,
): void {
  const keyB64 = base64Encode(ack.accepterPubkey);
  const accountIdHex = groupIdHex(ack.accepterAccountId);
  const found = foundNameOf(
    loadSentRequests(accountIdB64).find((r) => r.recipientAccountIdHex === accountIdHex),
    ack.accepterPubkey,
  );
  if (found !== null && session.current()) session.fileFoundName(keyB64, found);
  recordAckOnSentRequest(accountIdB64, accountIdHex, keyB64, base64Encode(ack.accepterX25519Pubkey));
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
