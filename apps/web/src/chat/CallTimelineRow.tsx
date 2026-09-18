// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A finished call, in the conversation (`docs/ux/calls.md` §6).
 *
 * Markup and nothing else: every rule it renders — the words, the arrow,
 * whether it counts as missed, and which kind of call a tap places back —
 * lives in `calls/call-log.ts`, where vitest can reach it. This file cannot be
 * tested at all (`vitest.config.ts` runs a plain `node` environment with no
 * jsdom and no Solid transform), which is exactly why it holds no decisions.
 *
 * ## Why a button and not a bubble
 *
 * A call is not something anyone said, so it is a centred system row like
 * "Members changed" rather than a message on one side of the timeline — and
 * `chat/timeline-layout.ts` already treats it as `side: "system"`, which
 * breaks the sender run around it. But unlike the other system rows it *does*
 * something: §6 says "Tapping the row calls back", so it is a real `<button>`
 * with a spoken label that says so, rather than a `<span>` with a click
 * handler nobody can reach from a keyboard.
 *
 * ## The same shape as the natives
 *
 * A centred pill, the call's own icon (video or phone, not a direction arrow),
 * and the one line — what `CallHistoryRow` draws in
 * `apps/android/.../ui/chat/ChatScreen.kt`. The icon says what KIND of call it
 * was; which way it went is in the words, where the natives put it too.
 *
 * ## Colour is never the signal
 *
 * A missed or declined call is tinted, and it also says `Missed call` or
 * `Declined` in words. §8: "nothing relies on colour alone".
 */
import type { JSX } from "solid-js";

import {
  callRowAccessibleLabel,
  callRowIsUnanswered,
  callRowLabel,
  type CallLogEntry,
} from "../calls/call-log";
import { Icon } from "../ui/icons";

export interface CallTimelineRowProps {
  readonly entry: CallLogEntry;
  /** Place the same kind of call again. */
  readonly onCallBack: (entry: CallLogEntry) => void;
  /** `false` once this conversation can no longer place a call (the
   *  membership ended, or a call is already up). The row still renders — the
   *  history is still true — it simply stops being a control. */
  readonly canCallBack: boolean;
}

export function CallTimelineRow(props: CallTimelineRowProps): JSX.Element {
  const label = (): string => callRowLabel(props.entry);
  return (
    <li class="lk-call-row" classList={{ unanswered: callRowIsUnanswered(props.entry) }}>
      <button
        type="button"
        class="lk-call-row-button"
        disabled={!props.canCallBack}
        aria-label={props.canCallBack ? callRowAccessibleLabel(props.entry) : label()}
        onClick={() => props.onCallBack(props.entry)}
      >
        <Icon name={props.entry.video ? "video" : "phone"} size={16} />
        <span>{label()}</span>
      </button>
    </li>
  );
}
