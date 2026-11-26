// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX section 6 — Settings > Language > Translation and transcription.
 *
 * Per device and never sent anywhere. Where this browser cannot do one of
 * the two at all, its rows are replaced by one line (`in this browser`), with
 * no action and no suggestion to go elsewhere (UX section 7).
 */

import { createResource, createSignal, For, Show, type JSX } from "solid-js";

import { currentLanguage, t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon } from "../ui/icons";
import { LakkoPicker } from "../ui/LakkoPicker";
import { CANDIDATE_LANGUAGES, languageName } from "./model";
import { translationRules } from "./rules";
import { packList, translationSettings, updateTranslationSettings, type PackRecord } from "./settings";
import { formatPackSize, languageListText } from "./pack-copy";
import { useTranslation } from "./context";

function SwitchRow(props: { title: string; on: boolean; onToggle: () => void }): JSX.Element {
  return (
    <div class="lk-card-row">
      <div class="lk-card-row-body">
        <span class="lk-card-row-title">{props.title}</span>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={props.on}
        aria-label={props.title}
        class="lk-card-row-action"
        classList={{ primary: props.on }}
        onClick={() => props.onToggle()}
      >
        {props.on ? t("common.on") : t("common.off")}
      </button>
    </div>
  );
}

/** The languages offered for the reading list and the target: the candidate
 *  set, by the interface-language name. */
function languageOptions(exclude: readonly string[]): { code: string; name: string }[] {
  const ui = currentLanguage();
  const r = translationRules();
  return CANDIDATE_LANGUAGES.filter((c) => !exclude.some((e) => (r === null ? e === c : r.sameLanguage(e, c))))
    .map((code) => ({ code, name: languageName(code, ui) }))
    .sort((a, b) => a.name.localeCompare(b.name, ui));
}

function packUseLabel(p: PackRecord): string {
  const tr = p.uses.includes("translation");
  const sp = p.uses.includes("transcription");
  if (tr && sp) return t("settings.translation.pack.for_both");
  return tr ? t("settings.translation.pack.for_translation") : t("settings.translation.pack.for_transcription");
}

