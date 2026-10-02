// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Choose a new password" (docs/ux/identity-recovery.md 7.1): shown once,
 * by the route guards, right after a forgot-password route succeeded on a
 * device that holds the account's keys (an authenticator code, a passkey,
 * another device). The person is already signed in.
 *
 * Saving replaces the password and opens the app. "Not now" opens the app
 * with the old (forgotten) password still valid; Settings > Security keeps
 * the same form. Either way the guard that rendered this re-decides.
 */
import type { JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { NewPasswordForm } from "./NewPasswordForm";
import { AuthLayout } from "../ui/AuthLayout";
import { t } from "../l10n";

export default function NewPasswordPrompt(): JSX.Element {
  const auth = useAuth();
  return (
    <AuthLayout>
      <h1>{t("recovery.new_password.title")}</h1>
      <p class="lk-auth-lede">{t("recovery.new_password.lede")}</p>
      <NewPasswordForm
        // `replacePassword` clears the offer itself; dismissing is harmless.
        onSaved={() => auth.dismissNewPasswordOffer()}
        onSkip={() => auth.dismissNewPasswordOffer()}
      />
    </AuthLayout>
  );
}
