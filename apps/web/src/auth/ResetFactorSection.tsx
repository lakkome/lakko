// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Confirm it is you" on the reset screen (docs/ux/identity-recovery.md
 * 15.1b and 15.1c; ADR-0073, amended 2026-11-26): the password, proven by a
 * password sign-in, and one of the factors the account has (a code from the
 * authenticator app, a code by email, a passkey), then one line that says
 * whether this reset brings the conversations back.
 *
 * The rules are in `seal/reset-factor.ts` and `seal/seal-announcement.ts`
 * (pure, tested); this file is the wiring and the markup. The password is
 * checked first, at the moment the person confirms a factor: a wrong
 * password stops there and no code is used up. Nothing is reset here.
 */
import { createEffect, createResource, createSignal, For, Match, onCleanup, Show, Switch, type JSX } from "solid-js";

import type { AuthApi } from "./AuthContext";
import { AuthHttpError, AuthNetworkError } from "./client";
import {
  FreshAuthRequiredError,
  PROOF_COPY,
  ProofCancelledError,
  ProofOtherAccountError,
  ProofPasskeyFailedError,
} from "./fresh-auth";
import { isOffline } from "./auth-error-copy";
import { createProofGate, type ProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import { heldAddress } from "./protect/address-register";
import { CodeField, ResendButton } from "./protect/CodeField";
import { WarningNote } from "./protect/PasswordOnlyNotice";
import { WaitNotice } from "./recover/FormNotices";
import { createRetryCountdown } from "./recover/retry-countdown";
import {
  emailCodeExpired,
  emailCodeRefusal,
  resetFactorDoneKey,
  resetFactorFailure,
  resetFactorRowKey,
  ResetFactorRefusedError,
  resetSection,
  type ResetFactorFailure,
  type ResetFactorKind,
  type ResetSection,
} from "./seal/reset-factor";
import type { AccountFactors } from "./protect/factors";
import type { ResetOutlook } from "./seal/seal-announcement";
import { WebauthnClient, WebauthnHttpError, WebauthnNetworkError } from "./webauthn-client";
import { t } from "../l10n";
import { Icon, type IconName } from "../ui/icons";

const ROW_ICON: Record<ResetFactorKind, IconName> = {
  totp: "smartphone",
  email: "envelope",
  passkey: "passkey",
};

/** What a thrown error is, for a row of the section. */
type RowFailure =
  | ResetFactorFailure
  | { readonly kind: "cancelled" }
  | { readonly kind: "other-account" }
  | { readonly kind: "passkey-failed" };

export function rowFailureOf(err: unknown): RowFailure {
  if (err instanceof FreshAuthRequiredError) return { kind: "password-needed" };
  if (err instanceof ResetFactorRefusedError) return { kind: "refused" };
  if (err instanceof ProofCancelledError) return { kind: "cancelled" };
  if (err instanceof ProofOtherAccountError) return { kind: "other-account" };
  if (err instanceof ProofPasskeyFailedError) return { kind: "passkey-failed" };
  if (err instanceof AuthHttpError) {
    return resetFactorFailure({
      status: err.status,
      errorCode: err.errorCode,
      retryAfterSecs: err.retryAfter,
      network: false,
    });
  }
  if (err instanceof WebauthnHttpError && err.status === 429) {
    return { kind: "rate-limited", retryAfterSecs: 60 };
  }
  if (err instanceof AuthNetworkError || err instanceof WebauthnNetworkError || isOffline(err)) {
    return { kind: "offline" };
  }
  return { kind: "generic" };
}

/** The section's state, shared with the panel that owns the reset button. */
export interface ResetFactors {
  readonly section: () => ResetSection;
  /** UX 15.1c, once a factor is confirmed; `null` while it is not known. */
  readonly outlook: () => ResetOutlook | null;
  /** The account's factors; `null` while they are not known. */
  readonly factors: () => AccountFactors | null;
  readonly gate: ProofGate;
  /** `Back` on the "cannot reset" panel. */
  backToRows(): void;
  /** @internal the view's own state. */
  readonly view: ResetFactorsView;
}

interface ResetFactorsView {
  readonly expanded: () => ResetFactorKind | null;
  expand(row: ResetFactorKind): void;
  cannotUse(): void;
  readonly username: () => string;
  readonly busy: () => boolean;
  readonly accountIdB64: () => string | null;
  readonly outlookFailed: () => boolean;
  retryOutlook(): void;
  confirmed(): void;
  /** Run a factor step with the password checked first. `null` when the
   *  password was not proven (the block says why); otherwise what the step
   *  answered, boxed (a step may answer nothing). */
  withPassword<T>(run: () => Promise<T>): Promise<{ readonly value: T } | null>;
  passwordNeeded(err: unknown): void;
  readonly auth: AuthApi;
}

export function createResetFactors(options: {
  readonly auth: AuthApi;
  readonly busy: () => boolean;
  readonly usernameKnown: () => boolean;
  readonly username: () => string;
  readonly accountIdB64: () => string | null;
}): ResetFactors {
  const { auth } = options;
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.reset,
    busy: options.busy,
    sessionToken: () => auth.pendingSessionToken(),
    usernameKnown: options.usernameKnown,
    passwordOnly: true,
  });
  const [cannotUse, setCannotUse] = createSignal(false);
  const [expanded, setExpanded] = createSignal<ResetFactorKind | null>(null);
  const [factors] = createResource(
    () => options.accountIdB64(),
    () => auth.resetFactor.factors(),
  );
  // A confirmation lasts ten minutes: the read checks the clock, so it is
  // polled, and the section opens again by itself when it runs out.
  const [confirmed, setConfirmed] = createSignal<ResetFactorKind | null>(auth.resetFactor.confirmed());
  const poll = setInterval(() => setConfirmed(auth.resetFactor.confirmed()), 1000);
  onCleanup(() => clearInterval(poll));

  const [outlookVersion, setOutlookVersion] = createSignal(0);
  const [outlook] = createResource(
    () => (confirmed() === null ? null : `${confirmed()}:${outlookVersion()}`),
    () => auth.resetFactor.outlook(),
  );

  const section = (): ResetSection =>
    resetSection({
      factors: factors() ?? null,
      passkeysSupported: WebauthnClient.isWebauthnAvailable(),
      passwordProofLive: gate.proofLive(),
      confirmed: confirmed(),
      cannotUse: cannotUse(),
    });

  return {
    section,
    outlook: () => (confirmed() === null || outlook.loading ? null : (outlook() ?? null)),
    factors: () => factors() ?? null,
    gate,
    backToRows: () => setCannotUse(false),
    view: {
      expanded,
      expand: (row) => setExpanded(row),
      cannotUse: () => setCannotUse(true),
      username: options.username,
      busy: options.busy,
      accountIdB64: options.accountIdB64,
      outlookFailed: () => confirmed() !== null && !outlook.loading && (outlook() ?? null) === null,
      retryOutlook: () => setOutlookVersion((v) => v + 1),
      confirmed: () => {
        setConfirmed(auth.resetFactor.confirmed());
        setOutlookVersion((v) => v + 1);
      },
      async withPassword(run) {
        if (!auth.resetFactor.passwordProofLive()) {
          // Signs in with the typed password; a wrong one stops here.
          if ((await gate.ensure()) === null) return null;
        }
        return { value: await run() };
      },
      passwordNeeded: (err) => {
        gate.failed(err, null);
      },
      auth,
    },
  };
}

