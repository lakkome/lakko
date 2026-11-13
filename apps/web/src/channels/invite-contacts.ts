// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Send a channel invitation to one contact (ADR-0087 section 14; UX
 * `docs/ux/group-and-channel-privacy.md` section 14.1, and 15.2 for a
 * publisher invitation): a message in the one-to-one with that person,
 * started if there is none, by the same route `Invite contacts` on a call
 * takes (`calls/invite-route.ts`).
 *
 * The message is `composeChannelInviteEnvelope(url, card)`: the card a
 * client draws (`Subscribe` for a subscriber invitation, `Accept` for a
 * publisher one, UX 15.2), and the link as its text, so a client that does
 * not draw the card still shows a link that opens the link page.
 *
 * Must be called inside the providers a chat screen runs in.
 */
import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { inviteContactToCall, type InviteOutcome } from "../calls/invite-route";
import { useChats } from "../chat/ChatContext";
import { ChatClient } from "../chat/client";
import { conversationKindOf } from "../chat/conversation-kind";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { sendTextToConversation } from "../chat/send-text";
import { ContactsClient } from "../contacts/client";
import { conversationWithContact } from "../contacts/contact-conversation";
import { foundOneToOneWith } from "../contacts/found-one-to-one";
import type { AcceptedContact } from "../contacts/local-store";
import { loadSentRequests, recordSentRequest } from "../contacts/sent-requests";
import { sharedGroupsFor } from "../contacts/shared-groups";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { heldGrantReaches } from "../search/wiring";
import { sealedWakeForGroup } from "../push/group-routing";
import { loadWasm } from "../wasm";
import { hexToBytes as hexToBytesOrNull } from "../calls/group-call-record";

/** What to send: an invitation card with its link, or a link alone. */
export type ChannelInvitation =
  | { readonly kind: "card"; readonly url: string; readonly card: Uint8Array }
  | { readonly kind: "link"; readonly url: string };

/** One sender for the screen that invites. */
export function useChannelInviteSender(): (
  contact: AcceptedContact,
  invitation: ChannelInvitation,
) => Promise<InviteOutcome> {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    wakeSealer: async (groupId, kind) => {
      const s = auth.state();
      if (s.kind !== "session") return null;
      return sealedWakeForGroup(s.account.accountId, base64Encode(groupId), kind);
    },
  });
  const contactsClient = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });

  return async (contact, invitation) => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) throw new Error("no session");
    const accountIdHex = contact.accountIdHex;
    const accountIdOf = (hex: string | undefined): Uint8Array => {
      const bytes = hex === undefined ? null : hexToBytesOrNull(hex);
      if (bytes === null) throw new Error("contact has no account id");
      return bytes;
    };
    const envelope =
      invitation.kind === "card"
        ? (await loadWasm()).composeChannelInviteEnvelope(invitation.url, invitation.card)
        : undefined;
    return inviteContactToCall({
      existing: () =>
        conversationWithContact({
          contact,
          conversations: chats.groups(),
          ownKeyB64: ownPersonKeyB64(chats.identity()),
          peopleOf: (g) => personKeysOf(g.group),
          isOneToOne: (g) => conversationKindOf(g.group) === "chat",
        }),
      accountKnown: accountIdHex !== undefined,
      grantReaches: () => heldGrantReaches(base64Decode(contact.signaturePubkeyB64)),
      requestWaiting: () =>
        loadSentRequests(s.account.accountId).some((r) => r.recipientAccountIdHex === accountIdHex),
      sharedGroups: () =>
        sharedGroupsFor({
          groups: chats.groups(),
          accountIdB64: s.account.accountId,
          ownKeyB64: ownPersonKeyB64(chats.identity()),
          personKeysB64: [contact.signaturePubkeyB64, ...(contact.previousSignaturePubkeysB64 ?? [])],
        }),
      found: async () =>
        (
          await foundOneToOneWith({
            client: contactsClient,
            identity: secrets,
            chats,
            profiles,
            contact,
            peerAccountId: accountIdOf(accountIdHex),
            peerPubkey: base64Decode(contact.signaturePubkeyB64),
          })
        ).conversation,
      sendLink: (entry) =>
        sendTextToConversation({
          chat,
          accountId: s.account.accountId,
          entry,
          text: invitation.url,
          ...(envelope === undefined ? {} : { envelope }),
        }),
      sendInGroup: async (groupIdB64) => {
        const shared = chats.get(groupIdB64);
        if (shared === undefined || accountIdHex === undefined) throw new Error("the shared group is gone");
        await contactsClient.sendContactRequestInGroup(
          secrets,
          accountIdOf(accountIdHex),
          base64Decode(contact.signaturePubkeyB64),
          shared.group,
        );
        const name = profiles.resolve(contact.signaturePubkeyB64);
        recordSentRequest(s.account.accountId, {
          recipientUsername: name.username ?? "",
          recipientName: memberNameOf(name, "a channel invitation's sent request"),
          recipientAccountIdHex: accountIdHex,
          sentAtMs: Date.now(),
          recipientKeyB64: contact.signaturePubkeyB64,
          throughGroupIdB64: groupIdB64,
        });
      },
    });
  };
}
