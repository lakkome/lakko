// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Send a plain text message into a conversation from OUTSIDE its screen.
 *
 * The chat screen sends through its own timeline. A send that starts
 * somewhere else — the call screen inviting a contact into the call — has no
 * timeline to append to, and the delivery service never hands a device its
 * own message back. So the sent row is written straight into the
 * conversation's slot of the persisted message log, the same way a forward
 * into another conversation records itself (`routes/Chat.tsx` `forwardOne`),
 * and the conversation shows it the next time it opens.
 *
 * The publish goes through the target group's own send path, so it takes
 * that group's lock like every other send.
 */
import { getDeviceKey } from "../auth/key-store";
import { groupIdHex } from "../devices/sync/state";
import { composeEnvelope, type AttachmentRefBytes, type ForwardRef } from "./attachments";
import { persistedFromForwardRef } from "./forward";
import { persistedFromAttachmentRef } from "./persisted-attachment";
import { authorIdentityToPersisted, resolveMemberIdentity, type RosterMember } from "./author-identity";
import type { ActiveGroup } from "./ChatContext";
import type { ChatClient } from "./client";
import { appendToGroupSlot } from "./background-log";
import { MESSAGE_LOG_VERSION, updateMessageLog, type PersistedMessage } from "./message-log-store";

/** Publish `text` to `entry` and record the sent row. Throws when the publish
 *  fails; nothing is recorded then. */
export async function sendTextToConversation(deps: {
  readonly chat: Pick<ChatClient, "sendMessage">;
  readonly accountId: string;
  readonly entry: ActiveGroup;
  readonly text: string;
  /** The envelope to send, when it is not the plain text one (a channel
   *  invitation: ADR-0087 section 14). `text` is still what the sent row
   *  shows. */
  readonly envelope?: Uint8Array;
  /** What the envelope carries besides its text, for the sent row: the
   *  attachments (already uploaded) and a forward's attribution (a post
   *  forwarded out of a channel, ADR-0087 section 12). */
  readonly attachments?: readonly AttachmentRefBytes[];
  readonly forward?: ForwardRef;
}): Promise<void> {
  const bytes = deps.envelope ?? (await composeEnvelope(deps.text, []));
  const result = await deps.chat.sendMessage(null, deps.entry.group, bytes);

  // The author as every other sent row names it: the person, resolved from
  // this device's own leaf in the target's roster. A roster that cannot be
  // read leaves the row without an author rather than inventing one.
  let author: string | undefined;
  try {
    author = resolveMemberIdentity(
      deps.entry.group.members() as RosterMember[],
      deps.entry.group.ownLeafIndex,
    );
  } catch {
    author = undefined;
  }
  const deviceKey = await getDeviceKey(deps.accountId);
  if (deviceKey === null) return;
  const hex = groupIdHex(deps.entry.group.groupId);
  const sent: PersistedMessage = {
    messageId: String(result.messageId),
    direction: "outgoing",
    text: deps.text,
    attachments: (deps.attachments ?? []).map((a) => persistedFromAttachmentRef(a)),
    ...(deps.forward === undefined ? {} : { forward: persistedFromForwardRef(deps.forward) }),
    createdAt: result.createdAt,
    authorKeyB64: authorIdentityToPersisted(author),
  };
  await updateMessageLog(deps.accountId, deviceKey, (existing) => {
    const groups = existing?.groups ?? {};
    return {
      version: MESSAGE_LOG_VERSION,
      groups: { ...groups, [hex]: appendToGroupSlot(groups[hex], [sent], 0) },
    };
  });
}
