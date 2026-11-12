// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The 12-word recovery phrase, read-only: numbered words on the sunken
 * surface, reading across the rows (3 columns x 4 rows at phone width, 4 x 3
 * on a wide screen) and a Copy button. `highlight` marks positions with an
 * outline and a dot, so the mark does not rely on colour (no caller passes
 * one at the moment). The layout rule lives in
 * `phrase-grid.ts`.
 */
import { createSignal, For, Show, type JSX } from "solid-js";
import { numberPhraseWords, phraseForClipboard } from "./phrase-grid";
import { Icon } from "./icons";
import { t } from "../l10n";

export function PhraseGrid(props: {
  readonly words: () => readonly string[];
  readonly highlight?: () => readonly number[];
  /** Show the Copy button (default true). */
  readonly copy?: boolean;
}): JSX.Element {
  const [copied, setCopied] = createSignal<"done" | "failed" | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;

  async function onCopy(): Promise<void> {
    if (timer !== null) clearTimeout(timer);
    try {
      await navigator.clipboard.writeText(phraseForClipboard(props.words()));
      setCopied("done");
    } catch {
      setCopied("failed");
    }
    timer = setTimeout(() => setCopied(null), 2000);
  }

  return (
    <div class="lk-phrase-block">
      <ol class="lk-phrase-cols" aria-label={t("recovery_phrase.label")}>
        <For each={numberPhraseWords(props.words())}>
          {(entry) => (
            <li classList={{ pick: props.highlight?.().includes(entry.position) === true }}>
              <span class="lk-phrase-num" aria-hidden="true">
                {entry.position}
              </span>
              <span class="lk-phrase-word">{entry.word}</span>
            </li>
          )}
        </For>
      </ol>
      <Show when={props.copy !== false}>
        <div class="lk-actions">
          <button type="button" class="small" onClick={() => void onCopy()}>
            <Icon name="copy" size={20} />
            {copied() === "done" ? t("common.copied") : t("common.copy")}
          </button>
          <Show when={copied() === "failed"}>
            <span class="form-error">{t("recovery_phrase.copy_failed")}</span>
          </Show>
        </div>
      </Show>
    </div>
  );
}
