// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device was removed" (docs/ux/identity-recovery.md section 18.2,
 * ADR-0066) and, in the same shape, "Your account keys were reset"
 * (section 15.2, ADR-0065).
 *
 * Replaces the whole signed-in app: no chat list behind it, no dismiss, no
 * retry. Variants:
 *
 *   - `device-removed`, or no `signedOut` prop at all (the DeviceGroup
 *     runtime saw this device's own removal and the auth layer has not put
 *     its state up yet): another device of the account removed this one.
 *     The deletion starts BY ITSELF, all of it; there is no button to tap
 *     first. "Removing...", then the one button, "Sign in".
 *   - `keys-reset`: the account's keys were reset on another device. The
 *     wipe runs by itself ("Removing..."), then the one button is "Sign in".
 *   - `recall`: the keys were reset on another device and this device held
 *     them: "Was it you?" (`RecallScreen`, section 18.4).
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
import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { RecallScreen } from "./RecallScreen";
import { REMOVED_DEVICE_COPY, SIGNED_OUT_COPY } from "./sync/removed-device";
import { REMOVED_SCREEN_COPY } from "./removal-copy";

export function RemovedDeviceScreen(props: { signedOut?: SignedOutState }): JSX.Element {
  // "Was it you?" (UX 18.4) is its own screen, driven by the recall record.
  const recall = () => {
    const s = props.signedOut;
    return s !== undefined && s.reason === "recall" ? s : null;
  };
  return (
    <Show when={recall()} fallback={<SignedOutScreen signedOut={props.signedOut} />}>
      {(r) => <RecallScreen username={r().username} learnedAtMs={r().learnedAtMs} />}
    </Show>
  );
}

function SignedOutScreen(props: { signedOut: SignedOutState | undefined }): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const [busy, setBusy] = createSignal(false);

  async function onConfirm(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    try {
      const current = props.signedOut;
      await auth.confirmSignedOut();
      if (current !== undefined && current.reason !== "recall" && current.phase === "removed") {
        navigate(paths.login, { replace: true });
      }
    } finally {
      setBusy(false);
    }
  }

  /** Why the take-the-account-back form ended, when it did (UX 18.4). */
  const recallNotice = (): string | null => {
    const s = props.signedOut;
    if (s === undefined || s.reason !== "keys-reset" || s.notice === undefined) return null;
    return s.notice === "exhausted"
      ? t("devices.recall.error.exhausted")
      : t("devices.recall.error.closed");
  };
  const keysReset = (phase: "removing" | "removed"): boolean => {
    const s = props.signedOut;
    return s !== undefined && s.reason !== "recall" && s.phase === phase;
  };

  const name = (): JSX.Element => {
    const username = props.signedOut?.username ?? "";
    return username.length > 0 ? <strong>@{username}</strong> : SIGNED_OUT_COPY.thisAccount();
  };

  return (
    <AuthLayout>
      <Switch
        fallback={
          // The removal was just read from the device list; the automatic
          // deletion is starting (the auth layer's state follows at once).
          <>
            <h1>{REMOVED_SCREEN_COPY.title()}</h1>
            <p class="lk-auth-lede">{tRich("devices.removed.body_removing", { name: name() })}</p>
            <p class="form-info" role="status" aria-live="polite">
              {REMOVED_SCREEN_COPY.removing()}
            </p>
          </>
        }
      >
        <Match when={props.signedOut?.reason === "device-removed" && props.signedOut.phase === "removing"}>
          <h1>{REMOVED_SCREEN_COPY.title()}</h1>
          <p class="lk-auth-lede">{tRich("devices.removed.body_removing", { name: name() })}</p>
          <p class="form-info" role="status" aria-live="polite">
            {REMOVED_SCREEN_COPY.removing()}
          </p>
        </Match>
        <Match when={props.signedOut?.reason === "device-removed" && props.signedOut.phase === "removed"}>
          <h1>{REMOVED_SCREEN_COPY.title()}</h1>
          <p class="lk-auth-lede">{tRich("devices.removed.body_done", { name: name() })}</p>
          <Show when={!busy()}>
            <button class="primary" type="button" onClick={() => void onConfirm()}>
              {REMOVED_SCREEN_COPY.signIn()}
            </button>
          </Show>
        </Match>
        <Match when={props.signedOut?.reason === "identity-mismatch" && props.signedOut.phase === "blocked"}>
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <p class="lk-auth-lede">{SIGNED_OUT_COPY.bodyMismatch()}</p>
          <button class="primary" type="button" disabled={busy()} onClick={() => void onConfirm()}>
            {REMOVED_DEVICE_COPY.signOut()}
          </button>
        </Match>
        <Match when={keysReset("removing")}>
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <Show when={recallNotice()}>
            {(line) => (
              <p class="form-error" role="alert">
                {line()}
              </p>
            )}
          </Show>
          <p class="lk-auth-lede" style={{ "white-space": "pre-line" }}>
            {tRich("devices.signed_out.body_removing", { username: name() })}
          </p>
          <p class="form-info" role="status" aria-live="polite">
            {SIGNED_OUT_COPY.removing()}
          </p>
        </Match>
        <Match when={keysReset("removed")}>
          <h1>{SIGNED_OUT_COPY.title()}</h1>
          <Show when={recallNotice()}>
            {(line) => <p class="form-error">{line()}</p>}
          </Show>
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
