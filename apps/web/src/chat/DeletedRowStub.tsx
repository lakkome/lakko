// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A deleted row in its hold (`docs/ux/message-actions.md` section 9), for
 * the lists that draw no bubble of their own for it: a channel post and a
 * comment. "Message deleted" in the row's own shape, no chips, no quote, no
 * attachments, no menu; the collapse starts when the phase turns to
 * `collapsing`.
 */
import { createEffect, on, type JSX } from "solid-js";

import { t } from "../l10n";
import { collapseRowHeight, type DeletedPhase } from "./deleted-rows";

export function DeletedRowStub(props: {
  /** The row's own classes (`lk-post lk-channel-post`, `lk-comment`). */
  readonly class: string;
  readonly phase: DeletedPhase;
}): JSX.Element {
  let el: HTMLLIElement | undefined;
  createEffect(
    on(
      () => props.phase,
      (phase) => {
        if (phase === "collapsing" && el !== undefined) collapseRowHeight(el);
      },
    ),
  );
  return (
    <li
      ref={el}
      class={props.class}
      classList={{ "deleted-hold": true, "deleted-collapsing": props.phase === "collapsing" }}
      aria-hidden={props.phase === "collapsing" ? "true" : undefined}
    >
      <div class="lk-bubble-deleted">{t("chat.message.deleted")}</div>
    </li>
  );
}
