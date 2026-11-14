// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This identity's provider holds what was on disk": the session's MLS
 * rehydrate (`chat/rehydrate.ts`) restored the device identity's provider
 * from its snapshot, or found none to restore. `restoreStorage` REPLACES the
 * whole provider, so a group opened in it before that moment would be lost,
 * or worse written back over the restored state.
 *
 * ADR-0090: a channel's PublisherGroup lives in the device identity's
 * provider, so the channel paths wait for this before they open one. The
 * wait is bounded: a session path that never rehydrates (none is known) must
 * not hang the channels forever.
 */

const settled = new WeakSet<object>();
const waiters = new Map<object, Array<() => void>>();

/** The rehydrate of `identity` is done (restored, or nothing to restore). */
export function markProviderSettled(identity: object): void {
  if (settled.has(identity)) return;
  settled.add(identity);
  const list = waiters.get(identity);
  waiters.delete(identity);
  for (const w of list ?? []) w();
}

/** Whether {@link markProviderSettled} ran for `identity`. */
export function isProviderSettled(identity: object): boolean {
  return settled.has(identity);
}

/** Resolves once `identity` is settled, or after `timeoutMs` (chosen, not
 *  measured: longer than a rehydrate seen on any device so far). */
export function awaitProviderSettled(identity: object, timeoutMs = 15_000): Promise<void> {
  if (settled.has(identity)) return Promise.resolve();
  return new Promise((resolve) => {
    let done = false;
    const finish = (): void => {
      if (done) return;
      done = true;
      resolve();
    };
    const list = waiters.get(identity) ?? [];
    list.push(finish);
    waiters.set(identity, list);
    setTimeout(finish, timeoutMs);
  });
}
