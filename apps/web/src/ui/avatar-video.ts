// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When a profile video plays in an avatar circle (docs/ux/profile-screen.md
 * section 7). It never has sound.
 *
 * - `true`: always (the large views).
 * - `"hover"`: while a mouse or pen is over the circle's row (Chats,
 *   Contacts); never on a touch-only device.
 * - `"header"`: the chat header: as `"hover"` where the device can hover,
 *   always where it cannot.
 * - `false` / absent: never, the still.
 */
export type AvatarVideoMode = boolean | "hover" | "header";

/** Whether the video plays now. */
export function avatarVideoPlays(mode: AvatarVideoMode | undefined, hovering: boolean, canHover: boolean): boolean {
  switch (mode) {
    case true:
      return true;
    case "hover":
      return canHover && hovering;
    case "header":
      return canHover ? hovering : true;
    default:
      return false;
  }
}

/** Whether a pointer event counts as hovering: a touch never does. */
export function pointerHovers(pointerType: string): boolean {
  return pointerType !== "touch";
}

/** Whether this device's primary input can hover. */
export function deviceCanHover(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  try {
    return window.matchMedia("(hover: hover)").matches;
  } catch {
    return false;
  }
}
