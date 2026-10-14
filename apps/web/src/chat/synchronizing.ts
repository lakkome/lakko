// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When the chat list shows its one "Synchronizing..." label
 * (`docs/ux/second-device.md` section 2).
 *
 * One rule: the pairing bundle (ADR-0050 section 2.9, `devices/state-bundle.ts`)
 * named groups the device that confirmed this one holds, and this device has
 * not joined all of them yet. That device is alive and is admitting this one,
 * so the label announces something that is happening.
 *
 * What is NOT a reason (2026-10-02): the account having been restored from
 * the recovery phrase with an empty chat list. With no other device alive
 * nobody is about to admit this one, so there is nothing to announce; the
 * conversations a restored backup named live in their own store
 * (`backup/restored-known-groups.ts`), which this rule never reads. Per-chat
 * states for that case come with a history backup.
 */
import { pendingKnownGroups } from "../devices/state-bundle";

/**
 * Whether the label shows, given the group ids this device holds. `owner` is
 * the account (default: the signed-in one). Reading it clears the pairing
 * list once every named group is held, so the label cannot outlive its cause.
 */
export function showsSynchronizing(joinedIdsB64: readonly string[], owner?: string): boolean {
  return pendingKnownGroups(joinedIdsB64, owner) > 0;
}
