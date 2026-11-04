// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/reactions-and-emoji.md` section 12 — the `Reactions` switch,
 * "When someone reacts to your messages", on by default. This device's
 * notification filter: nothing is sent and nothing syncs. The service worker
 * reads it from the notification words the page stores
 * (`l10n/notification-strings.ts`), since it cannot reach `localStorage`.
 */
import { createSignal } from "solid-js";

const KEY = "lakko.notify.reactions.off";

function read(): boolean {
  try {
    return globalThis.localStorage?.getItem(KEY) !== "1";
  } catch {
    return true;
  }
}

const [on, setOn] = createSignal<boolean>(read());

/** Whether reaction notifications are on for this device. Reactive. */
export const reactionNotificationsOn = on;

export function setReactionNotificationsOn(value: boolean): void {
  try {
    if (value) globalThis.localStorage?.removeItem(KEY);
    else globalThis.localStorage?.setItem(KEY, "1");
  } catch {
    // Storage blocked: the choice holds for this page only.
  }
  setOn(value);
}
