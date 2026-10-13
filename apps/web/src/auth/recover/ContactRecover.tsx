// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery with a code sent by email or phone (ADR-0018 factor (d)). Two
 * steps: username plus the email or number, then the six-digit code. Access
 * only, like the authenticator; the identity follows from a cached seed or
 * the restore prompt. No navigation here: the route's guard reacts.
 *
 * Enumeration-safe end to end: the code step is shown whether or not the
 * contact matched, because the server answers the same either way.
 */
import { isAbandoned } from "../binding-epoch";
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { AuthHttpError } from "../client";
import { authLimitMessage, screenText } from "../auth-error-copy";
import { t } from "../../l10n";
import { isPlausibleContact } from "../contact-shape";
import { normalizeTotpCode } from "../totp-code";
import { ContactKind } from "../wire";
import { requireValidUsername } from "../../profile/username-field";
import { UsernameField } from "./UsernameField";

/** 401 covers an unknown account, no live code, a wrong code and a replayed
 *  code alike. */
export function contactRecoverErrorMessage(err: unknown): string {
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (err instanceof AuthHttpError) {
    if (err.status === 401) {
      return t("recovery.contact.error.wrong");
    }
    if (err.status === 503) {
      return t("recovery.contact.error.unavailable");
    }
    if (err.status === 400) {
      return t("recovery.contact.error.bad_contact");
    }
  }
  if (err instanceof Error && /username not found/.test(err.message)) {
    return t("recovery.contact.error.send_failed");
  }
  return screenText(err) ?? t("recovery.contact.error.generic");
}

export function ContactRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
}): JSX.Element {
  const auth = useAuth();
  type Step = "request" | "code";
  const [step, setStep] = createSignal<Step>("request");
  const [kind, setKind] = createSignal<ContactKind>(ContactKind.Email);
  const [contact, setContact] = createSignal("");
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSendCode(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    let user = props.username().trim();
    if (user.length === 0) {
      setError(t("recovery.username_required"));
      return;
    }
    // ADR-0048 section 4: no lookup for a name that fails the username rule.
    try {
      user = await requireValidUsername(user);
    } catch (err) {
      setError(screenText(err) ?? t("username.error.unknown"));
      return;
    }
    // A light shape check saves a proof-of-work solve on an obviously
    // malformed contact. The server is authoritative.
    if (!isPlausibleContact(kind(), contact())) {
      setError(
        kind() === ContactKind.Email
          ? t("recovery.contact.error.invalid_email")
          : t("recovery.contact.error.invalid_phone"),
      );
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithContactStart(user, kind(), contact());
      setCode("");
      setStep("code");
    } catch (err) {
      setError(contactRecoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function onSubmitCode(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setError(t("recovery.contact.error.enter_code"));
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithContactFinish(normalized);
      setCode("");
    } catch (err) {
      // Overtaken by another session on this device: nothing to report.
      if (isAbandoned(err)) return;
      setError(contactRecoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show
      when={step() === "code"}
      fallback={
        <form onSubmit={(e) => void onSendCode(e)}>
          <p class="lk-muted">{t("recovery.contact.intro")}</p>
          <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
          <div class="lk-segmented" role="radiogroup" aria-label={t("recovery.contact.send_by")}>
            <label>
              <input
                type="radio"
                name="recover-contact-kind"
                checked={kind() === ContactKind.Email}
                disabled={busy()}
                onChange={() => setKind(ContactKind.Email)}
              />
              {t("common.email")}
            </label>
            <label>
              <input
                type="radio"
                name="recover-contact-kind"
                checked={kind() === ContactKind.Phone}
                disabled={busy()}
                onChange={() => setKind(ContactKind.Phone)}
              />
              {t("common.phone")}
            </label>
          </div>
          <label>
            {kind() === ContactKind.Email ? t("common.email") : t("recovery.contact.phone_label")}
            <input
              type={kind() === ContactKind.Email ? "email" : "tel"}
              autocomplete={kind() === ContactKind.Email ? "email" : "tel"}
              required
              value={contact()}
              onInput={(e) => setContact(e.currentTarget.value)}
              disabled={busy()}
              placeholder={kind() === ContactKind.Email ? "you@example.com" : "+15551234567"}
            />
          </label>
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? t("common.sending") : t("recovery.contact.send")}
          </button>
        </form>
      }
    >
      <form onSubmit={(e) => void onSubmitCode(e)}>
        <p class="lk-muted">
          {kind() === ContactKind.Email
            ? t("recovery.contact.sent_email")
            : t("recovery.contact.sent_phone")}
        </p>
        <label>
          {t("recovery.contact.code_label")}
          <input
            type="text"
            inputmode="numeric"
            autocomplete="one-time-code"
            pattern="[0-9]{6}"
            maxlength="6"
            required
            value={code()}
            onInput={(e) => setCode(e.currentTarget.value)}
            disabled={busy()}
            placeholder="123456"
            class="lk-code-input"
          />
        </label>
        <Show when={error() !== null}>
          <div class="form-error" role="alert">
            {error()}
          </div>
        </Show>
        <button class="primary" type="submit" disabled={busy()}>
          {busy() ? t("common.checking") : t("common.continue")}
        </button>
        <button
          type="button"
          class="ghost"
          disabled={busy()}
          onClick={() => {
            setError(null);
            setCode("");
            setStep("request");
          }}
        >
          {t("recovery.contact.change")}
        </button>
      </form>
    </Show>
  );
}
