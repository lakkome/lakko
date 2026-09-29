// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The auth screens' frame: the brand panel with the gradient on the start
 * side, the form card on a light-indigo ground on the end side (design
 * system §1, "Auth screen"). Login, Signup and Recover render their forms as
 * children.
 *
 * The language button sits in the top corner of the form side
 * (`docs/ux/localization.md` section 1): a person who cannot read English
 * must be able to switch before creating an account. It opens the same list
 * as Settings > Language, in a layer.
 */
import { createSignal, type JSX } from "solid-js";
import { currentLanguage, supportedLanguages, t } from "../l10n";
import { LanguagePicker } from "../l10n/LanguagePicker";
import { Icon } from "./icons";
import { ModalLayer } from "./ModalLayer";

export function AuthLayout(props: { children: JSX.Element }): JSX.Element {
  const [languageOpen, setLanguageOpen] = createSignal(false);
  const ownName = (): string =>
    supportedLanguages().find((l) => l.code === currentLanguage())?.ownName ?? "";

  return (
    <div class="auth-shell">
      <section class="auth-brand" aria-hidden="true">
        <div class="auth-brand-logo">
          <img src="/icon-192.png" alt="" width="44" height="44" />
          <span>Lakko</span>
        </div>
        <div>
          <h2>{t("auth.brand.tagline")}</h2>
          <p>{t("auth.brand.body")}</p>
        </div>
        <div class="auth-brand-foot">
          <Icon name="lock" size={16} />
          <span>{t("auth.brand.foot")}</span>
        </div>
      </section>
      <section class="auth-side">
        <button
          type="button"
          class="ghost small auth-language"
          aria-label={t("auth.language.button")}
          title={t("auth.language.button")}
          onClick={() => setLanguageOpen(true)}
        >
          <Icon name="globe" size={18} />
          <bdi lang={currentLanguage()}>{ownName()}</bdi>
        </button>
        <div class="auth-card">{props.children}</div>
      </section>
      <ModalLayer
        open={languageOpen()}
        title={t("settings.language.title")}
        onClose={() => setLanguageOpen(false)}
      >
        <LanguagePicker onChosen={() => setLanguageOpen(false)} />
      </ModalLayer>
    </div>
  );
}
