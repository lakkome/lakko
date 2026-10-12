// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A kept device after "remove and change keys" on another device
 * (`docs/ux/identity-recovery.md` section 18.3): `Updating keys...` while it
 * is fetching the new keys, then one dismissible line at the top of the chat
 * list with `Show new phrase`, which opens Settings > Security > Recovery
 * (the phrase there sits behind the fresh-auth proof, as it always does).
 * Never a full-screen state.
 */
import { Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { newWordsShown } from "../auth/protect/account-protection";
import { phraseProtectedHere } from "../auth/protect/protection-state";
import { t } from "../l10n";
import { paths } from "../routes/paths";

export function KeysRotatedNotice(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const inSession = (): boolean => auth.state().kind === "session";
  const wordsShown = (): boolean => {
    const s = auth.state();
    return (
      s.kind === "session" &&
      newWordsShown(phraseProtectedHere(s.account.accountId), s.account.username.trim().length > 0)
    );
  };
  return (
    <>
      <Show when={inSession() && auth.keyRotation.updating()}>
        <div class="lk-alert" role="status" aria-live="polite">
          <div class="lk-fill">{t("devices.rotated.updating")}</div>
        </div>
      </Show>
      <Show when={inSession() && !auth.keyRotation.updating() && auth.keyRotation.noticeOwed()}>
        {/* Polite: announced once when it appears, never takes focus. */}
        <div class="lk-alert accent" role="status" aria-live="polite">
          <div class="lk-fill">{t("devices.rotated.notice")}</div>
          {/* ADR-0067: only a phrase-protected account has words to show. */}
          <Show when={wordsShown()}>
            <button
              type="button"
              class="small"
              onClick={() => navigate(paths.settingsSection("recovery"))}
            >
              {t("devices.rotated.show_phrase")}
            </button>
          </Show>
          <button type="button" class="small ghost" onClick={() => auth.keyRotation.dismissNotice()}>
            {t("common.dismiss")}
          </button>
        </div>
      </Show>
    </>
  );
}

