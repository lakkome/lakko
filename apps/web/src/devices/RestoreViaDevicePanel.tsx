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
import { createSignal, onCleanup, Switch, Match, type JSX } from "solid-js";
import QRCode from "qrcode";
import { base64UrlEncodeNoPad } from "../auth/account-id";
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
      setState({ kind: "waiting", flow, abort });
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
          const outcome = await flow.awaitWelcome(abort.signal);
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
                  "The other device completed the pairing but sent no " +
                  "identity payload. Make sure it runs the latest Lakko " +
                  "version, or use your recovery phrase instead.",
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
              message: "Timed out waiting for the other device. Try again.",
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
          <p>Preparing pairing material…</p>
        </Match>
        <Match when={state().kind === "waiting"}>
          <div style={{ "text-align": "center", padding: "1rem 0" }}>
            <canvas ref={(el) => setCanvasRef(el)} />
            <p style={{ color: "var(--surface-muted)" }}>
              On your other device, open Devices → Add a device and
              scan this code…
            </p>
            <button type="button" onClick={cancel}>
              Cancel
            </button>
          </div>
        </Match>
        <Match when={state().kind === "joining"}>
          <p>Transferring your identity…</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">Restored. Welcome back.</p>
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
