// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Setting up an authenticator app (docs/ux/identity-recovery.md 19.1b;
 * ADR-0018 factor (c); ADR-0073 and ADR-0075, amended 2026-11-26): ONE
 * screen, used by the "Protect your account" step (`TotpSetup`) and by
 * Settings > Security > Recovery (`TotpEnrollView`, the card around it).
 *
 * The code signs the person in and confirms a key reset; it never brings
 * keys. The setup needs the 17.1 proof (it adds one of the reset's second
 * confirmations): the sign-up counts for ten minutes, otherwise the proof
 * block is drawn above the QR code.
 *
 * Every decision is `protect/totp-setup.ts`; this file wires it to the
 * server and draws the state. `Open in authenticator app` is phones-only
 * and is not drawn on web or desktop.
 */
import { createSignal, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "./AuthContext";
import { AuthClient, AuthHttpError, AuthNetworkError } from "./client";
import { PROOF_COPY, type ProofCopy } from "./fresh-auth";
import { createProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import { WaitNotice } from "./recover/FormNotices";
import { createRetryCountdown } from "./recover/retry-countdown";
import { accountFactors, refreshFactors } from "./protect/factors";
import {
  completeTotpCode,
  groupedTotpKey,
  TOTP_SETUP_START,
  totpSetupFailure,
  totpSetupFailureKey,
  totpSetupReduce,
  type TotpSetupEvent,
  type TotpSetupFailure,
  type TotpSetupState,
} from "./protect/totp-setup";
import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { BrandedQr } from "../ui/BrandedQr";
import { Icon } from "../ui/icons";

/** The proof block's lines on this surface: the recovery email's, with the
 *  authenticator's own helper. */
const TOTP_PROOF_COPY: ProofCopy = {
  ...PROOF_COPY.recoveryEmail,
  helpKey: "proof.add_totp.help",
  genericKey: "settings.recovery.totp.error.generic",
};

function answerOf(err: unknown): {
  readonly status: number | null;
  readonly errorCode: string | null;
  readonly retryAfterSecs: number | null;
} {
  if (err instanceof AuthHttpError) {
    return { status: err.status, errorCode: err.errorCode, retryAfterSecs: err.retryAfter };
  }
  if (err instanceof AuthNetworkError) return { status: null, errorCode: null, retryAfterSecs: null };
  // Not an answer of the server at all: the generic line.
  return { status: 0, errorCode: null, retryAfterSecs: null };
}

/**
 * The setup screen itself (UX 19.1b): the QR code, the key in groups of
 * four with `Copy`, the code field checked on the sixth digit, `Back`.
 */
export function TotpSetup(props: {
  /** The authenticator was confirmed. */
  readonly onDone: () => void;
  /** `Back` before Done: nothing is set up; the key shown is never used. */
  readonly onBack?: () => void;
  readonly backLabel?: string;
}): JSX.Element {
  const auth = useAuth();
  const [state, setState] = createSignal<TotpSetupState>(TOTP_SETUP_START);
  const dispatch = (event: TotpSetupEvent): void => {
    setState((current) => totpSetupReduce(current, event));
  };
  const [code, setCode] = createSignal("");
  const [copied, setCopied] = createSignal<"done" | "failed" | null>(null);
  const wait = createRetryCountdown();
  let codeInput: HTMLInputElement | undefined;
  let copyTimer: ReturnType<typeof setTimeout> | null = null;
  onCleanup(() => {
    if (copyTimer !== null) clearTimeout(copyTimer);
    setCode("");
  });

  const session = (): { accountIdB64: string; token: string; hasUsername: boolean } | null => {
    const s = auth.state();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (s.kind !== "session" || token === null) return null;
    return {
      accountIdB64: s.account.accountId,
      token,
      hasUsername: s.account.username.trim().length > 0,
    };
  };
  const starting = (): boolean => {
    const s = state();
    return s.step === "starting" || (s.step === "proof" && s.working);
  };
  const checking = (): boolean => {
    const s = state();
    return s.step === "ready" && s.checking;
  };

  const gate = createProofGate({
    auth,
    copy: TOTP_PROOF_COPY,
    busy: () => starting() || checking(),
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    usernameKnown: () => session()?.hasUsername === true,
  });
  onCleanup(() => gate.clear());

  function fail(request: "start" | "finish", err: unknown, grant: string | null): void {
    const failure: TotpSetupFailure = totpSetupFailure(request, answerOf(err));
    if (failure.kind === "proof-required") {
      // Drops the refused grant and shows the block with no error.
      gate.failed(err, grant);
    }
    if (failure.kind === "rate-limited" && failure.retryAfterSecs !== null) {
      wait.startFor(failure.retryAfterSecs);
    }
    if (failure.kind === "already") {
      // Set up elsewhere meanwhile: the factors say so from now on.
      const s = session();
      if (s !== null) void refreshFactors(s.accountIdB64, s.token);
    }
    if (failure.kind === "other") console.warn("[totp-setup] failed", err);
    dispatch({ type: "failed", failure });
  }

  /** Make the key: needs a live proof (the block is drawn when none is). */
  async function start(): Promise<void> {
    const s = session();
    if (s === null || starting() || wait.waiting()) return;
    let grant: string | null = null;
    try {
      grant = await gate.ensure();
      if (grant === null) return;
      dispatch({ type: "start" });
      const result = await new AuthClient().totpEnrollStart(s.token, grant);
      setCode("");
      dispatch({ type: "started", otpauthUri: result.otpauthUri, secretB32: result.secretB32 });
      queueMicrotask(() => codeInput?.focus());
    } catch (err) {
      fail("start", err, grant);
    }
  }

  /** The sixth digit was typed: check the code. */
  async function check(digits: string): Promise<void> {
    const s = session();
    const current = state();
    if (s === null || current.step !== "ready" || current.checking || wait.waiting()) return;
    let grant: string | null = null;
    try {
      grant = await gate.ensure();
      if (grant === null) {
        // The proof ran out while the key was on screen: the block appears
        // above it; the typed code is kept for the retry.
        dispatch({ type: "failed", failure: { kind: "proof-required" } });
        return;
      }
      dispatch({ type: "check" });
      await new AuthClient().totpEnrollFinish(s.token, digits, grant);
      setCode("");
      void refreshFactors(s.accountIdB64, s.token);
      dispatch({ type: "confirmed" });
      props.onDone();
    } catch (err) {
      const failure = totpSetupFailure("finish", answerOf(err));
      // A wrong code clears the digits and keeps focus; a lost connection
      // keeps what was typed.
      if (failure.kind === "wrong") setCode("");
      fail("finish", err, grant);
      queueMicrotask(() => codeInput?.focus());
    }
  }

  function onCodeInput(value: string): void {
    setCode(value);
    const digits = completeTotpCode(value);
    if (digits !== null) void check(digits);
  }

  async function onCopy(secretB32: string): Promise<void> {
    if (copyTimer !== null) clearTimeout(copyTimer);
    try {
      await navigator.clipboard.writeText(secretB32);
      setCopied("done");
    } catch {
      setCopied("failed");
    }
    copyTimer = setTimeout(() => setCopied(null), 2000);
  }

  // The sign-up's proof (or any live one) starts the setup at once; without
  // one the block is drawn and `Continue` starts it.
  onMount(() => {
    if (auth.freshProofLive()) void start();
  });

  const ready = () => {
    const s = state();
    return s.step === "ready" ? s : null;
  };
  const proofStep = () => {
    const s = state();
    return s.step === "proof" ? s : null;
  };
  const lineOf = (failure: TotpSetupFailure | null): string | null => {
    const key = totpSetupFailureKey(failure);
    return key === null ? null : t(key);
  };
  const back = (): JSX.Element => (
    <Show when={props.onBack}>
      {(onBack) => (
        <button type="button" class="ghost" disabled={checking()} onClick={() => onBack()()}>
          {props.backLabel ?? t("common.back")}
        </button>
      )}
    </Show>
  );

  return (
    <Switch>
      <Match when={state().step === "done"}>
        <p class="form-info" role="status">
          {t("settings.recovery.totp.done")}
        </p>
      </Match>
      <Match when={state().step === "already"}>
        <p class="form-info" role="status">
          {t("settings.recovery.totp.already_enabled")}
        </p>
        <div class="lk-actions">{back()}</div>
      </Match>
      <Match when={state().step === "failed"}>
        <p class="form-error" role="alert">
          {t("settings.recovery.totp.error.generic")}
        </p>
        <div class="lk-actions">
          <button
            type="button"
            class="primary"
            onClick={() => {
              dispatch({ type: "again" });
              void start();
            }}
          >
            {t("common.try_again")}
          </button>
          {back()}
        </div>
      </Match>
      <Match when={state().step === "starting"}>
        <p class="lk-muted" role="status">
          <span class="lakko-spinner" aria-hidden="true" /> {t("common.preparing")}
        </p>
        <div class="lk-actions">{back()}</div>
      </Match>
      <Match when={proofStep()}>
        {(current) => (
          <form
            class="lk-stack"
            onSubmit={(e) => {
              e.preventDefault();
              void start();
            }}
          >
            <ProofBlock gate={gate} />
            <Show when={lineOf(current().failure)}>
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
                disabled={starting() || wait.waiting() || !gate.view().ready}
              >
                {starting() ? t("common.preparing") : t("common.continue")}
              </button>
              {back()}
            </div>
          </form>
        )}
      </Match>
      <Match when={ready()}>
        {(current) => (
          <div class="lk-stack">
            {/* The proof ran out while the key was on screen: the block is
                drawn above the QR code, with no error styling. */}
            <ProofBlock gate={gate} />
            <p>{t("settings.recovery.totp.scan")}</p>
            <BrandedQr text={current().otpauthUri} />
            <p class="lk-meta lk-totp-key">
              {tRich("settings.recovery.totp.key", {
                key: <code class="lk-totp-key-value">{groupedTotpKey(current().secretB32)}</code>,
              })}{" "}
              <button type="button" class="small" onClick={() => void onCopy(current().secretB32)}>
                <Icon name="copy" size={20} />
                {copied() === "done" ? t("common.copied") : t("common.copy")}
              </button>
            </p>
            <Show when={copied() === "failed"}>
              <p class="form-error" role="alert">
                {t("common.copy_failed")}
              </p>
            </Show>
            <label>
              {t("recovery.totp.code_label")}
              <input
                ref={(el) => (codeInput = el)}
                type="text"
                inputmode="numeric"
                autocomplete="one-time-code"
                pattern="[0-9]{6}"
                maxlength="6"
                value={code()}
                onInput={(e) => onCodeInput(e.currentTarget.value)}
                disabled={current().checking || wait.waiting()}
                class="lk-code-input"
              />
            </label>
            <Show when={current().checking}>
              <p class="lk-muted" role="status">
                <span class="lakko-spinner" aria-hidden="true" /> {t("common.checking")}
              </p>
            </Show>
            <Show when={lineOf(current().failure)}>
              {(line) => (
                <div class="form-error" role="alert">
                  {line()}
                </div>
              )}
            </Show>
            <WaitNotice remainingMs={wait.remainingMs} />
            <div class="lk-actions">{back()}</div>
          </div>
        )}
      </Match>
    </Switch>
  );
}

/**
 * The Authenticator app card of Settings > Security > Recovery: "On" once
 * an authenticator is set up, else `Set up authenticator`, which opens the
 * setup in place.
 */
export function TotpEnrollView(): JSX.Element {
  const auth = useAuth();
  const [open, setOpen] = createSignal(false);
  const [justDone, setJustDone] = createSignal(false);
  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  const enabled = (): boolean => accountFactors(accountId())?.totp === true;

  return (
    <div class="lk-stack">
      <p class="lk-card-desc">{t("settings.recovery.totp.lede")}</p>
      <Switch>
        <Match when={justDone()}>
          <p class="form-info" role="status">
            {t("settings.recovery.totp.done")}
          </p>
          <p>
            <span class="lk-badge muted">{t("settings.recovery.totp.on")}</span>
          </p>
        </Match>
        <Match when={enabled()}>
          <p>
            <span class="lk-badge muted">{t("settings.recovery.totp.on")}</span>
          </p>
        </Match>
        <Match when={open()}>
          <TotpSetup
            onDone={() => {
              setJustDone(true);
              setOpen(false);
            }}
            onBack={() => setOpen(false)}
          />
        </Match>
        <Match when={true}>
          <div>
            <button class="primary" type="button" onClick={() => setOpen(true)}>
              {t("settings.recovery.totp.set_up")}
            </button>
          </div>
        </Match>
      </Switch>
    </div>
  );
}
