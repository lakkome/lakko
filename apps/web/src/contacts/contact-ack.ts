// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a verified ContactAck does on the requester's device: the accepter
 * becomes a contact and the sent record goes, and the name they were found
 * under stands until their own profile arrives (`docs/ux/who-can-find-me.md`
 * section 6.1).
 *
 * The Welcome usually lands before the ack, and the one-to-one rule then makes
 * the contact row first (`OneToOneContactAdoption.tsx`), so the name is filed
 * on either path. The sent record stands until the ack, which is why both can
 * read it.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import type { IncomingContactAck } from "./client";
import { foundNameOf, type FoundName } from "./contact-list-model";
import { loadAccepted, upsertAccepted, type AcceptedContact } from "./local-store";
import { forgetSentIntro } from "./sent-request-intros";
import { forgetSentRequest, loadSentRequests } from "./sent-requests";

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
  const accepter: AcceptedContact = {
    signaturePubkeyB64: keyB64,
    x25519PubkeyB64: base64Encode(ack.accepterX25519Pubkey),
    // ADR-0048 — the name comes from the resolver; a re-delivered ack must
    // not wipe a rename the user already gave this contact.
    displayName: loadAccepted(accountIdB64).find((c) => c.signaturePubkeyB64 === keyB64)?.displayName ?? "",
    acceptedAtMs: Date.now(),
    // The peer's account_id (hex), kept with the contact (founding a
    // one-to-one, the backup).
    accountIdHex,
  };
  // Read before the record goes.
  const found = foundNameOf(
    loadSentRequests(accountIdB64).find((r) => r.recipientAccountIdHex === accountIdHex),
    ack.accepterPubkey,
  );
  if (found !== null && session.current()) session.fileFoundName(keyB64, found);
  upsertAccepted(accepter, accountIdB64);
  // Answered: the sent record and the intro kept for a re-send go.
  forgetSentRequest(accountIdB64, accountIdHex);
  void forgetSentIntro(accountIdB64, accountIdHex);
}