export function ResetFactorSection(props: { readonly factors: ResetFactors }): JSX.Element {
  const view = props.factors.view;
  const section = props.factors.section;
  return (
    <Switch>
      <Match when={section().kind === "confirmed" ? (section() as { factor: ResetFactorKind }) : null}>
        {(done) => (
          <div class="lk-stack">
            <p class="form-info" role="status">
              <Icon name="checkmark-circle" size={16} /> {t(resetFactorDoneKey(done().factor))}
            </p>
            <OutlookLine factors={props.factors} />
          </div>
        )}
      </Match>
      <Match when={section().kind === "choose" ? (section() as Extract<ResetSection, { kind: "choose" }>) : null}>
        {(choose) => (
          <div class="lk-stack">
            <strong>{t("proof.reset.factor.heading")}</strong>
            <ProofBlock gate={props.factors.gate} />
            <p class="lk-muted">
              {t(choose().showPassword ? "proof.reset.factor.title" : "proof.reset.factor.title_no_password")}
            </p>
            <ul class="lk-method-list">
              <For each={choose().rows}>
                {(row) => (
                  <li>
                    <button
                      type="button"
                      class="lk-method-row"
                      aria-expanded={view.expanded() === row}
                      disabled={view.busy()}
                      onClick={() => view.expand(row)}
                    >
                      <span class="lk-card-row-icon">
                        <Icon name={ROW_ICON[row]} size={20} />
                      </span>
                      <span class="lk-method-row-body">
                        <span class="lk-method-row-title">{t(resetFactorRowKey(row))}</span>
                      </span>
                      <Icon name={view.expanded() === row ? "chevron-down" : "chevron-forward"} size={14} />
                    </button>
                    <Show when={view.expanded() === row}>
                      <div class="lk-stack lk-reset-factor-row">
                        <Switch>
                          <Match when={row === "totp"}>
                            <TotpRow factors={props.factors} />
                          </Match>
                          <Match when={row === "email"}>
                            <EmailRow factors={props.factors} />
                          </Match>
                          <Match when={row === "passkey"}>
                            <PasskeyRow factors={props.factors} />
                          </Match>
                        </Switch>
                      </div>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
            <div>
              <button type="button" class="ghost" disabled={view.busy()} onClick={() => view.cannotUse()}>
                {t("proof.reset.factor.none")}
              </button>
            </div>
          </div>
        )}
      </Match>
    </Switch>
  );
}

/** Whether the row's controls may be used: the password is proven, or typed
 *  (it is checked when the factor is confirmed). */
function passwordReady(factors: ResetFactors): boolean {
  return factors.gate.view().ready && !factors.view.busy();
}

function Line(props: { readonly text: string | null; readonly error: boolean }): JSX.Element {
  return (
    <Show when={props.text !== null}>
      <Show when={props.error} fallback={<p class="form-info" role="status">{props.text}</p>}>
        <div class="form-error" role="alert">
          {props.text}
        </div>
      </Show>
    </Show>
  );
}

const digitsOf = (value: string): string => value.replace(/\D+/g, "").slice(0, 6);

function TotpRow(props: { readonly factors: ResetFactors }): JSX.Element {
  const view = props.factors.view;
  const [code, setCode] = createSignal("");
  const [checking, setChecking] = createSignal(false);
  const [line, setLine] = createSignal<string | null>(null);
  const wait = createRetryCountdown();
  let input: HTMLInputElement | undefined;

  async function check(value: string): Promise<void> {
    setLine(null);
    setChecking(true);
    try {
      const done = await view.withPassword(() => view.auth.resetFactor.confirmTotp(value));
      if (done === null) return;
      view.confirmed();
    } catch (err) {
      const failure = rowFailureOf(err);
      switch (failure.kind) {
        case "password-needed":
          view.passwordNeeded(err);
          break;
        case "refused":
          setLine(t("settings.recovery.totp.error.wrong"));
          break;
        case "rate-limited":
          wait.startFor(failure.retryAfterSecs);
          break;
        case "offline":
          setLine(t("common.error.offline"));
          break;
        default:
          setLine(t("recovery.totp.error.generic"));
      }
    } finally {
      // The digits clear and keep focus.
      setCode("");
      setChecking(false);
      queueMicrotask(() => input?.focus());
    }
  }

  function onInput(value: string): void {
    const digits = digitsOf(value);
    setCode(digits);
    // Checked on the sixth digit, with no separate button.
    if (digits.length === 6 && !checking()) void check(digits);
  }

  return (
    <>
      <p class="lk-muted">{t("proof.reset.factor.totp.prompt")}</p>
      <CodeField
        label={t("recovery.totp.code_label")}
        value={code}
        setValue={onInput}
        disabled={() => checking() || wait.waiting() || !passwordReady(props.factors)}
        inputRef={(el) => {
          input = el;
        }}
      />
      <Line text={line()} error={true} />
      <WaitNotice remainingMs={wait.remainingMs} />
    </>
  );
}

function EmailRow(props: { readonly factors: ResetFactors }): JSX.Element {
  const view = props.factors.view;
  // Pre-filled when this device kept the address (ADR-0075, amended
  // 2026-10-13); otherwise typed.
  const [address, setAddress] = createSignal(heldAddress(view.accountIdB64()) ?? "");
  const [sent, setSent] = createSignal<{
    readonly challengeId: string;
    readonly sentAtMs: number;
    readonly resendAtMs: number;
    readonly expiresInSecs: number;
  } | null>(null);
  const [code, setCode] = createSignal("");
  const [tries, setTries] = createSignal(0);
  const [working, setWorking] = createSignal(false);
  const [line, setLine] = createSignal<string | null>(null);
  const wait = createRetryCountdown();
  let input: HTMLInputElement | undefined;

  const addressLooksWhole = (): boolean => /^[^@\s]+@[^@\s]+$/.test(address().trim());

  function show(failure: RowFailure, err: unknown): void {
    switch (failure.kind) {
      case "password-needed":
        view.passwordNeeded(err);
        break;
      case "rate-limited":
        wait.startFor(failure.retryAfterSecs);
        break;
      case "send-failed":
        setLine(t("recovery_email.send_failed"));
        break;
      case "invalid-address":
        setLine(t("recovery_email.invalid"));
        break;
      case "offline":
        setLine(t("common.error.offline"));
        break;
      default:
        setLine(t("recovery_email.send_failed"));
    }
  }

  async function send(): Promise<void> {
    if (!addressLooksWhole()) {
      setLine(t("recovery_email.invalid"));
      return;
    }
    setLine(null);
    setWorking(true);
    try {
      const answered = await view.withPassword(() => view.auth.resetFactor.emailStart(address()));
      if (answered === null) return;
      const started = answered.value;
      const now = Date.now();
      setSent({
        challengeId: started.challengeId,
        sentAtMs: now,
        resendAtMs: now + started.resendAfterSecs * 1000,
        expiresInSecs: started.expiresInSecs,
      });
      setTries(0);
      setCode("");
      queueMicrotask(() => input?.focus());
    } catch (err) {
      show(rowFailureOf(err), err);
    } finally {
      setWorking(false);
    }
  }

  async function check(value: string): Promise<void> {
    const current = sent();
    if (current === null) return;
    if (emailCodeExpired(current.sentAtMs, current.expiresInSecs, Date.now())) {
      setCode("");
      setLine(t("recovery_email.code.expired"));
      return;
    }
    setLine(null);
    setWorking(true);
    try {
      const done = await view.withPassword(() => view.auth.resetFactor.emailFinish(current.challengeId, value));
      if (done === null) return;
      view.confirmed();
    } catch (err) {
      const failure = rowFailureOf(err);
      if (failure.kind === "refused") {
        const used = tries() + 1;
        setTries(used);
        if (emailCodeRefusal(used) === "used-up") {
          // The row returns to its `Send code` state.
          setSent(null);
          setLine(t("recovery_email.code.used_up"));
        } else {
          setLine(t("recovery_email.code.wrong"));
        }
      } else {
        show(failure, err);
      }
    } finally {
      setCode("");
      setWorking(false);
      queueMicrotask(() => input?.focus());
    }
  }

  function onCode(value: string): void {
    const digits = digitsOf(value);
    setCode(digits);
    if (digits.length === 6 && !working()) void check(digits);
  }

  return (
    <Show
      when={sent()}
      fallback={
        <>
          <label>
            {t("recovery_email.title")}
            <input
              type="email"
              autocomplete="email"
              inputmode="email"
              dir="ltr"
              value={address()}
              onInput={(e) => {
                setLine(null);
                setAddress(e.currentTarget.value);
              }}
              disabled={working() || view.busy()}
            />
          </label>
          <Line text={line()} error={true} />
          <WaitNotice remainingMs={wait.remainingMs} />
          <div>
            <button
              type="button"
              class="primary"
              disabled={working() || wait.waiting() || !addressLooksWhole() || !passwordReady(props.factors)}
              onClick={() => void send()}
            >
              {t("recovery_email.send")}
            </button>
          </div>
        </>
      }
    >
      {(current) => (
        <>
          <p class="lk-muted">
            {t("proof.reset.factor.email.sent", { username: `@${view.username()}` })}
          </p>
          <CodeField
            label={t("recovery_email.code.title")}
            value={code}
            setValue={onCode}
            disabled={() => working() || !passwordReady(props.factors)}
            inputRef={(el) => {
              input = el;
            }}
          />
          <Line text={line()} error={true} />
          <WaitNotice remainingMs={wait.remainingMs} />
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
                setLine(null);
                setSent(null);
              }}
            >
              {t("recovery_email.code.other_address")}
            </button>
          </div>
        </>
      )}
    </Show>
  );
}

