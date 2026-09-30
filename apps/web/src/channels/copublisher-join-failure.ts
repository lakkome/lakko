// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the co-publisher join screen (`routes/Channels.tsx`, "Join as
 * co-publisher") says when the request fails. It uses the same knock as the
 * `/g/` group join (ADR-0053 contract point 6), so it uses the same three
 * sentences (`routes/join-group-failure.ts`): "not taking requests yet", the
 * clock sentence, and the general failure with `Try again`. The raw error is
 * never shown; the screen logs it.
 *
 * One case keeps its own sentence: a link minted under the retired v1 channel
 * derivation (`LegacyChannelError`), whose message is already fixed,
 * readable copy telling the person what to do, and which no retry can fix.
 *
 * Pure, so the mapping is testable without a DOM.
 */
import { LegacyChannelError } from "./legacy-v1";
import { joinFailureNotice, type JoinFailureNotice } from "../routes/join-group-failure";

export function coPublisherJoinFailure(err: unknown): JoinFailureNotice {
  if (err instanceof LegacyChannelError) return { render: () => err.text(), offerRetry: false };
  return joinFailureNotice(err);
}
