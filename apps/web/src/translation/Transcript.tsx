// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX section 4 — the `Aa` transcript button on a voice-message row and a
 * video note, and the transcript block under it.
 *
 * The button's space is part of the row in every state
 * (`docs/ux/media-messages.md` section 2): when it is not offered it is still
 * in the tree, hidden and out of the tab order, the way the speed control is
 * reserved, so the row never moves when it appears or changes state.
 */

import { createSignal, Match, Show, Switch, type JSX } from "solid-js";

import { currentLanguage, t } from "../l10n";
import type { MessageRef, TranslationController } from "./controller";
import { languageName, TRANSCRIPT_PREVIEW_CHARS } from "./model";
import { LineButton, TranslationLine } from "./TranslationLine";

export interface TranscriptHost {
  readonly controller: TranslationController;
  readonly messageRef: MessageRef;
  /** Fetch and decrypt the file the way playing does; `null` when it is gone. */
  readonly loadAudio: () => Promise<Blob | null>;
}

/** The `Aa` button. `host` absent = nothing to offer; the space stays. */
export function TranscriptButton(props: { host: TranscriptHost | undefined }): JSX.Element {
  const offered = (): boolean => {
    const h = props.host;
    return h !== undefined && h.controller.transcriptOffered(h.messageRef.conversation, true);
  };
  const view = () => props.host?.controller.transcriptView(props.host.messageRef) ?? { phase: "none" as const };
  const open = (): boolean => view().phase === "shown";
  return (
    <button
      type="button"
      class="lakko-transcript-button"
      data-reserved={offered() ? undefined : "true"}
      data-state={view().phase}
      tabindex={offered() ? undefined : -1}
      aria-hidden={offered() ? undefined : "true"}
      aria-pressed={offered() ? open() : undefined}
      aria-label={open() ? t("transcript.button.hide") : t("transcript.button.show")}
      title={open() ? t("transcript.button.hide") : t("transcript.button.show")}
      disabled={!offered() || view().phase === "unavailable-file"}
      onClick={(e) => {
        e.stopPropagation();
        const h = props.host;
        if (h === undefined || !offered()) return;
        h.controller.toggleTranscript(h.messageRef, h.loadAudio);
      }}
    >
      <Show
        when={view().phase === "loading" || view().phase === "installing"}
        fallback={<span aria-hidden="true">Aa</span>}
      >
        <span class="lakko-spinner" aria-hidden="true" />
      </Show>
    </button>
  );
}

/** The block under the row: the transcript, or what happened instead. */
export function TranscriptBlock(props: { host: TranscriptHost | undefined }): JSX.Element {
  const [expanded, setExpanded] = createSignal(false);
  const view = () => props.host?.controller.transcriptView(props.host.messageRef) ?? { phase: "none" as const };
  const translation = () => {
    const h = props.host;
    return h === undefined ? { phase: "none" as const } : h.controller.textView("transcript-translation", h.messageRef);
  };
  const clamp = (text: string): string =>
    expanded() || text.length <= TRANSCRIPT_PREVIEW_CHARS
      ? text
      : `${text.slice(0, TRANSCRIPT_PREVIEW_CHARS).trimEnd()}…`;
  const retry = (): void => {
    const h = props.host;
    if (h !== undefined) h.controller.toggleTranscript(h.messageRef, h.loadAudio);
  };
  return (
    <Show when={props.host}>
      {(h) => (
        <Switch>
          <Match when={view().phase === "transcribing" ? view() : null}>
            {(v) => (
              <div class="lakko-transcript" role="status" aria-live="polite">
                <Show when={"partial" in v() && (v() as { partial: string }).partial.length > 0}>
                  <div class="lakko-transcript-text" dir="auto">
                    {(v() as { partial: string }).partial}
                  </div>
                </Show>
                <div class="lk-translate-line">{t("transcript.transcribing")}</div>
              </div>
            )}
          </Match>
          <Match when={view().phase === "shown" ? (view() as { text: string; language: string }) : null}>
            {(v) => {
              const shownText = (): string => {
                const tr = translation();
                return tr.phase === "translated" ? tr.text : v().text;
              };
              return (
                <div class="lakko-transcript">
                  <div class="lakko-transcript-text" dir="auto">
                    {clamp(shownText())}
                  </div>
                  <Show when={!expanded() && shownText().length > TRANSCRIPT_PREVIEW_CHARS}>
                    <LineButton label={t("transcript.show_more")} onClick={() => setExpanded(true)} />
                  </Show>
                  <Show
                    when={translation().phase !== "none"}
                    fallback={
                      <div class="lk-translate-line">
                        <span>{t("transcript.line")}</span>
                        {" · "}
                        <LineButton
                          label={t("transcript.hide")}
                          onClick={() => h().controller.hideTranscript(h().messageRef)}
                        />
                        {" · "}
                        <LineButton
                          label={t("translate.line.not_language", {
                            language: languageName(v().language, currentLanguage()),
                          })}
                          onClick={() => h().controller.openTranscriptPicker(h().messageRef, h().loadAudio)}
                        />
                      </div>
                    }
                  >
                    <TranslationLine
                      controller={h().controller}
                      slot="transcript-translation"
                      messageRef={h().messageRef}
                      text={v().text}
                      view={translation()}
                    />
                  </Show>
                </div>
              );
            }}
          </Match>
          <Match when={view().phase === "installing" ? (view() as { language: string }) : null}>
            {(v) => (
              <div class="lakko-transcript lk-translate-line" role="status" aria-live="polite">
                <span>
                  {t("translate.pack.downloading_unsized", {
                    language: languageName(v().language, currentLanguage()),
                  })}
                </span>
                {" · "}
                <LineButton
                  label={t("translate.pack.cancel")}
                  onClick={() => h().controller.cancelTranscriptInstall(h().messageRef)}
                />
              </div>
            )}
          </Match>
          <Match when={view().phase === "packs-blocked"}>
            <div class="lakko-transcript lk-translate-line" role="status">
              {t("translate.pack.blocked_anti_censorship")}
            </div>
          </Match>
          <Match when={view().phase === "no-speech"}>
            <div class="lakko-transcript lk-translate-line">{t("transcript.no_speech")}</div>
          </Match>
          <Match when={view().phase === "failed"}>
            <div class="lakko-transcript lk-translate-line">
              <span>{t("transcript.failed")}</span>
              {" · "}
              <LineButton label={t("translate.line.try_again")} onClick={retry} />
            </div>
          </Match>
          <Match when={view().phase === "unsupported" ? (view() as { language: string }) : null}>
            {(v) => (
              <div class="lakko-transcript lk-translate-line">
                {t("transcript.unavailable_source", { language: languageName(v().language, currentLanguage()) })}
              </div>
            )}
          </Match>
        </Switch>
      )}
    </Show>
  );
}
