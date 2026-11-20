// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The long-lived DeviceGroup handle is replaced (a reload, a rejoin) by one
 * run at a time. Two at once both awaited the same loops; the second then
 * freed the handle the first had just started new loops on and replaced
 * their abort controller, leaving loops nothing could stop, reading with a
 * freed handle (seen live 2026-11-11: a page stopped answering knocks).
 */

/** A queue of handle replacements: each run starts after the previous one
 *  settled, whether it succeeded or failed. */
export function createReplacementQueue(): (run: () => Promise<void>) => Promise<void> {
  let tail: Promise<void> = Promise.resolve();
  return (run) => {
    const next = tail.then(run);
    tail = next.catch(() => undefined);
    return next;
  };
}
