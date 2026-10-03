// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Login route (docs/ux/client-redesign.md section 5.6): username, password
 * with a show / hide toggle, Log in, "Use passkey", "Forgot password?" and
 * one small link that explains how the password is protected. The screen
 * says nothing about how the login works.
 *
 * The form drives `AuthContext.login`, which resolves the typed username
 * through the directory when the local cache misses, so a fresh browser can
 * log in to any registered account. Errors are mapped to screen copy at the
 * edge (`auth-error-copy.ts`), which keeps the context's enumeration-safe
 * collapse: an unknown username and a wrong password read the same.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { loginErrorMessage, passkeyErrorMessage, screenText } from "../auth/auth-error-copy";
import { t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import { tRich } from "../l10n/rich";
import { PasswordExplainer } from "../auth/PasswordExplainer";
import { readReturnUrl } from "../auth/return-url";
import { WebauthnClient } from "../auth/webauthn-client";
import { UsernameHint } from "../profile/UsernameHint";
import { requireValidUsername } from "../profile/username-field";
import { AuthLayout } from "../ui/AuthLayout";
import { PasswordField } from "../ui/PasswordField";
import { Icon } from "../ui/icons";

export default function Login(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const location = useLocation();

  // Pre-filled with the last account seen on this device; any registered
  // username is accepted.
  const cached = (): string => {
    const s = auth.state();
    return s.kind === "anon" && s.cachedUsername !== undefined
      ? s.cachedUsername
      : "";
  };
  const sessionExpired = (): boolean => {
    const s = auth.state();
    return s.kind === "anon" && s.sessionExpired === true;
  };
  const [username, setUsername] = createSignal(cached());
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    // ADR-0048 section 4: a name that fails the username rule is never looked up.
    let canonical: string;
    try {
      canonical = await requireValidUsername(username());
    } catch (err) {
      setError(screenText(err) ?? t("username.error.unknown"));
      return;
    }
    setBusy(true);
    try {
      await auth.login(canonical, password());
      // `?return=<path>` from an invite landing is honoured; validated
      // path-only by `readReturnUrl`, `/` otherwise.
      navigate(readReturnUrl(location), { replace: true });
    } catch (err) {
      setError(loginErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function onPasskeyLogin(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await auth.loginWithPasskey(username());
      navigate(readReturnUrl(location), { replace: true });
    } catch (err) {
      // Dismissing the browser's sheet is not an error.
      if (!(err instanceof KeyedError && err.key === "auth.passkey.error.cancelled")) {
        setError(passkeyErrorMessage(err));
      }
    } finally {
      setBusy(false);
    }
  }

  // The passkey button shows wherever the browser supports passkeys: the
  // sign-in is discoverable, so it works on a device that has never held
  // the account (ADR-0064 decision 6). What it restores is decided after.
  const passkeyAvailable = (): boolean => WebauthnClient.isWebauthnAvailable();

  return (
    <AuthLayout>
      <h1>{t("auth.login.title")}</h1>
      <p class="lk-auth-lede">{t("auth.login.lede")}</p>
      {/* ADR-0065 section 3.3: the refresh credential expired or was
          refused as invalid; the session ended on its own, nothing wiped. */}
      <Show when={sessionExpired()}>
        <div class="lk-alert info" role="status">
          {t("common.error.session_expired")}
        </div>
      </Show>
      <form onSubmit={onSubmit}>
        <label>
          {t("auth.username.label")}
          <input
            type="text"
            autocomplete="username"
            autocapitalize="none"
            spellcheck={false}
            required
            value={username()}
            onInput={(e) => setUsername(e.currentTarget.value)}
            disabled={busy()}
          />
        </label>
        <UsernameHint value={username} />
        <PasswordField
          label={t("auth.password.label")}
          autocomplete="current-password"
          value={password}
          setValue={setPassword}
          disabled={busy}
        />
        <Show when={error() !== null}>
          <div class="form-error" role="alert">
            {error()}
          </div>
        </Show>
        <button class="primary" type="submit" disabled={busy()}>
          {busy() ? t("auth.login.submitting") : t("auth.login.submit")}
        </button>
      </form>
      <Show when={passkeyAvailable()}>
        <button
          type="button"
          disabled={busy()}
          onClick={() => void onPasskeyLogin()}
          style={{ "margin-top": "-8px" }}
        >
          <Icon name="key" size={18} />
          {t("auth.login.passkey")}
        </button>
      </Show>
      <p class="lk-auth-foot">
        <A href={paths.recover}>{t("auth.login.forgot")}</A>
      </p>
      <p class="lk-auth-foot">
        {tRich("auth.login.no_account", {
          link: <A href={paths.signup}>{t("auth.signup.submit")}</A>,
        })}
      </p>
      <PasswordExplainer />
    </AuthLayout>
  );
}
