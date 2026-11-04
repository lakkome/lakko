// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where the screens of `docs/ux/conversation-info.md` section 3 sit in the
 * web client's history, and where each one's back control goes.
 *
 * A group's info is a screen pushed from the conversation, not a layer: it
 * takes the conversation's column and has a history entry of its own, so the
 * browser's Back, `Esc` and the back icon in its header all return to the
 * conversation as it was left. The entry is a search parameter on the chat's
 * own route (`?info=group`), so the conversation's view stays mounted under it
 * and keeps its scroll position and composer draft. Add members is one more
 * step forward (`?info=add`), returning to the info.
 *
 * The contact screen lives on another route (`/contacts/:key`), so a screen
 * that opens it records where it was opened from in the history entry's state
 * (`{ lkBackTo: path }`); the contact screen then shows its back icon and
 * returns there (section 3.1). Opened from the Contacts list itself it carries
 * none: nothing is covered on a wide layout, and the narrow layout keeps its
 * own way back to the list.
 */

/** The search parameter the info screens are kept in. */
export const INFO_PARAM = "info";

/** The screens of a group's info, front to back. */
export type InfoScreen = "group" | "add-members" | "group-type" | "group-reactions";

/** The parameter value of each screen. */
export function infoParamOf(screen: InfoScreen): string {
  switch (screen) {
    case "group":
      return "group";
    case "add-members":
      return "add";
    case "group-type":
      return "type";
    case "group-reactions":
      return "reactions";
  }
}

/**
 * The info screen the URL asks for, or `null` (the conversation). Only a
 * group has these screens: for any other conversation, or before the
 * conversation's kind is known, the parameter is ignored.
 */
export function infoScreenOf(param: string | undefined, isGroup: boolean): InfoScreen | null {
  if (!isGroup) return null;
  if (param === "group") return "group";
  if (param === "add") return "add-members";
  if (param === "type") return "group-type";
  if (param === "reactions") return "group-reactions";
  return null;
}

/** Where a screen's back control goes: a screen pushed from the info (Add
 *  members, Who can join, Reactions) to the info, the info to the
 *  conversation (`null`). */
export function infoBackStep(screen: InfoScreen): InfoScreen | null {
  return screen === "group" ? null : "group";
}

/** The history-entry state a screen that opens the contact screen leaves. */
export interface ContactBackState {
  readonly lkBackTo: string;
}

/** The state to navigate to the contact screen with, from `path` (the
 *  current pathname and search). */
export function contactBackState(path: string): ContactBackState {
  return { lkBackTo: path };
}

/**
 * Where the contact screen's back control returns, read from its history
 * entry's state: an in-app path, or `null` when it was not opened from a
 * screen it covers. Anything that is not a same-origin absolute path is
 * ignored.
 */
export function contactBackTarget(state: unknown): string | null {
  if (state === null || typeof state !== "object") return null;
  const to = (state as { lkBackTo?: unknown }).lkBackTo;
  if (typeof to !== "string") return null;
  if (!to.startsWith("/") || to.startsWith("//") || to.startsWith("/\\")) return null;
  return to;
}

/** Whether a back target is a screen the contact screen covers. The Contacts
 *  route is not one: its list stays beside the contact on a wide layout. */
export function coversOrigin(origin: string): boolean {
  return !(origin === "/contacts" || origin.startsWith("/contacts/") || origin.startsWith("/contacts?"));
}
