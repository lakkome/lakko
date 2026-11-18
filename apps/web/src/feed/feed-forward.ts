// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Forward from the feed's item menu (`docs/ux/feed.md` section 5): the same
 * "Forward to" picker and the same composition as the chat
 * (`docs/ux/message-actions.md` 5.1, `chat/forward.ts`), built from the row
 * the feed holds rather than from a chat timeline.
 *
 * A group message carries what a forward from its chat would: its text (the
 * log keeps the current, edited text), its attachments (re-encrypted by the
 * sender), the original author's name, and the origin reference — or, when
 * it is itself a forward, the attribution it already carries. A channel post
 * carries its text, its files (re-encrypted by the sender) and, for a
 * location, contact card or article, its content (UX `attachments.md`
 * section 11a), with the channel's name and no origin: the origin
 * reference names a conversation by its group id, and a channel is not one.
 *
 * Pure: the send is the caller's.
 */

import { canForwardContent, forwardPayloadOf, forwardRefFromPersisted, type ForwardPayload } from "../chat/forward";
import type { PersistedAttachment, PersistedMessage } from "../chat/message-log-store";
import { contentFromPersisted } from "../attach/preview";

/** UX `attachments.md` section 11a — whether a feed item may be forwarded:
 *  the chat's rule (`canForwardContent`), for a group message and a channel
 *  post alike. */
export function feedItemForwardable(
  message: PersistedMessage,
  cardShareableOf: (accountKeyB64: string) => boolean,
): boolean {
  return canForwardContent(contentFromPersisted(message.content), cardShareableOf);
}

/** Section 5.1 payload of a feed item, with the SOURCE attachments as the
 *  log stores them. */
export function feedForwardPayload(
  item:
    | {
        readonly kind: "group";
        readonly message: PersistedMessage;
        /** The source group's id as the conversation registry keys it. */
        readonly originGroupIdB64: string;
        /** The author as a recipient should read it; never "You". */
        readonly authorLabel: string | undefined;
      }
    | {
        readonly kind: "channel";
        readonly message: PersistedMessage;
        readonly channelTitle: string;
      },
): ForwardPayload<PersistedAttachment> {
  if (item.kind === "channel") {
    const author = item.channelTitle.trim();
    const content = contentFromPersisted(item.message.content);
    const carried = content !== undefined && content.kind !== "poll" && content.kind !== "checklist" ? content : undefined;
    return {
      text: carried === undefined ? item.message.text : "",
      // The post's files (an article's images), re-encrypted by the sender.
      attachments: item.message.attachments,
      forward: author === "" ? {} : { author },
      ...(carried === undefined ? {} : { content: carried }),
    };
  }
  const m = item.message;
  return forwardPayloadOf(
    {
      text: m.text,
      attachments: m.attachments,
      messageId: Number(m.messageId),
      forward: forwardRefFromPersisted(m.forward),
      content: contentFromPersisted(m.content),
    },
    item.authorLabel,
    item.originGroupIdB64,
    // The feed never holds a one-to-one (UX spec section 3).
    false,
  );
}
