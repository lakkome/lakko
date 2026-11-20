// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 section 2, UX 2.1 - opening a report on a channel: from its info
 * (posts the reporter picks), from a post's menu (that post), from a
 * Discover card (its listing). One shape, so the three entry points cannot
 * build three different drafts.
 */

import type { ConversationKindCode, FixedEvidence } from "./report-model";
import { openReport } from "./report-session";

export function openChannelReport(input: {
  readonly channelIdHex: string;
  readonly name: string;
  readonly conversationKind: ConversationKindCode;
  /** Posts picked from the start (a post's menu), by cursor. */
  readonly picked?: readonly number[];
  /** Items that are part of the report (a Discover listing). */
  readonly fixed: readonly FixedEvidence[];
  /** Whether posts can be picked: the channel is open on this device. */
  readonly canPick?: boolean;
  /** `Unsubscribe` in the sheet; `null` hides the checkbox. */
  readonly leave: (() => Promise<void>) | null;
}): void {
  openReport({
    target: { kind: "channel", channelIdHex: input.channelIdHex, name: input.name },
    conversationKind: input.conversationKind,
    pickFrom: input.canPick === false ? null : { kind: "channel", channelIdHex: input.channelIdHex },
    picked: input.picked ?? [],
    fixed: input.fixed,
    canLeave: input.leave !== null,
    ...(input.leave === null ? {} : { leave: input.leave }),
  });
}
