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
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { AuthHttpError } from "../client";
import { isPlausibleContact } from "../contact-shape";
import { normalizeTotpCode } from "../totp-code";
import { ContactKind } from "../wire";
import { requireValidUsername } from "../../profile/username-field";
import { UsernameField } from "./UsernameField";

/** 401 covers an unknown account, no live code, a wrong code and a replayed
 *  code alike. */
export function contactRecoverErrorMessage(err: unknown): string {
  if (err instanceof AuthHttpError) {
    if (err.status === 401) {
      return "That code did not work. Check the code we sent and try again.";
    }
    if (err.status === 429) {
      return "Too many attempts. Wait a while, then request a new code.";
    }
    if (err.status === 503) {
      return "Email and phone recovery are not available here. Use another way in.";
    }
    if (err.status === 400) {
      return "That does not look like an email address or phone number. Check it and try again.";
    }
  }
  if (err instanceof Error && /username not found/.test(err.message)) {
    return "Could not send a code. Check the username and try again.";
  }
  return err instanceof Error ? err.message : String(err);
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
      setError("Enter the username of the account.");
      return;
    }
    // ADR-0048 section 4: no lookup for a name that fails the username rule.
    try {
      user = await requireValidUsername(user);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return;
    }
    // A light shape check saves a proof-of-work solve on an obviously
    // malformed contact. The server is authoritative.
    if (!isPlausibleContact(kind(), contact())) {
      setError(
        kind() === ContactKind.Email
          ? "Enter a valid email address."
          : "Enter the phone number in international format, like +15551234567.",
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
      setError("Enter the six-digit code we sent.");
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithContactFinish(normalized);
      setCode("");
    } catch (err) {
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
          <p class="lk-muted">
            We send a six-digit code to the email address or phone number you set up in
            Settings. It gets you back in; your chats then need your recovery phrase or
            another device.
          </p>
          <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
          <div class="lk-segmented" role="radiogroup" aria-label="Send the code by">
            <label>
              <input
                type="radio"
                name="recover-contact-kind"
                checked={kind() === ContactKind.Email}
                disabled={busy()}
                onChange={() => setKind(ContactKind.Email)}
              />
              Email
            </label>
            <label>
              <input
                type="radio"
                name="recover-contact-kind"
                checked={kind() === ContactKind.Phone}
                disabled={busy()}
                onChange={() => setKind(ContactKind.Phone)}
              />
              Phone
            </label>
          </div>
          <label>
            {kind() === ContactKind.Email ? "Email" : "Phone number"}
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
            {busy() ? "Sending..." : "Send code"}
          </button>
        </form>
      }
    >
      <form onSubmit={(e) => void onSubmitCode(e)}>
        <p class="lk-muted">
          If that {kind() === ContactKind.Email ? "address" : "number"} is set up for this
          account, a six-digit code is on its way. Enter it below.
        </p>
        <label>
          Code we sent
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
          {busy() ? "Checking..." : "Continue"}
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
          Use a different email or phone
        </button>
      </form>
    </Show>
  );
}
