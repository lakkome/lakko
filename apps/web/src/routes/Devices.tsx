// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Multi-device management route (Phase 2a3b2-ui).
 *
 * Two flows on one page, picked via a local tab:
 *
 *   1. **Pair this device with my existing one** — joining
 *      side. Generates an ephemeral DeviceIdentity-derived
 *      KeyPackage + rendezvous pseudonym, encodes the pairing
 *      QR, renders it via `qrcode`, long-polls the DS mailbox
 *      for the Welcome, drives `DeviceGroup.joinFromWelcome`
 *      on receipt.
 *
 *   2. **Add a device** — scanner side. Opens the camera,
 *      runs `jsqr` over each frame until a QR pops, decodes
 *      the wire bytes, runs `DeviceGroup.addDeviceFromWire`,
 *      PUTs the resulting Welcome to the rendezvous mailbox.
 *
 * Both flows pull credentials (accountId, deviceKey) from
 * `useAuth` + `getDeviceKey`. Page renders an "auth required"
 * state when the session isn't ready.
 *
 * The actual MLS / DS plumbing lives in
 * `apps/web/src/devices/orchestrator.ts`; this file is the
 * Solid view + the QR-render / camera bits.
 */
import {
  createSignal,
  createEffect,
  onCleanup,
  Show,
  Switch,
  Match,
  For,
  type JSX,
} from "solid-js";
import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import {
  prepareJoinFlow,
  runScanFlow,
  type JoinSession,
} from "../devices/orchestrator";
import {
  listDevices,
  removeDeviceAt,
  type DeviceListItem,
} from "../devices/remove";
import QRCode from "qrcode";
import jsQR from "jsqr";

type Tab = "list" | "pair" | "scan";

export default function Devices(): JSX.Element {
  const auth = useAuth();
  const [tab, setTab] = createSignal<Tab>("list");

  const session = (): { accountIdB64: string; accountIdBytes: Uint8Array } | null => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) return null;
    return {
      accountIdB64: s.account.accountId,
      accountIdBytes: secrets.accountId,
    };
  };

  return (
    <article>
      <h1>Devices</h1>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Link a second device to this account. The pairing happens
        end-to-end-encrypted: the server sees only the rendezvous
        pseudonym + opaque MLS bytes, never identity material.
      </p>
      <Show
        when={session() !== null}
        fallback={
          <p class="form-error">
            Sign in first to manage devices.
          </p>
        }
      >
        <div style={{ display: "flex", gap: "0.5rem", "margin-bottom": "1rem" }}>
          <button
            type="button"
            class={tab() === "list" ? "primary" : ""}
            onClick={() => setTab("list")}
          >
            Your devices
          </button>
          <button
            type="button"
            class={tab() === "pair" ? "primary" : ""}
            onClick={() => setTab("pair")}
          >
            Pair this device
          </button>
          <button
            type="button"
            class={tab() === "scan" ? "primary" : ""}
            onClick={() => setTab("scan")}
          >
            Add a device
          </button>
        </div>
        <Switch>
          <Match when={tab() === "list"}>
            <DevicesListView
              accountIdB64={session()!.accountIdB64}
              accountIdBytes={session()!.accountIdBytes}
            />
          </Match>
          <Match when={tab() === "pair"}>
            <PairView
              accountIdB64={session()!.accountIdB64}
              accountIdBytes={session()!.accountIdBytes}
            />
          </Match>
          <Match when={tab() === "scan"}>
            <ScanView
              accountIdB64={session()!.accountIdB64}
              accountIdBytes={session()!.accountIdBytes}
            />
          </Match>
        </Switch>
      </Show>
    </article>
  );
}

// ---- Your devices (list + remove) -------------------------------

