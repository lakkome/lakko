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
import { authLimitMessage, isOffline } from "./auth-error-copy";
import { t } from "../l10n";
import { tRich } from "../l10n/rich";

/**
 * The text a failed enrolment shows (ADR-0061 section 6: from the kind of
 * failure, never the server's prose, which goes to the log). Returned as a
 * function so it renders in the language current when it is shown.
 */
function failureText(err: unknown, genericKey: string): () => string {
  console.warn("[recovery-factors] enrolment failed", err);
  return () => authLimitMessage(err) ?? (isOffline(err) ? t("common.error.offline") : t(genericKey));
}

const sessionEnded = (): string => t("common.error.session_ended");

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
    | { kind: "error"; message: () => string };
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
      setState({ kind: "error", message: sessionEnded });
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
      setState({ kind: "error", message: failureText(err, "settings.recovery.totp.error.generic") });
    }
  }

  async function finish(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const s = state();
    if (s.kind !== "confirm") return;
    setConfirmError(null);
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setConfirmError(t("recovery.totp.error.enter_code"));
      return;
    }
    const token = sessionToken();
    if (token === null) {
      setState({ kind: "error", message: sessionEnded });
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
        setConfirmError(t("settings.recovery.totp.error.wrong"));
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
      setState({ kind: "error", message: failureText(err, "settings.recovery.totp.error.generic") });
    }
  }

  return (
    <div>
      <p class="lk-card-desc">{t("settings.recovery.totp.lede")}</p>
      <Switch>
        <Match when={state().kind === "idle"}>
          {/* HONESTY NOTE: the server has no TOTP "status" endpoint,
              so the enrolled/not-enrolled state cannot be probed
              up front. We render the enable button by default; a
              409 from /totp/enroll/start flips this card to
              "already enabled". */}
          <button class="primary" type="button" onClick={() => void start()}>
            {t("settings.recovery.totp.set_up")}
          </button>
        </Match>
        <Match when={state().kind === "starting"}>
          <p class="lk-muted">{t("common.preparing")}</p>
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
                <p>{t("settings.recovery.totp.scan")}</p>
                <canvas class="lk-qr" ref={(el) => setCanvasRef(el)} />
                <p class="lk-meta">
                  {tRich("settings.recovery.totp.key", {
                    key: <code style={{ "user-select": "all" }}>{secretB32}</code>,
                  })}
                </p>
                <form onSubmit={(e) => void finish(e)}>
                  <label>
                    {t("recovery.totp.code_label")}
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
                    {busy ? t("common.confirming") : t("common.confirm")}
                  </button>{" "}
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => setState({ kind: "idle" })}
                  >
                    {t("common.cancel")}
                  </button>
                </form>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">{t("settings.recovery.totp.done")}</p>
        </Match>
        <Match when={state().kind === "already-enabled"}>
          <p class="form-info">{t("settings.recovery.totp.already_enabled")}</p>
        </Match>
        <Match when={state().kind === "unavailable"}>
          <p class="form-error">{t("settings.recovery.totp.unavailable")}</p>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message() : "";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button
                  type="button"
                  onClick={() => setState({ kind: "idle" })}
                >
                  {t("common.try_again")}
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
    | { kind: "error"; message: () => string };
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
          ? t("recovery.contact.error.invalid_email")
          : t("recovery.contact.error.invalid_phone"),
      );
      return;
    }
    const token = sessionToken();
    if (token === null) {
      setState({ kind: "error", message: sessionEnded });
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
        setRequestError(t("settings.recovery.contact.error.bad_contact"));
        setState({ kind: "idle" });
        return;
      }
      setState({ kind: "error", message: failureText(err, "settings.recovery.contact.error.generic") });
    }
  }

  async function finish(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const s = state();
    if (s.kind !== "confirm") return;
    setConfirmError(null);
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setConfirmError(t("recovery.contact.error.enter_code"));
      return;
    }
    const token = sessionToken();
    if (token === null) {
      setState({ kind: "error", message: sessionEnded });
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
        setConfirmError(t("recovery.contact.error.wrong"));
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
      setState({ kind: "error", message: failureText(err, "settings.recovery.contact.error.generic") });
    }
  }

  return (
    <div>
      <p class="lk-card-desc">{t("settings.recovery.contact.lede")}</p>
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
                    {t("common.email")}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={contactKind() === ContactKind.Phone}
                    disabled={busy}
                    class={contactKind() === ContactKind.Phone ? "primary" : ""}
                    onClick={() => setContactKind(ContactKind.Phone)}
                  >
                    {t("common.phone")}
                  </button>
                </div>
                <label>
                  {contactKind() === ContactKind.Email ? t("common.email") : t("recovery.contact.phone_label")}
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
                  {busy ? t("common.sending") : t("recovery.contact.send")}
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
                  {tRich(
                    k === ContactKind.Email
                      ? "settings.recovery.contact.sent_email"
                      : "settings.recovery.contact.sent_phone",
                    { contact: <strong>{to}</strong> },
                  )}
                </p>
                <form onSubmit={(e) => void finish(e)}>
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
                    {busy ? t("common.confirming") : t("common.confirm")}
                  </button>{" "}
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => setState({ kind: "idle" })}
                  >
                    {t("common.cancel")}
                  </button>
                </form>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">{t("settings.recovery.contact.done")}</p>
        </Match>
        <Match when={state().kind === "already-enabled"}>
          <p class="form-info">{t("settings.recovery.contact.already_enabled")}</p>
        </Match>
        <Match when={state().kind === "unavailable"}>
          <p class="form-error">{t("settings.recovery.contact.unavailable")}</p>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message() : "";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button
                  type="button"
                  onClick={() => setState({ kind: "idle" })}
                >
                  {t("common.try_again")}
                </button>
              </div>
            );
          }}
        </Match>
      </Switch>
    </div>
  );
}

