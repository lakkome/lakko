// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Protect your account" (docs/ux/identity-recovery.md section 19.1a,
 * ADR-0075 D2 and D7): the one step after sign-up. The recovery phrase is
 * not shown at sign-up any more; the person adds an email, saves a recovery
 * phrase, or continues without either after the app's own confirmation.
 *
 * Rendered by the route GUARDS for as long as `auth.protectStepOwed()`,
 * whichever route the person is on and again after a reload
 * (`phrase-gate.ts`). The sign-up's fresh-auth proof covers the email step,
 * so nothing more is asked unless more than ten minutes passed.
 */
import { createEffect, createSignal, For, Match, onMount, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { PasskeyOffer } from "../SignupPhrasePrompt";
import { useWebauthnClient } from "../use-webauthn-client";
import { WebauthnClient } from "../webauthn-client";
import { t } from "../../l10n";
import { AuthLayout } from "../../ui/AuthLayout";
import { requestConfirm } from "../../ui/confirm-dialog";
import { Icon } from "../../ui/icons";
import { phraseProtectedHere, protectionStatus, refreshProtection } from "./protection-state";
import {
  protectStepDone,
  protectStepExit,
  protectStepRows,
  protectStepSettled,
  skipProtectConfirmation,
  type ProtectChoice,
} from "./protect-step";
import { RecoveryEmailForm } from "./RecoveryEmailForm";
import { SavePhrasePanel } from "./SavePhrasePanel";

const PASSKEYS_SUPPORTED = WebauthnClient.isWebauthnAvailable();

const ROW: Record<ProtectChoice, { icon: "mail" | "lock" | "key"; titleKey: string; bodyKey: string }> = {
  email: { icon: "mail", titleKey: "protect.email.title", bodyKey: "protect.email.body" },
  passkey: { icon: "lock", titleKey: "protect.passkey.title", bodyKey: "protect.passkey.body" },
  phrase: { icon: "key", titleKey: "protect.phrase.title", bodyKey: "protect.phrase.body" },
};

export default function ProtectAccountPrompt(): JSX.Element {
  const auth = useAuth();
  const [picked, setPicked] = createSignal<ProtectChoice | null>(null);

  const account = (): { accountIdB64: string; username: string } | null => {
    const s = auth.state();
    return s.kind === "session"
      ? { accountIdB64: s.account.accountId, username: s.account.username.trim() }
      : null;
  };
  const status = () => protectionStatus(account()?.accountIdB64 ?? null);
  const rows = (): ProtectChoice[] =>
    protectStepRows(status(), (account()?.username.length ?? 0) > 0, PASSKEYS_SUPPORTED);
  // A passkey created on this step, or one the account already has.
  const { client } = useWebauthnClient();
  const [hasPasskey, setHasPasskey] = createSignal(false);
  const done = (): ProtectChoice[] => {
    const acct = account();
    return protectStepDone({
      status: status(),
      hasPasskey: hasPasskey(),
      phraseProtectedHere: phraseProtectedHere(acct?.accountIdB64 ?? null),
    });
  };

  onMount(() => {
    const acct = account();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (acct !== null && token !== null) void refreshProtection(acct.accountIdB64, token);
    const c = client();
    if (c !== null && PASSKEYS_SUPPORTED) {
      void c
        .listCredentials()
        .then((list) => {
          if (list.credentials.length > 0) setHasPasskey(true);
        })
        .catch(() => undefined);
    }
  });

  // A phrase was saved (here or on another device): the step is over. It
  // ends at the commit, so the new words are followed by the app, not by
  // this step again. An email or a passkey marks its row done instead.
  createEffect(() => {
    const acct = account();
    if (acct === null) return;
    if (protectStepSettled(status(), phraseProtectedHere(acct.accountIdB64))) auth.finishProtectStep();
  });

  async function onSkip(): Promise<void> {
    const emailOffered = rows().includes("email");
    const addEmail = await requestConfirm(skipProtectConfirmation(emailOffered));
    if (!addEmail) {
      auth.finishProtectStep();
      return;
    }
    if (emailOffered) setPicked("email");
  }

  return (
    <AuthLayout>
      <Switch>
        <Match when={picked() === "email"}>
          <h1>{t("recovery_email.title")}</h1>
          <RecoveryEmailForm
            onDone={() => setPicked(null)}
            onCancel={() => setPicked(null)}
            cancelLabel={t("common.back")}
          />
        </Match>
        <Match when={picked() === "passkey"}>
          {/* The existing passkey creation, which stores the keys for it, under
              the sign-up's proof; then back to the step. */}
          <PasskeyOffer standalone onAdded={() => setHasPasskey(true)} onDone={() => setPicked(null)} />
        </Match>
        <Match when={picked() === "phrase"}>
          <h1>{t("protect.phrase.title")}</h1>
          <SavePhrasePanel
            onSaved={() => auth.finishProtectStep()}
            onCancel={() => setPicked(null)}
            cancelLabel={t("common.back")}
          />
        </Match>
        <Match when={true}>
          <h1>{t("protect.title")}</h1>
          <p class="lk-auth-lede">{t("protect.body")}</p>
          <ul class="lk-method-list">
            <For each={rows()}>
              {(row) => (
                <li>
                  <button type="button" class="lk-method-row" onClick={() => setPicked(row)}>
                    <span class="lk-card-row-icon">
                      <Icon name={ROW[row].icon} size={18} />
                    </span>
                    <span class="lk-method-row-body">
                      <span class="lk-method-row-title">
                        {t(ROW[row].titleKey)}
                        <Show
                          when={done().includes(row)}
                          fallback={
                            <Show when={row === "email"}>
                              {" "}
                              <span class="lk-badge">{t("protect.email.badge")}</span>
                            </Show>
                          }
                        >
                          {" "}
                          <span class="lk-badge muted">{t("protect.done")}</span>
                        </Show>
                      </span>
                      <span class="lk-method-row-sub">{t(ROW[row].bodyKey)}</span>
                    </span>
                    <Icon name="chevron" size={18} />
                  </button>
                </li>
              )}
            </For>
          </ul>
          <Show
            when={protectStepExit(done()) === "continue"}
            fallback={
              <button type="button" class="ghost" onClick={() => void onSkip()}>
                {t("protect.skip")}
              </button>
            }
          >
            <button type="button" class="primary" onClick={() => auth.finishProtectStep()}>
              {t("protect.continue")}
            </button>
          </Show>
        </Match>
      </Switch>
    </AuthLayout>
  );
}
