// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a conversation's avatar is looked up by (design system "Avatar":
 * every avatar of a conversation that has a photo shows the photo). A
 * one-to-one shows the other person's photo, a group its own photo, filed
 * under its group id (ADR-0087 section 18). One rule for every surface that
 * draws a conversation (Chats, Calls, the Feed's source avatar), so no surface
 * falls back to initials for a group that has a picture (base walk
 * 2026-11-04: Feed and Calls showed "QI" for a group Chats showed a photo
 * for).
 *
 * The kind is the conversation's own (ADR-0062), never a member count: a
 * two-person group is a group.
 *
 * Pure.
 */
import type { ConversationKind } from "./conversation-kind";

/** The `Avatar` props that carry the photo lookup. */
export interface ConversationAvatarKeys {
  readonly personKey?: string;
  readonly conversationKey?: string;
}

export function conversationAvatarKeys(input: {
  readonly kind: ConversationKind;
  /** The group the conversation lives in, standard base64. */
  readonly groupIdB64: string;
  /** In a one-to-one, the other person's key; ignored for a group. */
  readonly peerKeyB64?: string | undefined;
}): ConversationAvatarKeys {
  if (input.kind === "chat") {
    return input.peerKeyB64 === undefined ? {} : { personKey: input.peerKeyB64 };
  }
  return { conversationKey: input.groupIdB64 };
}
