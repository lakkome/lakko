// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/second-device.md section 3 - an open Devices screen follows the
 * account's roster.
 *
 * The list is read from the durable DeviceGroup, so it was right when it was
 * read and stale after the next commit. `DeviceGroupSyncContext.deviceRoster`
 * moves on both kinds of commit, whichever route made it: one made on this
 * device (the handle is reloaded from disk) and one received (the batch is
 * persisted first). By then the durable state holds the change, so reading the
 * list again finds it. No timer: the roster says when.
 */
import { createEffect, createMemo, on, type Accessor } from "solid-js";

import type { DeviceRoster } from "./sync/DeviceGroupSyncContext";

/** What the list is drawn from: which devices, and which one is this one.
 *  `addedHere` is who let them in, which changes no row. */
export function rosterListKey(roster: Pick<DeviceRoster, "keys" | "ownKey"> | null): string | null {
  return roster === null ? null : `${roster.ownKey}|${[...roster.keys].sort().join(",")}`;
}

/**
 * Call `reload` each time the roster holds different devices. Not on the first
 * read (the view has its own), and not when the roster goes away: there is then
 * nothing to read. Runs inside a reactive owner.
 */
export function reloadWhenRosterChanges(
  roster: Accessor<DeviceRoster | null>,
  reload: () => void,
): void {
  const key = createMemo(() => rosterListKey(roster()));
  createEffect(
    on(
      key,
      (now) => {
        if (now !== null) reload();
      },
      { defer: true },
    ),
  );
}
