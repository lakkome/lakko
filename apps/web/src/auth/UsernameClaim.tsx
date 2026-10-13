// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Choose a username" (ADR-0075 pass A, part 2): a guest account, created by
 * the call-link join, gets a username and its first password on one screen.
 *
 * Order: the name is validated here; `auth.claimGuestUsername` binds the
 * login handle and registers the name (the sign-up's claim, run later) and
 * saves it on this device; the password is set with the device's seed-based
 * change, with no proof block (the account has no known password); then the
 * own profile is published again with the username and its certificate. If
 * the password step fails after the name was saved, the name is kept, the
 * username field is gone and only the password is retried.
 *
 * After success the screen says the name once; closing it returns the
 * person to where they were and offers the "Protect your account" step,
 * which a guest never saw.
 *
 * Every decision is `username-claim.ts`. The host is whole-session; the
 * entry points open it through `username-claim-host.ts`.
 */
import { createSignal, Match, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "./AuthContext";
import { handleBindLimitedMessage, isHandleBindLimited, isOffline } from "./auth-error-copy";
import { AuthHttpError, AuthNetworkError } from "./client";
import { UsernameTakenError } from "./login-handle";
import { newPasswordErrorMessage } from "./NewPasswordForm";
import { MIN_PASSWORD_LENGTH, newPasswordProblem } from "./password-rules";
import {
  claimFailure,
  claimReduce,
  claimStart,
  claimSubmits,
  type ClaimEvent,
  type ClaimFailure,
  type ClaimState,
} from "./username-claim";
import { closeUsernameClaim, usernameClaimOpen } from "./username-claim-host";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { mayLookUp, usernameReason } from "../profile/rules";
import { createUsernameCheck } from "../profile/username-field";
import { ModalLayer } from "../ui/ModalLayer";
import { PasswordField } from "../ui/PasswordField";

/** The whole-session host: the screen in the app's own layer. */
export function UsernameClaimHost(): JSX.Element {
  const auth = useAuth();
  const shown = (): boolean => usernameClaimOpen() && auth.state().kind === "session";
  return (
    <ModalLayer open={shown()} title={t("username.claim.title")} onClose={closeUsernameClaim}>
      <Show when={shown()}>
        <UsernameClaimScreen onClose={closeUsernameClaim} />
      </Show>
    </ModalLayer>
  );
}

function failureText(failure: ClaimFailure): string {
  switch (failure.kind) {
    case "taken":
      return t("auth.signup.error.username_taken");
    case "bind-limited":
      return handleBindLimitedMessage(failure.retryAfterSecs);
    case "offline":
      return t("common.error.offline");
    case "error":
      return t("auth.signup.error.generic");
  }
}

function UsernameClaimScreen(props: { readonly onClose: () => void }): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();
  const sessionUsername = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : "";
  };
  const [state, setState] = createSignal<ClaimState>(claimStart(sessionUsername(), auth.firstPasswordOwed()));
  const dispatch = (event: ClaimEvent): void => {
    setState((current) => claimReduce(current, event));
  };
  const [username, setUsername] = createSignal("");
  const usernameCheck = createUsernameCheck(username);
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  /** The form's own line: a password that does not pass the rule, or the
   *  password step's error. */
  const [ownError, setOwnError] = createSignal<string | null>(null);
  // The certificate the register call returned, for the publish that
  // follows the password. A reload loses it; the profile layer then fetches
  // it as usual.
  let certificate: Uint8Array | null = null;

  const working = (): boolean => {
    const s = state();
    return s.step !== "done" && s.working;
  };
  const failure = (): ClaimFailure | null => {
    const s = state();
    return s.step === "done" ? null : s.failure;
  };

  async function setFirstPassword(): Promise<void> {
    try {
      await auth.replacePassword(password(), { firstPassword: true });
    } catch (err) {
      // The name is kept; only the password is retried.
      setOwnError(newPasswordErrorMessage(err));
      dispatch({ type: "failed", failure: { kind: "error" } });
      return;
    }
    setPassword("");
    setConfirm("");
    dispatch({ type: "password-saved" });
    try {
      await profiles.usernameClaimed(certificate);
    } catch {
      // The publishers retry by themselves; the claim itself is done.
    }
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const current = state();
    if (current.step === "done" || current.working) return;
    setOwnError(null);
    const problem = newPasswordProblem(password(), confirm());
    if (problem !== null) {
      setOwnError(problem);
      return;
    }
    if (claimSubmits(current) === "password-only") {
      dispatch({ type: "submitted" });
      await setFirstPassword();
      return;
    }
    const checked = usernameCheck();
    if (!mayLookUp(checked)) {
      setOwnError(usernameReason(checked) ?? t("auth.signup.error.enter_username"));
      return;
    }
    dispatch({ type: "submitted" });
    try {
      const claimed = await auth.claimGuestUsername(checked.normalised);
      certificate = claimed.certificate;
      dispatch({ type: "name-saved", username: claimed.username });
    } catch (err) {
      dispatch({
        type: "failed",
        failure: claimFailure({
          taken: err instanceof UsernameTakenError,
          bindLimited: isHandleBindLimited(err),
          retryAfterSecs: err instanceof AuthHttpError ? err.retryAfter : null,
          offline: err instanceof AuthNetworkError || isOffline(err),
        }),
      });
      return;
    }
    await setFirstPassword();
  }

  /** Closing after success: back to where the person was, with the
   *  "Protect your account" step offered once. */
  function onDone(): void {
    props.onClose();
    auth.offerProtectStep();
  }

  const done = () => {
    const s = state();
    return s.step === "done" ? s : null;
  };
  const passwordOnly = () => {
    const s = state();
    return s.step === "password" ? s : null;
  };

  return (
    <Switch>
      <Match when={done()}>
        {(s) => (
          <div class="lk-stack">
            <p class="form-info" role="status">
              {t("username.claim.done", { username: `@${s().username}` })}
            </p>
            <div class="lk-actions">
              <button type="button" class="primary" onClick={onDone}>
                {t("common.continue")}
              </button>
            </div>
          </div>
        )}
      </Match>
      <Match when={true}>
        <form onSubmit={(e) => void onSubmit(e)}>
          <p class="lk-muted">{t("username.claim.body")}</p>
          <Show
            when={passwordOnly()}
            fallback={
              <>
                <label>
                  {t("auth.username.label")}
                  <input
                    type="text"
                    autocomplete="username"
                    autocapitalize="none"
                    spellcheck={false}
                    required
                    value={username()}
                    onInput={(e) => setUsername(e.currentTarget.value)}
                    disabled={working()}
                    aria-invalid={usernameCheck().kind === "invalid"}
                  />
                </label>
                <div role="status" class="form-info">
                  <Show
                    when={usernameReason(usernameCheck())}
                    fallback={
                      <Show when={mayLookUp(usernameCheck()) ? usernameCheck() : null}>
                        {(c) => <span>@{(c() as { normalised: string }).normalised}</span>}
                      </Show>
                    }
                  >
                    {(reason) => <span class="form-error">{reason()}</span>}
                  </Show>
                </div>
              </>
            }
          >
            {(s) => (
              // The name is saved: only the password is asked again.
              <p class="form-info" role="status">
                {t("username.claim.done", { username: `@${s().username}` })}
              </p>
            )}
          </Show>
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
          <p class="lk-muted">{t("username.claim.password_help")}</p>
          <Show when={ownError() ?? (failure() !== null && passwordOnly() === null ? failureText(failure() as ClaimFailure) : null)}>
            {(line) => (
              <div class="form-error" role="alert">
                {line()}
              </div>
            )}
          </Show>
          <div class="lk-actions">
            <button class="primary" type="submit" disabled={working()}>
              {working() ? t("common.saving") : t("username.claim.save")}
            </button>
            <button type="button" class="ghost" disabled={working()} onClick={props.onClose}>
              {t("common.cancel")}
            </button>
          </div>
        </form>
      </Match>
    </Switch>
  );
}