/** Run the passkey factor and say what happened; `true` when confirmed. */
async function runPasskey(
  factors: ResetFactors,
  setLine: (line: { text: string; error: boolean } | null) => void,
  wait: ReturnType<typeof createRetryCountdown>,
): Promise<boolean> {
  const view = factors.view;
  setLine(null);
  try {
    const done = await view.withPassword(() => view.auth.resetFactor.confirmPasskey());
    if (done === null) return false;
    view.confirmed();
    return true;
  } catch (err) {
    const failure = rowFailureOf(err);
    switch (failure.kind) {
      case "password-needed":
        view.passwordNeeded(err);
        break;
      case "cancelled":
        setLine({ text: t("proof.reset.cancelled"), error: false });
        break;
      case "other-account":
        setLine({ text: t("proof.passkey.other_account"), error: true });
        break;
      case "rate-limited":
        wait.startFor(failure.retryAfterSecs);
        break;
      case "offline":
        setLine({ text: t("common.error.offline"), error: true });
        break;
      default:
        // A passkey that is not this account's, a refused assertion, any
        // other failure of the gesture.
        setLine({ text: t("proof.passkey.failed"), error: true });
    }
    return false;
  }
}

function PasskeyRow(props: { readonly factors: ResetFactors }): JSX.Element {
  const [waiting, setWaiting] = createSignal(false);
  const [line, setLine] = createSignal<{ text: string; error: boolean } | null>(null);
  const wait = createRetryCountdown();

  async function use(): Promise<void> {
    setWaiting(true);
    try {
      await runPasskey(props.factors, setLine, wait);
    } finally {
      setWaiting(false);
    }
  }

  return (
    <>
      <Show when={waiting()}>
        <p class="lk-muted" role="status">
          {t("recovery.passkey.waiting")}
        </p>
      </Show>
      <Line text={line()?.text ?? null} error={line()?.error ?? false} />
      <WaitNotice remainingMs={wait.remainingMs} />
      <div>
        <button
          type="button"
          class="primary"
          disabled={waiting() || wait.waiting() || !passwordReady(props.factors)}
          onClick={() => void use()}
        >
          {t("proof.reset.factor.passkey.use")}
        </button>
      </div>
    </>
  );
}

