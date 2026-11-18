// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 7.1 — the poll editor. It returns a draft;
 * the conversation signs and sends it (ADR-0092 section 3.1).
 */
import { createMemo, createSignal, For, Index, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon } from "../ui/icons";
import { AttachScreen, AttachSwitch } from "./AttachScreen";
import {
  CLOSE_AFTER_CHOICES,
  isPollDraftValid,
  moved,
  movedTo,
  POLL_EXPLANATION_MAX,
  POLL_OPTION_MAX,
  POLL_OPTIONS_MAX,
  POLL_QUESTION_MAX,
  withoutEmptyAt,
  withTrailingEmpty,
} from "./poll-draft";
import type { PollAnonymityMode, PollDraft } from "./types";
import "./editors.css";
import { remapIndex, startDragReorder } from "./drag-reorder";

export function PollEditor(props: {
  readonly anonymity: PollAnonymityMode;
  readonly onCancel: () => void;
  readonly onCreate: (d: PollDraft) => Promise<void> | void;
}): JSX.Element {
  const [question, setQuestion] = createSignal("");
  const [options, setOptions] = createSignal<string[]>(["", ""]);
  const [anonymous, setAnonymous] = createSignal(props.anonymity !== "not-offered");
  const [multiple, setMultiple] = createSignal(false);
  const [quiz, setQuiz] = createSignal(false);
  /** Index into `options()` of the correct answer, quiz mode only. */
  const [correct, setCorrect] = createSignal<number | null>(null);
  const [explanation, setExplanation] = createSignal("");
  const [closeAuto, setCloseAuto] = createSignal(false);
  const [closeAfterMs, setCloseAfterMs] = createSignal(CLOSE_AFTER_CHOICES[1]?.ms ?? 86_400_000);
  const [busy, setBusy] = createSignal(false);

  const draft = createMemo((): PollDraft | null => {
    const raw = options();
    const filledIdx: number[] = [];
    raw.forEach((o, i) => {
      if (o.trim().length > 0) filledIdx.push(i);
    });
    const filled = filledIdx.map((i) => (raw[i] ?? "").trim());
    let quizPart: PollDraft["quiz"];
    if (quiz()) {
      const c = correct();
      const at = c === null ? -1 : filledIdx.indexOf(c);
      if (at < 0) return null;
      quizPart = { correct: at, explanation: explanation().trim() };
    }
    const d: PollDraft = {
      question: question().trim(),
      options: filled,
      multiple: quiz() ? false : multiple(),
      anonymous: props.anonymity === "always-anonymous" ? true : props.anonymity === "not-offered" ? false : anonymous(),
      ...(quizPart === undefined ? {} : { quiz: quizPart }),
      ...(closeAuto() ? { closeAfterMs: closeAfterMs() } : {}),
    };
    return isPollDraftValid(d) ? d : null;
  });

  const dirty = (): boolean =>
    question().trim().length > 0 || options().some((o) => o.trim().length > 0) || explanation().trim().length > 0;

  async function close(): Promise<void> {
    if (busy()) return;
    if (!dirty()) {
      props.onCancel();
      return;
    }
    const discard = await requestConfirm({
      title: t("poll.editor.discard"),
      body: null,
      confirmLabel: t("editor.discard"),
      cancelLabel: t("editor.keep_editing"),
      tone: "danger",
    });
    if (discard) props.onCancel();
  }

  async function create(): Promise<void> {
    const d = draft();
    if (d === null || busy()) return;
    setBusy(true);
    try {
      await props.onCreate(d);
    } finally {
      setBusy(false);
    }
  }

  function setOption(i: number, value: string): void {
    const next = [...options()];
    next[i] = value;
    setOptions(withTrailingEmpty(next, POLL_OPTIONS_MAX));
  }

  function leaveOption(i: number): void {
    const before = options();
    const after = withoutEmptyAt(before, i);
    if (after.length !== before.length) {
      const c = correct();
      if (c === i) setCorrect(null);
      else if (c !== null && c > i) setCorrect(c - 1);
    }
    setOptions(after);
  }

  function removeOption(i: number): void {
    const c = correct();
    if (c === i) setCorrect(null);
    else if (c !== null && c > i) setCorrect(c - 1);
    const rest = options().filter((_, j) => j !== i);
    setOptions(withTrailingEmpty(rest.length < 2 ? [...rest, ""] : rest, POLL_OPTIONS_MAX));
  }

  function move(i: number, to: number): void {
    setOptions(moved(options(), i, to));
    const c = correct();
    if (c === i) setCorrect(to);
    else if (c === to) setCorrect(i);
  }

  /** A drag (UX 7.1): the option moves, the quiz's correct mark follows it. */
  function dragTo(from: number, to: number): void {
    setOptions(movedTo(options(), from, to));
    const c = correct();
    if (c !== null) setCorrect(remapIndex(c, from, to));
  }
  let rowsEl: HTMLDivElement | undefined;

  /** Last filled index: the trailing empty field is not moved. */
  const lastMovable = (): number => {
    const o = options();
    return (o[o.length - 1] ?? "").trim().length === 0 ? o.length - 2 : o.length - 1;
  };

  return (
    <AttachScreen
      title={t("poll.editor.title")}
      onClose={() => void close()}
      footer={
        <button type="button" class="primary" disabled={draft() === null || busy()} onClick={() => void create()}>
          {t("poll.editor.create")}
        </button>
      }
    >
      <label class="lk-attach-label" for="lk-poll-question">
        {t("poll.editor.question")}
      </label>
      <input
        id="lk-poll-question"
        class="lk-attach-field"
        type="text"
        maxLength={POLL_QUESTION_MAX}
        value={question()}
        onInput={(e) => setQuestion(e.currentTarget.value)}
      />
      <p class="lk-attach-label" id="lk-poll-options">
        {t("poll.editor.options")}
      </p>
      <div ref={rowsEl} class="lk-attach-rows" role="group" aria-labelledby="lk-poll-options">
        <Index each={options()}>
          {(value, i) => (
            <div class="lk-attach-row" data-reorder-row="">
              <span
                class="lk-attach-grip"
                aria-hidden="true"
                classList={{ inert: i > lastMovable() }}
                onPointerDown={(e) => startDragReorder(e, rowsEl, i, lastMovable(), dragTo)}
              >
                <Icon name="drag-handle" size={16} />
              </span>
              <Show when={quiz()}>
                <input
                  class="lk-attach-radio"
                  type="radio"
                  name="lk-poll-correct"
                  aria-label={t("poll.editor.correct")}
                  checked={correct() === i}
                  disabled={value().trim().length === 0}
                  onChange={() => setCorrect(i)}
                />
              </Show>
              <input
                type="text"
                maxLength={POLL_OPTION_MAX}
                placeholder={t("poll.editor.option", { n: i + 1 })}
                aria-label={t("poll.editor.option", { n: i + 1 })}
                value={value()}
                onInput={(e) => setOption(i, e.currentTarget.value)}
                onBlur={() => leaveOption(i)}
              />
              <button
                type="button"
                class="lk-icon-button"
                aria-label={t("poll.editor.move_up")}
                title={t("poll.editor.move_up")}
                disabled={i === 0 || i > lastMovable()}
                onClick={() => move(i, i - 1)}
              >
                <Icon name="chevron-up" size={16} />
              </button>
              <button
                type="button"
                class="lk-icon-button"
                aria-label={t("poll.editor.move_down")}
                title={t("poll.editor.move_down")}
                disabled={i >= lastMovable()}
                onClick={() => move(i, i + 1)}
              >
                <Icon name="chevron-down" size={16} />
              </button>
              <button
                type="button"
                class="lk-icon-button"
                aria-label={t("poll.editor.remove_option", { n: i + 1 })}
                title={t("poll.editor.remove_option", { n: i + 1 })}
                disabled={i > lastMovable() || options().length <= 2}
                onClick={() => removeOption(i)}
              >
                <Icon name="minus-circle" size={16} />
              </button>
            </div>
          )}
        </Index>
      </div>

      <Show when={props.anonymity === "switch-default-on"}>
        <AttachSwitch
          title={t("poll.editor.anonymous")}
          sub={anonymous() ? t("poll.editor.anonymous_on") : t("poll.editor.anonymous_off")}
          on={anonymous()}
          onChange={setAnonymous}
        />
      </Show>
      <Show when={props.anonymity === "not-offered"}>
        <p class="lk-attach-note">{t("poll.editor.anonymous_off")}</p>
      </Show>
      <AttachSwitch
        title={t("poll.editor.multiple")}
        on={!quiz() && multiple()}
        disabled={quiz()}
        onChange={setMultiple}
      />
      <AttachSwitch
        title={t("poll.editor.quiz")}
        on={quiz()}
        onChange={(on) => {
          setQuiz(on);
          if (on) setMultiple(false);
        }}
      />
      <Show when={quiz()}>
        <label class="lk-attach-label" for="lk-poll-explanation">
          {t("poll.editor.explanation")}
        </label>
        <input
          id="lk-poll-explanation"
          class="lk-attach-field"
          type="text"
          maxLength={POLL_EXPLANATION_MAX}
          placeholder={t("common.optional")}
          value={explanation()}
          onInput={(e) => setExplanation(e.currentTarget.value)}
        />
      </Show>
      <AttachSwitch title={t("poll.editor.close_auto")} on={closeAuto()} onChange={setCloseAuto} />
      <Show when={closeAuto()}>
        <div class="lk-attach-segments" role="radiogroup" aria-label={t("poll.editor.close_auto")}>
          <For each={CLOSE_AFTER_CHOICES}>
            {(c) => (
              <button
                type="button"
                role="radio"
                aria-checked={closeAfterMs() === c.ms}
                onClick={() => setCloseAfterMs(c.ms)}
              >
                {t(c.key)}
              </button>
            )}
          </For>
        </div>
      </Show>
    </AttachScreen>
  );
}
