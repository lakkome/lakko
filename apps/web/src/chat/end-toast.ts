// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 9 (2026-11-06) — the short
 * notice at the bottom when the person is no longer in a conversation that is
 * not open: one notice at a time, gone by itself after
 * {@link END_TOAST_MS}; removals noticed together, or while a notice is up,
 * make one notice, "You are no longer in {name} and {n} more.". Pure.
 */

/** How long the notice stays (UX section 9). */
export const END_TOAST_MS = 4_000;

/** How long a new end waits for a dialog over its open conversation to claim
 *  it before the notice takes it (chosen: one render is enough). */
export const END_TOAST_CLAIM_WAIT_MS = 300;

/** What the notice says: the names, the first one named. */
export interface EndToast {
  readonly names: readonly string[];
}

/** The notice after `names` are noticed: added to the one that is up, or a
 *  new one. `current` stays when nothing is added. */
export function addToEndToast(current: EndToast | null, names: readonly string[]): EndToast | null {
  if (names.length === 0) return current;
  return { names: [...(current?.names ?? []), ...names] };
}

/** The notice's text: one name, or the first name and how many more. */
export function endToastText(
  toast: EndToast,
  one: (name: string) => string,
  more: (name: string, count: number) => string,
): string {
  const first = toast.names[0] ?? "";
  return toast.names.length <= 1 ? one(first) : more(first, toast.names.length - 1);
}
