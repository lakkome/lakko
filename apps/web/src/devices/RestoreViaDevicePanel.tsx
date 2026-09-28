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
import { authLimitMessage } from "../auth/auth-error-copy";
import { createSignal, onCleanup, Show, Switch, Match, type JSX } from "solid-js";
import QRCode from "qrcode";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { AuthClient } from "../auth/client";
import {
  comparisonCode,
  DEVICE_REQUEST_STATUS_POLL_MS,
} from "./device-request";
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
  /** ADR-0049 section 2: when given, the pairing payload is ALSO posted to
   *  the auth service so a signed-in device can confirm without a camera;
   *  the panel then leads with the comparison code and keeps the QR code
   *  behind a toggle. Evaluated after `prepare()` (the account may only be
   *  known then). `null` keeps the QR-only behaviour. */
  readonly requestAccount?: () => Uint8Array | null;
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
    | { kind: "error"; message: string };
  const [state, setState] = createSignal<State>({ kind: "idle" });
  const [canvasRef, setCanvasRef] = createSignal<HTMLCanvasElement | null>(
    null,
  );
  // ADR-0049 section 2 — the camera-less route.
  const [code, setCode] = createSignal<string | null>(null);
  const [showQr, setShowQr] = createSignal(false);
  const [requestNote, setRequestNote] = createSignal<string | null>(null);
  let statusTimer: ReturnType<typeof setInterval> | null = null;
  let declinedTimer: ReturnType<typeof setTimeout> | null = null;
  const stopStatusPolling = (): void => {
    if (statusTimer !== null) {
      clearInterval(statusTimer);
      statusTimer = null;
    }
    if (declinedTimer !== null) {
      clearTimeout(declinedTimer);
      declinedTimer = null;
    }
  };

  /** Post the payload so the other device can confirm it, and watch for a
   *  decline. A vanished request is NOT declared declined at once: the
   *  confirming device deletes it right after publishing the Welcome, so
   *  the Welcome poll is given a grace period to land first. */
  async function publishRequest(flow: DeviceRestoreFlowHandle, abort: AbortController): Promise<void> {
    const account = props.requestAccount?.() ?? null;
    if (account === null) return;
    setCode(await comparisonCode(flow.qrPayload));
    const client = new AuthClient();
    let requestId: string;
    try {
      requestId = (await client.deviceRequestCreate(account, flow.qrPayload)).requestId;
    } catch (err) {
      // ADR-0054 S4: `device-request-limited` has its own copy.
      const limit = authLimitMessage(err);
      setRequestNote(
        limit ??
          "Could not reach the server to ask your other device; the QR code still works. " +
            (err instanceof Error ? err.message : String(err)),
      );
      setShowQr(true);
      return;
    }
    statusTimer = setInterval(() => {
      void client
        .deviceRequestPending(requestId)
        .then((pending) => {
          if (pending || state().kind !== "waiting" || declinedTimer !== null) return;
          declinedTimer = setTimeout(() => {
            if (state().kind === "waiting") {
              abort.abort();
              flow.dispose();
              stopStatusPolling();
              setState({ kind: "error", message: "Declined on another device." });
            }
          }, 20_000);
        })
        .catch(() => undefined);
    }, DEVICE_REQUEST_STATUS_POLL_MS);
  }

  async function start(): Promise<void> {
    setState({ kind: "preparing" });
    let flow: DeviceRestoreFlowHandle;
    try {
      flow = await props.prepare();
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
      return;
    }
    try {
      // Flip to `waiting` FIRST so the canvas mounts (Solid renders
      // the matching branch synchronously), THEN paint the QR into
      // it.
      const abort = new AbortController();
      setCode(null);
      setShowQr(false);
      setRequestNote(null);
      setState({ kind: "waiting", flow, abort });
      void publishRequest(flow, abort);
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
          const confirmRoute = (props.requestAccount?.() ?? null) !== null;
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
          stopStatusPolling();
          if (outcome.kind === "received") {
            setState({ kind: "joining" });
            const recovery = await flow.accept(
              outcome.welcome,
              outcome.welcomeCursor,
            );
            if (recovery === null) {
              flow.dispose();
              setState({
                kind: "error",
                message:
                  "The other device confirmed but did not send your identity. " +
                  "Update Lakko on it and try again, or use your recovery phrase.",
              });
              return;
            }
            await props.onRestored(recovery);
            flow.dispose();
            setState({ kind: "done" });
          } else if (outcome.kind === "timeout") {
            flow.dispose();
            setState({
              kind: "error",
              message: "The other device did not answer in time. Try again.",
            });
          } else {
            flow.dispose();
            setState({ kind: "idle" });
          }
        } catch (err) {
          flow.dispose();
          setState({
            kind: "error",
            message: err instanceof Error ? err.message : String(err),
          });
        }
      })();
    } catch (err) {
      flow.dispose();
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  function cancel(): void {
    const s = state();
    if (s.kind === "waiting") {
      s.abort.abort();
    }
  }

  onCleanup(() => {
    stopStatusPolling();
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
            Show pairing code
          </button>
        </Match>
        <Match when={state().kind === "preparing"}>
          <p class="lk-muted">Preparing...</p>
        </Match>
        <Match when={state().kind === "waiting"}>
          <div style={{ "text-align": "center", padding: "1rem 0" }}>
            <Show when={code() !== null}>
              <p style={{ "margin-top": 0 }}>
                Open Lakko on a device that is already signed in and confirm
                this sign-in there.
              </p>
              <p class="form-info" style={{ "margin-bottom": "4px" }}>Comparison code</p>
              <div class="lk-sas-code" aria-label="Comparison code">{code()}</div>
              <p class="form-info">Make sure the other device shows the same code.</p>
              <Show when={requestNote() !== null}>
                <p class="form-error">{requestNote()}</p>
              </Show>
              <button type="button" onClick={() => setShowQr((v) => !v)}>
                {showQr() ? "Hide the QR code" : "Show a QR code instead"}
              </button>
            </Show>
            <canvas ref={(el) => setCanvasRef(el)} hidden={code() !== null && !showQr()} />
            <Show when={code() === null || showQr()}>
              <p class="lk-muted">
                On your other device, open Settings, then Devices, then Add a device, and
                scan this code.
              </p>
            </Show>
            <button type="button" onClick={cancel}>
              Cancel
            </button>
          </div>
        </Match>
        <Match when={state().kind === "joining"}>
          <p class="lk-muted">Transferring...</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">Restored.</p>
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
