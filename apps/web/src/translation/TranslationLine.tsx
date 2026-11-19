// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX 3.2 / 3.3 / 3.5 — the one quiet line under a translated bubble (or a
 * transcript), in the bubble's secondary text colour.
 *
 * It is what marks machine output as such: a translation is never drawn
 * without it, and the original is one tap away on it.
 */

import { Match, Show, Switch, type JSX } from "solid-js";

import { currentLanguage, t } from "../l10n";
import type { MessageRef, TextSlot, TextView, TranslationController } from "./controller";
import { languageName } from "./model";

/** A text button the size of the line it sits in. */
export function LineButton(props: { label: string; onClick: () => void }): JSX.Element {
  return (
    <button
      type="button"
      class="lk-translate-line-action"
      onClick={(e) => {
        e.stopPropagation();
        props.onClick();
      }}
    >
      {props.label}
    </button>
  );
}

const SEP = " · ";

export function TranslationLine(props: {
  controller: TranslationController;
  slot: TextSlot;
  messageRef: MessageRef;
  /** The original text, which a retry or a re-pick translates. */
  text: string;
  view: TextView;
}): JSX.Element {
  const name = (tag: string): string => languageName(tag, currentLanguage());
  const c = (): TranslationController => props.controller;
  const notLanguage = (source: string | null): JSX.Element => (
    <Show when={source}>
      {(s) => (
        <>
          {SEP}
          <LineButton
            label={t("translate.line.not_language", { language: name(s()) })}
            onClick={() => c().openSourcePicker(props.slot, props.messageRef, props.text)}
          />
        </>
      )}
    </Show>
  );
  return (
    <Show when={props.view.phase !== "none"}>
      <div class="lk-translate-line" role="status" aria-live="polite">
        <Switch>
          <Match when={props.view.phase === "translating" ? props.view : null}>
            {(v) => (
              <>
                <span>{t("translate.line.translating")}</span>
                {notLanguage(v().source)}
              </>
            )}
          </Match>
          <Match when={props.view.phase === "translated" ? props.view : null}>
            {(v) => (
              <>
                <span>
                  {props.slot === "transcript-translation"
                    ? t("transcript.line.translated", { language: name(v().source) })
                    : t("translate.line.translated", { language: name(v().source) })}
                </span>
                {SEP}
                <LineButton
                  label={t("translate.line.show_original")}
                  onClick={() => c().showOriginal(props.slot, props.messageRef)}
                />
                {notLanguage(v().source)}
              </>
            )}
          </Match>
          <Match when={props.view.phase === "original" ? props.view : null}>
            {(v) => (
              <>
                <span>{name(v().source)}</span>
                {SEP}
                <LineButton
                  label={t("translate.line.translate")}
                  onClick={() => c().translate(props.slot, props.messageRef, props.text, v().source)}
                />
              </>
            )}
          </Match>
          <Match when={props.view.phase === "detected" ? props.view : null}>
            {(v) => (
              <>
                <span>{name(v().source)}</span>
                <Show when={v().canTranslate}>
                  {SEP}
                  <LineButton
                    label={t("translate.line.translate")}
                    onClick={() => c().translate(props.slot, props.messageRef, props.text)}
                  />
                </Show>
              </>
            )}
          </Match>
          <Match when={props.view.phase === "failed" ? props.view : null}>
            {(v) => (
              <>
                <span>{t("translate.line.failed")}</span>
                {SEP}
                <LineButton
                  label={t("translate.line.try_again")}
                  onClick={() =>
                    c().translate(props.slot, props.messageRef, props.text, v().source ?? undefined)
                  }
                />
              </>
            )}
          </Match>
          <Match when={props.view.phase === "same" ? props.view : null}>
            {(v) => <span>{t("translate.line.same_language", { language: name(v().language) })}</span>}
          </Match>
          <Match when={props.view.phase === "unsupported" ? props.view : null}>
            {(v) => <span>{t("translate.unsupported_source", { language: name(v().language) })}</span>}
          </Match>
          <Match when={props.view.phase === "downloading" ? props.view : null}>
            {(v) => (
              <>
                <span>
                  {v().percent === null
                    ? t("translate.pack.downloading_unsized", { language: name(v().language) })
                    : t("translate.pack.downloading", { language: name(v().language), percent: v().percent ?? 0 })}
                </span>
                {SEP}
                <LineButton
                  label={t("translate.pack.cancel")}
                  onClick={() => c().cancel(props.slot, props.messageRef)}
                />
              </>
            )}
          </Match>
          <Match when={props.view.phase === "waiting-wifi" ? props.view : null}>
            {(v) => (
              <>
                <span>{t("translate.pack.waiting_wifi", { language: name(v().language) })}</span>
                {SEP}
                <LineButton
                  label={t("translate.pack.cancel")}
                  onClick={() => c().cancel(props.slot, props.messageRef)}
                />
              </>
            )}
          </Match>
          <Match when={props.view.phase === "pack-failed" ? props.view : null}>
            {(v) => (
              <>
                <span>{t("translate.pack.failed", { language: name(v().language) })}</span>
                {SEP}
                <LineButton
                  label={t("translate.line.try_again")}
                  onClick={() => c().translate(props.slot, props.messageRef, props.text, v().language)}
                />
              </>
            )}
          </Match>
        </Switch>
      </div>
    </Show>
  );
}