/**
 * UX 15.1c: will this reset bring things back? Decided on this device from
 * the account's published protection and what confirmed the reset; the
 * server is never asked. Stands between the confirmed factor and the word.
 */
function OutlookLine(props: { readonly factors: ResetFactors }): JSX.Element {
  const view = props.factors.view;
  const [waiting, setWaiting] = createSignal(false);
  const [line, setLine] = createSignal<{ text: string; error: boolean } | null>(null);
  const wait = createRetryCountdown();
  // The line was read: a new outlook replaces whatever the last attempt said.
  createEffect(() => {
    if (props.factors.outlook() !== null) setLine(null);
  });

  async function usePasskey(): Promise<void> {
    setWaiting(true);
    try {
      await runPasskey(props.factors, setLine, wait);
    } finally {
      setWaiting(false);
    }
  }

  return (
    <Show
      when={props.factors.outlook()}
      fallback={
        <Show when={view.outlookFailed()}>
          <div class="form-error" role="alert">
            {t("common.error.offline")}
          </div>
          <div>
            <button type="button" class="ghost" disabled={view.busy()} onClick={() => view.retryOutlook()}>
              {t("common.try_again")}
            </button>
          </div>
        </Show>
      }
    >
      {(outlook) => (
        <Show when={!outlook().sealed} fallback={<p>{t("restore_identity.reset.sealed")}</p>}>
          <WarningNote text={t("restore_identity.reset.unsealed")} />
          <Show when={outlook().passkeyCouldSeal && WebauthnClient.isWebauthnAvailable()}>
            <p class="lk-muted">{t("restore_identity.reset.unsealed_use_passkey")}</p>
            <Show when={waiting()}>
              <p class="lk-muted" role="status">
                {t("recovery.passkey.waiting")}
              </p>
            </Show>
            <Line text={line()?.text ?? null} error={line()?.error ?? false} />
            <WaitNotice remainingMs={wait.remainingMs} />
            <div>
              <button
                type="button"
                class="ghost"
                disabled={waiting() || wait.waiting() || view.busy()}
                onClick={() => void usePasskey()}
              >
                {t("proof.reset.factor.passkey.use")}
              </button>
            </div>
          </Show>
        </Show>
      )}
    </Show>
  );
}
