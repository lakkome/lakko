// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A layer over the current screen, which stays where it is behind it
 * (`docs/ux/conversation-info.md` section 3).
 *
 * The web idiom the spec names: a centred card with a maximum width, dimmed
 * behind, dismissed by the backdrop or `Esc`. Before this, the conversation
 * info was a section that pushed the messages sideways - which reads as the
 * screen breaking rather than as something opening, and is the reason a
 * click on the header felt like nothing happened.
 *
 * ## Why a native `<dialog>`
 *
 * The same reason `ConfirmDialog` uses one: `showModal()` gives the focus
 * trap, the inert background and the `Esc` handling for free, and
 * hand-rolling those is how they end up subtly wrong. The visual vocabulary
 * is the existing `--surface-*` / `--radius-*` variables, so this is that
 * convention in a correct container rather than a third convention.
 *
 * No rules live here. `vitest.config.ts` runs a bare `node` environment with
 * no jsdom and no Solid transform, so nothing in a `.tsx` is reachable from
 * a test; anything that decides something belongs in a plain module beside
 * it (for the conversation info, `chat/conversation-info.ts`).
 */

import { type Component, type JSX, Show, createEffect, onCleanup } from "solid-js";

import { Icon } from "./icons";
import { t } from "../l10n";

export interface ModalLayerProps {
  readonly open: boolean;
  /** The heading, which is also the layer's accessible name. */
  readonly title: string;
  /** Backdrop, `Esc`, and the close button all arrive here. */
  readonly onClose: () => void;
  readonly children: JSX.Element;
  /** An extra class on the dialog (a bottom sheet on a phone width). */
  readonly class?: string;
}

let nextLayerId = 0;

export const ModalLayer: Component<ModalLayerProps> = (props) => {
  let el: HTMLDialogElement | undefined;
  nextLayerId += 1;
  const titleId = `lk-layer-title-${nextLayerId}`;

  // Open / close follows the caller's flag rather than a local one, so there
  // is a single source of truth for "is the layer on screen".
  createEffect(() => {
    const open = props.open;
    if (el === undefined) return;
    if (!open) {
      if (el.open) el.close();
      return;
    }
    if (!el.open) el.showModal();
  });

  // Unmounting with the layer open (navigating away while it is up) must not
  // leave a modal dialog behind.
  onCleanup(() => {
    if (el?.open === true) el.close();
  });

  return (
    <dialog
      ref={el}
      class={props.class === undefined ? "lk-layer" : `lk-layer ${props.class}`}
      aria-labelledby={titleId}
      onClose={() => props.onClose()}
      onCancel={(e) => {
        // `Esc` fires `cancel` then `close`; letting the default run closes
        // the element, and `close` is where the one answer is given.
        e.preventDefault();
        el?.close();
      }}
      onClick={(e) => {
        // A native `<dialog>` reports a backdrop click as a click on the
        // dialog ELEMENT, so the padding lives on the inner wrapper: with
        // padding here, a click on the card's own margin would dismiss.
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <Show when={props.open}>
        <div class="lk-layer-card">
          <header class="lk-layer-head">
            <h2 id={titleId}>{props.title}</h2>
            <button
              type="button"
              class="lk-icon-button"
              title={t("common.close")}
              aria-label={t("common.close")}
              onClick={() => props.onClose()}
            >
              <Icon name="close" size={20} />
            </button>
          </header>
          <div class="lk-layer-body">{props.children}</div>
        </div>
      </Show>
    </dialog>
  );
};
