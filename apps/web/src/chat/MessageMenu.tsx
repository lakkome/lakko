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
 * ones that only read (Copy, Translate - ADR-0082 - then Select - section 8), then the ones that
 * change this message (Pin, then Edit), then the destructive one, separated
 * and in the platform's destructive colour. Reply is offered on any message
 * with an id, which is every message this menu opens on; Forward on any such
 * message whose send is complete (`canForward` - section 5.1: a message whose
 * attachments are still uploading, or that failed to send, does not offer
 * it); Select on the same gate (`canSelect` - section 8 reads the gates the
 * single actions compute, once for the whole selection). The separation of
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

import { accountSuspended } from "../auth/suspension-state";
import { t } from "../l10n";
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { messageMenuLayout, SURFACE_GAP_PX, type MessageMenuLayout } from "./message-menu-placement";
import { Icon, type IconName } from "../ui/icons";

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
export { SURFACE_GAP_PX };

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
  /** `docs/ux/reactions-and-emoji.md` section 5 — the six emoji of the row,
   *  from `quickReactionRow` (the quick reaction, recent ones, the defaults,
   *  narrowed to what the conversation takes). Empty: no row, the card alone. */
  reactionRow: readonly string[];
  /** The emoji this person already gave the message (drawn on the accent-soft
   *  circle; pressing one takes it off). */
  ownReactions: readonly string[];
  /** Whether the trailing expand control is shown (`rowHasExpand`). */
  showExpand: boolean;
  /** The person holds the most reactions one person gives (3): the others are
   *  dimmed and do nothing, with the line under the row. */
  capReached: boolean;
  /** ADR-0052 — arm the composer's reply strip for this message. */
  onReply: () => void;
  /** ADR-0087 section 12 — `false` in a channel, where a post takes no reply
   *  (a subscriber writes a comment instead). Absent: offered. */
  canReply?: boolean;
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
  /** ADR-0065 section 2 — the `Save` / `Remove from saved` row under Pin,
   *  or `null` when not offered. */
  saveLabel?: "save" | "unsave" | null;
  onSave?: () => void;
  onCopy: (text: string) => void;
  /** ADR-0082 / translation UX 3.1 — the row after Copy, in the group of
   *  actions that only read: `Translate`, `Show original` on a message
   *  already showing a translation, or `null` when this device cannot
   *  translate it (the shared rule `translationOffered` decided). */
  translateLabel?: "translate" | "show-original" | null;
  onTranslate?: () => void;
  /** Section 8 — whether the `Select` row is offered: the same gate as
   *  Forward (an id, a complete send; the menu never opens on a deleted
   *  message). */
  canSelect: boolean;
  /** Section 8 — enter selection mode with this message already selected. */
  onSelect: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onClose: () => void;
  /** `false` hides Edit while Delete stays (a channel comment cannot be
   *  edited). Absent: Edit goes with Delete. */
  canEdit?: boolean;
  /** ADR-0092 — actions of a message's content, first in the card: `Open in
   *  OpenStreetMap` and `Copy coordinates` on a location, `Retract vote` and
   *  `Close poll` on a poll, `Edit checklist` / `Close checklist`, `Edit` on
   *  an article. */
  extraActions?: readonly {
    readonly label: string;
    readonly icon: IconName;
    readonly onClick: () => void;
  }[];
  /** Further destructive actions after Delete (a channel comment's `Ban`). */
  extraDestructive?: readonly {
    readonly label: string;
    readonly icon: IconName;
    readonly onClick: () => void;
  }[];
  /** ADR-0073 R.5 — a row of an OLDER segment of the conversation: the menu
   *  is the pin row alone (`pin-bar.ts` `historyRowOffersUnpin`). No
   *  reaction row and no other action: nothing else can name that row. */
  pinOnly?: boolean;
  /** `docs/ux/connection-state.md` section 7.3 — an unsent message (waiting
   *  or `Not sent`): the menu is the Delete row alone. Nothing else can name
   *  a message that has no id yet. */
  deleteOnly?: boolean;
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
  /** The reaction row is drawn: not on an older segment's row, and not when
   *  the conversation takes no reactions. Without it the card takes the
   *  row's single-surface slot. */
  const hasRow = (): boolean =>
    props.pinOnly !== true &&
    props.deleteOnly !== true &&
    props.reactionRow.length > 0 &&
    !accountSuspended();
  /** Only the restricted rows: an older segment's pin row, or an unsent
   *  message's Delete. */
  const restricted = (): boolean => props.pinOnly === true || props.deleteOnly === true;

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
        aria-label={t("chat.message.actions")}
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
        <Show when={hasRow()}>
        <div
          ref={row}
          class="lk-msg-menu-surface lk-msg-menu-reactions"
          role="group"
          aria-label={t("chat.message.react")}
          style={offset((l) => l.row)}
        >
          <div class="lk-msg-menu-reaction-row">
            <For each={props.reactionRow}>
              {(emoji, i) => {
                const mine = (): boolean => props.ownReactions.includes(emoji);
                const dimmed = (): boolean => props.capReached && !mine();
                return (
                  <button
                    ref={(el) => {
                      if (i() === 0) firstItem = el;
                    }}
                    type="button"
                    role="menuitem"
                    classList={{ mine: mine(), dimmed: dimmed() }}
                    aria-disabled={dimmed() ? "true" : undefined}
                    aria-label={
                      mine()
                        ? t("chat.reaction.chip_yours_a11y", { emoji })
                        : t("chat.message.react_with", { emoji })
                    }
                    onClick={() => {
                      if (dimmed()) return;
                      run(() => props.onReact(emoji));
                    }}
                  >
                    {emoji}
                  </button>
                );
              }}
            </For>
            <Show when={props.showExpand}>
              <button
                type="button"
                role="menuitem"
                class="more"
                aria-label={t("chat.message.more_reactions")}
                title={t("chat.message.more_reactions")}
                onClick={() => run(() => props.onMoreReactions())}
              >
                <Icon name="plus" size={20} />
              </button>
            </Show>
          </div>
          <Show when={props.capReached}>
            <p class="lk-msg-menu-reaction-cap">{t("chat.reaction.cap_reached")}</p>
          </Show>
        </div>
        </Show>
        {/* The second surface. It always has at least the Reply row now
            (section 2's first band), so it is always rendered; the `Show`s
            inside still keep Copy off a message with no text and Edit /
            Delete off somebody else's message. */}
        {/* With `pinOnly` this card is the ONLY surface: it takes the
            placement's single-surface slot (measured and placed as `row`). */}
        <div
          ref={(el) => {
            if (!hasRow()) row = el;
            else card = el;
          }}
          class="lk-msg-menu-surface lk-msg-menu-card lk-msg-menu-items"
          style={offset((l) => (!hasRow() ? l.row : l.card))}
        >
            {/* ADR-0099 step 3: the content actions (a poll's Close, a
                checklist's Edit), React, Reply, Forward, Edit and Delete each
                send; a suspended account's menu does not offer them. */}
            <Show when={!restricted() && !accountSuspended()}>
              <For each={props.extraActions ?? []}>
                {(a) => (
                  <button type="button" role="menuitem" onClick={() => run(() => a.onClick())}>
                    <Icon name={a.icon} size={16} />
                    <span>{a.label}</span>
                  </button>
                )}
              </For>
            </Show>
            <Show when={!restricted() && props.canReply !== false && !accountSuspended()}>
            <button
              ref={(el) => {
                if (!hasRow()) firstItem = el;
              }}
              type="button"
              role="menuitem"
              onClick={() => run(() => props.onReply())}
            >
              <Icon name="reply" size={20} />
              <span>{t("chat.message.reply")}</span>
            </button>
            </Show>
            <Show when={!restricted() && props.canForward && !accountSuspended()}>
              <button type="button" role="menuitem" onClick={() => run(() => props.onForward())}>
                <Icon name="forward" size={20} />
                <span>{t("chat.message.forward")}</span>
              </button>
            </Show>
            <Show when={restricted() ? null : copyable()}>
              {(text) => (
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => run(() => props.onCopy(text()))}
                >
                  <Icon name="copy" size={20} />
                  <span>{t("common.copy")}</span>
                </button>
              )}
            </Show>
            <Show when={restricted() ? null : (props.translateLabel ?? null)}>
              {(label) => (
                <button type="button" role="menuitem" onClick={() => run(() => props.onTranslate?.())}>
                  <Icon name={label() === "show-original" ? "text-bubble" : "translate"} size={20} />
                  <span>
                    {label() === "show-original"
                      ? t("chat.message.show_original")
                      : t("chat.message.translate")}
                  </span>
                </button>
              )}
            </Show>
            <Show when={!restricted() && props.canSelect}>
              <button type="button" role="menuitem" onClick={() => run(() => props.onSelect())}>
                <Icon name="checkmark-circle" size={20} />
                <span>{t("chat.message.select")}</span>
              </button>
            </Show>
            <Show when={props.deleteOnly === true ? null : props.pinLabel}>
              {(label) => (
                <button
                  ref={(el) => {
                    if (props.pinOnly === true) firstItem = el;
                  }}
                  type="button"
                  role="menuitem"
                  onClick={() => run(() => props.onPin())}
                >
                  <Icon name={label() === "Pin" ? "pin" : "pin-slash"} size={20} />
                  <span>{label() === "Pin" ? t("chat.message.pin") : t("chat.message.unpin")}</span>
                </button>
              )}
            </Show>
            <Show when={props.deleteOnly === true ? null : props.saveLabel}>
              {(label) => (
                <button
                  ref={(el) => {
                    // An older segment's menu with no Unpin row starts here.
                    if (props.pinOnly === true && firstItem === undefined) firstItem = el;
                  }}
                  type="button"
                  role="menuitem"
                  onClick={() => run(() => props.onSave?.())}
                >
                  <Icon name={label() === "save" ? "bookmark" : "bookmark-slash"} size={20} />
                  <span>{label() === "save" ? t("chat.message.save") : t("chat.message.unsave")}</span>
                </button>
              )}
            </Show>
            <Show when={!restricted() && props.canEditDelete && !accountSuspended()}>
              <Show when={props.canEdit !== false}>
                <button type="button" role="menuitem" onClick={() => run(() => props.onEdit())}>
                  <Icon name="pencil" size={20} />
                  <span>{t("chat.message.edit")}</span>
                </button>
              </Show>
              <div class="lk-msg-menu-destructive">
                <button
                  type="button"
                  role="menuitem"
                  class="danger"
                  onClick={() => run(() => props.onDelete())}
                >
                  <Icon name="trash" size={20} />
                  <span>{t("common.delete")}</span>
                </button>
              </div>
            </Show>
            <Show when={props.deleteOnly === true}>
              <div class="lk-msg-menu-destructive">
                <button
                  ref={(el) => {
                    firstItem = el;
                  }}
                  type="button"
                  role="menuitem"
                  class="danger"
                  onClick={() => run(() => props.onDelete())}
                >
                  <Icon name="trash" size={20} />
                  <span>{t("common.delete")}</span>
                </button>
              </div>
            </Show>
            <Show when={!restricted() && (props.extraDestructive?.length ?? 0) > 0}>
              <div class="lk-msg-menu-destructive">
                <For each={props.extraDestructive ?? []}>
                  {(a) => (
                    <button type="button" role="menuitem" class="danger" onClick={() => run(() => a.onClick())}>
                      <Icon name={a.icon} size={20} />
                      <span>{a.label}</span>
                    </button>
                  )}
                </For>
              </div>
            </Show>
        </div>
      </div>
    </Portal>
  );
}
