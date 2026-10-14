// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Getting back in with a passkey (docs/ux/identity-recovery.md section 4,
 * ADR-0072 decision 6). No username field: the browser's own sheet lists
 * the passkeys saved for Lakko, and the sign-in works on a device that has
 * never held the account.
 *
 * One button, three endings, all decided in `AuthContext.loginWithPasskey`:
 * a device that holds the keys lands signed in; a passkey that carries the
 * keys restores them; otherwise the guard shows the restore prompt, whose
 * first line says the passkey signed in and did not bring keys. No
 * navigation here: the route's guard reacts to the state flip.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { isAbandoned } from "../binding-epoch";
import { passkeyErrorMessage } from "../auth-error-copy";
import { WebauthnClient } from "../webauthn-client";
import { KeyedError } from "../../l10n/keyed-error";
import { t } from "../../l10n";
import { MethodUnavailable } from "./FormNotices";

export function PasskeyRecover(props: {
  /** The name typed in another form, if any: it is kept for the account
   *  only when it turns out to name the account the passkey signs in. */
  readonly username: () => string;
  /** Back to the list of methods (the `503` state offers it). */
  readonly onBack: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [unavailable, setUnavailable] = createSignal(false);
  const supported = WebauthnClient.isWebauthnAvailable();

  async function onUse(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await auth.loginWithPasskey(props.username(), { forgotPassword: true });
    } catch (err) {
      // Overtaken by another session on this device: nothing to report.
      if (isAbandoned(err)) return;
      // Dismissing the browser's sheet is not an error: back to idle.
      if (err instanceof KeyedError && err.key === "auth.passkey.error.server_disabled") {
        // Passkeys are switched off on this server.
        setUnavailable(true);
      } else if (!(err instanceof KeyedError && err.key === "auth.passkey.error.cancelled")) {
        setError(passkeyErrorMessage(err));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={!unavailable()} fallback={<MethodUnavailable onBack={props.onBack} />}>
    <div class="lk-stack">
      <Show
        when={supported}
        fallback={<p class="form-error">{t("recovery.passkey.unsupported")}</p>}
      >
        <p class="lk-muted">{t("recovery.passkey.intro")}</p>
        <Show when={error() !== null}>
          <div class="form-error" role="alert">
            {error()}
          </div>
        </Show>
        <button class="primary" type="button" disabled={busy()} onClick={() => void onUse()}>
          {busy() ? t("recovery.passkey.waiting") : t("recovery.passkey.submit")}
        </button>
      </Show>
    </div>
    </Show>
  );
}
