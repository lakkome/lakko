// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Protect your account" (docs/ux/identity-recovery.md 19.1a and 19.1c;
 * ADR-0075 D9, amended 2026-11-26): the one step after sign-up, and the
 * step shown before the app at every session landing to an account with a
 * username and no second way in.
 *
 * A second way to confirm it is you is REQUIRED: a recovery email, an
 * authenticator app or a passkey. The recovery phrase is an optional extra
 * and does not count. There is no `Not now`: `Continue` is the only way off
 * the step, and it is disabled until one of the three is done.
 *
 * Rendered by the route GUARDS for as long as `auth.protectStepOwed()`,
 * whichever route the person is on and again after a reload
 * (`phrase-gate.ts`). The sign-up's fresh-auth proof covers the three
 * flows, so nothing more is asked unless more than ten minutes passed.
 *
 * Every decision is `protect-step.ts`, `factors.ts` and `password-only.ts`.
 */
import { createSignal, createUniqueId, For, Match, onMount, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { TotpSetup } from "../RecoveryFactors";
import { ownSealFacts } from "../seal/own-seals";
import { PasskeyOffer } from "../SignupPhrasePrompt";
import { WebauthnClient } from "../webauthn-client";
import { t } from "../../l10n";
import { AuthLayout } from "../../ui/AuthLayout";
import { Icon, type IconName } from "../../ui/icons";
import { isProtectStepOwed, markPasswordOnlyDialogSeen } from "./account-protection";
import { accountFactors, refreshFactors } from "./factors";
import { plainStore } from "./local";
import {
  passwordOnlyNoticeApplies,
  passwordOnlyOffersPasskey,
  passwordOnlyShownIn,
} from "./password-only";
import { passkeyAddedWithoutSeal } from "./password-only-state";
import { PasswordOnlyNotice, WarningNote } from "./PasswordOnlyNotice";
import { phraseProtectedHere, protectionStatus } from "./protection-state";
import {
  protectStepCanContinue,
  protectStepDone,
  protectStepRows,
  protectStepTitleKey,
  protectStepTotpWarning,
  type ProtectChoice,
} from "./protect-step";
import { RecoveryEmailForm } from "./RecoveryEmailForm";
import { SavePhrasePanel } from "./SavePhrasePanel";

const PASSKEYS_SUPPORTED = WebauthnClient.isWebauthnAvailable();

const ROW: Record<ProtectChoice, { icon: IconName; titleKey: string; bodyKey: string }> = {
  email: { icon: "envelope", titleKey: "protect.email.title", bodyKey: "protect.email.body" },
  totp: { icon: "smartphone", titleKey: "protect.totp.title", bodyKey: "protect.totp.body" },
  passkey: { icon: "passkey", titleKey: "protect.passkey.title", bodyKey: "protect.passkey.body" },
  phrase: { icon: "key", titleKey: "protect.phrase.title_optional", bodyKey: "protect.phrase.body" },
};

export default function ProtectAccountPrompt(): JSX.Element {
  const auth = useAuth();
  const [picked, setPicked] = createSignal<ProtectChoice | null>(null);
  // The system passkey sheet was closed: one plain line under the row.
  const [passkeyCancelled, setPasskeyCancelled] = createSignal(false);
  const continueHelpId = createUniqueId();

  const account = (): { accountIdB64: string; username: string } | null => {
    const s = auth.state();
    return s.kind === "session"
      ? { accountIdB64: s.account.accountId, username: s.account.username.trim() }
      : null;
  };
  const accountId = (): string | null => account()?.accountIdB64 ?? null;
  const hasUsername = (): boolean => (account()?.username.length ?? 0) > 0;
  const factors = () => accountFactors(accountId());
  const seals = () => ownSealFacts(accountId());
  const phraseSaved = (): boolean => seals()?.phrase === true || phraseProtectedHere(accountId());
  const rows = () =>
    protectStepRows(protectionStatus(accountId())?.available ?? null, hasUsername(), PASSKEYS_SUPPORTED);
  const done = (): ProtectChoice[] => protectStepDone({ factors: factors(), phraseSaved: phraseSaved() });
  const canContinue = (): boolean => protectStepCanContinue(factors());
  const totpWarning = (): string | null => protectStepTotpWarning(factors(), phraseSaved());
  const noticeShown = (): boolean =>
    passwordOnlyShownIn("step", { applies: passwordOnlyNoticeApplies(factors(), seals()) });
  // Read once: the heading does not change while the step is up.
  const titleKey = protectStepTitleKey(
    (() => {
      const id = accountId();
      return id !== null && isProtectStepOwed(plainStore(), id);
    })(),
  );

  function refresh(): void {
    const acct = account();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (acct !== null && token !== null) void refreshFactors(acct.accountIdB64, token);
  }
  onMount(refresh);

  /** Back on the step from one of the flows. */
  function back(): void {
    setPicked(null);
    refresh();
  }
  function open(row: ProtectChoice): void {
    setPasskeyCancelled(false);
    setPicked(row);
  }
  function onContinue(): void {
    if (!canContinue()) return;
    // The step already said what keeps the chats (19.1d, place 1): the
    // dialog of place 3 does not open on this device.
    const id = accountId();
    if (id !== null) markPasswordOnlyDialogSeen(plainStore(), id);
    auth.finishProtectStep();
  }

  const row = (choice: ProtectChoice): JSX.Element => {
    const isDone = (): boolean => done().includes(choice);
    const body = (): JSX.Element => (
      <>
        <span class="lk-card-row-icon">
          <Icon name={ROW[choice].icon} size={20} />
        </span>
        <span class="lk-method-row-body">
          <span class="lk-method-row-title">
            {t(ROW[choice].titleKey)}
            <Show
              when={isDone()}
              fallback={
                <Show when={choice === "email"}>
                  {" "}
                  <span class="lk-badge">{t("protect.email.badge")}</span>
                </Show>
              }
            >
              {" "}
              <span class="lk-badge muted">{t("protect.done")}</span>
            </Show>
          </span>
          <span class="lk-method-row-sub">{t(ROW[choice].bodyKey)}</span>
        </span>
      </>
    );
    return (
      <li>
        {/* A row marked Done can be opened again only from Settings. */}
        <Show
          when={!isDone()}
          fallback={<div class="lk-method-row">{body()}</div>}
        >
          <button type="button" class="lk-method-row" onClick={() => open(choice)}>
            {body()}
            <Icon name="chevron-forward" size={14} />
          </button>
        </Show>
        <Show when={choice === "passkey" && passkeyCancelled() && !isDone()}>
          <p class="lk-muted" role="status">
            {t("protect.passkey.cancelled")}
          </p>
        </Show>
      </li>
    );
  };

  return (
    <AuthLayout>
      <Switch>
        <Match when={picked() === "email"}>
          <h1>{t("recovery_email.title")}</h1>
          <RecoveryEmailForm onDone={back} onCancel={back} cancelLabel={t("common.back")} />
        </Match>
        <Match when={picked() === "totp"}>
          <h1>{t("protect.totp.title")}</h1>
          <TotpSetup onDone={back} onBack={back} />
        </Match>
        <Match when={picked() === "passkey"}>
          {/* The existing passkey creation, which stores the keys for it, under
              the sign-up's proof; then back to the step. */}
          <PasskeyOffer
            onAdded={refresh}
            onDone={back}
            onCancelled={() => {
              setPasskeyCancelled(true);
              back();
            }}
          />
        </Match>
        <Match when={picked() === "phrase"}>
          <h1>{t("protect.phrase.title")}</h1>
          <SavePhrasePanel onSaved={back} onCancel={back} cancelLabel={t("common.back")} />
        </Match>
        <Match when={true}>
          <h1>{t(titleKey)}</h1>
          <p class="lk-auth-lede">{t("protect.body")}</p>
          <ul class="lk-method-list">
            <For each={rows()}>{(choice) => row(choice)}</For>
          </ul>
          <p class="lk-protect-caption">{t("protect.optional")}</p>
          <ul class="lk-method-list">{row("phrase")}</ul>
          <Show when={totpWarning()}>{(key) => <WarningNote text={t(key())} />}</Show>
          <Show when={noticeShown()}>
            <PasswordOnlyNotice
              onSavePhrase={() => open("phrase")}
              onAddPasskey={
                passwordOnlyOffersPasskey(PASSKEYS_SUPPORTED, hasUsername())
                  ? () => open("passkey")
                  : undefined
              }
              passkeyNoSeal={passkeyAddedWithoutSeal(accountId())}
            />
          </Show>
          <button
            type="button"
            class="primary"
            disabled={!canContinue()}
            aria-describedby={canContinue() ? undefined : continueHelpId}
            onClick={onContinue}
          >
            {t("protect.continue")}
          </button>
          <Show when={!canContinue()}>
            <p class="lk-muted" id={continueHelpId}>
              {t("protect.continue_needs")}
            </p>
          </Show>
        </Match>
      </Switch>
    </AuthLayout>
  );
}