function DevicesListView(props: {
  readonly accountIdB64: string;
  readonly accountIdBytes: Uint8Array;
}): JSX.Element {
  type State =
    | { kind: "loading" }
    | { kind: "ready"; devices: readonly DeviceListItem[] }
    | { kind: "removing"; leafIndex: number; devices: readonly DeviceListItem[] }
    | { kind: "error"; message: string };
  const [state, setState] = createSignal<State>({ kind: "loading" });

  async function refresh(): Promise<void> {
    setState({ kind: "loading" });
    try {
      const deviceKey = await getDeviceKey(props.accountIdB64);
      if (deviceKey === null) {
        setState({
          kind: "error",
          message: "No device key on this browser. Sign in again.",
        });
        return;
      }
      const list = await listDevices({
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
      });
      setState({ kind: "ready", devices: list.devices });
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  async function remove(leafIndex: number): Promise<void> {
    const current = state();
    const devices =
      current.kind === "ready" || current.kind === "removing"
        ? current.devices
        : [];
    setState({ kind: "removing", leafIndex, devices });
    try {
      const deviceKey = await getDeviceKey(props.accountIdB64);
      if (deviceKey === null) {
        setState({
          kind: "error",
          message: "No device key on this browser. Sign in again.",
        });
        return;
      }
      await removeDeviceAt({
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
        leafIndex,
      });
      // Re-fetch — the new epoch has its own leaf numbering.
      await refresh();
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  createEffect(() => {
    void refresh();
  });

  return (
    <div>
      <p>
        These are the devices currently linked to your account.
        Removing a device revokes its access via an MLS commit
        broadcast on the DeviceGroup journal — the revoked device
        can no longer decrypt any subsequent traffic on this
        account.
      </p>
      <Switch>
        <Match when={state().kind === "loading"}>
          <p>Loading devices…</p>
        </Match>
        <Match when={state().kind === "ready" || state().kind === "removing"}>
          {(_) => {
            const s = state();
            const devices =
              s.kind === "ready" || s.kind === "removing" ? s.devices : [];
            const removingLeaf =
              s.kind === "removing" ? s.leafIndex : null;
            return (
              <div>
                <ul style={{ "list-style": "none", padding: 0, margin: 0 }}>
                  <For each={devices}>
                    {(device) => (
                      <li
                        style={{
                          display: "flex",
                          "align-items": "center",
                          "justify-content": "space-between",
                          padding: "0.5rem 0",
                          "border-bottom":
                            "1px solid var(--surface-muted)",
                        }}
                      >
                        <div>
                          <div style={{ "font-family": "monospace" }}>
                            #{device.leafIndex}{" "}
                            {hexPrefix(device.signaturePublicKey, 8)}
                          </div>
                          <Show when={device.isSelf}>
                            <small style={{ color: "var(--surface-muted)" }}>
                              this device
                            </small>
                          </Show>
                        </div>
                        <Show
                          when={!device.isSelf}
                          fallback={
                            <span style={{ color: "var(--surface-muted)" }}>
                              —
                            </span>
                          }
                        >
                          <button
                            type="button"
                            disabled={removingLeaf !== null}
                            onClick={() => void remove(device.leafIndex)}
                          >
                            {removingLeaf === device.leafIndex
                              ? "Removing…"
                              : "Remove"}
                          </button>
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
                <p style={{ "margin-top": "1rem" }}>
                  <button
                    type="button"
                    disabled={removingLeaf !== null}
                    onClick={() => void refresh()}
                  >
                    Refresh
                  </button>
                </p>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message : "Unknown error.";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button type="button" onClick={() => void refresh()}>
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

function hexPrefix(bytes: Uint8Array, n: number): string {
  const slice = bytes.subarray(0, n);
  let out = "";
  for (let i = 0; i < slice.length; i += 1) {
    out += (slice[i] as number).toString(16).padStart(2, "0");
  }
  return out;
}

// ---- Pair (joining device) --------------------------------------

function PairView(props: {
  readonly accountIdB64: string;
  readonly accountIdBytes: Uint8Array;
}): JSX.Element {
  type State =
    | { kind: "idle" }
    | { kind: "preparing" }
    | { kind: "waiting"; session: JoinSession; abort: AbortController }
    | { kind: "joining" }
    | { kind: "done" }
    | { kind: "error"; message: string };
  const [state, setState] = createSignal<State>({ kind: "idle" });
  const [canvasRef, setCanvasRef] = createSignal<HTMLCanvasElement | null>(null);

  async function start(): Promise<void> {
    setState({ kind: "preparing" });
    try {
      const deviceKey = await getDeviceKey(props.accountIdB64);
      if (deviceKey === null) {
        setState({
          kind: "error",
          message: "No device key on this browser. Sign in again.",
        });
        return;
      }
      const sess = await prepareJoinFlow({
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
      });
      // Encode the wire bytes as a base64url string and feed
      // into `qrcode`. The library accepts arbitrary strings;
      // base64url keeps the QR within Byte mode without
      // ever crossing into binary-frame surprises that some
      // scanners reject.
      const b64 = base64UrlEncodeNoPad(sess.qrPayload);
      const canvas = canvasRef();
      if (canvas !== null) {
        await QRCode.toCanvas(canvas, b64, {
          errorCorrectionLevel: "M",
          width: 256,
          margin: 1,
        });
      }
      const abort = new AbortController();
      setState({ kind: "waiting", session: sess, abort });
      // Fire long-poll; await result inline.
      void (async (): Promise<void> => {
        try {
          const outcome = await sess.awaitWelcome(abort.signal);
          if (outcome.kind === "received") {
            setState({ kind: "joining" });
            await sess.accept(outcome.welcome);
            sess.dispose();
            setState({ kind: "done" });
          } else if (outcome.kind === "timeout") {
            sess.dispose();
            setState({
              kind: "error",
              message:
                "Timed out waiting for the other device. Try again.",
            });
          } else {
            // cancelled — silent (user closed the form).
            sess.dispose();
            setState({ kind: "idle" });
          }
        } catch (err) {
          sess.dispose();
          setState({
            kind: "error",
            message: err instanceof Error ? err.message : String(err),
          });
        }
      })();
    } catch (err) {
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
    }
  });

  return (
    <div>
      <p>
        Show this QR code to your already-paired device. It scans
        it, runs the MLS handshake, and your new device joins the
        DeviceGroup.
      </p>
      <Switch>
        <Match when={state().kind === "idle"}>
          <button class="primary" type="button" onClick={() => void start()}>
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
              Waiting for the other device to scan this code…
            </p>
            <button type="button" onClick={cancel}>
              Cancel
            </button>
          </div>
        </Match>
        <Match when={state().kind === "joining"}>
          <p>Joining the device group…</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">
            ✅ Linked. This device is now part of your DeviceGroup.
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

// ---- Scan (scanner device) --------------------------------------

function ScanView(props: {
  readonly accountIdB64: string;
  readonly accountIdBytes: Uint8Array;
}): JSX.Element {
  type State =
    | { kind: "idle" }
    | { kind: "opening" }
    | { kind: "scanning" }
    | { kind: "processing" }
    | { kind: "done" }
    | { kind: "error"; message: string };
  const [state, setState] = createSignal<State>({ kind: "idle" });
  const [videoRef, setVideoRef] = createSignal<HTMLVideoElement | null>(null);
  let stream: MediaStream | null = null;
  let rafHandle: number | null = null;

  function stopCamera(): void {
    if (rafHandle !== null) {
      cancelAnimationFrame(rafHandle);
      rafHandle = null;
    }
    if (stream !== null) {
      stream.getTracks().forEach((t) => t.stop());
      stream = null;
    }
    const v = videoRef();
    if (v !== null) {
      v.srcObject = null;
    }
  }

  onCleanup(stopCamera);

  async function handleDecoded(scannedBytes: Uint8Array): Promise<void> {
    setState({ kind: "processing" });
    stopCamera();
    try {
      const deviceKey = await getDeviceKey(props.accountIdB64);
      if (deviceKey === null) {
        setState({
          kind: "error",
          message: "No device key on this browser. Sign in again.",
        });
        return;
      }
      await runScanFlow({
        scannedBytes,
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
      });
      setState({ kind: "done" });
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  async function start(): Promise<void> {
    setState({ kind: "opening" });
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        video: { facingMode: "environment" },
        audio: false,
      });
    } catch (err) {
      setState({
        kind: "error",
        message:
          "Camera access denied or unavailable. " +
          (err instanceof Error ? err.message : String(err)),
      });
      return;
    }
    const video = videoRef();
    if (video === null) {
      stopCamera();
      setState({
        kind: "error",
        message: "Internal: video element missing.",
      });
      return;
    }
    video.srcObject = stream;
    await video.play();
    setState({ kind: "scanning" });

    const canvas = document.createElement("canvas");
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (ctx === null) {
      setState({
        kind: "error",
        message: "Canvas 2D context unavailable.",
      });
      return;
    }
    const loop = (): void => {
      if (state().kind !== "scanning") return;
      if (video.readyState !== video.HAVE_ENOUGH_DATA) {
        rafHandle = requestAnimationFrame(loop);
        return;
      }
      canvas.width = video.videoWidth;
      canvas.height = video.videoHeight;
      ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
      const image = ctx.getImageData(0, 0, canvas.width, canvas.height);
      const detected = jsQR(image.data, image.width, image.height);
      if (detected !== null) {
        const bytes = base64UrlDecodeNoPad(detected.data);
        if (bytes !== null) {
          void handleDecoded(bytes);
          return;
        }
        // Decoded text wasn't our base64url payload; keep
        // scanning — the user might have pointed the camera
        // at a different QR.
      }
      rafHandle = requestAnimationFrame(loop);
    };
    rafHandle = requestAnimationFrame(loop);
  }

  function reset(): void {
    stopCamera();
    setState({ kind: "idle" });
  }

  return (
    <div>
      <p>
        Aim your camera at the QR code your new device is showing.
        The pairing completes within a couple of seconds.
      </p>
      <Switch>
        <Match when={state().kind === "idle"}>
          <button class="primary" type="button" onClick={() => void start()}>
            Open camera
          </button>
        </Match>
        <Match when={state().kind === "opening"}>
          <p>Opening camera…</p>
        </Match>
        <Match when={state().kind === "scanning"}>
          <div style={{ "text-align": "center", padding: "1rem 0" }}>
            <video
              ref={(el) => setVideoRef(el)}
              style={{ "max-width": "100%", "max-height": "320px" }}
              playsinline
              muted
            />
            <button type="button" onClick={reset}>
              Cancel
            </button>
          </div>
        </Match>
        <Match when={state().kind === "processing"}>
          <p>Adding the device…</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">
            ✅ Device added. The other device should land on its
            success screen any moment now.
          </p>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message : "Unknown error.";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button type="button" onClick={reset}>
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

// ---- base64url helpers (no padding) -----------------------------

function base64UrlEncodeNoPad(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i] as number);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

function base64UrlDecodeNoPad(s: string): Uint8Array | null {
  const padded = s + "===".slice((s.length + 3) % 4);
  const std = padded.replace(/-/g, "+").replace(/_/g, "/");
  try {
    const binary = atob(std);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) {
      out[i] = binary.charCodeAt(i);
    }
    return out;
  } catch {
    return null;
  }
}
