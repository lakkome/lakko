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
 * The authenticator recovery factor that used to share this page lives
 * under Settings > Security > Recovery
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
import {
  listDevices,
  removeDeviceAt,
  type DeviceListItem,
} from "../devices/remove";
import { ownPairingHandle } from "../devices/pairing";
import { recoveryHalfFor } from "../devices/recovery-sender";
import QRCode from "qrcode";
import jsQR from "jsqr";
import { paths } from "./paths";
import { t } from "../l10n";
import { screenText } from "../auth/auth-error-copy";

/**
 * The text a failure shows (ADR-0061 section 6): a sentence written for a
 * screen (a keyed error, a pairing sentence) in the current language,
 * otherwise the flow's generic key. The diagnostic goes to the log. Returned
 * as a function so it renders in the language current when it is shown.
 */
function failureText(err: unknown, genericKey: string): () => string {
  console.warn("[devices] failed", err);
  return () => screenText(err) ?? t(genericKey);
}

const noDeviceKey = (): string => t("settings.devices.error.no_device_key");

/** The camera error, by the `getUserMedia` failure name. */
function cameraFailure(err: unknown): () => string {
  console.warn("[devices] camera failed", err);
  const name = err instanceof DOMException ? err.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return () => t("devices.scan.error.camera_denied_browser");
  }
  if (name === "NotFoundError" || name === "OverconstrainedError") {
    return () => t("devices.scan.error.no_camera");
  }
  return () => t("devices.scan.error.camera_failed");
}

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
          {t("settings.section.security")}
        </A>
        <div class="lk-page-head">
          <h1>{t("settings.devices.title")}</h1>
          <p>{t("settings.devices.lede")}</p>
        </div>
        <Show
          when={session() !== null}
          fallback={<p class="form-error">{t("settings.devices.sign_in_first")}</p>}
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
                {t("devices.add.title")}
              </button>
            </div>
            <Show when={!hasUsername()}>
              <p class="lk-meta" style={{ margin: 0 }}>{t("device_link.pairing.needs_username")}</p>
            </Show>
          </section>
          <Show when={add() !== "closed" && hasUsername()}>
            <section class="lk-card" aria-label={t("devices.add.title")}>
              <Switch>
                <Match when={add() === "pick"}>
                  <h2>{t("devices.add.title")}</h2>
                  <p class="lk-card-desc">{t("settings.devices.add.lede")}</p>
                  <div class="lk-device-add">
                    <button type="button" class="lk-option-card" onClick={() => setAdd("scan")}>
                      <Icon name="qr" size={22} />
                      <strong>{t("devices.add.side.scan")}</strong>
                      <span>{t("devices.add.side.scan_detail")}</span>
                    </button>
                    <button type="button" class="lk-option-card" onClick={() => setAdd("show")}>
                      <Icon name="devices" size={22} />
                      <strong>{t("devices.add.side.show")}</strong>
                      <span>{t("devices.add.side.show_detail")}</span>
                    </button>
                  </div>
                  <p class="lk-meta" style={{ margin: 0 }}>
                    {t("settings.devices.add.no_camera", {
                      method: t("recovery.method.device.title"),
                      prompt: t("device_link.request.title"),
                    })}
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
  return isSelf ? t("settings.devices.this_device") : t("settings.devices.device_n", { number: index + 1 });
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
    | { kind: "error"; message: () => string };
  const [state, setState] = createSignal<State>({ kind: "loading" });

  async function load(): Promise<void> {
    setState({ kind: "loading" });
    try {
      const deviceKey = await getDeviceKey(props.accountIdB64);
      if (deviceKey === null) {
        setState({ kind: "error", message: noDeviceKey });
        return;
      }
      const list = await listDevices({
        accountIdBytes: props.accountIdBytes,
        accountIdB64: props.accountIdB64,
        deviceKey,
      });
      setState({ kind: "ready", devices: list.devices });
    } catch (err) {
      setState({ kind: "error", message: failureText(err, "settings.devices.error.load") });
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
        setState({ kind: "error", message: noDeviceKey });
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
      setState({ kind: "error", message: failureText(err, "settings.devices.error.remove") });
    }
  }

  createEffect(() => {
    void load();
  });

  return (
    <div class="lk-stack" style={{ gap: "8px" }}>
      <h2>{t("settings.devices.list.title")}</h2>
      <p class="lk-card-desc">{t("settings.devices.list.lede")}</p>
      <Switch>
        <Match when={state().kind === "loading"}>
          <p class="lk-muted">{t("common.loading")}</p>
        </Match>
        <Match when={state().kind === "ready" || state().kind === "removing"}>
          {(_) => {
            const s = state();
            const devices =
              s.kind === "ready" || s.kind === "removing" ? s.devices : [];
            const removingLeaf =
              s.kind === "removing" ? s.leafIndex : null;
            return (
              <ul class="lk-list tight" aria-label={t("settings.devices.title")}>
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
                          <span class="lk-card-row-sub">{t("settings.devices.this_device.sub")}</span>
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
                          {removingLeaf === device.leafIndex ? t("common.removing") : t("common.remove")}
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
            const message = s.kind === "error" ? s.message() : "";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button type="button" onClick={() => void load()}>
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
    | { kind: "error"; message: () => string };
  const [state, setState] = createSignal<State>({ kind: "idle" });
  const [canvasRef, setCanvasRef] = createSignal<HTMLCanvasElement | null>(null);

  async function start(): Promise<void> {
    setState({ kind: "preparing" });
    try {
      const deviceKey = await getDeviceKey(props.accountIdB64);
      if (deviceKey === null) {
        setState({ kind: "error", message: noDeviceKey });
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
            setState({ kind: "error", message: () => t("settings.devices.show.error.timeout") });
          } else {
            // cancelled — silent (user closed the form).
            sess.dispose();
            setState({ kind: "idle" });
          }
        } catch (err) {
          sess.dispose();
          setState({
            kind: "error",
            message: failureText(err, "devices.add.error.generic"),
          });
        }
      })();
    } catch (err) {
      setState({ kind: "error", message: failureText(err, "devices.add.error.generic") });
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
        {t("devices.add.title")}
      </button>
      <h2>{t("devices.add.side.show")}</h2>
      <p class="lk-card-desc">{t("settings.devices.show.lede")}</p>
      <Switch>
        <Match when={state().kind === "idle"}>
          <div class="lk-actions">
            <button class="primary" type="button" onClick={() => void start()}>
              {t("settings.devices.show.start")}
            </button>
          </div>
        </Match>
        <Match when={state().kind === "preparing"}>
          <p class="lk-muted">{t("common.preparing")}</p>
        </Match>
        <Match when={state().kind === "waiting"}>
          <div class="lk-center">
            <canvas class="lk-qr" ref={(el) => setCanvasRef(el)} />
            <p class="lk-muted">{t("device_link.qr.waiting")}</p>
            <button type="button" onClick={cancel}>
              {t("common.cancel")}
            </button>
          </div>
        </Match>
        <Match when={state().kind === "joining"}>
          <p class="lk-muted">{t("devices.add.joining")}</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">{t("devices.add.linked")}</p>
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
    // `keysSent`: whether the new device was handed a session and the
    // recovery phrase (false when this device does not hold the phrase).
    | { kind: "done"; keysSent: boolean }
    | { kind: "error"; message: () => string };
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
        setState({ kind: "error", message: noDeviceKey });
        return;
      }
      // ADR-0018 factor (a), recovery payload v2: hand the new device a
      // session grant and the recovery phrase this device holds (as its
      // entropy). `runScanFlow` treats it strictly best-effort (a failure
      // there never breaks the pairing) and consumes the entropy. A device
      // that does not hold the phrase sends no recovery row, and the done
      // state says so.
      const secrets = auth.identitySecrets();
      const recovery =
        secrets !== null
          ? await recoveryHalfFor({
              accountIdB64: props.accountIdB64,
              deviceKey,
              deviceAuthorizeStart: (): Promise<string> =>
                new AuthClient().deviceAuthorizeStart(secrets.sessionToken),
            })
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
      });
      // ADR-0050 section 2.8 — admit the new device to every chat. Not
      // awaited: it waits for the device to land and publish its KeyPackage,
      // which is minutes, and the pairing itself is done.
      void receive.admitDevice(scanned.newDeviceKey).catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("ADR-0050 admit device: fan-out failed", err);
      });
      setState({ kind: "done", keysSent: scanned.recoveryPublished });
    } catch (err) {
      setState({ kind: "error", message: failureText(err, "settings.devices.error.scan") });
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
        message: cameraFailure(err),
      });
      return;
    }
    const video = videoRef();
    if (video === null) {
      stopCamera();
      setState({ kind: "error", message: () => t("devices.scan.error.camera_failed") });
      return;
    }
    video.srcObject = stream;
    await video.play();
    setState({ kind: "scanning" });

    const canvas = document.createElement("canvas");
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (ctx === null) {
      setState({ kind: "error", message: () => t("devices.scan.error.camera_failed") });
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
        {t("devices.add.title")}
      </button>
      <h2>{t("devices.add.side.scan")}</h2>
      <p class="lk-card-desc">{t("devices.scan.intro")}</p>
      <Switch>
        <Match when={state().kind === "idle"}>
          <div class="lk-actions">
            <button class="primary" type="button" onClick={() => void start()}>
              {t("devices.scan.open_camera")}
            </button>
          </div>
        </Match>
        <Match when={state().kind === "opening"}>
          <p class="lk-muted">{t("settings.devices.scan.opening")}</p>
        </Match>
        <Match when={state().kind === "scanning"}>
          <div class="lk-center">
            <video class="lk-camera" ref={(el) => setVideoRef(el)} playsinline muted />
            <button type="button" onClick={reset}>
              {t("common.cancel")}
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
                  <strong>{t("devices.scan.confirm.title")}</strong>
                </p>
                <p>{t("devices.scan.confirm.body")}</p>
                <div class="lk-actions">
                  <button
                    class="primary"
                    type="button"
                    onClick={() => {
                      if (scanned !== null) void handleDecoded(scanned);
                    }}
                  >
                    {t("devices.scan.confirm.submit")}
                  </button>
                  <button type="button" onClick={reset}>
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            );
          }}
        </Match>
        <Match when={state().kind === "processing"}>
          <p class="lk-muted">{t("device_link.request.adding")}</p>
        </Match>
        <Match when={state().kind === "done"}>
          <p class="form-info">{t("settings.devices.scan.done")}</p>
          <Show when={(state() as { keysSent?: boolean }).keysSent === false}>
            <p class="form-info" role="status">
              {t("device_link.keys_not_sent")}
            </p>
          </Show>
        </Match>
        <Match when={state().kind === "error"}>
          {(_) => {
            const s = state();
            const message = s.kind === "error" ? s.message() : "";
            return (
              <div>
                <p class="form-error">{message}</p>
                <button type="button" onClick={reset}>
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
