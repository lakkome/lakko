// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Account: how the account is protected
 * (docs/ux/identity-recovery.md sections 19.1, 19.1c, 19.1d, 19.2 and 19.5,
 * as amended 2026-11-26).
 *
 *   - the status line: what keeps the chats through a key reset: "Your chats
 *     are kept by your recovery phrase" / "...by a passkey" / "...by your
 *     password only", the last in the warning colour with the two actions of
 *     19.1d. Nothing while that is not known;
 *   - the `Recovery email` row: the address on a device that holds it (the
 *     synced register, ADR-0075 as amended 2026-10-13), else On / Off, with
 *     Replace and Remove. Removing the account's last second way in is
 *     refused with the app's own dialog (19.1c);
 *   - the `Recovery phrase` row: `Show recovery phrase` once the phrase was
 *     saved, `Save a recovery phrase` for every other account.
 *
 * There is no banner and no "Not protected" state any more: every account
 * has a second way in (19.1a), and the email vault is gone.
 *
 * What is shown is decided by `account-protection.ts` and `factors.ts` from
 * `GET /recovery-email`, the passkey list and the account's published
 * protection (`seal/own-seals.ts`).
 */
import { createEffect, createSignal, For, onMount, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { useAuth } from "../AuthContext";
import { chatsKeptBy, ownSealFacts } from "../seal/own-seals";
import { WebauthnClient } from "../webauthn-client";
import { t } from "../../l10n";
import { paths } from "../../routes/paths";
import { Icon } from "../../ui/icons";
import { ModalLayer } from "../../ui/ModalLayer";
import {
  chatsKeptStatusKey,
  emailFormPrefill,
  protectionRows,
  type EmailRowAction,
} from "./account-protection";
import { heldAddress } from "./address-register";
import { accountFactors, refreshFactors } from "./factors";
import { refusedAsLastFactor } from "./last-factor";
import { passwordOnlyNoticeApplies, passwordOnlyOffersPasskey } from "./password-only";
import { phraseProtectedHere, protectionStatus } from "./protection-state";
import { RecoveryEmailForm } from "./RecoveryEmailForm";
import { RemoveRecoveryEmail } from "./RemoveRecoveryEmail";
import { SavePhrasePanel } from "./SavePhrasePanel";

type Sheet = "email" | "remove" | "phrase" | null;

const PASSKEYS_SUPPORTED = WebauthnClient.isWebauthnAvailable();

const ACTION_KEY: Record<Exclude<EmailRowAction, "choose-username">, string> = {
  add: "protect.email.title",
  replace: "recovery_email.replace",
  remove: "recovery_email.remove",
};

export function ProtectionCard(): JSX.Element {
  const auth = useAuth();
  const [sheet, setSheet] = createSignal<Sheet>(null);

  const account = (): { accountIdB64: string; username: string } | null => {
    const s = auth.state();
    return s.kind === "session"
      ? { accountIdB64: s.account.accountId, username: s.account.username.trim() }
      : null;
  };
  const accountId = (): string | null => account()?.accountIdB64 ?? null;
  const hasUsername = (): boolean => (account()?.username.length ?? 0) > 0;
  const status = () => protectionStatus(accountId());
  const factors = () => accountFactors(accountId());
  const seals = () => ownSealFacts(accountId());
  // The address, on a device that holds it (the synced register).
  const address = (): string | null => heldAddress(accountId());
  // What the email form starts with: the held address when adding, nothing
  // when replacing.
  const [prefill, setPrefill] = createSignal("");
  const phraseSaved = (): boolean => seals()?.phrase === true || phraseProtectedHere(accountId());
  const rows = () => {
    const st = status();
    return st === null ? null : protectionRows(st, hasUsername(), phraseSaved());
  };
  /** The status line, or `null` while what keeps the chats is not known. */
  const statusKey = (): string | null => chatsKeptStatusKey(chatsKeptBy(seals()));
  /** "Kept by your password only", with a second way in: the 19.1d actions. */
  const passwordOnly = (): boolean => passwordOnlyNoticeApplies(factors(), seals());
  const refresh = (): void => {
    const acct = account();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (acct !== null && token !== null) void refreshFactors(acct.accountIdB64, token);
  };
  onMount(refresh);
  // New words are about to come up (the account's keys changed): the sheet
  // goes.
  createEffect(() => {
    if (auth.signupPhrase() !== null) setSheet(null);
  });

  /** `Remove`: refused with the app's own dialog when the email is the
   *  account's last second way in (19.1c). */
  async function onRemove(): Promise<void> {
    if (await refusedAsLastFactor(factors(), "email")) return;
    setSheet("remove");
  }
  function onAction(action: EmailRowAction): void {
    if (action === "remove") void onRemove();
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
      {(_status) => (
        <section class="lk-card">
          <h2>{t("protect.title")}</h2>
          <Show when={statusKey()}>
            {(key) => (
              <p
                class={key() === "protect.status.password_only" ? "lk-card-desc lk-status-warning" : "lk-card-desc"}
                role="status"
              >
                {t(key())}
              </p>
            )}
          </Show>
          <Show when={passwordOnly()}>
            <div class="lk-actions">
              <button type="button" onClick={() => setSheet("phrase")}>
                {t("protect.password_only.save_phrase")}
              </button>
              <Show when={passwordOnlyOffersPasskey(PASSKEYS_SUPPORTED, hasUsername())}>
                <A href={paths.settingsSection("security")} class="lk-card-row-action">
                  {t("protect.password_only.add_passkey")}
                </A>
              </Show>
            </div>
          </Show>
          <Show when={rows()?.email}>
            {(email) => (
              <div class="lk-card-row">
                <span class="lk-card-row-icon">
                  <Icon name="envelope" size={20} />
                </span>
                <div class="lk-card-row-body">
                  <span class="lk-card-row-title">{t("recovery_email.title")}</span>
                  <span class="lk-card-row-sub">
                    {email().actions.includes("choose-username")
                      ? t("recovery_email.needs_username")
                      : email().on
                        ? // A device that holds the address shows it; one
                          // that does not shows On.
                          (address() ?? t("recovery_email.status.on"))
                        : t("recovery_email.status.off")}
                  </span>
                </div>
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
              <Icon name="key" size={20} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t("recovery_phrase.label")}</span>
              <span class="lk-card-row-sub">{t("protect.phrase.body")}</span>
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
