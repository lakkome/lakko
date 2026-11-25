// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where the archived conversations sit in the conversation panel
 * (`docs/ux/navigation.md` section 2.2, step 3 and its note "Archived, as
 * built (2026-11-25)").
 *
 * **The rule is not here.** It is `archived_section` in
 * `crates/lakko-ui-rules`, shared with iOS and Android: archived
 * conversations are never in the chats list; one "Archived" bar at the
 * bottom of the panel opens them as a list of their own; while something is
 * typed in the chats view they are listed under a divider after the other
 * matches. It answers in counts over the ordered list; this adapter hands it
 * the two flags of each row and slices the web's rich rows by the counts, so
 * no row crosses the wasm boundary.
 */
import type { UiRules } from "./ui-rules";

/** Which of the two lists the panel shows. */
export type ArchivedView = "chats" | "archive";

/** What the panel draws, split around the archived conversations. */
export interface ArchivedSplit<T> {
  /** The list shown: the chats, or the archive opened from the bar. */
  readonly view: ArchivedView;
  /** The rows that are not archived (the pins included), in order. */
  readonly top: readonly T[];
  /** The archived rows, in order: the archive view's list, or the matches
   *  under the divider while searching in the chats view. */
  readonly archived: readonly T[];
  /** The "Archived" bar at the bottom of the panel, or `null`. */
  readonly bar: { readonly count: number } | null;
  /** Chats view, searching: a divider above the archived matches. */
  readonly divider: boolean;
}

export interface ArchivedSplitOptions {
  /** Something is typed in the search field (the rows given are the matches). */
  readonly searching: boolean;
  /** The person opened the archive from the bar and has not gone back. */
  readonly archiveOpen: boolean;
}

export type ArchivedRules = Pick<UiRules, "archivedSection">;

/**
 * Split an ordered list by the shared rule. `isArchived` / `isPinned` read a
 * row's flags; `isPinned` is the pin of the view shown (the folder's own pin
 * inside a folder). Without the rules loaded every row is listed in the chats
 * view, with no bar.
 */
export function splitArchived<T>(
  rows: readonly T[],
  isArchived: (row: T) => boolean,
  isPinned: (row: T) => boolean,
  options: ArchivedSplitOptions,
  rules: ArchivedRules | null,
): ArchivedSplit<T> {
  if (rules === null) return { view: "chats", top: rows, archived: [], bar: null, divider: false };
  const archived = Uint8Array.from(rows, (r) => (isArchived(r) ? 1 : 0));
  const pinned = Uint8Array.from(rows, (r) => (isPinned(r) ? 1 : 0));
  const section = rules.archivedSection(archived, pinned, options.searching, options.archiveOpen);
  try {
    const topCount = section.topCount;
    const archivedCount = section.archivedCount;
    return {
      view: section.view === "archive" ? "archive" : "chats",
      top: rows.slice(0, topCount),
      archived: rows.slice(topCount, topCount + archivedCount),
      bar: section.bar ? { count: archivedCount } : null,
      divider: section.divider,
    };
  } finally {
    section.free();
  }
}

/** The rows the panel lists in the view shown, in order (for list keyboard
 *  navigation): the archive alone, or the chats with the archived matches
 *  after them while searching. */
export function shownRows<T>(split: ArchivedSplit<T>): readonly T[] {
  if (split.view === "archive") return split.archived;
  return split.divider ? [...split.top, ...split.archived] : split.top;
}
