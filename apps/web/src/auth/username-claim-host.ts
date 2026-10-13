// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether the "Choose a username" screen is open. One switch for the page,
 * so every entry point (the recovery email surfaces, the protect card, the
 * add-device guard, Settings > Profile) opens the same screen and closing it
 * returns the person to where they were.
 */
import { createSignal } from "solid-js";

const [open, setOpen] = createSignal(false);

export const usernameClaimOpen = open;

export function openUsernameClaim(): void {
  setOpen(true);
}

export function closeUsernameClaim(): void {
  setOpen(false);
}

// ADR-0068: the "Change username" screen, the same way.
const [changeOpen, setChangeOpen] = createSignal(false);

export const usernameChangeOpen = changeOpen;

export function openUsernameChange(): void {
  setChangeOpen(true);
}

export function closeUsernameChange(): void {
  setChangeOpen(false);
}
