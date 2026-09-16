// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Display-only facts the channel list shows when it knows them: the
 * subscriber count the owner's manage panel last fetched. Deliberately
 * outside `state/channels-state.ts`, which holds key material and the
 * post cache; nothing here is needed to read or publish, and losing it only
 * blanks a subtitle.
 */

import { createSignal } from "solid-js";

const [counts, setCounts] = createSignal<ReadonlyMap<string, number>>(new Map());

/** The subscriber count last seen for `channelIdHex`, or `null`. */
export function knownSubscriberCount(channelIdHex: string): number | null {
  return counts().get(channelIdHex) ?? null;
}

/** Record a freshly listed subscriber count. */
export function rememberSubscriberCount(channelIdHex: string, count: number): void {
  setCounts((m) => {
    if (m.get(channelIdHex) === count) return m;
    const next = new Map(m);
    next.set(channelIdHex, count);
    return next;
  });
}
