// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one accept path of a contact request, shared by the person's Accept on
 * a request card and by the device's own accept of a request sent through one
 * of the account's live invite links (ADR-0079 section 3): the same four
 * steps in the same order, so an automatic accept cannot drift from a manual
 * one.
 *
 * 1. The requester becomes a contact (kept even if what follows fails: the
 *    person, or the link, consented).
 * 2. The one-to-one is founded and its Welcome written with the requester's
 *    `reply` grant (ADR-0060 section 6).
 * 3. The ContactAck is sent AFTER the Welcome: the requester closes the reply
 *    key once the ack arrives (seen live 2026-09-30).
 * 4. A request without an account id (a legacy wire) gets the ack only.
 */
import type { IdentitySecrets } from "../auth/AuthContext";
import type { ChatApi } from "../chat/ChatContext";
import { groupIdHex } from "../devices/sync/state";
import type { ProfileApi } from "../profile/ProfileContext";
import { base64Encode } from "../auth/account-id";
import type { ContactsClient, IncomingContactRequest } from "./client";
import { foundOneToOneWith } from "./found-one-to-one";
import { loadAccepted, upsertAccepted, type AcceptedContact } from "./local-store";

/** What an accept did. */
export interface AcceptOutcome {
  /** The accepted list after the accept. */
  readonly accepted: AcceptedContact[];
  /** base64 group id of the one-to-one, or `null` when none was founded. */
  readonly groupIdB64: string | null;
  /** The request carried no account id: only the ack went out. */
  readonly legacy: boolean;
  /** Why founding the one-to-one failed, or `null`. */
  readonly chatError: unknown;
}

export async function acceptContactRequest(
  deps: {
    readonly client: Pick<ContactsClient, "startGroupWith" | "sendContactAck">;
    readonly identity: IdentitySecrets;
    readonly chats: Pick<ChatApi, "add">;
    readonly profiles: Pick<ProfileApi, "groupJoined">;
  },
  req: IncomingContactRequest,
): Promise<AcceptOutcome> {
  const keyB64 = base64Encode(req.fromPubkey);
  const contact: AcceptedContact = {
    signaturePubkeyB64: keyB64,
    // ADR-0048 — no local rename on accept (keep one the user already set);
    // the name shown comes from the resolver.
    displayName: loadAccepted().find((c) => c.signaturePubkeyB64 === keyB64)?.displayName ?? "",
    acceptedAtMs: Date.now(),
    // ADR-0021 — the peer's account id (hex), for the call roster.
    ...(req.fromAccountId ? { accountIdHex: groupIdHex(req.fromAccountId) } : {}),
  };
  let accepted = upsertAccepted(contact);

  const sendAck = async (): Promise<void> => {
    try {
      await deps.client.sendContactAck(deps.identity, req);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("sendContactAck failed", err instanceof Error ? err.name : "");
    }
  };

  if (req.fromAccountId === null) {
    await sendAck();
    return { accepted, groupIdB64: null, legacy: true, chatError: null };
  }
  let groupIdB64: string | null = null;
  let chatError: unknown = null;
  try {
    const founded = await foundOneToOneWith({
      client: deps.client,
      identity: deps.identity,
      chats: deps.chats,
      profiles: deps.profiles,
      contact,
      peerAccountId: req.fromAccountId,
      peerPubkey: req.fromPubkey,
      replyGrantB64: req.replyGrantB64,
    });
    accepted = founded.accepted;
    groupIdB64 = founded.conversation.groupIdB64;
  } catch (err) {
    chatError = err;
  }
  await sendAck();
  return { accepted, groupIdB64, legacy: false, chatError };
}

/**
 * ADR-0079 section 3 — what the mailbox does with a request, by its link
 * answer and the link register's verdict on it. Pure.
 *
 * | link answer | verdict    | action                                   |
 * |-------------|------------|------------------------------------------|
 * | none        | -          | `card`: an ordinary request card          |
 * | present     | `accept`   | `accept`: accept now, no card             |
 * | present     | `answered` | `drop`: already accepted, show nothing    |
 * | present     | `dead`     | `card-dead-link`: the card with the note  |
 */
export function linkAnswerAction(
  hasLinkAnswer: boolean,
  verdict: "accept" | "answered" | "dead" | null,
): "card" | "accept" | "drop" | "card-dead-link" {
  if (!hasLinkAnswer) return "card";
  if (verdict === "accept") return "accept";
  if (verdict === "answered") return "drop";
  return "card-dead-link";
}
