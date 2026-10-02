// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a guest is shown outside a call (`docs/ux/call-links.md` section 9).
 *
 * A guest account is made for a person who opened a call link without an
 * account. They were not asked to save a recovery phrase and they were not
 * asked to choose a username: they went into the call. This is the one
 * question, asked once the call is over:
 *
 *   - `Create account`: a username and a password, then the recovery phrase
 *     (the route guards show it, as after any signup);
 *   - `No, thanks`: the account is deleted at once, on the servers and here.
 *
 * Two forms of it:
 *
 *   - {@link GuestAccountOffer}: the block, drawn by the call-link page under
 *     "The call has ended.";
 *   - {@link GuestAccountScreen}: the whole screen, drawn by the route guard
 *     in place of the app, which a guest never lands in. While a call runs
 *     it shows only the way back to the call.
 */
import { createSignal, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "./AuthContext";
import { guestClaimErrorMessage } from "./auth-error-copy";
import { guestScreen } from "./guest-account";
import { MIN_PASSWORD_LENGTH, newPasswordProblem } from "./password-rules";
import { useCallHost } from "../calls/CallHostContext";
import { t } from "../l10n";
import { createUsernameCheck } from "../profile/username-field";
import { mayLookUp, usernameReason } from "../profile/rules";
import { AuthLayout } from "../ui/AuthLayout";
import { PasswordField } from "../ui/PasswordField";

type Step = "ask" | "form";
type Busy = "none" | "creating" | "deleting";

/** The question and, behind `Create account`, the form. */
export function GuestAccountOffer(): JSX.Element {
  const auth = useAuth();
  const [step, setStep] = createSignal<Step>("ask");
  const [busy, setBusy] = createSignal<Busy>("none");
  const [error, setError] = createSignal<string | null>(null);
  /** The delete failed; the account is still there and still a guest. */
  const [deleteFailed, setDeleteFailed] = createSignal(false);
  /** This device cannot show the account's phrase, so it cannot be kept. */
  const [cannotKeep, setCannotKeep] = createSignal(false);

  const [username, setUsername] = createSignal("");
  const usernameCheck = createUsernameCheck(username);
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");

  // The component can be unmounted by its own success (the guard switches to
  // the phrase screen, or the session ends): nothing is written after that.
  let alive = true;
  onCleanup(() => {
    alive = false;
  });

  // A guest account past its lifetime is not offered anything: it is deleted,
  // and whoever drew this block lands on the signed-out screen.
  onMount(() => {
    void auth.expireGuestIfDue().catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("expiring the guest account failed", err);
    });
  });

  /** The username an earlier attempt already took: shown, never asked again. */
  const takenName = (): string | null => auth.guestClaimedUsername();
  const normalised = (): string | null => {
    const c = usernameCheck();
    return c.kind === "valid" ? c.normalised : null;
  };

  async function onCreate(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (busy() !== "none") return;
    setError(null);
    const fixed = takenName();
    let name: string;
    if (fixed !== null) {
      name = fixed;
    } else {
      const checked = usernameCheck();
      if (!mayLookUp(checked)) {
        setError(usernameReason(checked) ?? t("auth.signup.error.enter_username"));
        return;
      }
      name = checked.normalised;
    }
    const problem = newPasswordProblem(password(), confirm());
    if (problem !== null) {
      setError(problem);
      return;
    }
    setBusy("creating");
    try {
      // Resolving leaves the recovery phrase pending; whoever drew this
      // block switches to the phrase screen on that.
      await auth.claimGuestAccount(name, password());
      if (!alive) return;
      setPassword("");
      setConfirm("");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("keeping the guest account failed", err);
      if (!alive) return;
      // The account was past its lifetime and is gone; so is this block.
      if (err instanceof Error && err.name === "GuestAccountExpiredError") return;
      if (err instanceof Error && err.name === "GuestPhraseUnavailableError") {
        setCannotKeep(true);
        setStep("ask");
      }
      setError(guestClaimErrorMessage(err));
    } finally {
      if (alive) setBusy("none");
    }
  }

  async function onDecline(): Promise<void> {
    if (busy() !== "none") return;
    setError(null);
    setDeleteFailed(false);
    setBusy("deleting");
    try {
      await auth.discardGuestAccount();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("deleting the guest account failed", err);
      if (alive) setDeleteFailed(true);
    } finally {
      if (alive) setBusy("none");
    }
  }

  return (
    <div class="lk-stack lk-guest-offer" style={{ gap: "12px" }}>
      <Switch>
        <Match when={step() === "ask"}>
          <h2 style={{ margin: 0 }}>{t("guest.offer.title")}</h2>
          <p style={{ margin: 0 }}>{t("guest.offer.body")}</p>
          <Show when={error()}>
            {(message) => (
              <div class="form-error" role="alert">
                {message()}
              </div>
            )}
          </Show>
          <Show when={deleteFailed()}>
            <div class="form-error" role="alert">
              {t("guest.offer.delete_failed")}
            </div>
          </Show>
          <Show when={!cannotKeep()}>
            <button
              type="button"
              class="primary"
              disabled={busy() !== "none"}
              onClick={() => {
                setError(null);
                setDeleteFailed(false);
                setStep("form");
              }}
            >
              {t("auth.signup.submit")}
            </button>
          </Show>
          <button
            type="button"
            disabled={busy() !== "none"}
            aria-busy={busy() === "deleting"}
            onClick={() => void onDecline()}
          >
            <Show when={busy() === "deleting"} fallback={t("guest.offer.decline")}>
              <span class="lakko-spinner" aria-hidden="true" /> {t("guest.offer.deleting")}
            </Show>
          </button>
          <p class="form-info" style={{ margin: 0 }}>
            {t("guest.offer.decline_note")}
          </p>
        </Match>
        <Match when={step() === "form"}>
          <h2 style={{ margin: 0 }}>{t("auth.signup.title")}</h2>
          <p style={{ margin: 0 }}>{t("guest.claim.lede")}</p>
          <form onSubmit={(e) => void onCreate(e)} class="lk-stack" style={{ gap: "10px" }}>
            <Show
              when={takenName()}
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
                      disabled={busy() !== "none"}
                      aria-invalid={usernameCheck().kind === "invalid"}
                      aria-describedby="guest-username-hint"
                    />
                  </label>
                  <div id="guest-username-hint" role="status" class="form-info">
                    <Show
                      when={usernameReason(usernameCheck())}
                      fallback={<Show when={normalised()}>{(n) => <span>@{n()}</span>}</Show>}
                    >
                      {(reason) => <span class="form-error">{reason()}</span>}
                    </Show>
                  </div>
                </>
              }
            >
              {(name) => (
                <p class="form-info" style={{ margin: 0 }}>
                  {t("guest.claim.username_taken_for_you", { username: name() })}
                </p>
              )}
            </Show>
            <PasswordField
              label={t("auth.password.label")}
              autocomplete="new-password"
              value={password}
              setValue={setPassword}
              disabled={() => busy() !== "none"}
              minLength={MIN_PASSWORD_LENGTH}
            />
            <PasswordField
              label={t("auth.password.confirm.label")}
              autocomplete="new-password"
              value={confirm}
              setValue={setConfirm}
              disabled={() => busy() !== "none"}
              minLength={MIN_PASSWORD_LENGTH}
            />
            <Show when={error()}>
              {(message) => (
                <div class="form-error" role="alert">
                  {message()}
                </div>
              )}
            </Show>
            <button
              class="primary"
              type="submit"
              disabled={busy() !== "none" || (takenName() === null && !mayLookUp(usernameCheck()))}
              aria-busy={busy() === "creating"}
            >
              <Show when={busy() === "creating"} fallback={t("auth.signup.submit")}>
                <span class="lakko-spinner" aria-hidden="true" /> {t("guest.claim.working")}
              </Show>
            </button>
            <button
              type="button"
              disabled={busy() !== "none"}
              onClick={() => {
                setError(null);
                setStep("ask");
              }}
            >
              {t("common.back")}
            </button>
          </form>
        </Match>
      </Switch>
    </div>
  );
}

/**
 * The whole screen a guest sees in place of the app.
 *
 * While a call runs the person belongs in it: the screen says so and offers
 * the way back. The question is asked only when no call is running, so the
 * button that deletes the account is never one press away from a live call.
 */
export function GuestAccountScreen(): JSX.Element {
  const auth = useAuth();
  const callHost = useCallHost();
  const view = (): "none" | "in-call" | "offer" =>
    guestScreen({ guest: auth.isGuest(), callInProgress: callHost.anyCallInProgress() });
  return (
    <AuthLayout>
      <Switch>
        <Match when={view() === "in-call"}>
          <div class="lk-stack" style={{ gap: "12px" }}>
            <h2 style={{ margin: 0 }}>{t("guest.in_call.title")}</h2>
            <button type="button" class="primary" onClick={() => callHost.expandCall()}>
              {t("guest.in_call.return")}
            </button>
          </div>
        </Match>
        <Match when={view() === "offer"}>
          <GuestAccountOffer />
        </Match>
      </Switch>
    </AuthLayout>
  );
}
