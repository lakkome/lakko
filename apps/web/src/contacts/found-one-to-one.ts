// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Found the one-to-one conversation with a contact and link it back to the
 * contact record (ADR-0021 follow-up).
 *
 * One function, two callers that must do the same four things in the same
 * order: Contacts on `Message` for a contact with no conversation yet, and
 * the call screen's `Invite contacts` when the invited contact has never been
 * written to from this device (`docs/ux/call-links.md` section 3). Accepting a
 * request has its own path (`accept-request.ts`).
 */

import type { IdentitySecrets } from "../auth/AuthContext";
import type { ActiveGroup, ChatApi } from "../chat/ChatContext";
import { DIRECT_CHAT_STAND_IN } from "../chat/group-title";
import type { ProfileApi } from "../profile/ProfileContext";
import type { ContactsClient } from "./client";
import { upsertAccepted, type AcceptedContact } from "./local-store";

export async function foundOneToOneWith(deps: {
  readonly client: Pick<ContactsClient, "startGroupWith">;
  readonly identity: IdentitySecrets;
  readonly chats: Pick<ChatApi, "add">;
  readonly profiles: Pick<ProfileApi, "groupJoined">;
  readonly contact: AcceptedContact;
  readonly peerAccountId: Uint8Array;
  readonly peerPubkey: Uint8Array;
}): Promise<{ readonly conversation: ActiveGroup; readonly accepted: AcceptedContact[] }> {
  // ADR-0062 — a contact's conversation is a one-to-one for good.
  const result = await deps.client.startGroupWith(
    deps.identity,
    deps.peerAccountId,
    deps.peerPubkey,
    "chat",
  );
  // ADR-0048 — a 1x1 chat is labelled by the resolver (the peer's name);
  // this label is only the fallback.
  const conversation = deps.chats.add({ label: DIRECT_CHAT_STAND_IN, group: result.group });
  // ADR-0048 §2 — founding a group is joining it.
  deps.profiles.groupJoined(conversation.groupIdB64);
  const accepted = upsertAccepted({ ...deps.contact, groupIdB64: conversation.groupIdB64 });
  return { conversation, accepted };
}
