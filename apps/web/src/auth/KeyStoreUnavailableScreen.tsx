// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Lakko can't store your keys safely on this computer."
 * (`docs/ux/desktop.md` section 1.1, ADR-0085 section 2).
 *
 * Desktop only: the operating system's key store (Keychain, the Windows key
 * store, the Secret Service keyring) is missing or refused, so the host
 * could not keep this device's key. Sign-in stopped; nothing falls back to
 * keeping keys in a plain file. Replaces every route, in the auth screens'
 * frame. The one button repeats the step that stopped.
 */
import { createSignal, type JSX } from "solid-js";

import { useAuth } from "./AuthContext";
import { AuthLayout } from "../ui/AuthLayout";
import { t } from "../l10n";

export function KeyStoreUnavailableScreen(): JSX.Element {
  const auth = useAuth();
  const [busy, setBusy] = createSignal(false);

  async function onRetry(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    try {
      await auth.retryKeyStore();
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
      <h1>{t("auth.key_store_unavailable.title")}</h1>
      <p class="lk-auth-lede">{t("auth.key_store_unavailable.body")}</p>
      <button class="primary" type="button" disabled={busy()} onClick={() => void onRetry()}>
        {t("common.try_again")}
      </button>
    </AuthLayout>
  );
}
