// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/saved-messages-and-file-lifetime.md sections 4.1 and 4.2 — the
 * one way every media bubble says a file is gone or limited. The decision is
 * `chat/attachment-failure.ts`; this is the markup.
 *
 * `inline` is for a bubble that is itself a button (a photo, a video): the
 * note is plain text and the bubble's own click retries (or, for a gone
 * file, does nothing). Otherwise the limit state carries its own Try again.
 */
import { Match, Switch, type JSX } from "solid-js";

import { goneLines, openLimitText, type AttachmentFailure } from "../chat/attachment-failure";
import { t } from "../l10n";

export function AttachmentFailureNote(props: {
  readonly failure: AttachmentFailure;
  readonly onRetry?: () => void;
  readonly inline?: boolean;
  /** The generic failure, as the bubble always drew it. */
  readonly children: JSX.Element;
}): JSX.Element {
  return (
    <Switch fallback={props.children}>
      <Match when={props.failure === "gone"}>
        <span class="lk-attachment-gone" role="status">
          <span class="lk-attachment-gone-title">{goneLines().title}</span>
          <span class="lk-attachment-gone-body">{goneLines().body}</span>
        </span>
      </Match>
      <Match when={props.failure === "limit"}>
        <span class="lk-attachment-limit" role="status">
          <span>{openLimitText()}</span>
          {props.inline === true ? (
            <span class="lk-attachment-retry">{t("common.try_again")}</span>
          ) : (
            <button type="button" class="lk-attachment-retry" onClick={() => props.onRetry?.()}>
              {t("common.try_again")}
            </button>
          )}
        </span>
      </Match>
    </Switch>
  );
}
