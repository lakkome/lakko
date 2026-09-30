// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The title of a group that has no name (docs/ux/client-redesign.md
 * section 5.1): the members' names, "Anna, Ivan, Maria" and "+2" beyond
 * three, never an id.
 *
 * Two labels count as "no name": the new-group form's default when the field
 * is left blank, and the stand-in `rehydrate.ts` gives every group after a
 * reload (a prefix of the group id, which is exactly the kind of identifier
 * the spec bans from the screen).
 *
 * Pure: no wasm, no Solid.
 */
import { joinList, t } from "../l10n";

/** The new-group form's default label. */
export const DEFAULT_GROUP_LABEL = "New group";

/** The stand-in labels the store hands out when nobody named the group:
 *  `rehydrate.ts` after a reload, the mailbox on a Welcome, the join-link
 *  landing, and the 1x1 paths (whose label the peer's name always wins over). */
const STAND_IN_LABELS: readonly RegExp[] = [
  /^Group [A-Za-z0-9+/_-]{6}$/,
  /^Joined group \(Welcome \d+\)$/,
  /^Group$/,
  /^Direct chat$/,
];

/** `true` when `label` carries no user-given name. */
export function isUnnamedGroupLabel(label: string): boolean {
  const t = label.trim();
  return t.length === 0 || t === DEFAULT_GROUP_LABEL || STAND_IN_LABELS.some((re) => re.test(t));
}

/** How many names are spelled out before "+N". */
export const GROUP_TITLE_NAMES = 3;

/** Title an unnamed group from its members' names (the reader excluded). */
export function groupTitleFromNames(names: readonly string[]): string {
  const shown = names.slice(0, GROUP_TITLE_NAMES);
  if (shown.length === 0) return t("chat.group.default_name");
  // Joined through the catalog so each language picks its own separator.
  const joined = joinList(shown);
  const rest = names.length - shown.length;
  return rest > 0 ? t("chat.group.title_more", { names: joined, count: rest }) : joined;
}

/** The title to show for a group with `label`, given its members' names. */
export function groupTitle(label: string, memberNames: readonly string[]): string {
  return isUnnamedGroupLabel(label) ? groupTitleFromNames(memberNames) : label.trim();
}
