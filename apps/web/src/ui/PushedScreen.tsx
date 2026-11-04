// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A screen pushed over the view it was opened from, in that view's column,
 * with a back icon at the leading end of its top bar
 * (`docs/ux/conversation-info.md` section 3). Used by a channel's info; a
 * group's info renders the same classes itself, because it also keeps a
 * history entry (`chat/info-navigation.ts`).
 *
 * The parent must be positioned (`position: relative`): the screen covers it
 * exactly, and the view under it stays mounted and laid out. `Esc` goes back
 * too, unless a dialog of the screen's own is open over it.
 */
import { createEffect, on, onCleanup, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "./icons";

let nextScreenId = 0;

export function PushedScreen(props: {
  readonly open: boolean;
  readonly title: string;
  readonly onBack: () => void;
  readonly children: JSX.Element;
}): JSX.Element {
  nextScreenId += 1;
  const titleId = `lk-pushed-screen-title-${nextScreenId}`;
  let back: HTMLButtonElement | undefined;

  createEffect(
    on(
      () => props.open,
      (open) => {
        if (!open) return;
        queueMicrotask(() => back?.focus());
        const onKey = (e: KeyboardEvent): void => {
          if (e.key !== "Escape" || e.defaultPrevented) return;
          if (document.querySelector("dialog[open]") !== null) return;
          e.preventDefault();
          props.onBack();
        };
        document.addEventListener("keydown", onKey);
        onCleanup(() => document.removeEventListener("keydown", onKey));
      },
    ),
  );

  return (
    <Show when={props.open}>
      <section class="lk-info-screen" aria-labelledby={titleId}>
        <header class="lk-chat-head lk-info-head">
          <button
            ref={back}
            type="button"
            class="lk-icon-button"
            aria-label={t("common.back")}
            title={t("common.back")}
            onClick={() => props.onBack()}
          >
            <Icon name="back" size={20} />
          </button>
          <h1 id={titleId} class="lk-info-title">
            {props.title}
          </h1>
        </header>
        <div class="lk-info-scroll">{props.children}</div>
      </section>
    </Show>
  );
}
