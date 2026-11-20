// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Lakko icon set (`.claude/skills/ux-ui/design-system.md` section 8):
 * inline SVGs on a 24 x 24 grid, drawn from `icon-paths.generated.ts`
 * (exported from `docs/brand/icons/svg` by `just export-icons`), recoloured
 * through `currentColor`. One stroke width everywhere; only the box size
 * changes per site. Icons that point in the reading direction are mirrored
 * in right-to-left layouts. No emoji and no other icon drawings in product UI.
 */
import type { JSX } from "solid-js";

import { LK_ICON_MIRRORED, LK_ICON_PATHS, LK_ICON_STROKE_WIDTH } from "./icon-paths.generated";

/** Every icon name the set knows (the file names in `docs/brand/icons/svg`). */
export type IconName = keyof typeof LK_ICON_PATHS;

/** Whether `name` is one of the set's icons (a name that arrived as text). */
export function isIconName(name: string): name is IconName {
  return Object.prototype.hasOwnProperty.call(LK_ICON_PATHS, name);
}

/** One inline SVG icon; `size` in px (default 20), colour from `currentColor`. */
export function Icon(props: { name: IconName; size?: number; class?: string; title?: string }): JSX.Element {
  const size = (): number => props.size ?? 20;
  const className = (): string | undefined =>
    LK_ICON_MIRRORED.has(props.name) ? `lk-icon-flip-rtl ${props.class ?? ""}`.trim() : props.class;
  return (
    <svg
      width={size()}
      height={size()}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width={LK_ICON_STROKE_WIDTH}
      stroke-linecap="round"
      stroke-linejoin="round"
      class={className()}
      aria-hidden={props.title === undefined ? "true" : undefined}
      role={props.title === undefined ? undefined : "img"}
      style={{ "flex-shrink": 0, display: "block" }}
      // The paths are static strings from the generated module, never user input.
      // eslint-disable-next-line solid/no-innerhtml
      innerHTML={(props.title === undefined ? "" : `<title>${props.title}</title>`) + LK_ICON_PATHS[props.name]}
    />
  );
}