export function TranslationSettingsSection(): JSX.Element {
  const controller = useTranslation();
  const s = translationSettings;
  const [adding, setAdding] = createSignal(false);
  const translationPresent = (): boolean => controller?.translationPresent === true;
  const transcriptionPresent = (): boolean => controller?.transcriptionPresent === true;
  const ui = (): string => currentLanguage();
  // The packs the operating system reports holding (the desktop app on
  // macOS); asked again when the target changes, as iOS does.
  const [systemPacks] = createResource(
    () => (controller === undefined ? null : (s().target ?? ui())),
    (target) => controller?.systemPacks(target) ?? Promise.resolve(null),
  );
  const packs = (): PackRecord[] => {
    const r = translationRules();
    return packList(
      s().packs,
      systemPacks.latest ?? null,
      (a, b) => (r === null ? a === b : r.sameLanguage(a, b)),
      (tag) => r?.canonicalLanguageTag(tag) ?? tag,
      (tag) => languageName(tag, ui()),
    );
  };

  // Language groups Lakko's own engine downloaded (ADR-0100): listed from
  // the engine, each with its size and Remove.
  const [ownPacks, { refetch: refetchOwnPacks }] = createResource(
    () => controller,
    (c) => c.ownPacks(),
  );
  async function removeOwn(id: string): Promise<void> {
    await controller?.removeOwnPack(id).catch((err: unknown) => {
      console.warn("[translation] removing a pack failed", err instanceof Error ? err.name : "unknown");
    });
    void refetchOwnPacks();
  }

  async function deleteAll(): Promise<void> {
    if (controller === undefined) return;
    const count = await controller.count();
    const ok = await requestConfirm({
      title: t("settings.translation.delete_all.title"),
      body: t("settings.translation.delete_all.body", { count }),
      confirmLabel: t("settings.translation.delete_all.confirm"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (ok) await controller.deleteAll();
  }

  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.translation.title")}</h1>
      </div>
      <section class="lk-card">
        <Show
          when={translationPresent()}
          fallback={<p class="lk-card-desc">{t("settings.translation.unavailable.translation_web")}</p>}
        >
          <SwitchRow
            title={t("settings.translation.show_translate")}
            on={s().showTranslate}
            onToggle={() => updateTranslationSettings((x) => ({ ...x, showTranslate: !x.showTranslate }))}
          />
        </Show>
      </section>
      <Show when={translationPresent()}>
      <section class="lk-card">
        <h2>{t("settings.translation.reading")}</h2>
        <div class="lk-card-row">
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{languageName(ui(), ui())}</span>
          </div>
        </div>
        <For each={s().extraReading}>
          {(code) => (
            <div class="lk-card-row">
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">{languageName(code, ui())}</span>
              </div>
              <button
                type="button"
                class="lk-card-row-action"
                aria-label={t("settings.translation.reading.remove", { language: languageName(code, ui()) })}
                onClick={() =>
                  updateTranslationSettings((x) => ({ ...x, extraReading: x.extraReading.filter((l) => l !== code) }))
                }
              >
                <Icon name="minus-circle" size={20} />
              </button>
            </div>
          )}
        </For>
        <Show
          when={adding()}
          fallback={
            <button type="button" class="ghost small" onClick={() => setAdding(true)}>
              {t("settings.translation.reading.add")}
            </button>
          }
        >
          <LakkoPicker
            ariaLabel={t("settings.translation.reading.add")}
            value=""
            placeholder={t("settings.translation.reading.add")}
            options={languageOptions([ui(), ...s().extraReading]).map((o) => ({ value: o.code, label: o.name }))}
            onChange={(v) => {
              setAdding(false);
              if (v.length === 0) return;
              updateTranslationSettings((x) =>
                x.extraReading.includes(v) ? x : { ...x, extraReading: [...x.extraReading, v] },
              );
            }}
          />
        </Show>
      </section>
      </Show>
      <Show when={translationPresent()}>
        <section class="lk-card">
          <div class="lk-card-row" style={{ "flex-wrap": "wrap" }}>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t("settings.translation.target")}</span>
            </div>
            <LakkoPicker
              ariaLabel={t("settings.translation.target")}
              value={s().target ?? ""}
              options={[
                { value: "", label: languageName(ui(), ui()) },
                ...languageOptions([ui()]).map((o) => ({ value: o.code, label: o.name })),
              ]}
              onChange={(v) => updateTranslationSettings((x) => ({ ...x, target: v.length === 0 ? null : v }))}
            />
          </div>
        </section>
      </Show>
      <section class="lk-card">
        <Show
          when={transcriptionPresent()}
          fallback={<p class="lk-card-desc">{t("settings.translation.unavailable.transcription_web")}</p>}
        >
          <SwitchRow
            title={t("settings.translation.show_transcript")}
            on={s().showTranscript}
            onToggle={() => updateTranslationSettings((x) => ({ ...x, showTranscript: !x.showTranscript }))}
          />
        </Show>
      </section>
      <Show when={translationPresent() || transcriptionPresent()}>
        {/* Only where Lakko itself downloads a pack and can tell a mobile
            connection (UX section 6); in the desktop app it governs nothing. */}
        <Show when={controller?.mobileDataMatters() === true}>
          <section class="lk-card">
            <SwitchRow
              title={t("settings.translation.mobile_data")}
              on={s().mobileData}
              onToggle={() => updateTranslationSettings((x) => ({ ...x, mobileData: !x.mobileData }))}
            />
          </section>
        </Show>
        <section class="lk-card">
          <h2>{t("settings.translation.packs")}</h2>
          <Show
            when={packs().length > 0 || (ownPacks.latest ?? []).length > 0}
            fallback={<p class="lk-card-desc">{t("settings.translation.packs.empty")}</p>}
          >
            <For each={packs()}>
              {(p) => (
                <div class="lk-card-row">
                  <div class="lk-card-row-body">
                    <span class="lk-card-row-title">{languageName(p.language, ui())}</span>
                    <span class="lk-card-row-sub">
                      {packUseLabel(p)} · {t("settings.translation.pack.managed.web")}
                    </span>
                  </div>
                </div>
              )}
            </For>
            <For each={ownPacks.latest ?? []}>
              {(p) => (
                <div class="lk-card-row">
                  <div class="lk-card-row-body">
                    <span class="lk-card-row-title">{languageListText(p.languages, ui())}</span>
                    <span class="lk-card-row-sub">
                      {t("settings.translation.pack.for_translation")} · {formatPackSize(p.bytes, ui())}
                    </span>
                  </div>
                  <button type="button" class="lk-card-row-action" onClick={() => void removeOwn(p.id)}>
                    {t("settings.translation.pack.remove")}
                  </button>
                </div>
              )}
            </For>
          </Show>
        </section>
      </Show>
      <section class="lk-card">
        <button type="button" class="danger" onClick={() => void deleteAll()}>
          {t("settings.translation.delete_all")}
        </button>
        <p class="lk-card-desc">{t("settings.translation.footer")}</p>
      </section>
    </>
  );
}
