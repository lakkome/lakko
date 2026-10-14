// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a guest is shown outside a call (`docs/ux/call-links.md` section 9).
 *
 * A guest account is made for a person who opened a call link without an
 * account. They were not asked to save a recovery phrase and they were not
 * asked to choose a username: they went into the call. This is the one
 * question, asked once the call is over, never before or during it:
 *
 *   - `Create account`: a username and a first password on one screen
 *     (`UsernameClaimScreen`, ADR-0076). No recovery phrase is shown and no
 *     words are checked: once the password is set the account is an ordinary
 *     one, and the route guards show the "Protect your account" step
 *     (ADR-0075), exactly as after a sign-up;
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
import { guestScreen } from "./guest-account";
import { UsernameClaimScreen } from "./UsernameClaim";
import { useCallHost } from "../calls/CallHostContext";
import { t } from "../l10n";
import { AuthLayout } from "../ui/AuthLayout";

type Step = "ask" | "form";

/** The question and, behind `Create account`, the form. */
export function GuestAccountOffer(): JSX.Element {
  const auth = useAuth();
  const [step, setStep] = createSignal<Step>("ask");
  const [deleting, setDeleting] = createSignal(false);
  /** The delete failed; the account is still there and still a guest. */
  const [deleteFailed, setDeleteFailed] = createSignal(false);
  /** This device cannot read the account's phrase, so it cannot be kept. */
  const [cannotKeep, setCannotKeep] = createSignal(false);

  // The component can be unmounted by its own success (the guard switches to
  // the next step, or the session ends): nothing is written after that.
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

  async function onDecline(): Promise<void> {
    if (deleting()) return;
    setDeleteFailed(false);
    setDeleting(true);
    try {
      await auth.discardGuestAccount();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("deleting the guest account failed", err);
      if (alive) setDeleteFailed(true);
    } finally {
      if (alive) setDeleting(false);
    }
  }

  return (
    <div class="lk-stack lk-guest-offer" style={{ gap: "12px" }}>
      <Switch>
        <Match when={step() === "ask"}>
          <h2 style={{ margin: 0 }}>{t("guest.offer.title")}</h2>
          <p style={{ margin: 0 }}>{t("guest.offer.body")}</p>
          <Show when={cannotKeep()}>
            <div class="form-error" role="alert">
              {t("guest.claim.error.phrase_missing")}
            </div>
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
              disabled={deleting()}
              onClick={() => {
                setDeleteFailed(false);
                setStep("form");
              }}
            >
              {t("auth.signup.submit")}
            </button>
          </Show>
          <button
            type="button"
            disabled={deleting()}
            aria-busy={deleting()}
            onClick={() => void onDecline()}
          >
            <Show when={deleting()} fallback={t("guest.offer.decline")}>
              <span class="lakko-spinner" aria-hidden="true" /> {t("guest.offer.deleting")}
            </Show>
          </button>
          <p class="form-info" style={{ margin: 0 }}>
            {t("guest.offer.decline_note")}
          </p>
        </Match>
        <Match when={step() === "form"}>
          <h2 style={{ margin: 0 }}>{t("username.claim.title")}</h2>
          {/* A name an earlier attempt already took is kept: the screen then
              asks for the password alone (`claimStart`). */}
          <UsernameClaimScreen
            onClose={() => setStep("ask")}
            onCannotKeep={() => {
              setCannotKeep(true);
              setStep("ask");
            }}
          />
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
