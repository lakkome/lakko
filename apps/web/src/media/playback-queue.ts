// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec §2 timeline bubble — "Playback continues to the next consecutive
 * unplayed voice message from the same sender."
 *
 * "Consecutive" is read literally: from the message that just finished, walk
 * forward through the timeline while each row is a voice message by the same
 * sender. The first unplayed one in that run plays next. Any other row — a
 * text message, a voice message from someone else, a video note — ends the
 * run, so auto-continue never jumps over the conversation to an unrelated
 * recording. An unknown sender never matches, including another unknown one:
 * two unattributed rows are not known to share an author.
 */

export interface VoiceQueueItem {
  /** Stable key the player addresses the item by. */
  readonly key: string;
  readonly isVoice: boolean;
  /** Author identity (signature key), `undefined` when unresolved. */
  readonly senderKey: string | undefined;
  readonly played: boolean;
}

export function nextVoiceToPlay(
  items: readonly VoiceQueueItem[],
  finishedKey: string,
): string | null {
  const index = items.findIndex((i) => i.key === finishedKey);
  if (index === -1) return null;
  const sender = items[index]?.senderKey;
  if (sender === undefined) return null;
  for (let i = index + 1; i < items.length; i += 1) {
    const item = items[i];
    if (item === undefined || !item.isVoice || item.senderKey !== sender) return null;
    if (!item.played) return item.key;
  }
  return null;
}

/** §2 — the speed toggle cycles 1x → 1.5x → 2x → 1x. */
export const PLAYBACK_RATES: readonly number[] = [1, 1.5, 2];

export function nextPlaybackRate(current: number): number {
  const i = PLAYBACK_RATES.indexOf(current);
  return PLAYBACK_RATES[(i + 1) % PLAYBACK_RATES.length] ?? 1;
}

export function rateLabel(rate: number): string {
  return `${rate}x`;
}
