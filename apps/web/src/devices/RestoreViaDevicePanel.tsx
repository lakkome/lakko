// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Restore from your other device" QR panel (ADR-0018 factor (a)).
 *
 * Shared by the two web entry points of the another-device
 * recovery:
 *
 *   * path (a) — `RestoreIdentityPrompt` (password-authenticated
 *     fresh device missing the AccountIdentity seed);
 *   * path (b) — the no-password entry on the `/recover` route
 *     (anonymous pairing; the redeemed grant is the only ACCESS).
 *
 * The panel owns the QR render + the wait/join state machine —
 * the same shape as the `PairView` in `routes/Devices.tsx` — and
 * delegates flow construction (`prepare`) and completion
 * (`onRestored`) to the caller, which is where the two paths
 * diverge (authenticated vs anonymous join, `completeFromPairing`
 * vs `completeFromDeviceRestore`).
 */
import { isAbandoned } from "../auth/binding-epoch";
import { authLimitMessage, deviceRestoreErrorMessage } from "../auth/auth-error-copy";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { createSignal, onCleanup, Show, Switch, Match, type JSX } from "solid-js";
import QRCode from "qrcode";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { AuthClient, AuthHttpError } from "../auth/client";
import { loadWasm } from "../wasm";
import { DEVICE_REQUEST_STATUS_POLL_MS } from "./device-request";
import {
  LINK_NEW_INTRO,
  LINK_RESTORING,
  LINK_UNREACHABLE,
  LINK_WAITING,
  createDeviceLinkInitiator,
  deviceLinkCoreOf,
  initiatorEndMessage,
  isTerminalInitiatorView,
  type DeviceLinkInitiatorFlow,
  type InitiatorView,
  type LinkText,
} from "./device-link";
import type { JoinRecoveryOutcome } from "./orchestrator";
import type { WelcomeOutcome } from "./welcome-courier";

/** The slice of a join session the panel drives. Both
 *  `JoinSession` (path (a)) and `AnonymousJoinSession` (path (b))
 *  adapt onto this shape in their respective callers. */
export interface DeviceRestoreFlowHandle {
  /** Wire-format pairing QR bytes. */
  readonly qrPayload: Uint8Array;
  /** Long-poll the rendezvous mailbox for the Welcome. */
  awaitWelcome(signal?: AbortSignal): Promise<WelcomeOutcome>;
  /** Join + consume the recovery row; `null` when the scanner
   *  published none (older build / best-effort failure). */
  accept(
    welcomeBytes: Uint8Array,
    welcomeCursor: number,
  ): Promise<JoinRecoveryOutcome | null>;
  /** Release wasm handles / zeroize in-memory secrets. */
  dispose(): void;
}

