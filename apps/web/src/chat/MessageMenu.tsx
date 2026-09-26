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
 * **They are TWO surfaces, not one card.** `message-actions.md` section 2: "The
 * action card is sized by its own content. It is a separate surface from the
 * reaction row and must not inherit its width." They used to be one bordered
 * box, a flex column whose `align-items` was the default `stretch`, so the
 * action rows were as wide as seven emoji and a plus - a card three words long
 * drawn at the width of something else.
 *
 * Sizing them separately was not enough either. The next version drew two
 * cards with the same chrome, left-aligned 6px apart in one flex column, and
 * the maintainer still read one panel with an odd top row (2026-09-25). So
 * section 2 now asks for "separate floating surfaces - each with its own
 * background, border and shadow, and a visible gap between them": the row is a
 * pill (`.lk-msg-menu-reactions`), the card keeps the look the maintainer chose
 * (`.lk-msg-menu-card`, unchanged - the natives copy it), and each is placed on
 * its own by `messageMenuLayout`, which puts the row above the card, or below
 * it when there is no room above.
 *
 * The outer element is a positioner with no surface of its own. It is sized to
 * the footprint of both surfaces and the gap, so it still measures as the
 * union - which keeps "never overlaps the bubble" a statement about the whole
 * footprint wherever something measures the menu as one box - and it takes no
 * pointer events, so a press in the gap between the two surfaces reaches the
 * backdrop and closes the menu like any other press outside it.
 *
 * Order within the action card is section 2's: the actions that produce a NEW
 * message first (Reply, then Forward - ADR-0052 decisions 1 and 2), then the
 * ones that only read (Copy), then the ones that change this message (Edit),
 * then the destructive one, separated and in the platform's destructive
 * colour. Reply is offered on any message with an id, which is every message
 * this menu opens on; Forward on any such message whose send is complete
 * (`canForward` - section 5.1: a message whose attachments are still
 * uploading, or that failed to send, does not offer it). The separation of
 * the destructive row is its own element
 * rather than a border hung off `.danger`, because "separated" and "red" are
 * two of the spec's requirements and a selector that conflates them breaks
 * both at once when a second destructive action lands.
 *
 * A full-screen backdrop sits under it. It is what closes the menu on an
 * outside press, and it is also what stops a touch long-press's trailing
 * `click` from landing on whatever was under the finger. It is NOT a scrim:
 * section 7 gives web the pointer-anchored card and asks for the receding
 * backdrop on the two platforms whose menu replaces the screen.
 */

import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { REACTION_EMOJI } from "./message-ops";
import { messageMenuLayout, type MessageMenuLayout } from "./message-menu-placement";
import { Icon } from "../ui/icons";

/** The part of a DOM rect the anchor carries. Viewport coordinates. */
export interface MenuAnchorRect {
  readonly left: number;
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
}

/** Where the menu was asked for, and by what. */
export interface MenuAnchor {
  /** The bubble's rectangle at the moment of the gesture. Not consulted by
   *  the placement any more - the menu opens at the pointer, over the
   *  message - but a keyboard trigger has no pointer and takes its x/y from
   *  this rectangle's corner. */
  readonly bubble: MenuAnchorRect;
  /** The pointer, or the bubble's top-left corner for a keyboard trigger. */
  readonly x: number;
  readonly y: number;
}

/** Hold this long for a touch press to become the menu gesture. Matches the
 *  platform long-press the natives use; shorter and a scroll that starts with
 *  a pause opens a menu nobody asked for. */
export const LONG_PRESS_MS = 500;
/** How far a touch may drift and still be a long press rather than a scroll. */
export const LONG_PRESS_SLOP_PX = 10;
const MARGIN_PX = 8;
/** Between the reaction row and the action card. Section 2 asks for "a visible
 *  gap"; the 6px the two cards used to sit apart did not read as one. */
export const SURFACE_GAP_PX = 8;

export interface MessageMenuProps {
  anchor: MenuAnchor;
  /** Edit and Delete are offered on our own messages only; React on any. */
  canEditDelete: boolean;
  /** The text Copy would put on the clipboard, or `null` when this message has
   *  none - section 3 offers Copy on "any message with text", so a lone voice
   *  message or photo does not get the row at all. It is the text the person is
   *  looking at, which on an edited message is the edited one. */
  copyText: string | null;
  onReact: (emoji: string) => void;
  onMoreReactions: () => void;
  /** ADR-0052 — arm the composer's reply strip for this message. */
  onReply: () => void;
  /** ADR-0052 decision 2 — whether Forward is offered: the message has an id
   *  and its send is complete. */
  canForward: boolean;
  /** ADR-0052 decision 2 — open the "Forward to" picker for this message. */
  onForward: () => void;
  /** ADR-0052 decision 3 / section 4.1 — the `Pin` / `Unpin` row after
   *  Forward, or `null` when the message offers neither (its send is not
   *  complete). Which word is `pin-sheet.ts`'s decision, not the menu's. */
  pinLabel: "Pin" | "Unpin" | null;
  /** The pin row was chosen: the view runs `pinRowDecision` and either acts
   *  at once or opens the sheet. */
  onPin: () => void;
  onCopy: (text: string) => void;
  onEdit: () => void;
  onDelete: () => void;
  onClose: () => void;
}

