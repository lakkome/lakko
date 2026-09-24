// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The per-message action menu, `docs/ux/client-redesign.md` section 5.2.
 *
 * It replaces a strip that was revealed on `:hover` and positioned inside the
 * bubble. Both halves of that were wrong and the second was the one the
 * maintainer hit: on a voice message the strip covered the play button, so
 * pressing play activated React and the message paused instead of playing.
 *
 * Two rules, and everything here follows from them.
 *
 * **It opens only on a deliberate act.** A pointer crossing a bubble on its
 * way somewhere else asks for nothing. On a pointer device that act is a
 * right-click (`contextmenu`, with the browser's own menu suppressed); from
 * the keyboard it is the context-menu key or `Shift+F10` on the focused
 * bubble, which browsers deliver as the same `contextmenu` event and which
 * this also handles directly for the ones that do not; on a touch screen it is
 * a long press. `:focus-within` is gone with hover for the same reason:
 * tabbing onto a voice message's waveform slider is navigation, not a request
 * for a menu, and revealing the strip there put it over the play button by
 * keyboard instead of by mouse.
 *
 * **It is rendered outside the bubble.** Not merely at different coordinates -
 * in a portal at the document root, positioned by `message-menu-placement.ts`,
 * which is given the two measured rectangles and refuses to return anything
 * that intersects the bubble except in the one case it names. That is what
 * makes "never overlaps the bubble's own controls" a property rather than a
 * number somebody has to keep re-checking when a new bubble kind lands.
 *
 * The menu carries BOTH halves the spec names - the reaction bar and the
 * actions - because one gesture opens both: "The reaction bar and the message
 * actions (Reply later, Edit, Delete, Copy) open on a deliberate gesture".
 * Before this the quick reaction row was a second step behind a React button,
 * and it was positioned inside the bubble too (`top: 1.2rem`), so it had the
 * same defect one press further in.
 *
 * A full-screen backdrop sits under it. It is what closes the menu on an
 * outside press, and it is also what stops a touch long-press's trailing
 * `click` from landing on whatever was under the finger.
 */

import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { REACTION_EMOJI } from "./message-ops";
import {
  messageMenuPlacement,
  type MenuAnchorRect,
  type MenuPlacement,
} from "./message-menu-placement";
import { Icon } from "../ui/icons";

/** Where the menu was asked for, and by what. */
export interface MenuAnchor {
  /** The bubble's rectangle at the moment of the gesture. */
  readonly bubble: MenuAnchorRect;
  /** The pointer, or the bubble's top-left corner for a keyboard trigger. */
  readonly x: number;
  readonly y: number;
  /** Which side of the row is empty: `right` beside an incoming bubble. */
  readonly away: "left" | "right";
}

/** Hold this long for a touch press to become the menu gesture. Matches the
 *  platform long-press the natives use; shorter and a scroll that starts with
 *  a pause opens a menu nobody asked for. */
export const LONG_PRESS_MS = 500;
/** How far a touch may drift and still be a long press rather than a scroll. */
export const LONG_PRESS_SLOP_PX = 10;

const GAP_PX = 8;
const MARGIN_PX = 8;

export interface MessageMenuProps {
  anchor: MenuAnchor;
  /** Edit and Delete are offered on our own messages only; React on any. */
  canEditDelete: boolean;
  onReact: (emoji: string) => void;
  onMoreReactions: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onClose: () => void;
}

export function MessageMenu(props: MessageMenuProps): JSX.Element {
  // The placement needs the menu's own size, which only exists once it has
  // rendered. So it renders hidden, is measured, and is then placed and shown.
  // `null` is the pre-measurement state and is what keeps a wrongly-positioned
  // first frame off the screen.
  const [placement, setPlacement] = createSignal<MenuPlacement | null>(null);
  let menu: HTMLDivElement | undefined;
  let firstItem: HTMLButtonElement | undefined;

  const measureAndPlace = (): void => {
    if (menu === undefined) return;
    const rect = menu.getBoundingClientRect();
    setPlacement(
      messageMenuPlacement({
        bubble: props.anchor.bubble,
        pointer: { x: props.anchor.x, y: props.anchor.y },
        menu: { width: rect.width, height: rect.height },
        viewport: {
          width: globalThis.innerWidth ?? rect.width,
          height: globalThis.innerHeight ?? rect.height,
        },
        away: props.anchor.away,
        gap: GAP_PX,
        margin: MARGIN_PX,
      }),
    );
  };

  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") {
      e.preventDefault();
      props.onClose();
    }
  };

  onMount(() => {
    measureAndPlace();
    // Focus moves into the menu so Escape and a screen reader both find it,
    // and so the keyboard path does not open a menu it cannot then reach.
    firstItem?.focus();
    globalThis.document?.addEventListener("keydown", onKey);
    // The anchor rectangle was taken at the gesture; a scroll or a resize
    // moves the bubble out from under it, and a menu pointing at where a
    // message used to be is worse than no menu.
    globalThis.addEventListener("scroll", props.onClose, true);
    globalThis.addEventListener("resize", props.onClose);
  });
  onCleanup(() => {
    globalThis.document?.removeEventListener("keydown", onKey);
    globalThis.removeEventListener("scroll", props.onClose, true);
    globalThis.removeEventListener("resize", props.onClose);
  });

  const run = (action: () => void): void => {
    action();
    props.onClose();
  };

  return (
    <Portal>
      <div
        class="lk-msg-menu-backdrop"
        onPointerDown={() => props.onClose()}
        onContextMenu={(e) => {
          // A second right-click closes rather than stacking the browser's own
          // menu on top of this one.
          e.preventDefault();
          props.onClose();
        }}
      />
      <div
        ref={menu}
        class="lk-msg-menu"
        role="menu"
        aria-label="Message actions"
        style={
          placement() === null
            ? { visibility: "hidden", left: "0px", top: "0px" }
            : { left: `${placement()?.left ?? 0}px`, top: `${placement()?.top ?? 0}px` }
        }
      >
        <div class="lk-msg-menu-reactions" role="group" aria-label="React">
          <For each={REACTION_EMOJI}>
            {(emoji, i) => (
              <button
                ref={(el) => {
                  if (i() === 0) firstItem = el;
                }}
                type="button"
                role="menuitem"
                aria-label={`React ${emoji}`}
                onClick={() => run(() => props.onReact(emoji))}
              >
                {emoji}
              </button>
            )}
          </For>
          <button
            type="button"
            role="menuitem"
            class="more"
            aria-label="More reactions"
            title="More reactions"
            onClick={() => run(() => props.onMoreReactions())}
          >
            <Icon name="plus" size={16} />
          </button>
        </div>
        <Show when={props.canEditDelete}>
          <div class="lk-msg-menu-items">
            <button type="button" role="menuitem" onClick={() => run(() => props.onEdit())}>
              <Icon name="pencil" size={16} />
              <span>Edit</span>
            </button>
            <button
              type="button"
              role="menuitem"
              class="danger"
              onClick={() => run(() => props.onDelete())}
            >
              <Icon name="trash" size={16} />
              <span>Delete</span>
            </button>
          </div>
        </Show>
      </div>
    </Portal>
  );
}
