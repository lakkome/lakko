// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 item 6 - "the list is written when the device creates
 * or deletes a channel, after each rotation it makes, and when it mints an
 * admission seed". Those call sites say so here; the publisher loops install
 * what runs (`ChannelPublisherLoops.tsx`). A leaf (it imports only the
 * stored facts, which import nothing), so the channel modules that call it
 * take on no dependency.
 */
import { factsStorage, oweListDelete } from "./owned-facts";

let wake: (() => void) | null = null;
let pending = false;

/** Something the owned-channels list carries changed on this device. */
export function noteOwnedChannelsChanged(): void {
  if (wake === null) {
    pending = true;
    return;
  }
  wake();
}

/** Install (or remove, with `null`) what a change runs. A change noted
 *  while nothing was installed runs once now. */
export function installOwnedChannelsWake(fn: (() => void) | null): void {
  wake = fn;
  if (fn !== null && pending) {
    pending = false;
    fn();
  }
}

/** A channel was deleted on this device: its entry is owed out of the list
 *  (kept across a reload until the list write lands). Nothing for a session
 *  with no account. */
export function noteOwnedChannelDeleted(accountIdB64: string | null, idHex: string): void {
  if (accountIdB64 === null) return;
  oweListDelete(factsStorage(), accountIdB64, idHex);
  noteOwnedChannelsChanged();
}
