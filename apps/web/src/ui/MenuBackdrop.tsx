// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The layer behind an open `.lk-menu`: a click outside the menu closes it, and
 * so does Escape (a menu that only a pointer can dismiss traps a keyboard
 * user; seen on the chat-row menu 2026-11-10).
 */
import { onCleanup, onMount, type JSX } from "solid-js";

export function MenuBackdrop(props: {
  onClose: () => void;
  /** A right-click outside closes the menu too, instead of opening the browser's. */
  closeOnContextMenu?: boolean;
}): JSX.Element {
  onMount(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      props.onClose();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });
  return (
    <div
      class="lk-menu-backdrop"
      onClick={() => props.onClose()}
      onContextMenu={(e) => {
        if (props.closeOnContextMenu !== true) return;
        e.preventDefault();
        props.onClose();
      }}
    />
  );
}
