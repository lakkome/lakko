// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Was it you?" (docs/ux/identity-recovery.md section 18.4, ADR-0074
 * part 1): the screen of a device that held the account's keys when they
 * were reset on another device.
 *
 * The chats, contacts and conversation data are already deleted; what is
 * left is the recall record. The screen asks the question, and:
 *
 *   - "Yes, it was me": one confirmation inside this screen, then the rest
 *     of the wipe and "Sign in" (section 15.2);
 *   - "No, take the account back": the form. The password from before the
 *     reset proves it; the account gets a new password, new keys and a new
 *     recovery phrase, and whoever reset the keys is signed out.
 *
 * Every decision is `auth/recall.ts` (`recallReduce`); this component draws
 * the state and runs the three actions of `auth.recall`. No system dialog.
 */
import {
  createEffect,
  createSignal,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
  type JSX,
} from "solid-js";

import { useAuth } from "../auth/AuthContext";
import {
  RECALL_SCREEN_START,
  phraseRecallOffered,
  recallExpired,
  recallFormReady,
  recallNoEnabled,
  recallReduce,
  type RecallEvent,
  type RecallScreen as RecallScreenState,
} from "../auth/recall";
import { MIN_PASSWORD_LENGTH } from "../auth/password-rules";
import { WaitNotice } from "../auth/recover/FormNotices";
import { currentLanguage, t } from "../l10n";
import { tRich } from "../l10n/rich";
import { AuthLayout } from "../ui/AuthLayout";
import { PasswordField } from "../ui/PasswordField";
import { PhraseInput } from "../ui/PhraseInput";
import { SIGNED_OUT_COPY } from "./sync/removed-device";

/** How often an unanswered check is repeated, and the record's own end is
 *  looked at. Chosen. */
const RECHECK_MS = 30_000;

/** The platform's medium date in the interface language (UX 18.8). */
function mediumDate(ms: number): string {
  return new Date(ms).toLocaleDateString(currentLanguage(), { dateStyle: "medium" });
}

