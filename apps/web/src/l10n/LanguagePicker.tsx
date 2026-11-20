// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The language list (`docs/ux/localization.md` section 3), shown by
 * Settings > Language and by the globe button on the auth screens.
 *
 * "Same as system" first, then every language by its own name with the
 * translated name underneath, sorted by the translated name. A tap switches
 * the whole interface at once, this list included. While a language file is
 * still loading after a moment the tapped row shows a spinner; if the load
 * fails the interface stays as it was and the row says so, and tapping again
 * retries. The rules are in `language-list.ts`.
 */
import { createMemo, createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { Icon } from "../ui/icons";
import {
  currentLanguage,
  selectedLanguage,
  selectLanguage,
  supportedLanguages,
  systemLanguage,
  t,
} from "./index";
import { languageRows, systemRowMatches } from "./language-list";

/** The key of the "Same as system" row in the pending / failed state. */
const SYSTEM = "";

/** How long a switch may take before its row shows a spinner. */
const SPINNER_DELAY_MS = 300;

export function LanguagePicker(props: { onChosen?: () => void }): JSX.Element {
  const [query, setQuery] = createSignal("");
  const [pending, setPending] = createSignal<string | null>(null);
  const [spinning, setSpinning] = createSignal(false);
  const [failed, setFailed] = createSignal<string | null>(null);
  let spinnerTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(spinnerTimer));

  const rows = createMemo(() =>
    languageRows(supportedLanguages(), t, query(), currentLanguage()),
  );
  const systemOwnName = (): string =>
    supportedLanguages().find((l) => l.code === systemLanguage())?.ownName ?? systemLanguage();
  const showSystemRow = (): boolean =>
    systemRowMatches(t("settings.language.same_as_system"), query(), currentLanguage());

  async function choose(key: string): Promise<void> {
    if (pending() !== null) return;
    setFailed(null);
    setPending(key);
    clearTimeout(spinnerTimer);
    spinnerTimer = setTimeout(() => setSpinning(true), SPINNER_DELAY_MS);
    try {
      await selectLanguage(key === SYSTEM ? null : key);
      props.onChosen?.();
    } catch {
      setFailed(key);
    } finally {
      clearTimeout(spinnerTimer);
      setSpinning(false);
      setPending(null);
    }
  }

  const isSelected = (key: string): boolean =>
    key === SYSTEM ? selectedLanguage() === null : selectedLanguage() === key;

  function Mark(p: { key: string }): JSX.Element {
    return (
      <span class="lk-lang-mark" aria-hidden="true">
        <Show
          when={pending() === p.key && spinning()}
          fallback={
            <Show when={isSelected(p.key)}>
              <Icon name="checkmark" size={20} />
            </Show>
          }
        >
          <span class="lakko-spinner" />
        </Show>
      </span>
    );
  }

  function Failure(p: { key: string }): JSX.Element {
    return (
      <Show when={failed() === p.key}>
        <span class="lk-lang-error" role="alert">
          {t("settings.language.load_failed")}
        </span>
      </Show>
    );
  }

  return (
    <div class="lk-lang">
      <label class="lk-lang-search">
        <Icon name="search" size={20} />
        <input
          type="search"
          dir="auto"
          placeholder={t("settings.language.search")}
          aria-label={t("settings.language.search")}
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
      </label>
      <div class="lk-lang-list" role="radiogroup" aria-label={t("settings.language.title")}>
        <Show when={showSystemRow()}>
          <button
            type="button"
            role="radio"
            class="lk-lang-row"
            aria-checked={isSelected(SYSTEM)}
            aria-busy={pending() === SYSTEM}
            onClick={() => void choose(SYSTEM)}
          >
            <span class="lk-lang-text">
              <span class="lk-lang-title">{t("settings.language.same_as_system")}</span>
              <span class="lk-lang-sub">
                {t("settings.language.same_as_system_detail", { language: systemOwnName() })}
              </span>
              <Failure key={SYSTEM} />
            </span>
            <Mark key={SYSTEM} />
          </button>
        </Show>
        <For each={rows()}>
          {(row) => (
            <button
              type="button"
              role="radio"
              class="lk-lang-row"
              aria-checked={isSelected(row.code)}
              aria-busy={pending() === row.code}
              onClick={() => void choose(row.code)}
            >
              <span class="lk-lang-text">
                {/* Isolated in its own direction but aligned with the list,
                    so an Arabic name in an English list stays on the start
                    side. */}
                <span class="lk-lang-title">
                  <bdi lang={row.code}>{row.ownName}</bdi>
                </span>
                <span class="lk-lang-sub">{row.translatedName}</span>
                <Failure key={row.code} />
              </span>
              <Mark key={row.code} />
            </button>
          )}
        </For>
        <Show when={rows().length === 0 && !showSystemRow()}>
          <p class="lk-lang-empty">{t("settings.language.no_results")}</p>
        </Show>
      </div>
    </div>
  );
}
