// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Devices (Settings > Security > Devices; docs/ux/client-redesign.md
 * section 5.5): the list of this account's devices with "This device" and
 * Remove, and one "Add a device" button that reveals the pairing flow as a
 * secondary panel — show a code for this device, or scan the code another
 * device shows. A device without a camera asks through the "Is this you?"
 * prompt (ADR-0049), which the app shell shows on every screen.
 *
 * The authenticator and email / phone recovery factors that used to share
 * this page live under Settings > Security > Recovery
 * (`auth/RecoveryFactors.tsx`).
 *
 * The device-group plumbing lives in `devices/orchestrator.ts` and
 * `devices/remove.ts`; this file is the Solid view + the QR-render / camera
 * bits.
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
import { A } from "@solidjs/router";
import { Icon } from "../ui/icons";
import { useAuth } from "../auth/AuthContext";
import { AuthClient } from "../auth/client";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";
import { getDeviceKey } from "../auth/key-store";
import {
  WebauthnClient,
  WebauthnHttpError,
  actionHashForRemoveDevice,
} from "../auth/webauthn-client";
import {
  prepareJoinFlow,
  runScanFlow,
  type JoinSession,
} from "../devices/orchestrator";
import { currentServiceEnvelopes } from "../reach/service-envelope-outbox";
import {
  listDevices,
  removeDeviceAt,
  type DeviceListItem,
} from "../devices/remove";
import { PAIRING_NEEDS_USERNAME, ownPairingHandle } from "../devices/pairing";
import QRCode from "qrcode";
import jsQR from "jsqr";
import { paths } from "./paths";

type AddMode = "closed" | "pick" | "show" | "scan";

export default function Devices(): JSX.Element {
  const auth = useAuth();
  const [add, setAdd] = createSignal<AddMode>("closed");

  const session = (): {
    accountIdB64: string;
    accountIdBytes: Uint8Array;
    username: string;
  } | null => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) return null;
    return {
      accountIdB64: s.account.accountId,
      accountIdBytes: secrets.accountId,
      username: s.account.username,
    };
  };
  // ADR-0056: both halves of adding a device address the account by the
  // login handle of its username, so an account with no username (a guest)
  // cannot add one.
  const hasUsername = (): boolean => (session()?.username.trim().length ?? 0) > 0;

  return (
    <article>
      <div class="lk-page lk-stack">
        <A href={paths.settingsSection("security")} class="ghost small" style={{ "align-self": "flex-start" }}>
          <Icon name="back" size={16} />
          Security
        </A>
        <div class="lk-page-head">
          <h1>Devices</h1>
          <p>
            The phones, tablets and computers signed in to your account. Adding one is
            end-to-end encrypted; Lakko's servers cannot read it.
          </p>
        </div>
        <Show
          when={session() !== null}
          fallback={<p class="form-error">Sign in first to manage devices.</p>}
        >
          <section class="lk-card">
            <DevicesListView
              accountIdB64={session()!.accountIdB64}
              accountIdBytes={session()!.accountIdBytes}
            />
            <div class="lk-actions">
              <button
                type="button"
                class="primary"
                aria-expanded={add() !== "closed"}
                disabled={!hasUsername()}
                onClick={() => setAdd(add() === "closed" ? "pick" : "closed")}
              >
                <Icon name="plus" size={18} />
                Add a device
              </button>
            </div>
            <Show when={!hasUsername()}>
              <p class="lk-meta" style={{ margin: 0 }}>{PAIRING_NEEDS_USERNAME}</p>
            </Show>
          </section>
          <Show when={add() !== "closed" && hasUsername()}>
            <section class="lk-card" aria-label="Add a device">
              <Switch>
                <Match when={add() === "pick"}>
                  <h2>Add a device</h2>
                  <p class="lk-card-desc">
                    Sign in to Lakko on the new device first. Then pick which of the two
                    is showing a code.
                  </p>
                  <div class="lk-device-add">
                    <button type="button" class="lk-option-card" onClick={() => setAdd("scan")}>
                      <Icon name="qr" size={22} />
                      <strong>Scan a code</strong>
                      <span>The new device shows a code; scan it with this one.</span>
                    </button>
                    <button type="button" class="lk-option-card" onClick={() => setAdd("show")}>
                      <Icon name="devices" size={22} />
                      <strong>Show a code</strong>
                      <span>This device is the new one; another device scans its code.</span>
                    </button>
                  </div>
                  <p class="lk-meta" style={{ margin: 0 }}>
                    No camera on either device? Sign in on the new one and choose "Another
                    device": an "Is this you?" prompt with a code to compare appears here.
                  </p>
                </Match>
                <Match when={add() === "show"}>
                  <PairView
                    accountIdB64={session()!.accountIdB64}
                    accountIdBytes={session()!.accountIdBytes}
                    username={session()!.username}
                    onBack={() => setAdd("pick")}
                  />
                </Match>
                <Match when={add() === "scan"}>
                  <ScanView
                    accountIdB64={session()!.accountIdB64}
                    accountIdBytes={session()!.accountIdBytes}
                    username={session()!.username}
                    onBack={() => setAdd("pick")}
                  />
                </Match>
              </Switch>
            </section>
          </Show>
        </Show>
      </div>
    </article>
  );
}

