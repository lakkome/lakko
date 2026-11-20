// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 7.1 — the poll editor. It returns a draft;
 * the conversation signs and sends it (ADR-0092 section 3.1).
 */
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { AttachScreen, AttachSwitch, AttachSwitchGroup } from "./AttachScreen";
import { EditorRows } from "./EditorRows";
import {
  CLOSE_AFTER_CHOICES,
  isPollDraftValid,
  movedTo,
  POLL_EXPLANATION_MAX,
  POLL_OPTION_MAX,
  POLL_OPTIONS_MAX,
  POLL_OPTIONS_MIN,
  POLL_QUESTION_MAX,
  withAddedRow,
  withoutRow,
} from "./poll-draft";
import type { PollAnonymityMode, PollDraft } from "./types";
import "./editors.css";
import { remapIndex } from "./drag-reorder";

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
    setOptions(next);
  }

  function removeOption(i: number): void {
    const before = options();
    const after = withoutRow(before, i, POLL_OPTIONS_MIN);
    if (after.length === before.length) return;
    const c = correct();
    if (c === i) setCorrect(null);
    else if (c !== null && c > i) setCorrect(c - 1);
    setOptions(after);
  }

  /** A drag or `Alt+Up` / `Alt+Down` (UX 7.1a): the option moves, the quiz's
   *  correct mark follows it. */
  function moveOption(from: number, to: number): void {
    setOptions(movedTo(options(), from, to));
    const c = correct();
    if (c !== null) setCorrect(remapIndex(c, from, to));
  }

  return (
    <AttachScreen
      compact
      title={t("poll.editor.title")}
      onClose={() => void close()}
      footer={
        <button type="button" class="primary" disabled={draft() === null || busy()} onClick={() => void create()}>
          {t("poll.editor.create")}
        </button>
      }
    >
      <input
        class="lk-attach-field"
        type="text"
        maxLength={POLL_QUESTION_MAX}
        placeholder={t("poll.editor.question")}
        aria-label={t("poll.editor.question")}
        value={question()}
        onInput={(e) => setQuestion(e.currentTarget.value)}
      />
      <EditorRows
        label={t("poll.editor.options")}
        items={options()}
        min={POLL_OPTIONS_MIN}
        max={POLL_OPTIONS_MAX}
        maxLength={POLL_OPTION_MAX}
        placeholder={(i) => t("poll.editor.option", { n: i + 1 })}
        removeLabel={(i) => t("poll.editor.remove_option", { n: i + 1 })}
        addLabel={t("poll.editor.add_option")}
        onInput={setOption}
        onAdd={() => setOptions(withAddedRow(options(), "", POLL_OPTIONS_MAX))}
        onRemove={removeOption}
        onMove={moveOption}
        leadingShown={quiz()}
        leading={(i) => (
          <Show when={quiz()}>
            <input
              class="lk-attach-radio"
              type="radio"
              name="lk-poll-correct"
              aria-label={t("poll.editor.correct")}
              checked={correct() === i}
              disabled={(options()[i] ?? "").trim().length === 0}
              onChange={() => setCorrect(i)}
            />
          </Show>
        )}
      />

      <AttachSwitchGroup>
        <Show when={props.anonymity === "switch-default-on"}>
          <AttachSwitch
            title={t("poll.editor.anonymous")}
            sub={anonymous() ? t("poll.editor.anonymous_on") : t("poll.editor.anonymous_off")}
            on={anonymous()}
            onChange={setAnonymous}
          />
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
        <AttachSwitch title={t("poll.editor.close_auto")} on={closeAuto()} onChange={setCloseAuto} />
        <Show when={closeAuto()}>
          <div class="lk-attach-switch-extra">
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
          </div>
        </Show>
      </AttachSwitchGroup>
      <Show when={props.anonymity === "not-offered"}>
        <p class="lk-attach-note">{t("poll.editor.anonymous_off")}</p>
      </Show>
      <Show when={quiz()}>
        <p class="lk-attach-section">{t("poll.editor.explanation")}</p>
        <input
          class="lk-attach-field"
          type="text"
          maxLength={POLL_EXPLANATION_MAX}
          placeholder={t("common.optional")}
          aria-label={t("poll.editor.explanation")}
          value={explanation()}
          onInput={(e) => setExplanation(e.currentTarget.value)}
        />
      </Show>
    </AttachScreen>
  );
}
