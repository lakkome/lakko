// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The username prompt (`docs/ux/identity-recovery.md` section 8b), mounted
 * once beside the other whole-session runtimes in `App.tsx`. It looks for the
 * account's username when a session landed with none (a passkey sign-in on a
 * new device), adopts the backup's name when auth confirms it, and otherwise
 * asks in a layer over the app, which stays usable behind it.
 *
 * Every rule is in `username-prompt.ts`; this file is markup and wiring.
 */
import { type JSX, Match, Show, Switch, createEffect, createSignal, onCleanup, untrack } from "solid-js";

import { t } from "../l10n";
import { ModalLayer } from "../ui/ModalLayer";
import { landingRestoreSettled } from "../backup/restore";
import { restoredOwnUsername } from "../backup/own-state";
import { storedOwnUsername } from "../devices/sync/stored-own-username";
import { registerAccountMemory } from "./account-memory";
import { useAuth } from "./AuthContext";
import { AuthClient } from "./client";
import { getDeviceKey } from "./key-store";
import { nameTargetFor } from "./login-handle";
import { createUsernamePromptController, shouldLookForUsername } from "./username-prompt";

export function UsernamePrompt(): JSX.Element {
  const auth = useAuth();
  const client = new AuthClient();
  const controller = createUsernamePromptController({
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    nameTarget: (typed) => nameTargetFor(typed),
    check: (token, handle) => client.checkLoginHandle(token, handle),
    async backupCandidates(accountIdB64) {
      const names: string[] = [];
      const restored = restoredOwnUsername(accountIdB64);
      if (restored !== null) names.push(restored);
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey !== null) names.push(await storedOwnUsername(accountIdB64, deviceKey));
      return names;
    },
    adopt: (username) => auth.adoptVerifiedUsername(username),
  });
  onCleanup(registerAccountMemory("username-prompt", () => controller.reset(), { signOut: true }));

  createEffect(() => {
    const s = auth.state();
    const landed = s.kind === "session" && auth.identitySecrets() !== null;
    const accountIdB64 = s.kind === "session" ? s.account.accountId : null;
    const username = s.kind === "session" ? s.account.username : "";
    // The name became known another way (the register brought it): the
    // prompt has nothing left to ask.
    if (username.trim().length > 0 && untrack(controller.view).phase === "asking") controller.close();
    if (accountIdB64 === null) return;
    if (
      !shouldLookForUsername({
        sessionLanded: landed,
        username,
        guest: auth.isGuest(),
        backupSettled: landingRestoreSettled(accountIdB64),
      })
    ) {
      return;
    }
    void untrack(() => controller.start(accountIdB64));
  });

  const [typed, setTyped] = createSignal("");
  const view = controller.view;
  const busy = (): boolean => {
    const v = view();
    return v.phase === "asking" && v.busy;
  };
  const error = (): string | null => {
    const v = view();
    if (v.phase !== "asking" || v.error === null) return null;
    return v.errorIsText ? v.error : t(v.error);
  };

  return (
    <ModalLayer
      open={view().phase !== "hidden"}
      title={t("auth.username_prompt.title")}
      onClose={() => controller.close()}
    >
      <Switch>
        <Match when={view().phase === "asking"}>
          <form
            class="lk-stack"
            onSubmit={(e) => {
              e.preventDefault();
              void controller.submit(typed());
            }}
          >
            <p class="lk-muted">{t("auth.username_prompt.lede")}</p>
            <label>
              {t("auth.username_prompt.field")}
              <input
                type="text"
                autocomplete="username"
                autocapitalize="none"
                spellcheck={false}
                required
                value={typed()}
                onInput={(e) => setTyped(e.currentTarget.value)}
                disabled={busy()}
                aria-invalid={error() !== null}
              />
            </label>
            <Show when={error()}>
              {(message) => (
                <div class="form-error" role="alert">
                  {message()}
                </div>
              )}
            </Show>
            <div class="lk-actions">
              <button type="submit" class="primary" disabled={busy() || typed().trim().length === 0}>
                {t("auth.username_prompt.continue")}
              </button>
              <button type="button" disabled={busy()} onClick={() => controller.forgot()}>
                {t("auth.username_prompt.forgot")}
              </button>
            </div>
          </form>
        </Match>
        <Match when={view().phase === "forgot"}>
          <div class="lk-stack">
            <p>{t("auth.username_prompt.forgot_note")}</p>
            <div class="lk-actions">
              <button type="button" class="primary" onClick={() => controller.close()}>
                {t("common.ok")}
              </button>
            </div>
          </div>
        </Match>
      </Switch>
    </ModalLayer>
  );
}
