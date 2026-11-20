// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Get back in with a code sent to the recovery email
 * (docs/ux/identity-recovery.md section 19.3, ADR-0075 section 7).
 *
 *   1. username and address;
 *   2. the code. This step looks and behaves the same for a matching pair, a
 *      wrong pair, an unknown username and an account with no email;
 *   3. the authenticator code, only when the account has an authenticator;
 *   4. a new password. Only now does the server say which kind of account
 *      this is (`recover/open`):
 *        - with an email vault (no phrase saved): the vault is opened with
 *          the typed username and address, off the main thread, the phrase
 *          rebuilt, and the EXISTING phrase restore runs
 *          (`AuthContext.recover`: the bundle check, the recovery login with
 *          the new password, the backup restore);
 *        - without one (a phrase is saved): the new password is registered
 *          under the ticket and the session lands; on a device without the
 *          account's keys the guard then shows "Restore your keys".
 *
 * When the vault does not open, `recovery.email.failed` is shown with the
 * way back to the Get back in list; nothing was changed on the server.
 *
 * Every decision is `protect/email-flow.ts`. The address and the codes live
 * in this component's memory only and are never logged.
 */
import { isAbandoned } from "../binding-epoch";
import { plainStore } from "../protect/local";
import { createSignal, Match, onCleanup, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { base64Decode, base64Encode } from "../account-id";
import { recoverErrorMessage, screenText } from "../auth-error-copy";
import { nameTargetFor } from "../login-handle";
import { newPasswordProblem, MIN_PASSWORD_LENGTH } from "../password-rules";
import { normalizeTotpCode } from "../totp-code";
import { rememberAddress } from "../protect/address-register";
import { CodeField, ResendButton } from "../protect/CodeField";
import {
  emailFailure,
  emailFailureKey,
  normalizeEmailCode,
  RECOVER_START,
  recoverReduce,
  type EmailFailure,
  type EmailRequest,
  type RecoverEvent,
  type RecoverState,
} from "../protect/email-flow";
import { runEmailVault } from "../protect/email-vault";
import { markOwnAction, type NoticeKind } from "../protect/notices";
import { answerOf, RecoveryEmailClient } from "../protect/recovery-email-client";
import { currentLanguage, t } from "../../l10n";
import { requireValidUsername } from "../../profile/username-field";
import { PasswordField } from "../../ui/PasswordField";
import { loadWasm } from "../../wasm";
import { WaitNotice } from "./FormNotices";
import { attemptsRetryAfterSecs, createRetryCountdown } from "./retry-countdown";
import { UsernameField } from "./UsernameField";

export function EmailRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  /** Back to the list of methods. */
  readonly onBack: () => void;
  /** The session landed on a device that holds the keys. */
  readonly onDone: () => void;
  /** The routes' client (a test supplies its own). */
  readonly client?: RecoveryEmailClient;
}): JSX.Element {
  const auth = useAuth();
  const client = props.client ?? new RecoveryEmailClient();
  const [state, setState] = createSignal<RecoverState>(RECOVER_START);
  const dispatch = (event: RecoverEvent): void => {
    setState((current) => recoverReduce(current, event));
  };
  const [address, setAddress] = createSignal("");
  const [addressValid, setAddressValid] = createSignal(false);
  const [code, setCode] = createSignal("");
  const [totp, setTotp] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  /** A line that is the form's own (a password that does not pass the
   *  rule, a failed phrase restore). */
  const [ownError, setOwnError] = createSignal<string | null>(null);
  const wait = createRetryCountdown();
  let codeInput: HTMLInputElement | undefined;
  // The username this run was started for, canonical.
  let canonicalUsername = "";
  // The phrase the vault held, kept until the restore landed: the ticket
  // is spent once the vault left the server, so a retry repeats only the
  // phrase restore.
  let openedPhrase: string | null = null;
  // When this device asked for the account to be opened or its password
  // changed: the notice the server writes is then its own action.
  let actedAtMs = 0;
  let actedKind: NoticeKind | null = null;
  onCleanup(() => {
    openedPhrase = null;
    setAddress("");
    setCode("");
    setTotp("");
    setPassword("");
    setConfirm("");
  });

  const working = (): boolean => {
    const s = state();
    return "working" in s && s.working;
  };
  const failure = (): EmailFailure | null => {
    const s = state();
    return "failure" in s ? s.failure : null;
  };
  const failureLine = (): string | null => {
    const own = ownError();
    if (own !== null) return own;
    const f = failure();
    if (f === null) return null;
    const key = emailFailureKey(f);
    return key === null ? null : t(key);
  };

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

  function fail(request: EmailRequest, err: unknown): void {
    // Overtaken by another session on this device: nothing to report.
    if (isAbandoned(err)) return;
    const answer = answerOf(err);
    const read: EmailFailure = answer === null ? { kind: "error" } : emailFailure(request, answer);
    if (read.kind === "rate-limited" && read.retryAfterSecs !== null) wait.startFor(read.retryAfterSecs);
    if (read.kind === "used-up" || read.kind === "start-over") {
      setCode("");
      setTotp("");
    }
    dispatch({ type: "failed", failure: read });
  }

  /** Step 1, and "Send again" on step 2. */
  async function start(): Promise<void> {
    if (working() || wait.waiting()) return;
    setOwnError(null);
    if (props.username().trim().length === 0) {
      setOwnError(t("recovery.username_required"));
      return;
    }
    try {
      canonicalUsername = await requireValidUsername(props.username());
    } catch (err) {
      setOwnError(screenText(err) ?? t("username.error.unknown"));
      return;
    }
    dispatch({ type: "submitted" });
    try {
      const target = await nameTargetFor(canonicalUsername);
      const sent = await client.recoverStart(target.handle.wire, address(), currentLanguage());
      setCode("");
      dispatch({
        type: "started",
        challengeId: sent.challengeId,
        resendAfterSecs: sent.resendAfterSecs,
        nowMs: Date.now(),
      });
      queueMicrotask(() => codeInput?.focus());
    } catch (err) {
      fail("recover-start", err);
    }
  }

  async function verify(): Promise<void> {
    const current = state();
    if (current.step !== "code" || current.working) return;
    const digits = normalizeEmailCode(code());
    if (digits === null) {
      dispatch({ type: "failed", failure: { kind: "code-wrong" } });
      return;
    }
    dispatch({ type: "submitted" });
    try {
      const verified = await client.recoverVerify(current.challengeId, digits);
      setCode("");
      dispatch({ type: "verified", ticket: verified.ticket, totpRequired: verified.totpRequired });
    } catch (err) {
      setCode("");
      fail("recover-verify", err);
      queueMicrotask(() => codeInput?.focus());
    }
  }

  async function passTotp(): Promise<void> {
    const current = state();
    if (current.step !== "totp" || current.working) return;
    const digits = normalizeTotpCode(totp());
    if (digits === null) {
      setOwnError(t("recovery.totp.error.enter_code"));
      return;
    }
    setOwnError(null);
    dispatch({ type: "submitted" });
    try {
      await client.recoverTotp(current.ticket, digits);
      setTotp("");
      dispatch({ type: "totp-passed" });
    } catch (err) {
      setTotp("");
      fail("recover-totp", err);
    }
  }

  /** The notice this device's own action caused is not shown to it, and
   *  the address the person just typed is kept by the account's devices
   *  (it was accepted: the vault opened under it). */
  function rememberOwnAction(): void {
    const s = auth.state();
    if (actedKind === null || s.kind === "anon" || s.kind === "loading") return;
    markOwnAction(plainStore(), s.account.accountId, actedKind, actedAtMs);
    rememberAddress(s.account.accountId, address());
  }

  /** The existing phrase restore, with the phrase the vault held. */
  async function restoreWithPhrase(phrase: string): Promise<void> {
    try {
      // Opened from the email vault, never typed: an owed phrase screen
      // stays owed.
      await auth.recover(phrase, password(), canonicalUsername, "vault");
      openedPhrase = null;
      rememberOwnAction();
      setPassword("");
      setConfirm("");
      dispatch({ type: "landed" });
      props.onDone();
    } catch (err) {
      // Overtaken by another session on this device: nothing to report.
      if (isAbandoned(err)) return;
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) wait.startFor(secs);
      else setOwnError(recoverErrorMessage(err));
      dispatch({ type: "restore-failed" });
    }
  }

  /** Step 4. */
  async function finish(): Promise<void> {
    const current = state();
    if (current.step !== "password" || current.working || wait.waiting()) return;
    setOwnError(null);
    const problem = newPasswordProblem(password(), confirm());
    if (problem !== null) {
      setOwnError(problem);
      return;
    }
    dispatch({ type: "opening" });
    // The vault already left the server on an earlier try: only the phrase
    // restore is repeated.
    if (current.vaultOpened && openedPhrase !== null) {
      dispatch({ type: "vault-opened" });
      await restoreWithPhrase(openedPhrase);
      return;
    }
    let opened: Awaited<ReturnType<RecoveryEmailClient["recoverOpen"]>>;
    actedAtMs = Date.now();
    try {
      opened = await client.recoverOpen(current.ticket);
    } catch (err) {
      fail("recover-open", err);
      return;
    }
    if (opened.mode === "vault") {
      actedKind = "account-opened-by-email";
      let result: Awaited<ReturnType<typeof runEmailVault>>;
      try {
        const target = await nameTargetFor(canonicalUsername);
        result = await runEmailVault({
          kind: "open",
          loginHandle: target.handle.bytes,
          address: address(),
          vaultSalt: base64Decode(opened.vaultSalt),
          sealed: base64Decode(opened.sealedEntropy),
        });
      } catch {
        result = { kind: "not-opened" };
      }
      if (result.kind !== "opened") {
        // The ticket is spent and the keys did not come out. Nothing was
        // changed on the server.
        dispatch({ type: "vault-not-opened" });
        return;
      }
      openedPhrase = result.phrase;
      dispatch({ type: "vault-opened" });
      await restoreWithPhrase(result.phrase);
      return;
    }
    // Password mode: a phrase is saved (or the account has no vault). The
    // code gives a new password and a session, nothing else.
    actedKind = "password-changed-by-email";
    try {
      const landed = await client.recoverPassword({
        ticket: current.ticket,
        newPassword: new TextEncoder().encode(password()),
        ...(() => {
          const id = auth.landingDeviceIdFor(canonicalUsername);
          return id === undefined ? {} : { deviceIdB64: id };
        })(),
      });
      const landedB64 = base64Encode(landed.accountId);
      const kind = actedKind;
      const typed = address();
      setPassword("");
      setConfirm("");
      // Written once the landing opened the account's stores: before that a
      // write would be dropped for an account wiped earlier on this page.
      await auth.landEmailPasswordRecovery(canonicalUsername, landed.accountId, landed.session, () => {
        markOwnAction(plainStore(), landedB64, kind, actedAtMs);
        // The mailed code was accepted for this address.
        rememberAddress(landedB64, typed);
      });
      dispatch({ type: "landed" });
      // On a device without the account's keys the guard shows "Restore
      // your keys" on this very state change; otherwise the app opens.
      if (auth.state().kind === "session") props.onDone();
    } catch (err) {
      // ADR-0098: the code was right and the account is suspended; the
      // password was not changed. The suspended screen takes over.
      if (await auth.landSuspendedFrom(err, canonicalUsername).catch(() => false)) {
        props.onDone();
        return;
      }
      fail("recover-password", err);
    }
  }

  const codeStep = () => {
    const s = state();
    return s.step === "code" ? s : null;
  };
  const passwordStep = () => {
    const s = state();
    return s.step === "password" ? s : null;
  };

  return (
    <Switch>
      <Match when={state().step === "unavailable"}>
        <div class="lk-stack">
          <p class="form-error" role="alert">
            {t("recovery_email.unavailable")}
          </p>
          <button type="button" onClick={props.onBack}>
            {t("recovery.methods.back_to_list")}
          </button>
        </div>
      </Match>
      <Match when={state().step === "not-opened"}>
        <div class="lk-stack">
          <p class="form-error" role="alert">
            {t("recovery.email.failed")}
          </p>
          <button type="button" onClick={props.onBack}>
            {t("recovery.methods.back_to_list")}
          </button>
        </div>
      </Match>
      <Match when={codeStep()}>
        {(current) => (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void verify();
            }}
          >
            <h3>{t("recovery.email.check.title")}</h3>
            <p class="lk-muted">{t("recovery.email.check.body")}</p>
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
            <button class="primary" type="submit" disabled={working() || normalizeEmailCode(code()) === null}>
              {working() ? t("common.checking") : t("common.continue")}
            </button>
            <div class="lk-actions">
              <ResendButton
                resendAtMs={() => current().resendAtMs}
                disabled={() => working() || wait.waiting()}
                onResend={() => void start()}
              />
              <button
                type="button"
                class="ghost"
                disabled={working()}
                onClick={() => {
                  setCode("");
                  dispatch({ type: "back" });
                }}
              >
                {t("common.back")}
              </button>
            </div>
          </form>
        )}
      </Match>
      <Match when={state().step === "totp"}>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void passTotp();
          }}
        >
          <h3>{t("recovery.email.totp.title")}</h3>
          <p class="lk-muted">{t("recovery.email.totp.body")}</p>
          <CodeField label={t("recovery.totp.code_label")} value={totp} setValue={setTotp} disabled={working} />
          <Show when={failureLine()}>
            {(line) => (
              <div class="form-error" role="alert">
                {line()}
              </div>
            )}
          </Show>
          <WaitNotice remainingMs={wait.remainingMs} />
          <button class="primary" type="submit" disabled={working() || wait.waiting()}>
            {working() ? t("common.checking") : t("common.continue")}
          </button>
          <button type="button" class="ghost" disabled={working()} onClick={props.onBack}>
            {t("recovery.email.totp.lost")}
          </button>
        </form>
      </Match>
      <Match when={passwordStep()}>
        {(current) => (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void finish();
            }}
          >
            <h3>{t("recovery.new_password.title")}</h3>
            <PasswordField
              label={t("auth.password.new.label")}
              autocomplete="new-password"
              value={password}
              setValue={setPassword}
              disabled={working}
              minLength={MIN_PASSWORD_LENGTH}
            />
            <PasswordField
              label={t("auth.password.new_confirm.label")}
              autocomplete="new-password"
              value={confirm}
              setValue={setConfirm}
              disabled={working}
            />
            <Show when={failureLine()}>
              {(line) => (
                <div class="form-error" role="alert">
                  {line()}
                </div>
              )}
            </Show>
            <WaitNotice remainingMs={wait.remainingMs} />
            <Show when={current().phase === "restoring"}>
              <p class="form-info" role="status" aria-live="polite">
                <span class="lakko-spinner" aria-hidden="true" /> {t("recovery.email.restoring")}
              </p>
            </Show>
            <button class="primary" type="submit" disabled={working() || wait.waiting()}>
              {working() ? t("common.checking") : t("recovery.email.save")}
            </button>
          </form>
        )}
      </Match>
      <Match when={state().step === "landed"}>
        <p class="form-info" role="status">
          {t("recovery.email.restoring")}
        </p>
      </Match>
      <Match when={true}>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void start();
          }}
        >
          <p class="lk-muted">{t("recovery.email.start.body")}</p>
          <UsernameField value={props.username} setValue={props.setUsername} disabled={working} />
          <label>
            {t("recovery_email.field")}
            <input
              type="email"
              autocomplete="off"
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
          <Show when={failureLine()}>
            {(line) => (
              <div class="form-error" role="alert">
                {line()}
              </div>
            )}
          </Show>
          <WaitNotice remainingMs={wait.remainingMs} />
          <button class="primary" type="submit" disabled={working() || !addressValid() || wait.waiting()}>
            {working() ? t("common.sending") : t("recovery_email.send")}
          </button>
        </form>
      </Match>
    </Switch>
  );
}
