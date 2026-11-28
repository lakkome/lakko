// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether the person is using the pointer or the keyboard right now, as an
 * attribute on `<html>` the stylesheet can read.
 *
 * A message bubble is a tab stop and shows a ring on `:focus-visible`. The
 * browsers disagree on when that matches: Safari does not focus a button on a
 * click, so a click on a control inside a bubble (Translate, a reaction) lands
 * the focus on the bubble itself, and Safari then draws the ring as if the
 * keyboard had put it there. Seen in Safari on test.lakko.me, 2026-11-27;
 * Chromium browsers do not do it.
 *
 * The rule is decided here instead of by each engine's heuristic: after a
 * pointer press the attribute is present and the bubble rings are suppressed;
 * the next key press removes it.
 */

/** Attribute on `<html>` while the last input was a pointer press. */
export const POINTER_INPUT_ATTR = "data-pointer-input";

/** The next modality after an event of `type`; `current` for any other event. */
export function nextPointerInput(type: string, current: boolean): boolean {
  if (type === "pointerdown" || type === "mousedown" || type === "touchstart") return true;
  if (type === "keydown") return false;
  return current;
}

/** Track the modality on `doc`. Returns the remover. */
export function installInputModality(doc: Document = document): () => void {
  const root = doc.documentElement;
  const apply = (ev: Event): void => {
    const pointer = nextPointerInput(ev.type, root.hasAttribute(POINTER_INPUT_ATTR));
    if (pointer) root.setAttribute(POINTER_INPUT_ATTR, "");
    else root.removeAttribute(POINTER_INPUT_ATTR);
  };
  const types = ["pointerdown", "mousedown", "touchstart", "keydown"] as const;
  for (const t of types) doc.addEventListener(t, apply, { capture: true, passive: true });
  return () => {
    for (const t of types) doc.removeEventListener(t, apply, { capture: true });
  };
}
