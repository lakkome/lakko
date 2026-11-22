// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The order of a composer draft write: into this device's synced state
 * first, then to the account's other devices.
 *
 * A device never receives its own DeviceGroup messages, so the local fold is
 * the only place this device keeps its draft. Folding it only after the
 * publish succeeded lost the draft whenever the publish was slow or failed:
 * a takeover by another tab (docs/ux/one-tab.md section 4) spent its whole
 * bound on the network and wrote a synced state without the draft, and a
 * Delivery Service refusal lost it outright (seen on test.lakko.me
 * 2026-11-21).
 *
 * `fold` runs synchronously, before this returns; the returned promise is
 * the publish alone, which keeps going after the caller stops waiting.
 */
export function foldThenPublish(fold: () => void, publish: () => Promise<unknown>): Promise<void> {
  fold();
  return publish().then(() => undefined);
}
