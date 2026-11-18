// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 7.4 — who voted for what in a
 * non-anonymous poll. Read only. An anonymous poll has no such screen.
 */
import { For, onCleanup, onMount, type JSX } from "solid-js";

import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import type { PollResultSection } from "./types";
import "./bubbles.css";

export function PollResults(props: {
  readonly sections: readonly PollResultSection[];
  readonly onClose: () => void;
}): JSX.Element {
  let close: HTMLButtonElement | undefined;
  onMount(() => {
    queueMicrotask(() => close?.focus());
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      props.onClose();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });
  return (
    <section class="lk-article-screen" role="dialog" aria-modal="true" aria-labelledby="lk-poll-results-title">
      <header class="lk-article-bar">
        <h2 id="lk-poll-results-title" class="lk-article-bar-meta" style={{ margin: 0, "font-size": "17px" }}>
          {t("poll.results.title")}
        </h2>
        <button
          ref={close}
          type="button"
          class="lk-icon-button"
          aria-label={t("common.close")}
          title={t("common.close")}
          onClick={() => props.onClose()}
        >
          <Icon name="xmark" />
        </button>
      </header>
      <div class="lk-article-scroll">
        <div class="lk-poll-results lk-article-column">
          <For each={props.sections}>
            {(s) => (
              <section>
                <h3>
                  <span>{s.text}</span>
                  <span class="lk-ak-muted">{t("poll.votes", { n: s.count })}</span>
                </h3>
                <ul>
                  <For each={s.voters}>
                    {(v) => (
                      <li>
                        <Avatar label={v.name} id={v.keyB64} personKey={v.keyB64} size="sm" />
                        <span class="lk-ak-ellipsis">{v.name}</span>
                      </li>
                    )}
                  </For>
                </ul>
              </section>
            )}
          </For>
        </div>
      </div>
    </section>
  );
}
