// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 4 / 4.1 — the pinned bar under the
 * chat header, and the list every pin opens as rows.
 *
 * The bar shows ONE pin: the author (or `For you` on a personal pin) and the
 * excerpt on one line, a thin stacked indicator beside it saying how many
 * there are and which this is, and a list control at the far end. Tapping
 * the bar is the view's business (`onTap`: jump to the message, then advance
 * to the next older pin, wrapping) — the order, the advance and the wrap are
 * `pin-bar.ts`, where a test runs them. This draws what it is handed.
 *
 * No rules live here (`vitest` runs the rule tests in bare node, so nothing
 * in a `.tsx` is reachable from one).
 */

import { t } from "../l10n";
import { For, Show, type JSX } from "solid-js";

import { Icon } from "../ui/icons";
import { ModalLayer } from "../ui/ModalLayer";
import { PIN_FOR_YOU, pinBarLabel, type PinBarItem } from "./pin-bar";

/** One pin with its text resolved by the view. */
export interface PinBarEntry extends PinBarItem {
  /** The author as the timeline names them; ignored for a personal pin,
   *  which reads `For you` instead. */
  readonly author: string;
  /** The message's text on one line, or the one-word stand-in. */
  readonly excerpt: string;
}

/** The label line: the author for a shared pin, `For you` for a personal one
 *  — so a message pinned only for me never looks like one the room sees. */
export function pinEntryLabel(entry: Pick<PinBarEntry, "scope" | "author">): string {
  return entry.scope === "personal" ? PIN_FOR_YOU() : entry.author;
}

/** The most segments the stacked indicator draws; past that the count is in
 *  the accessible name and the position in the highlighted segment. */
const MAX_INDICATOR_SEGMENTS = 4;

export function PinnedBar(props: {
  readonly entries: readonly PinBarEntry[];
  readonly index: number;
  readonly onTap: () => void;
  readonly onOpenList: () => void;
}): JSX.Element {
  const current = (): PinBarEntry | undefined => props.entries[props.index];
  const count = (): number => props.entries.length;
  const segments = (): number[] =>
    Array.from({ length: Math.min(count(), MAX_INDICATOR_SEGMENTS) }, (_, i) => i);
  /** Which segment is lit: the position, folded onto the drawn segments when
   *  there are more pins than segments. */
  const litSegment = (): number =>
    count() <= MAX_INDICATOR_SEGMENTS
      ? props.index
      : Math.min(MAX_INDICATOR_SEGMENTS - 1, Math.floor((props.index * MAX_INDICATOR_SEGMENTS) / count()));

  return (
    <Show when={current()}>
      {(entry) => (
        <div class="lk-pinned-bar" role="region" aria-label={pinBarLabel(props.index, count())}>
          <button
            type="button"
            class="lk-pinned-bar-main"
            title={pinBarLabel(props.index, count())}
            onClick={() => props.onTap()}
          >
            <span class="lk-pinned-bar-stack" aria-hidden="true">
              <For each={segments()}>
                {(i) => <span class="lk-pinned-bar-seg" classList={{ lit: i === litSegment() }} />}
              </For>
            </span>
            <span class="lk-pinned-bar-body">
              <span class="lk-pinned-bar-name" classList={{ personal: entry().scope === "personal" }}>
                {pinEntryLabel(entry())}
              </span>
              <span class="lk-pinned-bar-excerpt">{entry().excerpt}</span>
            </span>
          </button>
          <button
            type="button"
            class="lk-icon-button lk-pinned-bar-list"
            title={t("chat.pinned_bar.all")}
            aria-label={t("chat.pinned_bar.all")}
            onClick={() => props.onOpenList()}
          >
            <Icon name="lines" size={18} />
          </button>
        </div>
      )}
    </Show>
  );
}

/** Section 4.1 — every pin as rows; a row jumps to its message and closes
 *  the list. */
export function PinList(props: {
  readonly open: boolean;
  readonly entries: readonly PinBarEntry[];
  readonly onPick: (messageId: number) => void;
  readonly onClose: () => void;
}): JSX.Element {
  return (
    <ModalLayer open={props.open} title={t("chat.pinned_bar.list_title")} onClose={props.onClose}>
      <ul class="lk-list tight">
        <For each={props.entries}>
          {(entry) => (
            <li>
              <button
                type="button"
                class="lk-row lk-pinned-row"
                onClick={() => props.onPick(entry.messageId)}
              >
                <Icon name="pin" size={16} />
                <span class="lk-row-body">
                  <span class="lk-row-title" classList={{ personal: entry.scope === "personal" }}>
                    {pinEntryLabel(entry)}
                  </span>
                  <span class="lk-row-sub">{entry.excerpt}</span>
                </span>
              </button>
            </li>
          )}
        </For>
      </ul>
    </ModalLayer>
  );
}
