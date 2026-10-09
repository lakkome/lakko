// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "You took your account back." (docs/ux/identity-recovery.md section 18.4,
 * the Done row): one line at the top of the chat list for the session in
 * which the account was taken back on this device. In memory only: a
 * reload drops it.
 */
import { Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { t } from "../l10n";

export function RecallDoneNotice(): JSX.Element {
  const auth = useAuth();
  return (
    <Show when={auth.recallDone() && auth.state().kind === "session"}>
      {/* Polite: announced once when it appears, never takes focus. */}
      <div class="lk-alert accent" role="status" aria-live="polite">
        <div class="lk-fill">{t("devices.recall.done")}</div>
        <button type="button" class="small ghost" onClick={() => auth.dismissRecallDone()}>
          {t("common.dismiss")}
        </button>
      </div>
    </Show>
  );
}