export function MessageMenu(props: MessageMenuProps): JSX.Element {
  // The placement needs the menu's own size, which only exists once it has
  // rendered. So it renders hidden, is measured, and is then placed and shown.
  // `null` is the pre-measurement state and is what keeps a wrongly-positioned
  // first frame off the screen.
  const [layout, setLayout] = createSignal<MessageMenuLayout | null>(null);
  let row: HTMLDivElement | undefined;
  let card: HTMLDivElement | undefined;
  let firstItem: HTMLButtonElement | undefined;

  const measureAndPlace = (): void => {
    if (row === undefined) return;
    // Each surface is measured on its own, while hidden at the positioner's
    // origin - never where a previous menu stood, so the answer depends on the
    // gesture alone.
    const r = row.getBoundingClientRect();
    const c = card?.getBoundingClientRect();
    setLayout(
      messageMenuLayout({
        pointer: { x: props.anchor.x, y: props.anchor.y },
        row: { width: r.width, height: r.height },
        card: c === undefined ? null : { width: c.width, height: c.height },
        viewport: {
          width: globalThis.innerWidth ?? r.width,
          height: globalThis.innerHeight ?? r.height,
        },
        between: SURFACE_GAP_PX,
        margin: MARGIN_PX,
      }),
    );
  };

  /** A surface's offset inside the positioner, which sits at the footprint's
   *  top-left corner. Before measurement both surfaces sit at its origin. */
  const offset = (at: (l: MessageMenuLayout) => { left: number; top: number } | null) => {
    const l = layout();
    const p = l === null ? null : at(l);
    return l === null || p === null
      ? { left: "0px", top: "0px" }
      : { left: `${p.left - l.block.left}px`, top: `${p.top - l.block.top}px` };
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

  // ONE predicate for "there is something to copy", read both by the row and by
  // the card that holds it. Two spellings of it - `!== null` on the card and
  // truthiness on the row - differ on the empty string, and would draw an empty
  // bordered card for a caller that passed `""` rather than `null`.
  const copyable = (): string | null =>
    props.copyText !== null && props.copyText.length > 0 ? props.copyText : null;

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
        class="lk-msg-menu"
        role="menu"
        aria-label="Message actions"
        data-row-at={layout()?.rowAt}
        style={(() => {
          const l = layout();
          return l === null
            ? { visibility: "hidden", left: "0px", top: "0px" }
            : {
                left: `${l.block.left}px`,
                top: `${l.block.top}px`,
                width: `${l.block.width}px`,
                height: `${l.block.height}px`,
              };
        })()}
      >
        <div
          ref={row}
          class="lk-msg-menu-surface lk-msg-menu-reactions"
          role="group"
          aria-label="React"
          style={offset((l) => l.row)}
        >
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
        {/* The second surface. It always has at least the Reply row now
            (section 2's first band), so it is always rendered; the `Show`s
            inside still keep Copy off a message with no text and Edit /
            Delete off somebody else's message. */}
        <div
          ref={card}
          class="lk-msg-menu-surface lk-msg-menu-card lk-msg-menu-items"
          style={offset((l) => l.card)}
        >
            <button type="button" role="menuitem" onClick={() => run(() => props.onReply())}>
              <Icon name="reply" size={16} />
              <span>Reply</span>
            </button>
            <Show when={props.canForward}>
              <button type="button" role="menuitem" onClick={() => run(() => props.onForward())}>
                <Icon name="forward" size={16} />
                <span>Forward</span>
              </button>
            </Show>
            <Show when={props.pinLabel}>
              {(label) => (
                <button type="button" role="menuitem" onClick={() => run(() => props.onPin())}>
                  <Icon name="pin" size={16} />
                  <span>{label()}</span>
                </button>
              )}
            </Show>
            <Show when={copyable()}>
              {(text) => (
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => run(() => props.onCopy(text()))}
                >
                  <Icon name="copy" size={16} />
                  <span>Copy</span>
                </button>
              )}
            </Show>
            <Show when={props.canEditDelete}>
              <button type="button" role="menuitem" onClick={() => run(() => props.onEdit())}>
                <Icon name="pencil" size={16} />
                <span>Edit</span>
              </button>
              <div class="lk-msg-menu-destructive">
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
      </div>
    </Portal>
  );
}
