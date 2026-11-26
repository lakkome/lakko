// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One `TranslationController` per session, and the two surfaces it can put
 * on screen from anywhere: the pack dialog (UX 3.5) and the language picker
 * (UX 3.4).
 *
 * `useTranslation()` returns `undefined` outside the provider, which is every
 * component test: a bubble rendered without it offers no translation and no
 * transcript button (the button's space stays reserved).
 */

import { createEffect, For, on, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { currentLanguage, t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { ModalLayer } from "../ui/ModalLayer";
import { deviceEngines } from "./device-engines";
import { watchCaptures } from "./capture-watch";
import { TranslationContext } from "./context";
import { TranslationController } from "./controller";
import { languageName } from "./model";
import { IdbResultBackend } from "./result-idb";
import { formatPackSize, languageListText } from "./pack-copy";
import { ResultStore } from "./result-store";
import { orderPickerLanguagesFor, translationRules } from "./rules";
import { translationSettings, updateTranslationSettings, withPack } from "./settings";

export { useTranslation } from "./context";

export function TranslationProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  const store = new ResultStore(new IdbResultBackend(getDeviceKey));
  const controller = new TranslationController({
    engines: deviceEngines(),
    store,
    rules: translationRules,
    accountId,
    interfaceLanguage: currentLanguage,
    settings: translationSettings,
    recordPack: (language, use) => updateTranslationSettings((s) => withPack(s, language, use)),
    blockTranscription: () => updateTranslationSettings((s) => ({ ...s, transcriptionBlocked: true })),
  });
  // Every capture this app starts is seen by a running transcription
  // (`capture-watch.ts`).
  watchCaptures();
  // Another account on this browser sees nothing of the previous one's.
  createEffect(on(accountId, () => controller.reset(), { defer: true }));
  onCleanup(() => store.dispose());

  return (
    <TranslationContext.Provider value={controller}>
      {props.children}
      <PackDialog controller={controller} />
      <SourcePicker controller={controller} />
    </TranslationContext.Provider>
  );
}

/**
 * UX 3.5 — "Download Spanish for translation?", in Lakko's own dialog
 * (`requestConfirm`, the design system's `LakkoDialog`): the title, one body
 * of two sentences (the on-device sentence and where the pack comes from),
 * `Not now` first and the confirming `Download` (or `Download on Wi-Fi`).
 * A browser reports no pack size, so its dialog has no size; Lakko's own
 * engine names its language groups and their size (ADR-0100 section 4).
 *
 * The dialog resolves after the press, inside the person's transient
 * activation, which is what lets the browser start the download.
 */
function PackDialog(props: { controller: TranslationController }): JSX.Element {
  createEffect(
    on(
      () => props.controller.packRequest(),
      (r) => {
        if (r === null) return;
        const name = languageName(r.language, currentLanguage());
        // macOS downloads its translation languages in System Settings: the
        // dialog says where and opens it (ADR-0082 note of 2026-11-18).
        // Lakko's own engine downloads language groups from Lakko's server
        // and says which and how big (ADR-0100 section 4).
        const groups = r.disclosure?.groups ?? [];
        const body = groups.length > 0
          ? lakkoPackBody(name, groups, currentLanguage())
          : r.systemSettings
          ? t("translate.pack.body.translation_system_settings", { language: name })
          : (r.use === "transcription"
              ? t("translate.pack.body.transcription", { language: name })
              : t("translate.pack.body.translation", { language: name })) +
            " " +
            t("translate.pack.source.web");
        void requestConfirm({
          title:
            r.use === "transcription"
              ? t("translate.pack.title.transcription", { language: name })
              : t("translate.pack.title.translation", { language: name }),
          body,
          confirmLabel: r.systemSettings
            ? t("translate.pack.open_system_settings")
            : r.wifiOnly
              ? t("translate.pack.download_wifi")
              : t("translate.pack.download"),
          cancelLabel: t("translate.pack.not_now"),
          tone: "neutral",
        }).then((ok) => {
          // Only the request this dialog was raised for is answered.
          if (props.controller.packRequest() !== r) return;
          if (ok) r.download();
          else r.dismiss();
        });
      },
    ),
  );
  return null;
}

/** The UX 3.5 body for Lakko's own engine: the group or two groups to
 *  download, their size, and that Lakko's server learns which group. */
function lakkoPackBody(
  language: string,
  groups: readonly { readonly languages: readonly string[]; readonly bytes: number }[],
  ui: string,
): string {
  const size = formatPackSize(groups.reduce((n, g) => n + g.bytes, 0), ui);
  const [first, second] = groups.map((g) => languageListText(g.languages, ui));
  const need =
    second === undefined
      ? t("translate.pack.body.lakko", { language, languages: first ?? "", size })
      : t("translate.pack.body.lakko_two", { language, first: first ?? "", second, size });
  return need + " " + t("translate.pack.source.lakko");
}

/** UX 3.4 — the languages this device can translate from (or transcribe),
 *  each by its own name with the interface-language name underneath. */
function SourcePicker(props: { controller: TranslationController }): JSX.Element {
  const request = () => props.controller.pickerRequest();
  const languages = (): string[] => {
    const r = request();
    if (r === null) return [];
    const ui = currentLanguage();
    const sorted = props.controller
      .pickerLanguages(r.use)
      .map((code) => ({ code, name: languageName(code, ui) }))
      .sort((a, b) => a.name.localeCompare(b.name, ui))
      .map((x) => x.code);
    // `Not Spanish?`: the message's other close candidates first (UX 3.4).
    const rules = translationRules();
    return rules === null ? sorted : orderPickerLanguagesFor(rules, sorted, r.candidates);
  };
  return (
    <ModalLayer open={request() !== null} title={t("translate.picker.title")} onClose={() => request()?.dismiss()}>
      <Show when={request()}>
        {(r) => (
          <div class="lk-lang-list" role="radiogroup" aria-label={t("translate.picker.title")}>
            <For each={languages()}>
              {(code) => (
                <button
                  type="button"
                  role="radio"
                  class="lk-lang-row"
                  aria-checked={r().current === code}
                  onClick={() => r().choose(code)}
                >
                  <span class="lk-lang-text">
                    <span class="lk-lang-title">
                      <bdi lang={code}>{languageName(code, code)}</bdi>
                    </span>
                    <span class="lk-lang-sub">{languageName(code, currentLanguage())}</span>
                  </span>
                </button>
              )}
            </For>
          </div>
        )}
      </Show>
    </ModalLayer>
  );
}
