// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Account: how the account is protected
 * (docs/ux/identity-recovery.md sections 19.1, 19.1a, 19.2 and 19.5).
 *
 *   - the status line: "Protected by your email" / "Protected by your
 *     recovery phrase" / "Not protected";
 *   - one dismissible banner for an unprotected account without a passkey,
 *     "This account has no way back in.", which returns after 30 days;
 *   - the `Recovery email` row: the address on a device that holds it (the
 *     synced register, ADR-0075 as amended 2026-10-13), else On / Off, with
 *     Replace and Remove;
 *   - the `Recovery phrase` row: `Show recovery phrase` for a
 *     phrase-protected account, `Save a recovery phrase` for every other.
 *
 * After a rotation, a key reset or a recall on an account without a saved
 * phrase the server has dropped the email vault and this device holds no
 * address (and no new words were shown): the account reads "Not protected",
 * and the banner and the email row say "Add your email again", which seals
 * the new phrase.
 *
 * What is shown is decided by `account-protection.ts` from
 * `GET /recovery-email`.
 */
import { plainStore } from "./local";
import { createEffect, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { useAuth } from "../AuthContext";
import { useWebauthnClient } from "../use-webauthn-client";
import { t } from "../../l10n";
import { paths } from "../../routes/paths";
import { Icon } from "../../ui/icons";
import { ModalLayer } from "../../ui/ModalLayer";
import {
  bannerDismissedAt,
  dismissBanner,
  emailFormPrefill,
  emailNeedsAddingAgain,
  protectBannerKeys,
  protectBannerShown,
  protectionOf,
  protectionRows,
  protectionStatusKey,
  type EmailRowAction,
} from "./account-protection";
import { openUsernameClaim } from "../username-claim-host";
import { heldAddress } from "./address-register";
import { protectionStatus, refreshProtection } from "./protection-state";
import { RecoveryEmailForm } from "./RecoveryEmailForm";
import { resealNeeded } from "./reseal";
import { RemoveRecoveryEmail } from "./RemoveRecoveryEmail";
import { SavePhrasePanel } from "./SavePhrasePanel";

type Sheet = "email" | "remove" | "phrase" | null;

const ACTION_KEY: Record<Exclude<EmailRowAction, "choose-username">, string> = {
  add: "protect.email.title",
  addAgain: "recovery_email.add_again.title",
  replace: "recovery_email.replace",
  remove: "recovery_email.remove",
};

export function ProtectionCard(): JSX.Element {
  const auth = useAuth();
  const [sheet, setSheet] = createSignal<Sheet>(null);
  const [bannerVersion, setBannerVersion] = createSignal(0);

  const account = (): { accountIdB64: string; username: string } | null => {
    const s = auth.state();
    return s.kind === "session"
      ? { accountIdB64: s.account.accountId, username: s.account.username.trim() }
      : null;
  };
  const status = () => protectionStatus(account()?.accountIdB64 ?? null);
  // The address, on a device that holds it (the synced register).
  const address = (): string | null => heldAddress(account()?.accountIdB64 ?? null);
  // What the email form starts with: the held address when adding, nothing
  // when replacing.
  const [prefill, setPrefill] = createSignal("");
  const rows = () => {
    const st = status();
    return st === null ? null : protectionRows(st, (account()?.username.length ?? 0) > 0, address() !== null);
  };
  const refresh = (): void => {
    const acct = account();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (acct !== null && token !== null) void refreshProtection(acct.accountIdB64, token);
  };
  // Whether the account has a passkey, from the list Settings already
  // loads: a passkey is a way back in, so the banner is not for it. `null`
  // while the list is not known.
  const { client } = useWebauthnClient();
  const [hasPasskey, setHasPasskey] = createSignal<boolean | null>(null);
  onMount(() => {
    refresh();
    const c = client();
    if (c === null) {
      setHasPasskey(false);
      return;
    }
    void c
      .listCredentials()
      .then((list) => setHasPasskey(list.credentials.length > 0))
      .catch(() => setHasPasskey(false));
  });
  // New words are about to come up (a phrase was saved): the sheet goes.
  createEffect(() => {
    if (auth.signupPhrase() !== null) setSheet(null);
  });

  const banner = (): boolean => {
    bannerVersion();
    const acct = account();
    const st = status();
    if (acct === null || st === null) return false;
    return protectBannerShown(
      protectionOf(st),
      bannerDismissedAt(plainStore(), acct.accountIdB64),
      Date.now(),
      hasPasskey(),
      resealNeeded(st.enabled, st.vault, st.phraseSaved, address() !== null),
    );
  };
  function onDismissBanner(): void {
    const acct = account();
    if (acct !== null) dismissBanner(plainStore(), acct.accountIdB64, Date.now());
    setBannerVersion((v) => v + 1);
  }
  /** `Protect it`: the email when it can be added, else the phrase. */
  function onProtect(): void {
    const email = rows()?.email ?? null;
    const canAdd = email !== null && email.actions.some((a) => a === "add" || a === "addAgain");
    setPrefill(canAdd ? (address() ?? "") : "");
    setSheet(canAdd ? "email" : "phrase");
  }
  function onAction(action: EmailRowAction): void {
    if (action === "remove") setSheet("remove");
    else if (action !== "choose-username") {
      setPrefill(emailFormPrefill(action, address()));
      setSheet("email");
    }
  }
  function closed(): void {
    setSheet(null);
    refresh();
  }

  return (
    <Show when={status()}>
      {(st) => (
        <section class="lk-card">
          <h2>{t("protect.title")}</h2>
          <p class="lk-card-desc" role="status">
            {t(protectionStatusKey(protectionOf(st())))}
          </p>
          <Show when={banner()}>
            <div class="form-error" role="status">
              <span>{t(protectBannerKeys(st(), address() !== null).titleKey)}</span>
              <Show when={protectBannerKeys(st(), address() !== null).bodyKey}>
                {(key) => <span>{t(key())}</span>}
              </Show>
              <div class="lk-actions">
                <button type="button" class="primary" onClick={onProtect}>
                  {t("protect.banner.action")}
                </button>
                <button type="button" class="ghost" onClick={onDismissBanner}>
                  {t("common.dismiss")}
                </button>
              </div>
            </div>
          </Show>
          <Show when={rows()?.email}>
            {(email) => (
              <div class="lk-card-row">
                <span class="lk-card-row-icon">
                  <Icon name="mail" size={18} />
                </span>
                <div class="lk-card-row-body">
                  <span class="lk-card-row-title">{t("recovery_email.title")}</span>
                  <span class="lk-card-row-sub">
                    {email().actions.includes("choose-username")
                      ? t("recovery_email.needs_username")
                      : emailNeedsAddingAgain(st(), address() !== null)
                        ? t("recovery_email.add_again.body")
                        : email().on
                          ? // A device that holds the address shows it; one
                            // that does not shows On.
                            (address() ?? t("recovery_email.status.on"))
                          : t("recovery_email.status.off")}
                  </span>
                </div>
                <Show when={email().actions.includes("choose-username")}>
                  <button type="button" class="lk-card-row-action" onClick={openUsernameClaim}>
                    {t("username.claim.title")}
                  </button>
                </Show>
                <For each={email().actions.filter((a) => a !== "choose-username")}>
                  {(action) => (
                    <button
                      type="button"
                      class={action === "remove" ? "lk-card-row-action danger" : "lk-card-row-action"}
                      onClick={() => onAction(action)}
                    >
                      {t(ACTION_KEY[action as Exclude<EmailRowAction, "choose-username">])}
                    </button>
                  )}
                </For>
              </div>
            )}
          </Show>
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="key" size={18} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t("recovery_phrase.label")}</span>
              <span class="lk-card-row-sub">
                {rows()?.phrase === "show" ? t("protect.status.phrase") : t("protect.phrase.body")}
              </span>
            </div>
            <Show
              when={rows()?.phrase === "show"}
              fallback={
                <button type="button" class="lk-card-row-action" onClick={() => setSheet("phrase")}>
                  {t("protect.phrase.title")}
                </button>
              }
            >
              <A href={paths.settingsSection("recovery")} class="lk-card-row-action">
                {t("settings.recovery.phrase.show")}
              </A>
            </Show>
          </div>

          <ModalLayer open={sheet() === "email"} title={t("recovery_email.title")} onClose={closed}>
            <Show when={sheet() === "email"}>
              <RecoveryEmailForm onDone={closed} onCancel={closed} prefill={prefill()} />
            </Show>
          </ModalLayer>
          <ModalLayer
            open={sheet() === "remove"}
            title={t("recovery_email.remove.confirm.title")}
            onClose={closed}
          >
            <Show when={sheet() === "remove"}>
              <RemoveRecoveryEmail onRemoved={closed} onCancel={closed} />
            </Show>
          </ModalLayer>
          <ModalLayer open={sheet() === "phrase"} title={t("protect.phrase.title")} onClose={closed}>
            <Show when={sheet() === "phrase"}>
              <SavePhrasePanel onSaved={closed} onCancel={closed} />
            </Show>
          </ModalLayer>
        </section>
      )}
    </Show>
  );
}
