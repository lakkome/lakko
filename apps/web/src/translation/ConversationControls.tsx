// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX 3.6 / 3.7 — the per-conversation surfaces: the "Messages here are in
 * Spanish." bar at the top of a conversation, and the `Translate
 * automatically` switch in its info screen. Both are settings of this device
 * only (UX open question 1).
 */

import { Show, type JSX } from "solid-js";

import { currentLanguage, t } from "../l10n";
import { Icon } from "../ui/icons";
import type { Detection } from "./engines";
import type { MessageRef, TranslationController } from "./controller";
import { languageName } from "./model";
import { translationRules } from "./rules";
import { useTranslation } from "./context";

/** One incoming message the bar's rule may read. */
export interface BarCandidate {
  readonly messageRef: MessageRef;
  readonly text: string;
}

/**
 * The recent incoming messages as the shared rule reads them, newest first,
 * at most the rule's window. A message whose language is not known yet is
 * passed as `und`, which the rule treats as nothing, so the window is the
 * window of messages and not of detections.
 */
export function recentDetections(
  controller: TranslationController,
  newestFirst: readonly BarCandidate[],
): Detection[] {
  const r = translationRules();
  const window = r === null ? 0 : r.translationBarWindow();
  return newestFirst
    .slice(0, window)
    .map((c) => controller.detected(c.messageRef, c.text) ?? { tag: "und", permille: 0 });
}

export function TranslateBar(props: {
  conversation: string;
  /** Newest first. */
  candidates: () => readonly BarCandidate[];
}): JSX.Element {
  const controller = useTranslation();
  const language = (): string | null =>
    controller === undefined
      ? null
      : controller.barLanguage(props.conversation, recentDetections(controller, props.candidates()));
  return (
    <Show when={controller !== undefined ? language() : null}>
      {(lang) => (
        <div class="lk-translate-bar" role="region" aria-label={t("translate.auto")}>
          <span class="lk-translate-bar-text">
            {t("translate.bar.body", { language: languageName(lang(), currentLanguage()) })}
          </span>
          <button
            type="button"
            class="primary small"
            onClick={() => controller?.enableAutoFromBar(props.conversation, lang())}
          >
            {t("translate.bar.translate")}
          </button>
          <button
            type="button"
            class="ghost small"
            onClick={() => controller?.dismissBar(props.conversation, lang())}
          >
            {t("translate.bar.not_now")}
          </button>
          <button
            type="button"
            class="ghost small"
            aria-label={t("translate.bar.close")}
            title={t("translate.bar.close")}
            onClick={() => controller?.dismissBar(props.conversation, lang())}
          >
            <Icon name="close" size={14} />
          </button>
        </div>
      )}
    </Show>
  );
}

/** UX 3.7 — `Translate automatically`, shown only where this device can
 *  translate at all and the person has the Translate button on. */
export function AutoTranslateSwitch(props: { conversation: string }): JSX.Element {
  const controller = useTranslation();
  const on = (): boolean => controller?.prefs(props.conversation).autoTranslate === true;
  return (
    <Show when={controller !== undefined && controller.translationPresent && controller.anyEngine()}>
      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="globe" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("translate.auto")}</span>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={on()}
            aria-label={t("translate.auto")}
            class="lk-card-row-action"
            classList={{ primary: on() }}
            onClick={() => controller?.setAutoTranslate(props.conversation, !on())}
          >
            {on() ? t("common.on") : t("common.off")}
          </button>
        </div>
      </section>
    </Show>
  );
}