export function RecallScreen(props: {
  readonly username: string;
  readonly learnedAtMs: number;
}): JSX.Element {
  const auth = useAuth();
  const [screen, setScreen] = createSignal<RecallScreenState>(RECALL_SCREEN_START);
  const dispatch = (event: RecallEvent): void => {
    setScreen((current) => recallReduce(current, event));
  };

  const [oldPassword, setOldPassword] = createSignal("");
  const [newPassword, setNewPassword] = createSignal("");
  const [repeatPassword, setRepeatPassword] = createSignal("");
  // ADR-0075 section 8 (UX 19.7): "Use my recovery phrase instead". The 12
  // words from before the reset stand in for the old password.
  const [usePhrase, setUsePhrase] = createSignal(false);
  const [oldPhrase, setOldPhrase] = createSignal("");
  // The end of a rate limit, unix ms, and a tick that redraws the countdown.
  const [waitUntilMs, setWaitUntilMs] = createSignal(0);
  const [nowMs, setNowMs] = createSignal(Date.now());
  // A limit that named no wait: the plain sentence instead of a countdown.
  const [waitUnnamed, setWaitUnnamed] = createSignal(false);
  const waitRemainingMs = (): number => Math.max(0, waitUntilMs() - nowMs());

  let oldPasswordInput: HTMLInputElement | undefined;
  let noButton: HTMLButtonElement | undefined;
  let checking = false;

  async function runCheck(): Promise<void> {
    if (checking) return;
    const step = screen().step;
    if (step === "ending" || step === "landed") return;
    checking = true;
    try {
      dispatch({ type: "checked", check: await auth.recall.check() });
    } finally {
      checking = false;
    }
  }

  onMount(() => {
    void runCheck();
    const timer = setInterval(() => {
      setNowMs(Date.now());
    }, 1_000);
    const recheck = setInterval(() => {
      // The window by this device's clock ends the record with no request.
      if (recallExpired(props.learnedAtMs, Date.now())) {
        dispatch({ type: "expired" });
        return;
      }
      const s = screen();
      if ((s.step === "question" || s.step === "confirm-yes") && (s.check === "checking" || s.check.kind === "unknown")) {
        void runCheck();
      }
    }, RECHECK_MS);
    const onOnline = (): void => void runCheck();
    window.addEventListener("online", onOnline);
    onCleanup(() => {
      clearInterval(timer);
      clearInterval(recheck);
      window.removeEventListener("online", onOnline);
    });
  });

  // The record is destroyed and the wipe finished exactly once.
  let ended = false;
  createEffect(() => {
    const s = screen();
    if (s.step !== "ending" || ended) return;
    ended = true;
    setOldPassword("");
    setNewPassword("");
    setRepeatPassword("");
    void auth.recall.end(s.notice);
  });

  // Focus lands on "No, take the account back" once it can be used.
  let focusedNo = false;
  createEffect(() => {
    if (recallNoEnabled(screen()) && !focusedNo) {
      focusedNo = true;
      queueMicrotask(() => noButton?.focus());
    }
  });

  const name = (): JSX.Element =>
    props.username.length > 0 ? <strong>@{props.username}</strong> : SIGNED_OUT_COPY.thisAccount();

  /** What the question step knows about taking back. */
  const check = () => {
    const s = screen();
    return s.step === "question" || s.step === "confirm-yes" ? s.check : null;
  };
  const openStatus = () => {
    const s = screen();
    if (s.step === "form") return s.status;
    const c = check();
    return c !== null && c !== "checking" && c.kind === "open" ? c.status : null;
  };
  /** The date the keys were reset: the server's when known, else the day
   *  this device learned of it. */
  const resetDate = (): string => mediumDate(openStatus()?.changedAtMs ?? props.learnedAtMs);

  const form = () => {
    const s = screen();
    return s.step === "form" ? s : null;
  };
  const formReady = (): boolean =>
    recallFormReady({
      oldPassword: oldPassword(),
      newPassword: newPassword(),
      repeatPassword: repeatPassword(),
      phrase: usePhrase() ? oldPhrase() : null,
    });
  /** The phrase may prove it only when the server said so for this anchor. */
  const phraseOffered = (): boolean => {
    const current = form();
    return current !== null && phraseRecallOffered({ kind: "open", status: current.status });
  };

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const current = form();
    if (current === null || current.working || !formReady() || waitRemainingMs() > 0) return;
    dispatch({ type: "submitted" });
    const byPhrase = usePhrase() && phraseOffered();
    const failure = byPhrase
      ? await auth.recall.takeBack("", newPassword(), { phrase: oldPhrase() })
      : await auth.recall.takeBack(oldPassword(), newPassword());
    if (failure === null) {
      setOldPhrase("");
      setOldPassword("");
      setNewPassword("");
      setRepeatPassword("");
      dispatch({ type: "landed" });
      return;
    }
    // A wrong password: how many tries are left, or that none is.
    const after = failure.kind === "wrong-password" ? await auth.recall.check() : null;
    if (failure.kind === "rate-limited") {
      setWaitUnnamed(failure.retryAfterSecs === null);
      setNowMs(Date.now());
      setWaitUntilMs(Date.now() + (failure.retryAfterSecs ?? 0) * 1_000);
    }
    dispatch({ type: "attempt-failed", failure, check: after });
    if (failure.kind === "wrong-password") {
      // The field keeps focus and is selected.
      queueMicrotask(() => {
        oldPasswordInput?.focus();
        oldPasswordInput?.select();
      });
    }
  }

  return (
    <AuthLayout>
      <Switch>
        <Match when={screen().step === "question"}>
          {/* One announcement for the title, the body and the question. */}
          <div role="group" aria-live="polite">
            <h1>{SIGNED_OUT_COPY.title()}</h1>
            <p class="lk-auth-lede">
              {tRich("devices.recall.body", { name: name() }, { date: resetDate() })}
            </p>
            <p>
              <strong>{t("devices.recall.question")}</strong>
            </p>
          </div>
          <div class="lk-stack" style={{ gap: "8px" }}>
            <button type="button" style={{ width: "100%" }} onClick={() => dispatch({ type: "yes" })}>
              {t("devices.recall.yes")}
            </button>
            <button
              ref={(el) => (noButton = el)}
              type="button"
              class="primary"
              style={{ width: "100%" }}
              disabled={!recallNoEnabled(screen())}
              onClick={() => dispatch({ type: "no" })}
            >
              {t("devices.recall.no")}
            </button>
          </div>
          <Switch>
            <Match when={openStatus()}>
              {(status) => (
                <p class="form-info" role="status">
                  {t("devices.recall.until", { date: mediumDate(status().expiresAtMs) })}
                </p>
              )}
            </Match>
            <Match
              when={(() => {
                const c = check();
                return c !== null && c !== "checking" && c.kind === "unknown" && c.offline;
              })()}
            >
              <p class="form-error" role="status">
                {t("common.error.offline")}
              </p>
            </Match>
            <Match when={true}>
              <p class="form-info" role="status">
                {t("devices.recall.checking")}
              </p>
            </Match>
          </Switch>
        </Match>
        <Match when={screen().step === "confirm-yes"}>
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <p class="lk-auth-lede" role="status">
            {t("devices.recall.yes.confirm")}
          </p>
          <div class="lk-actions">
            <button type="button" onClick={() => dispatch({ type: "yes-back" })}>
              {t("devices.recall.yes.back")}
            </button>
            <button type="button" class="primary" onClick={() => dispatch({ type: "yes-confirmed" })}>
              {t("devices.recall.yes.delete")}
            </button>
          </div>
        </Match>
        <Match when={form()}>
          {(current) => (
            <form onSubmit={(e) => void onSubmit(e)}>
              <h1>{t("devices.recall.form.title")}</h1>
              <p class="lk-auth-lede">{t("devices.recall.form.lede")}</p>
              <Show
                when={usePhrase() && phraseOffered()}
                fallback={
                  <PasswordField
                    label={t("devices.recall.form.old_password")}
                    autocomplete="current-password"
                    value={oldPassword}
                    setValue={setOldPassword}
                    disabled={() => current().working}
                    inputRef={(el) => (oldPasswordInput = el)}
                    describedBy="lk-recall-old-password-error"
                  />
                }
              >
                <PhraseInput value={oldPhrase} setValue={setOldPhrase} disabled={() => current().working} />
              </Show>
              <Show when={phraseOffered()}>
                <button
                  type="button"
                  class="ghost"
                  disabled={current().working}
                  onClick={() => setUsePhrase((v) => !v)}
                >
                  {usePhrase() ? t("proof.password.use") : t("recall.phrase.use")}
                </button>
              </Show>
              <div id="lk-recall-old-password-error">
                <Show
                  when={(() => {
                    const error = current().error;
                    return error !== null && error.kind === "wrong-password" ? error : null;
                  })()}
                >
                  {(error) => (
                    <>
                      <div class="form-error" role="alert">
                        {usePhrase() && phraseOffered()
                          ? t("recall.phrase.wrong")
                          : t("devices.recall.error.wrong_password")}
                      </div>
                      <Show when={error().triesLeft}>
                        {(left) => (
                          <div class="form-error" role="status" aria-live="polite">
                            {t("devices.recall.error.tries_left", { count: left() })}
                          </div>
                        )}
                      </Show>
                    </>
                  )}
                </Show>
              </div>
              <PasswordField
                label={t("devices.recall.form.new_password")}
                autocomplete="new-password"
                value={newPassword}
                setValue={setNewPassword}
                disabled={() => current().working}
                minLength={MIN_PASSWORD_LENGTH}
              />
              <PasswordField
                label={t("devices.recall.form.repeat_password")}
                autocomplete="new-password"
                value={repeatPassword}
                setValue={setRepeatPassword}
                disabled={() => current().working}
              />
              <ul>
                <li>{t("devices.recall.form.effect_keys")}</li>
                <li>{t("devices.recall.form.effect_factors")}</li>
                <li>{t("devices.recall.form.effect_messages")}</li>
              </ul>
              <Switch>
                <Match when={current().error?.kind === "rate-limited" && waitRemainingMs() > 0}>
                  <WaitNotice remainingMs={waitRemainingMs} />
                </Match>
                <Match when={current().error?.kind === "rate-limited" && waitUnnamed()}>
                  <div class="form-error" role="alert">
                    {t("common.error.too_many_attempts")}
                  </div>
                </Match>
                <Match when={current().error?.kind === "offline"}>
                  <div class="form-error" role="alert">
                    {t("common.error.offline")}
                  </div>
                </Match>
                <Match when={current().error?.kind === "error"}>
                  <div class="form-error" role="alert">
                    {t("common.error.unavailable")}
                  </div>
                </Match>
              </Switch>
              <div class="lk-actions">
                <button
                  type="button"
                  disabled={current().working}
                  onClick={() => dispatch({ type: "form-back" })}
                >
                  {t("common.back")}
                </button>
                <button
                  class="primary"
                  type="submit"
                  disabled={current().working || !formReady() || waitRemainingMs() > 0}
                >
                  {current().working
                    ? t("devices.recall.form.working")
                    : t("devices.recall.form.submit")}
                </button>
              </div>
            </form>
          )}
        </Match>
        <Match when={true}>
          {/* `ending` / `landed`: the auth layer's next state replaces this
              screen at once. */}
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <p class="form-info" role="status" aria-live="polite">
            {SIGNED_OUT_COPY.removing()}
          </p>
        </Match>
      </Switch>
    </AuthLayout>
  );
}