/**
 * The label for a device row. The store carries no name, platform or added
 * date for a device (only its position and key), so the rule is: "This
 * device" for the one you are on, otherwise "Device N" by order in the list.
 * Never a key prefix or an internal index (docs/ux section 6).
 */
export function deviceRowLabel(index: number, isSelf: boolean): string {
  return isSelf ? "This device" : `Device ${index + 1}`;
}

// ---- Your devices (list + remove) -------------------------------

function DevicesListView(props: {
  readonly accountIdB64: string;
  readonly accountIdBytes: Uint8Array;
}): JSX.Element {
  // Same auth context the outer page reads — used to bind the device-
  // removal action to a fresh passkey gesture (G-13).
  const auth = useAuth();
  // ADR-0050 section 2.10 — the removal fan-out over every chat.
  const receive = useBackgroundReceive();
  type State =
    | { kind: "loading" }
    | { kind: "ready"; devices: readonly DeviceListItem[] }
    | { kind: "removing"; leafIndex: number; devices: readonly DeviceListItem[] }
    | { kind: "error"; message: string };
  const [state, setState] = createSignal<State>({ kind: "loading" });

  async function load(): Promise<void> {
    setState({ kind: "loading" });
    try {
      const deviceKey = await getDeviceKey(props.accountIdB64);
      if (deviceKey === null) {
        setState({
          kind: "error",
          message: "This browser is not set up for your account. Sign in again.",
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
          message: "This browser is not set up for your account. Sign in again.",
        });
        return;
      }

      // G-13 (ADR-0014): gate the eviction on a fresh passkey gesture
      // cryptographically bound to the removed leaf — but only if the
      // account actually has a passkey. OPAQUE-only accounts (no passkey)
      // fall through on session-token auth alone (a 404 from the ceremony
      // start signals "no credentials"). A user-cancelled gesture or any
      // other failure aborts the removal (fail-closed).
      const sessionToken = auth.identitySecrets()?.sessionToken ?? null;
      const target = devices.find((d) => d.leafIndex === leafIndex);
      // G-13 (ADR-0015): the passkey ceremony yields an attestation bound to
      // the leaf being removed; carry it with the Remove commit so peers can
      // verify it. A 404 means the account has no passkey (OPAQUE-only) —
      // proceed without an attestation (the DeviceGroup policy is off there).
      let attestation: Uint8Array | null = null;
      if (sessionToken !== null && target !== undefined) {
        const wc = new WebauthnClient();
        wc.setSessionToken(sessionToken);
        try {
          const actionHashB64 = await actionHashForRemoveDevice(
            target.signaturePublicKey,
          );
          const result = await wc.assertAction(actionHashB64);
          attestation = base64UrlDecodeNoPad(result.attestation_b64);
        } catch (err) {
          if (!(err instanceof WebauthnHttpError && err.status === 404)) {
            throw err;
          }
        }
      }

      await removeDeviceAt({
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
        leafIndex,
        attestation,
      });
      // ADR-0050 section 2.10 — the device is out of the DeviceGroup; now out
      // of every chat and out of the directory. Runs on after the list reload;
      // a chat it fails on is logged and retried by nothing (the leaf stays
      // until a member removes it by hand), so the warning is worth reading.
      if (target !== undefined) {
        void receive.retireDevice(target.signaturePublicKey).catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("ADR-0050 retire device: fan-out failed", err);
        });
      }
      // Re-fetch — the new epoch has its own leaf numbering.
      await load();
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  createEffect(() => {
    void load();
  });

  return (
    <div class="lk-stack" style={{ gap: "8px" }}>
      <h2>Your devices</h2>
      <p class="lk-card-desc">
        A removed device loses access to your messages from that moment on. Removing one may
        ask for your passkey.
      </p>
      <Switch>
        <Match when={state().kind === "loading"}>
          <p class="lk-muted">Loading...</p>
        </Match>
        <Match when={state().kind === "ready" || state().kind === "removing"}>
          {(_) => {
            const s = state();
            const devices =
              s.kind === "ready" || s.kind === "removing" ? s.devices : [];
            const removingLeaf =
              s.kind === "removing" ? s.leafIndex : null;
            return (
              <ul class="lk-list tight" aria-label="Devices">
                <For each={devices}>
                  {(device, i) => (
                    <li class="lk-card-row">
                      <span class="lk-card-row-icon">
                        <Icon name={device.isSelf ? "monitor" : "devices"} size={18} />
                      </span>
                      <div class="lk-card-row-body">
                        <span class="lk-card-row-title">
                          {deviceRowLabel(i(), device.isSelf)}
                        </span>
                        <Show when={device.isSelf}>
                          <span class="lk-card-row-sub">The device you are using now.</span>
                        </Show>
                      </div>
                      <Show when={!device.isSelf} fallback={<span />}>
                        <button
                          type="button"
                          class="ghost small"
                          style={{ color: "var(--danger)" }}
                          disabled={removingLeaf !== null}
                          onClick={() => void remove(device.leafIndex)}
                        >
                          {removingLeaf === device.leafIndex ? "Removing..." : "Remove"}
                        </button>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            );
          }}
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message : "Something went wrong.";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button type="button" onClick={() => void load()}>
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

// ---- Show a code (this device joins) ----------------------------

function PairView(props: {
  readonly accountIdB64: string;
  readonly accountIdBytes: Uint8Array;
  readonly username: string;
  readonly onBack: () => void;
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
          message: "This browser is not set up for your account. Sign in again.",
        });
        return;
      }
      const sess = await prepareJoinFlow({
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
        loginHandle: await ownPairingHandle(props.username),
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
            // Phase 2a3d8-ui — pass the Welcome's DS cursor so
            // `accept` can poll past it for the scanner-side
            // history catch-up bundle.
            await sess.accept(outcome.welcome, {
              welcomeCursor: outcome.welcomeCursor,
            });
            sess.dispose();
            setState({ kind: "done" });
          } else if (outcome.kind === "timeout") {
            sess.dispose();
            setState({
              kind: "error",
              message: "The other device did not scan in time. Try again.",
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
    <div class="lk-stack" style={{ gap: "10px" }}>
      <button type="button" class="ghost small" style={{ "align-self": "flex-start" }} onClick={props.onBack}>
        <Icon name="back" size={16} />
        Add a device
      </button>
      <h2>Show a code</h2>
      <p class="lk-card-desc">
        Scan this code with a device that is already signed in. Once it confirms, this
        device joins your account.
      </p>
      <Switch>
        <Match when={state().kind === "idle"}>
          <div class="lk-actions">
            <button class="primary" type="button" onClick={() => void start()}>
              Show the code
            </button>
          </div>
        </Match>
        <Match when={state().kind === "preparing"}>
          <p class="lk-muted">Preparing...</p>
        </Match>
        <Match when={state().kind === "waiting"}>
          <div class="lk-center">
            <canvas class="lk-qr" ref={(el) => setCanvasRef(el)} />
            <p class="lk-muted">Waiting for the other device to scan this code...</p>
            <button type="button" onClick={cancel}>
              Cancel
            </button>
          </div>
        </Match>
        <Match when={state().kind === "joining"}>
          <p class="lk-muted">Joining your account...</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">Done. This device is now on your account.</p>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message : "Something went wrong.";
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

// ---- Scan a code (this device admits) ---------------------------

function ScanView(props: {
  readonly accountIdB64: string;
  readonly accountIdBytes: Uint8Array;
  readonly username: string;
  readonly onBack: () => void;
}): JSX.Element {
  // ADR-0018 factor (a): the scanner side also transfers ACCESS +
  // IDENTITY to the fresh device (grant + phrase seed, sealed over
  // the pairing channel). Needs the live IdentitySecrets.
  const auth = useAuth();
  // ADR-0050 section 2.8 — the admission fan-out over every chat.
  const receive = useBackgroundReceive();
  type State =
    | { kind: "idle" }
    | { kind: "opening" }
    | { kind: "scanning" }
    // ADR-0018 / lakko-security finding #1 — a decoded pairing QR
    // parks here until the user explicitly confirms: the scan now
    // transfers DeviceGroup membership AND the identity seed AND a
    // session grant, so it must not run on a bare camera glance.
    | { kind: "confirm"; scannedBytes: Uint8Array }
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
          message: "This browser is not set up for your account. Sign in again.",
        });
        return;
      }
      // ADR-0018 factor (a): pass the recovery half when the live
      // session has the AccountIdentity seed + a bearer token —
      // `runScanFlow` treats it strictly best-effort (a failure
      // there never breaks the pairing). The seed is passed as a
      // dedicated copy; `runScanFlow` consumes (zeroizes) it.
      const secrets = auth.identitySecrets();
      const recovery =
        secrets !== null
          ? {
              accountIdentitySeed: secrets.accountIdentitySeed.slice(),
              deviceAuthorizeStart: (): Promise<string> =>
                new AuthClient().deviceAuthorizeStart(secrets.sessionToken),
            }
          : undefined;
      const scanned = await runScanFlow({
        scannedBytes,
        // ADR-0056: this account's own handle; a code made for another
        // account's name is refused with the wrong-account copy.
        loginHandle: await ownPairingHandle(props.username),
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
        ...(recovery !== undefined ? { recovery } : {}),
        // ADR-0060 section 7: the new device starts with this account's
        // "who can find me" setting and reach key versions.
        serviceEnvelopes: await currentServiceEnvelopes(),
      });
      // ADR-0050 section 2.8 — admit the new device to every chat. Not
      // awaited: it waits for the device to land and publish its KeyPackage,
      // which is minutes, and the pairing itself is done.
      void receive.admitDevice(scanned.newDeviceKey).catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("ADR-0050 admit device: fan-out failed", err);
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
          "The camera is not available. " +
          (err instanceof Error ? err.message : String(err)),
      });
      return;
    }
    const video = videoRef();
    if (video === null) {
      stopCamera();
      setState({
        kind: "error",
        message: "The camera view could not be opened.",
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
        message: "The camera view could not be opened.",
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
          // Park for explicit confirmation (lakko-security #1)
          // rather than running the transfer on a bare decode.
          stopCamera();
          setState({ kind: "confirm", scannedBytes: bytes });
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
    <div class="lk-stack" style={{ gap: "10px" }}>
      <button type="button" class="ghost small" style={{ "align-self": "flex-start" }} onClick={props.onBack}>
        <Icon name="back" size={16} />
        Add a device
      </button>
      <h2>Scan a code</h2>
      <p class="lk-card-desc">
        Point the camera at the code your new device is showing.
      </p>
      <Switch>
        <Match when={state().kind === "idle"}>
          <div class="lk-actions">
            <button class="primary" type="button" onClick={() => void start()}>
              Open camera
            </button>
          </div>
        </Match>
        <Match when={state().kind === "opening"}>
          <p class="lk-muted">Opening the camera...</p>
        </Match>
        <Match when={state().kind === "scanning"}>
          <div class="lk-center">
            <video class="lk-camera" ref={(el) => setVideoRef(el)} playsinline muted />
            <button type="button" onClick={reset}>
              Cancel
            </button>
          </div>
        </Match>
        <Match when={state().kind === "confirm"}>
          {(_) => {
            const s = state();
            const scanned = s.kind === "confirm" ? s.scannedBytes : null;
            return (
              <div>
                <p>
                  <strong>Add this device?</strong>
                </p>
                <p>
                  It gets full access to your account: your messages, your contacts and
                  your identity. Continue only if the code is on your own new device.
                </p>
                <div class="lk-actions">
                  <button
                    class="primary"
                    type="button"
                    onClick={() => {
                      if (scanned !== null) void handleDecoded(scanned);
                    }}
                  >
                    Add device
                  </button>
                  <button type="button" onClick={reset}>
                    Cancel
                  </button>
                </div>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "processing"}>
          <p class="lk-muted">Adding the device...</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">
            Device added. It will appear in the list once it has finished joining.
          </p>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message : "Something went wrong.";
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
