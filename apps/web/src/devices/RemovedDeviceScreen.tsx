// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device was removed" (docs/ux/second-device.md section 3.1) and, in
 * the same shape, "Your account keys were reset"
 * (docs/ux/identity-recovery.md section 15.2, ADR-0065).
 *
 * Replaces the whole signed-in app: no chat list behind it, no dismiss, no
 * retry. Three variants:
 *
 *   - no `signedOut` prop: another device of the account removed this one
 *     (the DeviceGroup commit landed). The one button is Settings' "Forget
 *     this device" (`auth.forgetDevice`): this device's keys and local
 *     history for the account go, then the login screen.
 *   - `keys-reset`: the account's keys were reset on another device. The
 *     wipe runs by itself ("Removing..."), then the one button is "Sign in".
 *   - `identity-mismatch`: the directory publishes other keys of the same
 *     generation (an anomaly). The screen blocks; "Sign out" runs the wipe,
 *     then "Sign in".
 *
 * Drawn in the auth screens' frame, since where it leads is sign-in. The
 * title and the body are one announcement; the progress line is announced
 * once; the single button is the only focusable element.
 */
import { createSignal, Match, Show, Switch, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth, type SignedOutState } from "../auth/AuthContext";
import { paths } from "../routes/paths";
import { AuthLayout } from "../ui/AuthLayout";
import { tRich } from "../l10n/rich";
import { REMOVED_DEVICE_COPY, SIGNED_OUT_COPY } from "./sync/removed-device";

export function RemovedDeviceScreen(props: { signedOut?: SignedOutState }): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const [busy, setBusy] = createSignal(false);

  async function onSignOut(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    try {
      await auth.forgetDevice();
    } finally {
      setBusy(false);
      navigate(paths.login, { replace: true });
    }
  }

  async function onConfirm(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    try {
      const current = props.signedOut;
      await auth.confirmSignedOut();
      if (current !== undefined && current.phase === "removed") {
        navigate(paths.login, { replace: true });
      }
    } finally {
      setBusy(false);
    }
  }

  const name = (): JSX.Element => {
    const username = props.signedOut?.username ?? "";
    return username.length > 0 ? <strong>@{username}</strong> : SIGNED_OUT_COPY.thisAccount();
  };

  return (
    <AuthLayout>
      <Switch
        fallback={
          <>
            <h1>{REMOVED_DEVICE_COPY.title()}</h1>
            <p class="lk-auth-lede">{REMOVED_DEVICE_COPY.body()}</p>
            <button class="primary" type="button" disabled={busy()} onClick={() => void onSignOut()}>
              {REMOVED_DEVICE_COPY.signOut()}
            </button>
          </>
        }
      >
        <Match when={props.signedOut?.reason === "identity-mismatch" && props.signedOut.phase === "blocked"}>
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <p class="lk-auth-lede">{SIGNED_OUT_COPY.bodyMismatch()}</p>
          <button class="primary" type="button" disabled={busy()} onClick={() => void onConfirm()}>
            {REMOVED_DEVICE_COPY.signOut()}
          </button>
        </Match>
        <Match when={props.signedOut !== undefined && props.signedOut.phase === "removing"}>
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <p class="lk-auth-lede" style={{ "white-space": "pre-line" }}>
            {tRich("devices.signed_out.body_removing", { username: name() })}
          </p>
          <p class="form-info" role="status" aria-live="polite">
            {SIGNED_OUT_COPY.removing()}
          </p>
        </Match>
        <Match when={props.signedOut !== undefined && props.signedOut.phase === "removed"}>
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <p class="lk-auth-lede" style={{ "white-space": "pre-line" }}>
            {tRich("devices.signed_out.body_removed", { username: name() })}
          </p>
          <Show when={!busy()}>
            <button class="primary" type="button" onClick={() => void onConfirm()}>
              {SIGNED_OUT_COPY.signIn()}
            </button>
          </Show>
        </Match>
      </Switch>
    </AuthLayout>
  );
}
