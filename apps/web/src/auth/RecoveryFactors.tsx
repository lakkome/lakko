// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery factors (Settings > Security > Recovery): enrol an authenticator
 * app (ADR-0018 factor (c)) or an email address / phone number (factor (d)).
 * Both are ACCESS factors — a code gets the user a session; the identity then
 * comes from the recovery phrase or another device. Moved verbatim from the
 * Devices route (docs/ux/client-redesign.md section 5.5); only the copy
 * changed.
 */
import { createEffect, createSignal, Match, Show, Switch, type JSX } from "solid-js";
import QRCode from "qrcode";

import { useAuth } from "./AuthContext";
import { AuthClient, AuthHttpError } from "./client";
import { isPlausibleContact } from "./contact-shape";
import { normalizeTotpCode } from "./totp-code";
import { ContactKind } from "./wire";

// ---- Authenticator (TOTP) enrollment — ADR-0018 factor (c) ------

export function TotpEnrollView(): JSX.Element {
  const auth = useAuth();
  type State =
    | { kind: "idle" }
    | { kind: "starting" }
    // Provisioning material on screen; waiting for the confirm code.
    | { kind: "confirm"; otpauthUri: string; secretB32: string }
    | { kind: "finishing"; otpauthUri: string; secretB32: string }
    | { kind: "done" }
    | { kind: "already-enabled" }
    | { kind: "unavailable" }
    | { kind: "error"; message: string };
  const [state, setState] = createSignal<State>({ kind: "idle" });
  const [code, setCode] = createSignal("");
  const [confirmError, setConfirmError] = createSignal<string | null>(null);
  const [canvasRef, setCanvasRef] = createSignal<HTMLCanvasElement | null>(
    null,
  );

  // Render the otpauth:// provisioning URI as a QR whenever the
  // confirm card is up. canvasRef is a signal, so this effect
  // re-runs once the <canvas> mounts after the state flip — no
  // draw-before-mount race.
  createEffect(() => {
    const s = state();
    const canvas = canvasRef();
    const uri =
      s.kind === "confirm" || s.kind === "finishing" ? s.otpauthUri : null;
    if (uri !== null && canvas !== null) {
      void QRCode.toCanvas(canvas, uri, {
        errorCorrectionLevel: "M",
        width: 224,
        margin: 1,
      }).catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("totp provisioning QR render failed", err);
      });
    }
  });

  const sessionToken = (): string | null =>
    auth.identitySecrets()?.sessionToken ?? null;

  async function start(): Promise<void> {
    const token = sessionToken();
    if (token === null) {
      setState({
        kind: "error",
        message: "Your session has ended. Sign in again.",
      });
      return;
    }
    setState({ kind: "starting" });
    try {
      const result = await new AuthClient().totpEnrollStart(token);
      setCode("");
      setConfirmError(null);
      setState({
        kind: "confirm",
        otpauthUri: result.otpauthUri,
        secretB32: result.secretB32,
      });
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 409) {
        // A CONFIRMED authenticator already exists; the server
        // refuses a silent swap.
        setState({ kind: "already-enabled" });
        return;
      }
      if (err instanceof AuthHttpError && err.status === 503) {
        setState({ kind: "unavailable" });
        return;
      }
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  async function finish(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const s = state();
    if (s.kind !== "confirm") return;
    setConfirmError(null);
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setConfirmError("Enter the 6-digit code from your authenticator app.");
      return;
    }
    const token = sessionToken();
    if (token === null) {
      setState({
        kind: "error",
        message: "Your session has ended. Sign in again.",
      });
      return;
    }
    setState({
      kind: "finishing",
      otpauthUri: s.otpauthUri,
      secretB32: s.secretB32,
    });
    try {
      await new AuthClient().totpEnrollFinish(token, normalized);
      setCode("");
      setState({ kind: "done" });
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 401) {
        // Wrong / stale code — the pending enrollment survives, so
        // just let the user retry with the next code.
        setConfirmError(
          "That code did not work. Wait for the next code in the app and try again.",
        );
        setState({
          kind: "confirm",
          otpauthUri: s.otpauthUri,
          secretB32: s.secretB32,
        });
        return;
      }
      if (err instanceof AuthHttpError && err.status === 409) {
        setState({ kind: "already-enabled" });
        return;
      }
      if (err instanceof AuthHttpError && err.status === 503) {
        setState({ kind: "unavailable" });
        return;
      }
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  return (
    <div>
      <p class="lk-card-desc">
        A six-digit code from an authenticator app lets you sign in again if you lose
        your password.
      </p>
      <Switch>
        <Match when={state().kind === "idle"}>
          {/* HONESTY NOTE: the server has no TOTP "status" endpoint,
              so the enrolled/not-enrolled state cannot be probed
              up front. We render the enable button by default; a
              409 from /totp/enroll/start flips this card to
              "already enabled". */}
          <button class="primary" type="button" onClick={() => void start()}>
            Set up authenticator
          </button>
        </Match>
        <Match when={state().kind === "starting"}>
          <p class="lk-muted">Preparing...</p>
        </Match>
        <Match
          when={state().kind === "confirm" || state().kind === "finishing"}
        >
          {(_) => {
            const s = state();
            const secretB32 =
              s.kind === "confirm" || s.kind === "finishing"
                ? s.secretB32
                : "";
            const busy = s.kind === "finishing";
            return (
              <div>
                <p>
                  Scan this code with your authenticator app, or type the key in by hand,
                  then enter the six-digit code the app shows.
                </p>
                <canvas class="lk-qr" ref={(el) => setCanvasRef(el)} />
                <p class="lk-meta">
                  Key: <code style={{ "user-select": "all" }}>{secretB32}</code>
                </p>
                <form onSubmit={(e) => void finish(e)}>
                  <label>
                    Code from the app
                    <input
                      type="text"
                      inputmode="numeric"
                      autocomplete="one-time-code"
                      pattern="[0-9]{6}"
                      maxlength="6"
                      required
                      value={code()}
                      onInput={(e) => setCode(e.currentTarget.value)}
                      disabled={busy}
                      placeholder="123456"
                      style={{
                        "font-family":
                          "ui-monospace, SFMono-Regular, Menlo, monospace",
                        "letter-spacing": "0.25em",
                      }}
                    />
                  </label>
                  <Show when={confirmError() !== null}>
                    <p class="form-error">{confirmError()}</p>
                  </Show>
                  <button class="primary" type="submit" disabled={busy}>
                    {busy ? "Confirming..." : "Confirm"}
                  </button>{" "}
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => setState({ kind: "idle" })}
                  >
                    Cancel
                  </button>
                </form>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">
            Authenticator set up. A code from the app can now get you back into your account.
          </p>
        </Match>
        <Match when={state().kind === "already-enabled"}>
          <p class="form-info">
            An authenticator is already set up for this account. To replace it, sign in
            again with your recovery phrase or from another device first.
          </p>
        </Match>
        <Match when={state().kind === "unavailable"}>
          <p class="form-error">
            Authenticator setup is not available right now.
          </p>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message : "Unknown error.";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button
                  type="button"
                  onClick={() => setState({ kind: "idle" })}
                >
                  Try again
                </button>
              </div>
            );
          }}
        </Match>
      </Switch>
    </div>
  );
}

