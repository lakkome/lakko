// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` sections 7.2 and 7.5 — the poll card. It draws a
 * view the conversation already folded and reports the person's choice; it
 * knows nothing of ballots, rings or credentials.
 */
import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";
import { Icon } from "../ui/icons";
import {
  formatDuration,
  pollFooterStatus,
  pollKindKey,
  pollShowsResults,
} from "./poll-view";
import type { PollOptionView, PollView } from "./types";
import "./bubbles.css";

export function PollCard(props: {
  readonly view: PollView;
  readonly onVote: (choices: number[]) => void;
  readonly onRetry: () => void;
  readonly onViewResults: () => void;
}): JSX.Element {
  /** Multiple answers: the ticks collected before `Vote`. */
  const [ticked, setTicked] = createSignal<ReadonlySet<number>>(new Set());
  /** The option tapped last, which shows the spinner while sending. */
  const [tapped, setTapped] = createSignal<number | null>(null);
  // The footer's countdown re-reads the clock once a minute.
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 60_000);
  onCleanup(() => clearInterval(timer));

  const showResults = (): boolean => pollShowsResults(props.view);
  const interactive = (): boolean => props.view.canVote && !props.view.unverifiable && !showResults();

  const kindLine = (): string => {
    const base = t(pollKindKey(props.view.anonymous, props.view.quiz));
    return props.view.multiple ? `${base} · ${t("poll.kind.multiple")}` : base;
  };

  const pick = (index: number): void => {
    if (!interactive()) return;
    if (props.view.multiple) {
      const next = new Set(ticked());
      if (next.has(index)) next.delete(index);
      else next.add(index);
      setTicked(next);
      return;
    }
    setTapped(index);
    props.onVote([index]);
  };

  const submitMultiple = (): void => {
    const choices = [...ticked()].sort((a, b) => a - b);
    if (choices.length === 0) return;
    setTapped(choices[0] ?? null);
    props.onVote(choices);
    setTicked(new Set<number>());
  };

  const resultLabel = (o: PollOptionView): string => {
    const base = t("poll.option_result", { option: o.text, percent: o.percent, count: o.count });
    return o.mine ? `${base}, ${t("poll.your_choice")}` : base;
  };

  const optionClass = (o: PollOptionView): string => {
    if (!showResults() || !props.view.quiz) return "lk-poll-option";
    if (o.correct === true) return "lk-poll-option correct";
    if (o.mine) return "lk-poll-option wrong";
    return "lk-poll-option";
  };

  const footerStatus = (): JSX.Element => {
    const status = pollFooterStatus(props.view, now());
    switch (status.kind) {
      case "sending":
        return <span>{t("poll.sending_vote")}</span>;
      case "failed":
        return (
          <>
            <span class="lk-ak-error">{t("poll.vote_failed")}</span>
            <button type="button" class="lk-ak-button-link" onClick={() => props.onRetry()}>
              {t("poll.try_again")}
            </button>
          </>
        );
      case "closed":
        return <span>{t("poll.closed")}</span>;
      case "closes-in":
        return <span>{t("poll.closes_in", { time: formatDuration(status.remainingMs, displayLocale()) })}</span>;
      default:
        return null;
    }
  };

  return (
    <div class="lk-ak-card lk-poll" role="group" aria-label={props.view.question}>
      <div class="lk-poll-question">{props.view.question}</div>
      <div class="lk-poll-kind">{kindLine()}</div>
      <Show when={props.view.unverifiable}>
        <div class="lk-ak-error" role="note">{t("poll.unverifiable")}</div>
      </Show>
      <ul
        class="lk-poll-options"
        role={showResults() ? "list" : props.view.multiple ? "group" : "radiogroup"}
        aria-label={props.view.question}
      >
        <For each={props.view.options}>
          {(o, i) => (
            <li>
              <Show
                when={showResults()}
                fallback={
                  <button
                    type="button"
                    class="lk-poll-option"
                    role={props.view.multiple ? "checkbox" : "radio"}
                    aria-checked={props.view.multiple ? ticked().has(i()) : false}
                    disabled={!interactive()}
                    onClick={() => pick(i())}
                  >
                    <Show
                      when={props.view.sending && tapped() === i()}
                      fallback={
                        <span
                          class={`lk-poll-mark${props.view.multiple ? " box" : ""}${
                            props.view.multiple && ticked().has(i()) ? " on" : ""
                          }`}
                          aria-hidden="true"
                        >
                          <Show when={props.view.multiple && ticked().has(i())}>
                            <Icon name="check" size={12} strokeWidth={3} />
                          </Show>
                        </span>
                      }
                    >
                      <span class="lk-ak-spinner" aria-hidden="true" />
                    </Show>
                    <span class="lk-poll-text">{o.text}</span>
                  </button>
                }
              >
                <div class={optionClass(o)} aria-label={resultLabel(o)}>
                  <span class="lk-poll-bar" style={{ width: `${o.percent}%` }} aria-hidden="true" />
                  <Show when={props.view.sending && tapped() === i()}>
                    <span class="lk-ak-spinner" aria-hidden="true" />
                  </Show>
                  <span class="lk-poll-text" aria-hidden="true">{o.text}</span>
                  <Show when={o.mine}>
                    <Icon name="check" size={16} strokeWidth={2.4} class="lk-poll-check" />
                  </Show>
                  <span class="lk-poll-percent" aria-hidden="true">{`${o.percent}%`}</span>
                </div>
              </Show>
            </li>
          )}
        </For>
      </ul>
      <Show when={props.view.multiple && interactive() && ticked().size > 0}>
        <button type="button" class="lk-poll-vote-button" onClick={submitMultiple}>
          {t("poll.vote")}
        </button>
      </Show>
      <Show when={props.view.quiz && showResults() && (props.view.explanation ?? "").length > 0}>
        <div class="lk-poll-explanation" role="note">{props.view.explanation}</div>
      </Show>
      <div class="lk-poll-footer" aria-live="polite">
        <span>{t("poll.votes", { n: props.view.voters })}</span>
        {footerStatus()}
      </div>
      <Show when={props.view.canViewResults}>
        <button type="button" class="lk-ak-button-link" onClick={() => props.onViewResults()}>
          {t("poll.view_results")}
        </button>
      </Show>
    </div>
  );
}