export default function RestoreViaDevicePanel(props: {
  /** Build the join flow (lookup / key load happens inside; throw
   *  with a user-facing message on failure). */
  readonly prepare: () => Promise<DeviceRestoreFlowHandle>;
  /** Complete the restore from a received recovery outcome. The
   *  flow handle is still alive when this runs (so a path-(b)
   *  caller can persist join state from inside); the panel
   *  disposes it afterwards. Throw to surface an error. */
  readonly onRestored: (outcome: JoinRecoveryOutcome) => Promise<void>;
  /** Disable the start affordance (parent form busy). */
  readonly disabled?: boolean;
  /** ADR-0049 section 2 (amended 2026-09-28): when given, the panel asks a
   *  signed-in device through the auth service and leads with a six-digit
   *  field for the code that device shows (the device link, `device-link.ts`);
   *  the QR code stays one button away. The pairing payload leaves this
   *  device only sealed inside the exchange, and the Welcome is trusted only
   *  after the other device's key confirmation verifies. ADR-0056: the
   *  request is addressed by the 32-byte login handle of the typed username,
   *  never by an account id. Evaluated after `prepare()`. `null` keeps the
   *  QR-only behaviour. */
  readonly requestHandle?: () => Uint8Array | null;
}): JSX.Element {
  type State =
    | { kind: "idle" }
    | { kind: "preparing" }
    | {
        kind: "waiting";
        flow: DeviceRestoreFlowHandle;
        abort: AbortController;
      }
    | { kind: "joining" }
    | { kind: "done" }
    /** `messageKey` is a catalog key; `error` a caught value rendered through
     *  `deviceRestoreErrorMessage` when it is shown. */
    | { kind: "error"; messageKey?: string; error?: unknown }
    /** The device-link request ended (wrong code, declined, expired):
     *  Start again posts a new one. */
    | { kind: "link-ended"; message: LinkText };
  const [state, setState] = createSignal<State>({ kind: "idle" });
  const [canvasRef, setCanvasRef] = createSignal<HTMLCanvasElement | null>(
    null,
  );
  // ADR-0049 section 2 — the camera-less route (the device link).
  const [link, setLink] = createSignal<DeviceLinkInitiatorFlow | null>(null);
  const [linkView, setLinkView] = createSignal<InitiatorView | null>(null);
  const [typed, setTyped] = createSignal("");
  const [showQr, setShowQr] = createSignal(false);
  const [requestNote, setRequestNote] = createLiveText();
  /** `true` while this attempt goes through the device link: the Welcome
   *  then needs the other device's verified confirmation. Cleared only by
   *  an explicit switch to the QR code (or a request that could not be
   *  posted, which falls back to it). */
  const [linkRequired, setLinkRequired] = createSignal(false);
  /** Set while `start()` of the link runs: a `failed` view then means the
   *  request could not be posted, which falls back to the QR code instead
   *  of ending the attempt. */
  let linkStarting = false;
  /** Bumped by every `start()`, so a finishing Welcome loop of an earlier
   *  attempt never touches the link or the state of a newer one. */
  let attemptSeq = 0;
  /** Set by `onCleanup`, which also bumps `attemptSeq`: once the panel is
   *  gone no attempt is current, so nothing asks another device, arms a
   *  timer or lands a session on a screen the person has left. */
  let disposed = false;
  let statusTimer: ReturnType<typeof setInterval> | null = null;
  const stopStatusPolling = (): void => {
    if (statusTimer !== null) {
      clearInterval(statusTimer);
      statusTimer = null;
    }
  };
  const abandonLink = (): void => {
    stopStatusPolling();
    link()?.dispose();
    setLink(null);
    setLinkView(null);
  };

  /** A terminal link view ends the whole attempt: the Welcome loop stops
   *  and the join flow is dropped, so nothing that reaches this device's
   *  rendezvous afterwards is accepted. Start again prepares afresh. */
  function onLinkView(v: InitiatorView): void {
    setLinkView(v);
    if (!isTerminalInitiatorView(v) || (v.kind === "failed" && linkStarting)) return;
    const s = state();
    stopStatusPolling();
    // Aborting ends the Welcome loop, which disposes the join flow.
    if (s.kind === "waiting") s.abort.abort();
    setState({ kind: "link-ended", message: initiatorEndMessage(v) ?? LINK_UNREACHABLE });
  }

  /** Ask the other device, and poll for its answer and decision. */
  async function publishRequest(flow: DeviceRestoreFlowHandle, attempt: number): Promise<void> {
    const handle = props.requestHandle?.() ?? null;
    if (handle === null) return;
    const client = new AuthClient();
    const initiator = createDeviceLinkInitiator(
      {
        core: deviceLinkCoreOf(await loadWasm()),
        create: (h) => client.deviceRequestCreate(h),
        status: (id) => client.deviceRequestStatus(id),
        postPayload: (id, sealed) => client.deviceRequestPayload(id, sealed),
        now: () => Date.now(),
        sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
        // A superseded or abandoned request never drives this screen.
        onView: (v) => {
          if (attempt === attemptSeq && link() === initiator) onLinkView(v);
        },
      },
      { loginHandle: handle, pairingPayload: flow.qrPayload },
    );
    if (attempt !== attemptSeq) {
      initiator.dispose();
      return;
    }
    setLink(initiator);
    linkStarting = true;
    let reached: InitiatorView;
    try {
      reached = await initiator.start();
    } finally {
      linkStarting = false;
    }
    if (attempt !== attemptSeq || link() !== initiator) return;
    if (reached.kind === "failed") {
      const err = reached.error;
      // ADR-0054 S4: `device-request-limited` has its own copy.
      const limit = authLimitMessage(err);
      const byNameOff =
        err instanceof AuthHttpError &&
        err.status === 503 &&
        err.errorCode === "login-by-name-disabled";
      // eslint-disable-next-line no-console
      if (limit === null && !byNameOff) console.warn("device request: create failed", err);
      setRequestNote(() =>
        authLimitMessage(err) ??
          (byNameOff
            ? t("device_link.error.ask_unavailable")
            : t("device_link.error.ask_unreachable")),
      );
      abandonLink();
      setLinkRequired(false);
      setShowQr(true);
      return;
    }
    stopStatusPolling();
    statusTimer = setInterval(() => void initiator.poll(), DEVICE_REQUEST_STATUS_POLL_MS);
  }

  function submitCode(e: Event): void {
    e.preventDefault();
    void link()?.submit(typed());
  }

  /** "Show a QR code instead": the request is abandoned (the other device's
   *  prompt expires on its own) and the QR code is what pairs. */
  function switchToQr(): void {
    abandonLink();
    setLinkRequired(false);
    setShowQr(true);
  }

  /** The Welcome may be accepted only after the other device's key
   *  confirmation verified, when the device link is the route in use. A QR
   *  scan needs no such gate: the person pointed the camera. */
  async function welcomeTrusted(abort: AbortController): Promise<boolean> {
    if (abort.signal.aborted) return false;
    if (!linkRequired()) return true;
    const l = link();
    if (l === null) return false;
    return l.awaitConfirmation(abort.signal);
  }

  async function start(): Promise<void> {
    setState({ kind: "preparing" });
    let flow: DeviceRestoreFlowHandle;
    try {
      flow = await props.prepare();
    } catch (err) {
      setState({ kind: "error", error: err });
      return;
    }
    if (disposed) {
      // Left while the join flow was being prepared: no request goes out
      // (the account's other devices are not asked "Is this you?").
      flow.dispose();
      return;
    }
    try {
      // Flip to `waiting` FIRST so the canvas mounts (Solid renders
      // the matching branch synchronously), THEN paint the QR into
      // it.
      const abort = new AbortController();
      const attempt = ++attemptSeq;
      const current = (): boolean => attempt === attemptSeq;
      const endLink = (): void => {
        if (current()) abandonLink();
      };
      abandonLink();
      setLinkRequired((props.requestHandle?.() ?? null) !== null);
      setTyped("");
      setShowQr(false);
      setRequestNote(null);
      setState({ kind: "waiting", flow, abort });
      void publishRequest(flow, attempt);
      const b64 = base64UrlEncodeNoPad(flow.qrPayload);
      const canvas = canvasRef();
      if (canvas !== null) {
        await QRCode.toCanvas(canvas, b64, {
          errorCorrectionLevel: "M",
          width: 256,
          margin: 1,
        });
      }
      void (async (): Promise<void> => {
        try {
          // A confirmation on another device can take minutes (the request
          // itself lives ten); a QR scan is quick. Keep polling for the
          // Welcome as long as the request is alive, in one-minute legs.
          const confirmRoute = (props.requestHandle?.() ?? null) !== null;
          const deadline = Date.now() + (confirmRoute ? 10 * 60_000 : 60_000);
          let outcome = await flow.awaitWelcome(abort.signal);
          while (
            outcome.kind === "timeout" &&
            Date.now() < deadline &&
            !abort.signal.aborted &&
            state().kind === "waiting"
          ) {
            outcome = await flow.awaitWelcome(abort.signal);
          }
          if (outcome.kind === "received") {
            if (!(await welcomeTrusted(abort))) {
              // A terminal link view already ended the attempt (or it was
              // cancelled): never accept this Welcome.
              endLink();
              flow.dispose();
              if (current() && state().kind === "waiting") setState({ kind: "idle" });
              return;
            }
            stopStatusPolling();
            setState({ kind: "joining" });
            const recovery = await flow.accept(
              outcome.welcome,
              outcome.welcomeCursor,
            );
            if (recovery === null) {
              flow.dispose();
              setState({ kind: "error", messageKey: "device_link.error.no_identity" });
              return;
            }
            if (!current()) {
              // The panel was left while the Welcome was being accepted. No
              // session is landed on whatever screen is up now; the join is
              // dropped with the flow, and pairing again recovers it.
              recovery.phraseEntropy.fill(0);
              flow.dispose();
              return;
            }
            await props.onRestored(recovery);
            flow.dispose();
            setState({ kind: "done" });
          } else if (outcome.kind === "timeout") {
            endLink();
            flow.dispose();
            if (!current() || state().kind === "link-ended") return;
            setState({ kind: "error", messageKey: "device_link.error.timeout" });
          } else {
            endLink();
            flow.dispose();
            if (current() && state().kind === "waiting") setState({ kind: "idle" });
          }
        } catch (err) {
          endLink();
          flow.dispose();
          if (!current() || state().kind === "link-ended") return;
          // The completion was overtaken by another session on this device:
          // back to the start, with no error.
          if (isAbandoned(err)) {
            setState({ kind: "idle" });
            return;
          }
          setState({ kind: "error", error: err });
        }
      })();
    } catch (err) {
      flow.dispose();
      setState({ kind: "error", error: err });
    }
  }

  function cancel(): void {
    const s = state();
    abandonLink();
    if (s.kind === "waiting") {
      s.abort.abort();
    }
  }

  onCleanup(() => {
    disposed = true;
    attemptSeq += 1;
    abandonLink();
    const s = state();
    if (s.kind === "waiting") {
      s.abort.abort();
      s.flow.dispose();
    }
  });

  return (
    <div>
      <Switch>
        <Match when={state().kind === "idle"}>
          <button
            class="primary"
            type="button"
            disabled={props.disabled === true}
            onClick={() => void start()}
          >
            {t("device_link.show_code")}
          </button>
        </Match>
        <Match when={state().kind === "preparing"}>
          <p class="lk-muted">{t("common.preparing")}</p>
        </Match>
        <Match when={state().kind === "waiting"}>
          <div style={{ "text-align": "center", padding: "1rem 0" }}>
            <Show when={linkRequired() && !showQr()}>
              {(_) => {
                const v = (): InitiatorView => linkView() ?? { kind: "requesting" };
                const fieldOn = (): boolean => v().kind === "ready";
                return (
                  <form onSubmit={submitCode}>
                    <p style={{ "margin-top": 0 }}>{LINK_NEW_INTRO()}</p>
                    <input
                      type="text"
                      inputmode="numeric"
                      autocomplete="one-time-code"
                      aria-label={t("device_link.code.field_a11y")}
                      placeholder="47 19 03"
                      maxLength={8}
                      class="lk-sas-code"
                      value={typed()}
                      disabled={!fieldOn()}
                      onInput={(e) => setTyped(e.currentTarget.value)}
                    />
                    <Show when={v().kind === "requesting" || v().kind === "waiting"}>
                      <p class="lk-muted">{LINK_WAITING()}</p>
                    </Show>
                    <Show when={v().kind === "ready" ? (v() as { error: LinkText | null }).error : null}>
                      {(err) => <p class="form-error">{err()()}</p>}
                    </Show>
                    <Show when={v().kind === "checking"}>
                      <p class="lk-muted">{t("common.checking")}</p>
                    </Show>
                    <Show when={v().kind === "confirmed"}>
                      <p class="lk-muted">{LINK_RESTORING()}</p>
                    </Show>
                    <Show when={v().kind === "failed"}>
                      <p class="form-error">{LINK_UNREACHABLE()}</p>
                    </Show>
                    <div class="lk-actions">
                      <button
                        class="primary"
                        type="submit"
                        disabled={!fieldOn() || typed().replace(/\D/g, "").length !== 6}
                      >
                        {t("common.continue")}
                      </button>
                      <Show when={v().kind === "requesting" || v().kind === "waiting" || v().kind === "ready"}>
                        <button type="button" onClick={switchToQr}>
                          {t("device_link.qr.show")}
                        </button>
                      </Show>
                    </div>
                  </form>
                );
              }}
            </Show>
            <Show when={requestNote() !== null}>
              <p class="form-error">{requestNote()}</p>
            </Show>
            <canvas ref={(el) => setCanvasRef(el)} hidden={linkRequired() && !showQr()} />
            <Show when={!linkRequired() || showQr()}>
              <p class="lk-muted">{t("device_link.qr.scan_hint")}</p>
            </Show>
            <button type="button" onClick={cancel}>
              {t("common.cancel")}
            </button>
          </div>
        </Match>
        <Match when={state().kind === "joining"}>
          <p class="lk-muted">{t("device_link.transferring")}</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">{t("device_link.restored")}</p>
        </Match>
        <Match when={state().kind === "link-ended"}>
          {(_) => {
            const s = state();
            const message = (): string => (s.kind === "link-ended" ? s.message() : "");
            return (
              <div>
                <p class="form-error">{message()}</p>
                <button class="primary" type="button" onClick={() => void start()}>
                  {t("device_link.code.start_again")}
                </button>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const message = (): string => {
              const s = state();
              if (s.kind !== "error") return "";
              if (s.messageKey !== undefined) return t(s.messageKey);
              return deviceRestoreErrorMessage(s.error);
            };
            return (
              <div>
                <p class="form-error">{message()}</p>
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
