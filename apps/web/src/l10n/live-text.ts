// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A message held in a signal as the way to render it rather than as rendered
 * text, so a notice or an error already on screen follows a language switch
 * (ADR-0058, `docs/ux/localization.md`).
 *
 * The setter takes either a function (`() => t("key", args)`), which is
 * re-run on every read and so re-renders in the current language, or a plain
 * string, kept as it is. The getter returns the rendered text, or `null`.
 */
import { createSignal, type Accessor } from "solid-js";

/** How to render a message: a function re-run on each read, or fixed text. */
export type LiveTextInput = string | (() => string) | null;

/** A signal holding a message to render; see the module comment. */
export function createLiveText(): [Accessor<string | null>, (next: LiveTextInput) => void] {
  const [render, setRender] = createSignal<(() => string) | null>(null);
  const get = (): string | null => {
    const r = render();
    return r === null ? null : r();
  };
  const set = (next: LiveTextInput): void => {
    if (next === null) setRender(null);
    else if (typeof next === "string") setRender(() => () => next);
    else setRender(() => next);
  };
  return [get, set];
}
