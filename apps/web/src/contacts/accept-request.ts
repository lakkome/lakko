// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A person's Accept on a request card. The device's own accept of a request
 * sent through a live invite link is `runLinkBuild`, which keeps its own
 * durable record; both answer with the same grants.
 *
 * What an accept leaves is one of three things, never a contact with no chat:
 *
 * 1. **Failed**: no group stands and nothing was sent. The request stays
 *    listed and Accept works again.
 * 2. **Owed**: the group stands, and the Welcome or the ack has not landed.
 *    The person is a contact with the chat, and the outbox holds what is owed
 *    (`owed-answer.ts`), retried at the tail of every poll and at launch.
 * 3. **Done**: the Welcome landed, then the ack: the requester closes its
 *    `reply` key once the ack arrives (seen live 2026-09-30).
 *
 * The answer goes under the request's `reply` grant, then a held `contact`
 * grant, and nothing else (`reach/mailbox-write.ts`).
 *
 * A request without an account id (a legacy wire) can found nothing: it is
 * answered with the ack alone.
 */
import type { IdentitySecrets } from "../auth/AuthContext";
import type { ActiveGroup } from "../chat/ChatContext";
import { DIRECT_CHAT_STAND_IN } from "../chat/group-title";
import { groupIdHex } from "../devices/sync/state";
import type { ProfileApi } from "../profile/ProfileContext";
import { base64Encode } from "../auth/account-id";
import type { ContactsClient, IncomingContactRequest } from "./client";
import { loadAccepted, upsertAccepted, type AcceptedContact } from "./local-store";

/** What an accept did. */
export interface AcceptOutcome {
  /** The accepted list after the accept. */
  readonly accepted: AcceptedContact[];
  /** base64 group id of the one-to-one, or `null` when none stands. */
  readonly groupIdB64: string | null;
  /** See the module docs. `legacy`: the request carried no account id. */
  readonly status: "done" | "owed" | "failed" | "legacy";
}

/** Whether the request stays in the incoming list after this accept. */
export function acceptKeepsRequest(outcome: Pick<AcceptOutcome, "status">): boolean {
  return outcome.status === "failed";
}

export async function acceptContactRequest(
  deps: {
    readonly client: Pick<
      ContactsClient,
      "prepareGroupWith" | "recordOwedAnswer" | "settleOwedAnswer" | "sendContactAck"
    >;
    readonly identity: IdentitySecrets;
    readonly accountIdB64: string;
    readonly chats: {
      add(args: { label: string; group: ActiveGroup["group"] }): ActiveGroup;
      /** Record the held conversations durably. */
      persistHeld(): Promise<void>;
    };
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

  if (req.fromAccountId === null) {
    try {
      await deps.client.sendContactAck(deps.identity, req);
    } catch {
      // eslint-disable-next-line no-console
      console.warn("contact accept: the ack did not land");
      return { accepted: loadAccepted(), groupIdB64: null, status: "failed" };
    }
    // A contact is made only with its one-to-one (base walk 2026-11-04), and
    // a request with no account id cannot found one: no contact.
    return { accepted: loadAccepted(), groupIdB64: null, status: "legacy" };
  }

  // ADR-0062 — a contact's conversation is a one-to-one for good.
  let prepared;
  try {
    prepared = await deps.client.prepareGroupWith(deps.identity, req.fromAccountId, "chat", req.fromPubkey);
  } catch {
    // eslint-disable-next-line no-console
    console.warn("contact accept: the group could not be founded");
    return { accepted: loadAccepted(), groupIdB64: null, status: "failed" };
  }
  // Kept before the group is shown or anything is sent: from here on the
  // accept cannot be lost.
  let entry;
  try {
    entry = await deps.client.recordOwedAnswer(deps.accountIdB64, req, prepared);
  } catch {
    // eslint-disable-next-line no-console
    console.warn("contact accept: the answer could not be kept");
    try {
      prepared.group.free();
    } catch {
      /* already freed */
    }
    return { accepted: loadAccepted(), groupIdB64: null, status: "failed" };
  }
  // ADR-0048 — a 1x1 chat is labelled by the resolver (the peer's name);
  // this label is only the fallback.
  const conversation = deps.chats.add({ label: DIRECT_CHAT_STAND_IN, group: prepared.group });
  // ADR-0048 §2 — founding a group is joining it.
  deps.profiles.groupJoined(conversation.groupIdB64);
  const accepted = upsertAccepted({ ...contact, groupIdB64: conversation.groupIdB64 });
  try {
    await deps.chats.persistHeld();
  } catch {
    // eslint-disable-next-line no-console
    console.warn("contact accept: the held conversations could not be recorded");
  }
  const settled = await deps.client.settleOwedAnswer(deps.identity, entry);
  return {
    accepted,
    groupIdB64: conversation.groupIdB64,
    status: settled === "settled" ? "done" : "owed",
  };
}
