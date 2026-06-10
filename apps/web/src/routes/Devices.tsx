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
 * Plus the account-management "Authenticator" section (ADR-0018
 * factor (c)): enroll a TOTP authenticator app as an
 * ACCESS-recovery factor — `/totp/enroll/start` renders the
 * `otpauth://` QR + manual-entry secret, `/totp/enroll/finish`
 * confirms with a live 6-digit code.
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
import { AuthClient, AuthHttpError } from "../auth/client";
import { normalizeTotpCode } from "../auth/totp-code";
import { isPlausibleContact } from "../auth/contact-shape";
import { ContactKind } from "../auth/wire";
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
import {
  listDevices,
  removeDeviceAt,
  type DeviceListItem,
} from "../devices/remove";
import QRCode from "qrcode";
import jsQR from "jsqr";

type Tab = "list" | "pair" | "scan" | "totp" | "contact";

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
          <button
            type="button"
            class={tab() === "totp" ? "primary" : ""}
            onClick={() => setTab("totp")}
          >
            Authenticator
          </button>
          <button
            type="button"
            class={tab() === "contact" ? "primary" : ""}
            onClick={() => setTab("contact")}
          >
            Email or phone
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
          <Match when={tab() === "totp"}>
            <TotpEnrollView />
          </Match>
          <Match when={tab() === "contact"}>
            <ContactEnrollView />
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
  // Same auth context the outer page reads — used to bind the device-
  // removal action to a fresh passkey gesture (G-13).
  const auth = useAuth();
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
  // ADR-0018 factor (a): the scanner side also transfers ACCESS +
  // IDENTITY to the fresh device (grant + phrase seed, sealed over
  // the pairing channel). Needs the live IdentitySecrets.
  const auth = useAuth();
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
          message: "No device key on this browser. Sign in again.",
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
      await runScanFlow({
        scannedBytes,
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
        ...(recovery !== undefined ? { recovery } : {}),
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
        <Match when={state().kind === "confirm"}>
          {(_) => {
            const s = state();
            const scanned = s.kind === "confirm" ? s.scannedBytes : null;
            return (
              <div>
                <p>
                  <strong>Link this device?</strong>
                </p>
                <p>
                  This gives the scanned device FULL access to your
                  account: your messages, your contacts, and your
                  secure identity. Only continue if the QR code is on
                  YOUR OWN new device.
                </p>
                <button
                  class="primary"
                  type="button"
                  onClick={() => {
                    if (scanned !== null) void handleDecoded(scanned);
                  }}
                >
                  Link device
                </button>{" "}
                <button type="button" onClick={reset}>
                  Cancel
                </button>
              </div>
            );
          }}
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

// ---- Authenticator (TOTP) enrollment — ADR-0018 factor (c) ------

function TotpEnrollView(): JSX.Element {
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
        message: "No live session token in this tab. Sign in again.",
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
        message: "No live session token in this tab. Sign in again.",
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
          "That code didn't verify. Wait for the next code in the app " +
            "and try again.",
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
      <p>
        Enroll an authenticator app (TOTP) as a recovery factor. If
        you ever lose this device, a 6-digit code from the app
        restores <em>access</em> to your account — your secure
        identity is then restored from your recovery phrase or
        another signed-in device.
      </p>
      <Switch>
        <Match when={state().kind === "idle"}>
          {/* HONESTY NOTE: the server has no TOTP "status" endpoint,
              so the enrolled/not-enrolled state cannot be probed
              up front. We render the enable button by default; a
              409 from /totp/enroll/start flips this card to
              "already enabled". */}
          <button class="primary" type="button" onClick={() => void start()}>
            Enable authenticator
          </button>
        </Match>
        <Match when={state().kind === "starting"}>
          <p>Generating the authenticator secret…</p>
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
                  Scan this QR with your authenticator app (or type
                  the secret in manually), then enter the 6-digit
                  code it shows to confirm.
                </p>
                <div style={{ "text-align": "center", padding: "0.5rem 0" }}>
                  <canvas ref={(el) => setCanvasRef(el)} />
                </div>
                <p>
                  Manual-entry secret:{" "}
                  <code
                    style={{
                      "user-select": "all",
                      "word-break": "break-all",
                    }}
                  >
                    {secretB32}
                  </code>
                </p>
                <form onSubmit={(e) => void finish(e)}>
                  <label>
                    Confirmation code
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
                    {busy ? "Confirming…" : "Confirm"}
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
            Authenticator enabled. A code from the app can now recover
            access to your account (Recover → Authenticator).
          </p>
        </Match>
        <Match when={state().kind === "already-enabled"}>
          <p class="form-info">
            An authenticator is already enabled for this account.
            Replacing a confirmed authenticator from a signed-in
            session is refused by design — a stolen session must not
            be able to swap your recovery factor.
          </p>
        </Match>
        <Match when={state().kind === "unavailable"}>
          <p class="form-error">
            Authenticator enrollment is not available on this server.
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

function ContactEnrollView(): JSX.Element {
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
    // Light client-side shape check — saves a PoW solve on an
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
        message: "No live session token in this tab. Sign in again.",
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
        setRequestError("That contact looks malformed. Check it and retry.");
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
        message: "No live session token in this tab. Sign in again.",
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
          "That code didn't verify. Check the code we sent, or re-send.",
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
      <p>
        Enroll a phone number or email as a recovery factor. If you ever
        lose this device, a one-time code sent there restores{" "}
        <em>access</em> to your account — your secure identity is then
        restored from your recovery phrase or another signed-in device.
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
                  {busy ? "Sending…" : "Send code"}
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
                  We sent a 6-digit code to{" "}
                  <code style={{ "word-break": "break-all" }}>{to}</code> (
                  {k === ContactKind.Email ? "email" : "SMS"}). Enter it below
                  to confirm.
                </p>
                <form onSubmit={(e) => void finish(e)}>
                  <label>
                    Confirmation code
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
                    {busy ? "Confirming…" : "Confirm"}
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
            Contact enrolled. A code sent there can now recover access to
            your account (Recover → Email or SMS code).
          </p>
        </Match>
        <Match when={state().kind === "already-enabled"}>
          <p class="form-info">
            A confirmed contact is already enabled for this account.
            Replacing it from a signed-in session is refused by design — a
            stolen session must not be able to swap your recovery factor.
          </p>
        </Match>
        <Match when={state().kind === "unavailable"}>
          <p class="form-error">
            Email / phone recovery is not available on this server.
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
