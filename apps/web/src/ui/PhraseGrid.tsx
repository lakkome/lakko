// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The 24-word recovery phrase, read-only: numbered words in two columns on
 * the sunken surface (1..12 down the left, 13..24 down the right) and a
 * Copy button. `highlight` marks positions (the ADR-0049 vault positions on
 * the signup screen). The column rule lives in `phrase-grid.ts`.
 */
import { createSignal, For, Show, type JSX } from "solid-js";
import { chunkPhraseColumns, phraseForClipboard } from "./phrase-grid";
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
      <div class="lk-phrase-cols" aria-label={t("recovery_phrase.label")}>
        <For each={chunkPhraseColumns(props.words())}>
          {(column) => (
            <ol>
              <For each={column}>
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
          )}
        </For>
      </div>
      <Show when={props.copy !== false}>
        <div class="lk-actions">
          <button type="button" class="small" onClick={() => void onCopy()}>
            <Icon name="copy" size={16} />
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
