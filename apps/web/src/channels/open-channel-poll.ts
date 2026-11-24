// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Keeping an open channel current. Nothing pushes a post to a reader, so a
 * channel on screen asks for its page on a slow tick, and only while the
 * person can see it: one request per open, visible channel.
 */

import { onNetworkReturned } from "../net/connection-state";

/** Chosen, not measured: a post appears within this long. */
export const OPEN_CHANNEL_POLL_MS = 10_000;

/** Run `load` every {@link OPEN_CHANNEL_POLL_MS} while the document is
 *  visible, one at a time; a failed run waits for the next tick. Returns the
 *  stop. */
export function pollWhileVisible(
  load: () => Promise<void>,
  visible: () => boolean = () => typeof document === "undefined" || document.visibilityState === "visible",
  everyMs: number = OPEN_CHANNEL_POLL_MS,
): () => void {
  let running = false;
  const tick = (): void => {
    if (running || !visible()) return;
    running = true;
    void load()
      .catch(() => undefined)
      .finally(() => {
        running = false;
      });
  };
  const timer = setInterval(tick, everyMs);
  // docs/ux/connection-state.md section 3: the network coming back loads at
  // once rather than at the next tick.
  const stopNetworkReturn = onNetworkReturned(tick);
  return () => {
    clearInterval(timer);
    stopNetworkReturn();
  };
}

/** Whether two pages hold the same posts, by cursor: an unchanged page decodes
 *  nothing again. */
export function samePosts(
  a: ReadonlyArray<{ readonly cursor: number }>,
  b: ReadonlyArray<{ readonly cursor: number }>,
): boolean {
  return a.length === b.length && a.every((p, i) => p.cursor === b[i]?.cursor);
}
