// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether the "Change username" screen (ADR-0076) is open. One switch for
 * the page, so every entry point opens the same screen and closing it
 * returns the person to where they were.
 */
import { createSignal } from "solid-js";

const [changeOpen, setChangeOpen] = createSignal(false);

export const usernameChangeOpen = changeOpen;

export function openUsernameChange(): void {
  setChangeOpen(true);
}

export function closeUsernameChange(): void {
  setChangeOpen(false);
}
