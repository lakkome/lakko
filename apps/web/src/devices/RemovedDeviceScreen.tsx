// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device was removed" (docs/ux/second-device.md section 3.1).
 *
 * Replaces the whole signed-in app once another device of the account has
 * removed this one: no chat list behind it, no dismiss, no retry. The one
 * button is Settings' "Forget this device" (`auth.forgetDevice`): this
 * device's keys and local history for the account go, then the login screen.
 * Drawn in the auth screens' frame, since where it leads is sign-in.
 */
import { createSignal, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { paths } from "../routes/paths";
import { AuthLayout } from "../ui/AuthLayout";
import { REMOVED_DEVICE_COPY } from "./sync/removed-device";

export function RemovedDeviceScreen(): JSX.Element {
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

  return (
    <AuthLayout>
      <h1>{REMOVED_DEVICE_COPY.title()}</h1>
      <p class="lk-auth-lede">{REMOVED_DEVICE_COPY.body()}</p>
      <button class="primary" type="button" disabled={busy()} onClick={() => void onSignOut()}>
        {REMOVED_DEVICE_COPY.signOut()}
      </button>
    </AuthLayout>
  );
}