// ---- Email / phone OTP enrollment (ADR-0018 factor (d)) ----------

export function ContactEnrollView(): JSX.Element {
  const auth = useAuth();
  type State =
    | { kind: "idle" }
    | { kind: "starting" }
    // A code has been sent; waiting for the confirm code.
    | { kind: "confirm"; contactKind: ContactKind; contact: string }
    | { kind: "finishing"; contactKind: ContactKind; contact: string }
    | { kind: "done" }
    | { kind: "already-enabled" }
    | { kind: "unavailable" }
    | { kind: "error"; message: string };
  const [state, setState] = createSignal<State>({ kind: "idle" });
  const [contactKind, setContactKind] = createSignal<ContactKind>(
    ContactKind.Email,
  );
  const [contact, setContact] = createSignal("");
  const [code, setCode] = createSignal("");
  const [confirmError, setConfirmError] = createSignal<string | null>(null);
  const [requestError, setRequestError] = createSignal<string | null>(null);

  const sessionToken = (): string | null =>
    auth.identitySecrets()?.sessionToken ?? null;

  async function start(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setRequestError(null);
    const k = contactKind();
    // Light client-side shape check — saves a limited attempt on an
    // obviously-malformed contact. The server is authoritative.
    if (!isPlausibleContact(k, contact())) {
      setRequestError(
        k === ContactKind.Email
          ? "Enter a valid email address."
          : "Enter a phone number in international format (e.g. +15551234567).",
      );
      return;
    }
    const token = sessionToken();
    if (token === null) {
      setState({
        kind: "error",
        message: "Your session has ended. Sign in again.",
      });
      return;
    }
    setState({ kind: "starting" });
    try {
      await new AuthClient().recoveryContactEnrollStart(token, k, contact());
      setCode("");
      setConfirmError(null);
      setState({ kind: "confirm", contactKind: k, contact: contact() });
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 503) {
        setState({ kind: "unavailable" });
        return;
      }
      if (err instanceof AuthHttpError && err.status === 409) {
        // The server returns 409 only if it refuses a swap of a
        // confirmed contact; surface it the same way TOTP does.
        setState({ kind: "already-enabled" });
        return;
      }
      // A 400 (malformed contact) bounces back to the request step so
      // the user can correct it.
      if (err instanceof AuthHttpError && err.status === 400) {
        setRequestError("That does not look right. Check it and try again.");
        setState({ kind: "idle" });
        return;
      }
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  async function finish(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const s = state();
    if (s.kind !== "confirm") return;
    setConfirmError(null);
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setConfirmError("Enter the 6-digit code we sent.");
      return;
    }
    const token = sessionToken();
    if (token === null) {
      setState({
        kind: "error",
        message: "Your session has ended. Sign in again.",
      });
      return;
    }
    setState({
      kind: "finishing",
      contactKind: s.contactKind,
      contact: s.contact,
    });
    try {
      await new AuthClient().recoveryContactEnrollFinish(
        token,
        s.contactKind,
        normalized,
      );
      setCode("");
      setState({ kind: "done" });
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 401) {
        // Wrong / stale code — the pending challenge survives, so just
        // let the user retry with the code (or re-send one).
        setConfirmError(
          "That code did not work. Check the code we sent and try again.",
        );
        setState({ kind: "confirm", contactKind: s.contactKind, contact: s.contact });
        return;
      }
      if (err instanceof AuthHttpError && err.status === 409) {
        setState({ kind: "already-enabled" });
        return;
      }
      if (err instanceof AuthHttpError && err.status === 503) {
        setState({ kind: "unavailable" });
        return;
      }
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  return (
    <div>
      <p class="lk-card-desc">
        A one-time code sent to your email or phone lets you sign in again if you lose
        your password.
      </p>
      <Switch>
        <Match when={state().kind === "idle" || state().kind === "starting"}>
          {/* HONESTY NOTE: the server exposes no recovery-contact
              "status" endpoint, so the enrolled/not-enrolled state
              cannot be probed up front. We render the request form by
              default; a 503 from enroll/start flips this card to
              "not available", a 409 (if the server returns one) to
              "already enabled". */}
          {(_) => {
            const busy = state().kind === "starting";
            return (
              <form onSubmit={(e) => void start(e)}>
                <div
                  role="tablist"
                  style={{ display: "flex", gap: "0.4rem", margin: "0.5rem 0" }}
                >
                  <button
                    type="button"
                    role="tab"
                    aria-selected={contactKind() === ContactKind.Email}
                    disabled={busy}
                    class={contactKind() === ContactKind.Email ? "primary" : ""}
                    onClick={() => setContactKind(ContactKind.Email)}
                  >
                    Email
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={contactKind() === ContactKind.Phone}
                    disabled={busy}
                    class={contactKind() === ContactKind.Phone ? "primary" : ""}
                    onClick={() => setContactKind(ContactKind.Phone)}
                  >
                    Phone
                  </button>
                </div>
                <label>
                  {contactKind() === ContactKind.Email ? "Email" : "Phone number"}
                  <input
                    type={contactKind() === ContactKind.Email ? "email" : "tel"}
                    autocomplete={
                      contactKind() === ContactKind.Email ? "email" : "tel"
                    }
                    required
                    value={contact()}
                    onInput={(e) => setContact(e.currentTarget.value)}
                    disabled={busy}
                    placeholder={
                      contactKind() === ContactKind.Email
                        ? "alice@example.com"
                        : "+15551234567"
                    }
                  />
                </label>
                <Show when={requestError() !== null}>
                  <p class="form-error">{requestError()}</p>
                </Show>
                <button class="primary" type="submit" disabled={busy}>
                  {busy ? "Sending..." : "Send code"}
                </button>
              </form>
            );
          }}
        </Match>
        <Match
          when={state().kind === "confirm" || state().kind === "finishing"}
        >
          {(_) => {
            const s = state();
            const k =
              s.kind === "confirm" || s.kind === "finishing"
                ? s.contactKind
                : ContactKind.Email;
            const to =
              s.kind === "confirm" || s.kind === "finishing" ? s.contact : "";
            const busy = s.kind === "finishing";
            return (
              <div>
                <p>
                  We sent a six-digit code to <strong>{to}</strong>
                  {k === ContactKind.Email ? " by email" : " by text message"}. Enter it below.
                </p>
                <form onSubmit={(e) => void finish(e)}>
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
                      disabled={busy}
                      placeholder="123456"
                      style={{
                        "font-family":
                          "ui-monospace, SFMono-Regular, Menlo, monospace",
                        "letter-spacing": "0.25em",
                      }}
                    />
                  </label>
                  <Show when={confirmError() !== null}>
                    <p class="form-error">{confirmError()}</p>
                  </Show>
                  <button class="primary" type="submit" disabled={busy}>
                    {busy ? "Confirming..." : "Confirm"}
                  </button>{" "}
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => setState({ kind: "idle" })}
                  >
                    Cancel
                  </button>
                </form>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">
            Done. A code sent there can now get you back into your account.
          </p>
        </Match>
        <Match when={state().kind === "already-enabled"}>
          <p class="form-info">
            An email or phone is already set for this account. To replace it, sign in again
            with your recovery phrase or from another device first.
          </p>
        </Match>
        <Match when={state().kind === "unavailable"}>
          <p class="form-error">
            Email and phone recovery are not available right now.
          </p>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message : "Unknown error.";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button
                  type="button"
                  onClick={() => setState({ kind: "idle" })}
                >
                  Try again
                </button>
              </div>
            );
          }}
        </Match>
      </Switch>
    </div>
  );
}

