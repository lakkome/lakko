// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Adding or replacing the recovery email (docs/ux/identity-recovery.md
 * section 19.2, ADR-0075 section 6): the address, then the six-digit code,
 * on one surface. Used by Settings > Account and by the sign-up's "Protect
 * your account" step.
 *
 * The proof block of section 17.1 is drawn above the email field when no
 * proof is live (sign-up counts as one). Once confirmed, the address is
 * kept in the synced state's register (encrypted, shared with the account's
 * other devices); it is never written to plain storage and never logged.
 *
 * Every decision is `email-flow.ts` (the step machine and the reading of
 * the server's answers). Nothing is sealed here: the email vault was removed
 * on 2026-11-26 (ADR-0075, second amendment), so `enrol/finish` carries the
 * code and nothing else.
 */
import { plainStore } from "./local";
import { createSignal, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { PROOF_COPY } from "../fresh-auth";
import { loginHandleFor } from "../login-handle";
import { createProofGate } from "../proof-gate";
import { ProofBlock } from "../ProofBlock";
import { WaitNotice } from "../recover/FormNotices";
import { createRetryCountdown } from "../recover/retry-countdown";
import { currentLanguage, t } from "../../l10n";
import { loadWasm } from "../../wasm";
import { rememberAddress } from "./address-register";
import { CodeField, ResendButton } from "./CodeField";
import {
  emailFailure,
  emailFailureKey,
  ENROL_START,
  enrolReduce,
  normalizeEmailCode,
  type EmailFailure,
  type EnrolEvent,
  type EnrolState,
} from "./email-flow";
import { refreshFactors } from "./factors";
import { markOwnAction } from "./notices";
import { protectionStatus, refreshProtection } from "./protection-state";
import { answerOf, RecoveryEmailClient } from "./recovery-email-client";

export function RecoveryEmailForm(props: {
  /** The address was confirmed. */
  readonly onDone: () => void;
  /** A second, plain button beside the main one (Cancel in a dialog, Back
   *  on the sign-up step). */
  readonly onCancel?: () => void;
  readonly cancelLabel?: string;
  /** What the address field starts with: the address this device holds
   *  when the email is being added back, nothing when it is replaced. */
  readonly prefill?: string;
}): JSX.Element {
  const auth = useAuth();
  const client = new RecoveryEmailClient();
  const [state, setState] = createSignal<EnrolState>(ENROL_START);
  const dispatch = (event: EnrolEvent): void => {
    setState((current) => enrolReduce(current, event));
  };
  const [address, setAddress] = createSignal("");
  const [code, setCode] = createSignal("");
  const [addressValid, setAddressValid] = createSignal(false);
  const wait = createRetryCountdown();
  let codeInput: HTMLInputElement | undefined;
  onMount(() => {
    if ((props.prefill ?? "").length > 0) void checkAddress(props.prefill ?? "");
  });
  // The address never outlives the surface.
  onCleanup(() => {
    setAddress("");
    setCode("");
  });

  const account = (): { accountIdB64: string; username: string } | null => {
    const s = auth.state();
    return s.kind === "session"
      ? { accountIdB64: s.account.accountId, username: s.account.username.trim() }
      : null;
  };
  const status = () => protectionStatus(account()?.accountIdB64 ?? null);
  const hasUsername = (): boolean => (account()?.username.length ?? 0) > 0;
  const working = (): boolean => {
    const s = state();
    return s.step !== "done" && s.working;
  };
  const failure = (): EmailFailure | null => {
    const s = state();
    return s.step === "done" ? null : s.failure;
  };
  const failureLine = (): string | null => {
    const f = failure();
    if (f === null) return null;
    const key = emailFailureKey(f);
    return key === null ? null : t(key);
  };

  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.recoveryEmail,
    busy: working,
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    usernameKnown: hasUsername,
  });
  onCleanup(() => gate.clear());

  // The address rule is the core's (`normalizeRecoveryEmail`); the button
  // stays disabled until the typed text passes it.
  async function checkAddress(typed: string): Promise<void> {
    setAddress(typed);
    try {
      const wasm = await loadWasm();
      wasm.normalizeRecoveryEmail(typed);
      if (address() === typed) setAddressValid(true);
    } catch {
      if (address() === typed) setAddressValid(false);
    }
  }

  function fail(request: "enrol-start" | "enrol-finish", err: unknown, grant: string | null): void {
    const answer = answerOf(err);
    const read: EmailFailure = answer === null ? { kind: "error" } : emailFailure(request, answer);
    if (read.kind === "proof-required") {
      // Drops the refused grant and shows the block with no error.
      gate.failed(err, grant);
      dispatch({ type: "failed", failure: read });
      return;
    }
    if (read.kind === "rate-limited" && read.retryAfterSecs !== null) wait.startFor(read.retryAfterSecs);
    dispatch({ type: "failed", failure: read });
  }

  async function send(): Promise<void> {
    const acct = account();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (acct === null || token === null || working() || !addressValid() || wait.waiting()) return;
    dispatch({ type: "submitted" });
    let grant: string | null = null;
    try {
      grant = await gate.ensure();
      if (grant === null) {
        dispatch({ type: "failed", failure: { kind: "proof-required" } });
        return;
      }
      const handle = await loginHandleFor(acct.username);
      const sent = await client.enrolStart({
        sessionToken: token,
        freshGrant: grant,
        handle: handle.wire,
        address: address(),
        lang: currentLanguage(),
      });
      setCode("");
      dispatch({
        type: "sent",
        challengeId: sent.challengeId,
        resendAfterSecs: sent.resendAfterSecs,
        nowMs: Date.now(),
      });
      queueMicrotask(() => codeInput?.focus());
    } catch (err) {
      fail("enrol-start", err, grant);
    }
  }

  async function confirmCode(): Promise<void> {
    const acct = account();
    const current = state();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (acct === null || token === null || current.step !== "code" || current.working) return;
    const digits = normalizeEmailCode(code());
    if (digits === null) {
      dispatch({ type: "failed", failure: { kind: "code-wrong" } });
      return;
    }
    dispatch({ type: "submitted" });
    try {
      // Whether this adds or replaces decides which notice the account's
      // other devices show; the last known status is enough for that.
      const known = (await refreshProtection(acct.accountIdB64, token)) ?? status();
      // This device is about to cause the notice: remembered first, so the
      // notice is recognised as its own action.
      markOwnAction(
        plainStore(),
        acct.accountIdB64,
        known?.enabled === true ? "recovery-email-replaced" : "recovery-email-added",
        Date.now(),
      );
      await client.enrolFinish({
        sessionToken: token,
        challengeId: current.challengeId,
        code: digits,
      });
      // The address is confirmed: the account's devices keep it (the synced
      // register), so the reset's email row and a username change can
      // pre-fill it.
      rememberAddress(acct.accountIdB64, address());
      setAddress("");
      setCode("");
      void refreshFactors(acct.accountIdB64, token);
      dispatch({ type: "finished" });
      props.onDone();
    } catch (err) {
      setCode("");
      fail("enrol-finish", err, null);
      queueMicrotask(() => codeInput?.focus());
    }
  }

  const codeStep = () => {
    const s = state();
    return s.step === "code" ? s : null;
  };

  return (
    <Switch>
      <Match when={!hasUsername()}>
        <p class="form-info" role="status">
          {t("recovery_email.needs_username")}
        </p>
        <Show when={props.onCancel}>
          {(cancel) => (
            <button type="button" onClick={() => cancel()()}>
              {props.cancelLabel ?? t("common.back")}
            </button>
          )}
        </Show>
      </Match>
      <Match when={codeStep()}>
        {(current) => (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void confirmCode();
            }}
          >
            <h2>{t("recovery_email.code.title")}</h2>
            <p class="lk-muted">{t("recovery_email.code.sent_to", { address: address() })}</p>
            <CodeField
              label={t("recovery_email.code.title")}
              value={code}
              setValue={setCode}
              disabled={working}
              inputRef={(el) => (codeInput = el)}
            />
            <Show when={failureLine()}>
              {(line) => (
                <div class="form-error" role="alert">
                  {line()}
                </div>
              )}
            </Show>
            <WaitNotice remainingMs={wait.remainingMs} />
            <div class="lk-actions">
              <button class="primary" type="submit" disabled={working() || normalizeEmailCode(code()) === null}>
                {working() ? t("common.checking") : t("common.continue")}
              </button>
            </div>
            <div class="lk-actions">
              <ResendButton
                resendAtMs={() => current().resendAtMs}
                disabled={() => working() || wait.waiting()}
                onResend={() => void send()}
              />
              <button
                type="button"
                class="ghost"
                disabled={working()}
                onClick={() => {
                  setCode("");
                  dispatch({ type: "other-address" });
                }}
              >
                {t("recovery_email.code.other_address")}
              </button>
            </div>
          </form>
        )}
      </Match>
      <Match when={true}>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          <p class="lk-muted">{t("recovery_email.body")}</p>
          <ProofBlock gate={gate} />
          <label>
            {t("recovery_email.field")}
            <input
              type="email"
              autocomplete="email"
              autocapitalize="none"
              spellcheck={false}
              required
              value={address()}
              onInput={(e) => void checkAddress(e.currentTarget.value)}
              disabled={working()}
              aria-invalid={address().length > 0 && !addressValid()}
            />
          </label>
          <Show when={address().length > 0 && !addressValid()}>
            <p class="form-info" role="status">
              {t("recovery_email.invalid")}
            </p>
          </Show>
          <p class="form-info">{t("recovery_email.not_kept")}</p>
          {/* Shown on every account (19.2, amended 2026-11-26). */}
          <p class="form-info">{t("recovery_email.mailbox_warning")}</p>
          {/* The account has an authenticator: its code is asked as well. */}
          <Show when={status()?.totp === true}>
            <p class="form-info">{t("recovery_email.totp_warning")}</p>
          </Show>
          <Show when={failureLine()}>
            {(line) => (
              <div class="form-error" role="alert">
                {line()}
              </div>
            )}
          </Show>
          <WaitNotice remainingMs={wait.remainingMs} />
          <div class="lk-actions">
            <button
              class="primary"
              type="submit"
              disabled={working() || !addressValid() || wait.waiting() || !gate.view().ready}
            >
              {working() ? t("common.sending") : t("recovery_email.send")}
            </button>
            <Show when={props.onCancel}>
              {(cancel) => (
                <button type="button" class="ghost" disabled={working()} onClick={() => cancel()()}>
                  {props.cancelLabel ?? t("common.cancel")}
                </button>
              )}
            </Show>
          </div>
        </form>
      </Match>
    </Switch>
  );
}
