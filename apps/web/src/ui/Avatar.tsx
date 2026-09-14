// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Initials avatar. The tint is one of four, chosen by a hash of the id so the
 * same person always gets the same colour on every device.
 */
import type { JSX } from "solid-js";

/** Up to two initials from a display label. */
export function initialsOf(label: string): string {
  const trimmed = label.replace(/^@/, "").trim();
  if (trimmed.length === 0) return "?";
  const parts = trimmed.split(/\s+/).filter((p) => p.length > 0);
  if (parts.length >= 2) {
    return ((parts[0]?.[0] ?? "") + (parts[1]?.[0] ?? "")).toUpperCase();
  }
  return trimmed.slice(0, 2).toUpperCase();
}

/** One of the four avatar tints, stable per id. */
export function toneOf(id: string): 1 | 2 | 3 | 4 {
  let h = 0;
  for (let i = 0; i < id.length; i += 1) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return ((h % 4) + 1) as 1 | 2 | 3 | 4;
}

export function Avatar(props: {
  /** What the initials come from. */
  label: string;
  /** What the tint comes from (defaults to the label). */
  id?: string;
  size?: "sm" | "md" | "lg" | "xl";
  class?: string;
}): JSX.Element {
  return (
    <span
      class={`lk-avatar ${props.size ?? ""} ${props.class ?? ""}`}
      data-tone={toneOf(props.id ?? props.label)}
      aria-hidden="true"
    >
      {initialsOf(props.label)}
    </span>
  );
}
