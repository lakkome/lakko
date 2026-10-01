// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A catalog message with parts rendered as elements (a bold name, a link),
 * so a translator still sees one whole sentence and can move the part
 * anywhere in it (ADR-0061).
 *
 * Each element argument is formatted as a marker the text can never contain
 * (private-use characters), then the output is split on those markers. The
 * Unicode isolates the formatter puts around a text argument stay around the
 * element, which is where they belong.
 */
import type { JSX } from "solid-js";
import { t, type L10nArgs } from "./index";

const OPEN = "";
const CLOSE = "";

/** Render `key` with `nodes[name]` in place of each `{name}` it names. */
export function tRich(
  key: string,
  nodes: Readonly<Record<string, JSX.Element>>,
  args?: L10nArgs,
): JSX.Element {
  const merged: Record<string, string | number> = { ...(args ?? {}) };
  for (const name of Object.keys(nodes)) merged[name] = `${OPEN}${name}${CLOSE}`;
  const text = t(key, merged);
  const parts = text.split(new RegExp(`${OPEN}([^${CLOSE}]*)${CLOSE}`, "u"));
  // `split` with a capture group alternates text and captured names.
  return parts.map((part, i) => (i % 2 === 1 ? nodes[part] ?? part : part));
}
