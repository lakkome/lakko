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
  on,
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
import { openUsernameClaim } from "../auth/username-claim-host";
import { newWordsShown } from "../auth/protect/account-protection";
import { phraseProtectedHere } from "../auth/protect/protection-state";
import { AuthClient } from "../auth/client";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";
import { deviceIdB64Url } from "../auth/session-device";
import { useDeviceRemoval } from "../devices/removal-runtime";
import { removalRowState } from "../devices/removal";
import { REMOVE_DEVICE_COPY, ROTATE_COPY, revokeFailureLine } from "../devices/removal-copy";
import { ModalLayer } from "../ui/ModalLayer";
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
import { listDevices, type DeviceListItem } from "../devices/remove";
import { useKeyRotation, type RotateResult } from "../devices/key-rotation-runtime";
import { rotationCommitted, rotationStepLine, type RotationStepLine,
  rotationSavesPhrase,
} from "../devices/key-rotation";
import { ownPairingHandle } from "../devices/pairing";
import { recoveryHalfFor } from "../devices/recovery-sender";
import QRCode from "qrcode";
import jsQR from "jsqr";
import { paths } from "./paths";
import { t } from "../l10n";
import { screenText } from "../auth/auth-error-copy";
import { FreshAuthRequiredError, PROOF_COPY } from "../auth/fresh-auth";
import { createProofGate } from "../auth/proof-gate";
import { ProofBlock } from "../auth/ProofBlock";
import { addedNotice, noteDeviceLetIn } from "../devices/added-notice";
import { DeviceAddedBanner } from "../devices/DeviceAddedNotice";
import { bytesToHex } from "../chat/folders/folder-model";

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
  // UX 17.5: a device carrying the `New` label was just removed here.
  const [removedNew, setRemovedNew] = createSignal(false);

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
        {/* UX 17.5: a device joined the account; the same banner as on the
            Chats list. After a `New` device is removed, the line below it
            takes its place. */}
        <DeviceAddedBanner onDevices />
        <Show when={removedNew()}>
          <div class="lk-alert accent lk-device-added" role="status">
            <div class="lk-fill">{t("settings.devices.removed_change_password")}</div>
            <A href={paths.settingsSection("security")} class="lk-button-link">
              {t("settings.password.change")}
            </A>
            <button type="button" class="small ghost" onClick={() => setRemovedNew(false)}>
              {t("common.dismiss")}
            </button>
          </div>
        </Show>
        {/* UX 15.3: after a reset on this device, the other devices are not
            devices of the account any more and the account holds no list of
            them; one dismissible note for the rest of the session. */}
        <Show when={auth.keyResetNote()}>
          <div class="lk-alert info" role="status">
            <div class="lk-fill">{t("settings.devices.reset_note")}</div>
            <button type="button" class="small" onClick={() => auth.dismissKeyResetNote()}>
              {t("common.dismiss")}
            </button>
          </div>
        </Show>
        <Show
          when={session() !== null}
          fallback={<p class="form-error">{t("settings.devices.sign_in_first")}</p>}
        >
          <section class="lk-card">
            <DevicesListView
              accountIdB64={session()!.accountIdB64}
              accountIdBytes={session()!.accountIdBytes}
              onRemovedNew={() => setRemovedNew(true)}
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
              <button type="button" onClick={openUsernameClaim}>
                {t("username.claim.title")}
              </button>
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
  /** A row that carried the `New` label was removed. */
  readonly onRemovedNew: () => void;
}): JSX.Element {
  /** UX 17.5: the row is a device the notice is about. */
  const isNew = (device: DeviceListItem): boolean =>
    !device.isSelf && addedNotice().newKeys.includes(bytesToHex(device.signaturePublicKey));
  const auth = useAuth();
  // ADR-0074 section 2.3: removing a device is one operation with an owed
  // record (`devices/removal.ts`); the sheet below starts it.
  const removal = useDeviceRemoval();
  type State =
    | { kind: "loading" }
    | { kind: "ready"; devices: readonly DeviceListItem[] }
    | { kind: "error"; message: () => string };
  const [state, setState] = createSignal<State>({ kind: "loading" });
  // The remove sheet (UX 18.1): the device it is about and its label.
  const [sheet, setSheet] = createSignal<{ device: DeviceListItem; label: string } | null>(null);
  const [working, setWorking] = createSignal(false);
  const [sheetLine, setSheetLine] = createSignal<(() => string) | null>(null);
  // The row whose owed removal is being continued from its retry button.
  const [retrying, setRetrying] = createSignal<string | null>(null);
  const [rowLine, setRowLine] = createSignal<(() => string) | null>(null);
  const [doneLine, setDoneLine] = createSignal(false);
  // UX 18.1: why the device is removed. Neither is chosen at first and the
  // main button waits for one. "stolen" removes the device AND changes the
  // account keys (ADR-0074 part 3).
  const rotation = useKeyRotation();
  const [reason, setReason] = createSignal<"unused" | "stolen" | null>(null);
  // The device was removed and the keys were not changed: `Try again` /
  // `Not now`, in the sheet (or, after a reload, in the sheet opened from
  // the list's line).
  const [notStarted, setNotStarted] = createSignal(false);
  // The sheet opened from the list's line for a rotation left unfinished.
  const [retryOpen, setRetryOpen] = createSignal(false);
  const [rotateDone, setRotateDone] = createSignal(false);
  /** A rotation whose device was removed but whose keys were not changed. */
  const rotationLeftover = (): boolean => {
    const r = rotation.record();
    // "Save a recovery phrase" removes no device: its record is not one.
    return r !== null && !rotationSavesPhrase(r) && !rotationCommitted(r) && r.done.includes("remove");
  };
  const activeLine = (): RotationStepLine | null => {
    const step = rotation.activeStep();
    return step === null ? null : rotationStepLine(step);
  };

  const username = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : "";
  };
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.removeDevice,
    busy: working,
    sessionToken: () =>
      sheet() !== null || retryOpen() ? (auth.identitySecrets()?.sessionToken ?? null) : null,
    usernameKnown: () => username().trim().length > 0,
  });

  /** The owed record of a row, by the device's id. */
  const recordOf = (device: DeviceListItem) => {
    const id = deviceIdB64Url(device.signaturePublicKey);
    return removal.owed().find((r) => r.deviceIdB64 === id) ?? null;
  };
  const rowState = (device: DeviceListItem) => removalRowState(recordOf(device));
  /** Signed out at the server; the rest is still owed. */
  const finishing = (device: DeviceListItem): boolean => rowState(device) !== "normal";
  /** The owed step waits on the person's passkey (`Finish removing`). */
  const needsPerson = (device: DeviceListItem): boolean => rowState(device) === "needs-person";

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

  /**
   * G-13 (ADR-0014 / ADR-0015): the passkey attestation bound to the removed
   * leaf, carried with the DeviceGroup Remove commit so peers can verify it.
   * `null` when the account has no passkey (a 404 from the ceremony start;
   * the DeviceGroup policy is off there). A cancelled gesture or any other
   * failure throws, and the caller removes nothing (fail-closed).
   */
  async function removalAttestation(target: DeviceListItem): Promise<Uint8Array | null> {
    const sessionToken = auth.identitySecrets()?.sessionToken ?? null;
    if (sessionToken === null) return null;
    const wc = new WebauthnClient();
    wc.setSessionToken(sessionToken);
    try {
      const actionHashB64 = await actionHashForRemoveDevice(target.signaturePublicKey);
      const result = await wc.assertAction(actionHashB64);
      return base64UrlDecodeNoPad(result.attestation_b64);
    } catch (err) {
      if (err instanceof WebauthnHttpError && err.status === 404) return null;
      throw err;
    }
  }

  function openSheet(device: DeviceListItem, label: string): void {
    gate.clear();
    setSheetLine(null);
    setRowLine(null);
    setDoneLine(false);
    setRotateDone(false);
    setReason(null);
    setNotStarted(false);
    closeWhenDone = false;
    setSheet({ device, label });
  }

  /** The list's line for a rotation left unfinished: the same sheet, with
   *  the proof block and `Try again` / `Not now`. */
  function openRetrySheet(): void {
    gate.clear();
    setSheetLine(null);
    setReason("stolen");
    setNotStarted(true);
    closeWhenDone = false;
    setRetryOpen(true);
  }

  // `Esc` closes the dialog element even while the removal is running; the
  // state then follows it once the run has its answer (the row shows it).
  let closeWhenDone = false;
  function closeSheet(): void {
    if (working()) {
      closeWhenDone = true;
      return;
    }
    gate.clear();
    setSheetLine(null);
    setSheet(null);
    setRetryOpen(false);
    setNotStarted(false);
  }

  /** What a run of the rotation came to, for the sheet (UX 18.1's table). */
  async function showRotation(result: RotateResult, grant: string | null): Promise<void> {
    if (result.kind === "blocked") {
      setSheetLine(() => REMOVE_DEVICE_COPY.blocked);
      return;
    }
    if (result.kind === "not-ready") {
      setSheetLine(() => () => t("common.error.session_not_ready"));
      return;
    }
    const outcome = result.outcome;
    if (outcome.kind === "not-removed") {
      // Nothing was removed: the removal's own states.
      if (outcome.failure === "proof-required" || outcome.failure === "rate-limited") {
        const failure = gate.failed(result.error ?? new FreshAuthRequiredError(), grant);
        if (failure.kind === "other") setSheetLine(() => REMOVE_DEVICE_COPY.notRevoked);
        return;
      }
      setSheetLine(() => revokeFailureLine(outcome.failure));
      return;
    }
    if (outcome.kind === "not-started") {
      // The device WAS removed; the keys are unchanged.
      setNotStarted(true);
      if (outcome.reason === "proof-required" || outcome.reason === "rate-limited") {
        // The block words these: the proof again, or the wait.
        gate.failed(result.error ?? new FreshAuthRequiredError(), grant);
      }
      setSheetLine(() =>
        outcome.reason === "account-key-required" ? REMOVE_DEVICE_COPY.tryLater : ROTATE_COPY.notStarted,
      );
      await load();
      return;
    }
    // The keys were changed. Publishing still owed: the words are withheld
    // with the existing "publish owed" copy and retried by itself. Complete:
    // the new-phrase screen is up, then this line.
    setWorking(false);
    gate.clear();
    setSheet(null);
    setRetryOpen(false);
    setNotStarted(false);
    if (outcome.kind === "complete") setRotateDone(true);
    await load();
  }

  /** `Remove and change keys`, and `Try again`. */
  async function onConfirmRotate(): Promise<void> {
    if (working()) return;
    const open = sheet();
    setSheetLine(null);
    setWorking(true);
    try {
      const grant = await gate.ensure();
      if (grant === null) return;
      if (open === null || notStarted()) {
        // The record names the device; its removal already has its answer.
        await showRotation(await rotation.retry(grant), grant);
        return;
      }
      const target = open.device;
      const targetWasNew = isNew(target);
      let attestation: Uint8Array | null;
      try {
        attestation = await removalAttestation(target);
      } catch (err) {
        setSheetLine(() => failureText(err, "settings.devices.error.remove"));
        return;
      }
      const result = await rotation.rotate(target.signaturePublicKey, grant, attestation);
      if (result.kind === "ran" && result.outcome.kind !== "not-removed" && targetWasNew) {
        props.onRemovedNew();
      }
      await showRotation(result, grant);
    } catch (err) {
      setSheetLine(() => failureText(err, "settings.devices.error.remove"));
    } finally {
      setWorking(false);
      if (closeWhenDone) {
        closeWhenDone = false;
        closeSheet();
      }
    }
  }

  /** `Not now`: the device stays removed, the keys stay as they are. */
  async function onNotNow(): Promise<void> {
    if (working()) return;
    await rotation.notNow();
    closeSheet();
    await load();
  }

  /** The sheet's main button. Nothing is removed before the proof passes;
   *  nothing else happens when the sign-out at the server is refused. */
  async function onConfirmRemove(): Promise<void> {
    if (reason() === "stolen") {
      await onConfirmRotate();
      return;
    }
    const open = sheet();
    if (open === null || working() || reason() === null) return;
    const target = open.device;
    setSheetLine(null);
    setWorking(true);
    try {
      if (removal.blocked()) {
        setSheetLine(() => REMOVE_DEVICE_COPY.blocked);
        return;
      }
      const grant = await gate.ensure();
      if (grant === null) return;
      const targetWasNew = isNew(target);
      let attestation: Uint8Array | null;
      try {
        attestation = await removalAttestation(target);
      } catch (err) {
        // The gesture was dismissed or failed: the device was not removed.
        setSheetLine(() => failureText(err, "settings.devices.error.remove"));
        return;
      }
      const result = await removal.remove(target.signaturePublicKey, grant, attestation);
      if (result.kind === "blocked") {
        setSheetLine(() => REMOVE_DEVICE_COPY.blocked);
        return;
      }
      if (result.kind === "not-ready") {
        setSheetLine(() => () => t("common.error.session_not_ready"));
        return;
      }
      const outcome = result.outcome;
      if (outcome.kind === "not-revoked") {
        if (outcome.failure === "proof-required" || outcome.failure === "rate-limited") {
          // The block words these: the proof again (no error), or the wait.
          const failure = gate.failed(result.revokeError ?? new FreshAuthRequiredError(), grant);
          if (failure.kind === "other") setSheetLine(() => REMOVE_DEVICE_COPY.notRevoked);
          return;
        }
        setSheetLine(() => revokeFailureLine(outcome.failure));
        return;
      }
      // Signed out at the server: the sheet closes. A removal still
      // finishing shows on the row; nothing is an error from here.
      setWorking(false);
      gate.clear();
      setSheet(null);
      if (outcome.kind === "complete") setDoneLine(true);
      if (targetWasNew) props.onRemovedNew();
      await load();
    } catch (err) {
      setSheetLine(() => failureText(err, "settings.devices.error.remove"));
    } finally {
      setWorking(false);
      if (closeWhenDone) {
        closeWhenDone = false;
        closeSheet();
      }
    }
  }

  /** `Finish removing`: a removal that was signed out at the server but
   *  needs the passkey gesture again to leave the device list (after a
   *  relaunch, or once the first attestation aged out). */
  async function onRetryRow(device: DeviceListItem): Promise<void> {
    const id = deviceIdB64Url(device.signaturePublicKey);
    if (retrying() !== null) return;
    setRetrying(id);
    setRowLine(null);
    try {
      // The passkey again, then the owed steps from the record.
      const attestation = await removalAttestation(device);
      const result = await removal.remove(device.signaturePublicKey, null, attestation);
      if (result.kind === "ran" && result.outcome.kind === "complete") setDoneLine(true);
      await load();
    } catch (err) {
      setRowLine(() => failureText(err, "settings.devices.error.remove"));
    } finally {
      setRetrying(null);
    }
  }

  createEffect(() => {
    void load();
  });
  // A record that finished elsewhere (the whole-session runtime) removes a
  // row: read the list again when the set of records changes.
  createEffect(
    on(
      () => removal.owed().length,
      () => void load(),
      { defer: true },
    ),
  );

  return (
    <div class="lk-stack" style={{ gap: "8px" }}>
      <h2>{t("settings.devices.list.title")}</h2>
      <p class="lk-card-desc">{t("settings.devices.list.lede")}</p>
      {/* ADR-0074 T1 point 15: while a key-reset publish is owed, auth and the
          directory hold different account keys; no removal runs then. */}
      <Show when={removal.blocked()}>
        <p class="form-info" role="status">{REMOVE_DEVICE_COPY.blocked()}</p>
      </Show>
      <Show when={doneLine()}>
        <p class="form-info" role="status">{REMOVE_DEVICE_COPY.done()}</p>
      </Show>
      <Show when={rotateDone()}>
        <p class="form-info" role="status">{ROTATE_COPY.done()}</p>
      </Show>
      {/* UX 18.1: the device was removed, the keys were not changed (a
          rotation left unfinished, also after a relaunch). */}
      <Show when={rotationLeftover() && sheet() === null && !retryOpen()}>
        <div class="form-error" role="alert">
          <p>{ROTATE_COPY.notStarted()}</p>
          <div class="lk-actions">
            <button type="button" class="small ghost" onClick={() => void onNotNow()}>
              {ROTATE_COPY.notNow()}
            </button>
            <button type="button" class="small" onClick={openRetrySheet}>
              {ROTATE_COPY.retry()}
            </button>
          </div>
        </div>
      </Show>
      <Show when={rowLine()}>
        {(line) => (
          <p class="form-error" role="alert">
            {line()()}
          </p>
        )}
      </Show>
      <Switch>
        <Match when={state().kind === "loading"}>
          <p class="lk-muted">{t("common.loading")}</p>
        </Match>
        <Match when={state().kind === "ready"}>
          {(_) => {
            const devices = (): readonly DeviceListItem[] => {
              const s = state();
              return s.kind === "ready" ? s.devices : [];
            };
            return (
              <ul class="lk-list tight" aria-label={t("settings.devices.title")}>
                <For each={devices()}>
                  {(device, i) => (
                    <li
                      class="lk-card-row"
                      ref={(el) => {
                        // The new device's row is scrolled into view.
                        if (isNew(device)) queueMicrotask(() => el.scrollIntoView({ block: "nearest" }));
                      }}
                    >
                      <span class="lk-card-row-icon">
                        <Icon name={device.isSelf ? "monitor" : "devices"} size={18} />
                      </span>
                      <div class="lk-card-row-body">
                        <span class="lk-card-row-title">
                          {deviceRowLabel(i(), device.isSelf)}
                          <Show when={isNew(device)}>
                            {" "}
                            <span class="lk-badge">{t("settings.devices.new_badge")}</span>
                          </Show>
                        </span>
                        <Show when={device.isSelf}>
                          <span class="lk-card-row-sub">{t("settings.devices.this_device.sub")}</span>
                        </Show>
                        {/* UX 18.1: signed out, the removal still finishing.
                            No Remove control and no error. */}
                        <Show when={finishing(device)}>
                          <span class="lk-card-row-sub" role="status">
                            {REMOVE_DEVICE_COPY.pending()}
                          </span>
                        </Show>
                      </div>
                      <Show when={!device.isSelf} fallback={<span />}>
                        <Show
                          when={!finishing(device)}
                          fallback={
                            // Only when the owed step is waiting on the
                            // person: the passkey again, then the rest.
                            <Show when={needsPerson(device)} fallback={<span />}>
                              <button
                                type="button"
                                class="ghost small"
                                disabled={retrying() !== null || removal.blocked()}
                                onClick={() => void onRetryRow(device)}
                              >
                                {REMOVE_DEVICE_COPY.finish()}
                              </button>
                            </Show>
                          }
                        >
                          <button
                            type="button"
                            class="ghost small"
                            style={{ color: "var(--danger)" }}
                            disabled={removal.blocked()}
                            onClick={() => openSheet(device, deviceRowLabel(i(), false))}
                          >
                            {t("common.remove")}
                          </button>
                        </Show>
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
      {/* UX 18.1: the app's own sheet, never a system dialog. The proof
          block sits in it; the main button is the only way forward. */}
      <ModalLayer
        open={sheet() !== null || retryOpen()}
        title={reason() === "stolen" ? ROTATE_COPY.title() : REMOVE_DEVICE_COPY.title(sheet()?.label ?? "")}
        onClose={closeSheet}
      >
        <div class="lk-stack" style={{ gap: "12px" }}>
          <Show when={reason() !== "stolen"}>
            <p>{REMOVE_DEVICE_COPY.body()}</p>
          </Show>
          {/* The reason pair: one group, neither chosen at first. Hidden once
              the device is removed (the rotation is being retried). */}
          <Show when={!notStarted() && !(working() && reason() === "stolen")}>
            <div role="radiogroup" aria-label={REMOVE_DEVICE_COPY.title(sheet()?.label ?? "")} class="lk-stack" style={{ gap: "6px" }}>
              <label class="lk-row" style={{ gap: "8px" }}>
                <input
                  type="radio"
                  name="lk-remove-reason"
                  checked={reason() === "unused"}
                  disabled={working()}
                  onChange={() => setReason("unused")}
                />
                <span>{ROTATE_COPY.reasonUnused()}</span>
              </label>
              <label class="lk-row" style={{ gap: "8px" }}>
                <input
                  type="radio"
                  name="lk-remove-reason"
                  checked={reason() === "stolen"}
                  disabled={working()}
                  onChange={() => setReason("stolen")}
                />
                <span>{ROTATE_COPY.reasonStolen()}</span>
              </label>
            </div>
          </Show>
          <Show when={reason() === "stolen"}>
            <Show
              when={working()}
              fallback={
                <Show when={!notStarted()}>
                  <p>{ROTATE_COPY.lede()}</p>
                  <ul class="lk-list tight">
                    <For
                      each={ROTATE_COPY.effects(
                        newWordsShown(phraseProtectedHere(props.accountIdB64), username().trim().length > 0),
                      )}
                    >{(line) => <li>{line}</li>}</For>
                  </ul>
                </Show>
              }
            >
              {/* One line active at a time, no percentage; each is announced
                  once when it becomes active. */}
              <ol class="lk-list tight" aria-live="polite">
                <li aria-current={activeLine() === "remove" ? "step" : undefined} classList={{ "lk-muted": activeLine() !== "remove" }}>
                  {ROTATE_COPY.stepRemove()}
                </li>
                <li aria-current={activeLine() === "keys" ? "step" : undefined} classList={{ "lk-muted": activeLine() !== "keys" }}>
                  {ROTATE_COPY.stepKeys()}
                </li>
                <li aria-current={activeLine() === "devices" ? "step" : undefined} classList={{ "lk-muted": activeLine() !== "devices" }}>
                  {ROTATE_COPY.stepDevices()}
                </li>
              </ol>
            </Show>
          </Show>
          <ProofBlock gate={gate} />
          <Show when={sheetLine()}>
            {(line) => (
              <div class="form-error" role="alert">
                {line()()}
              </div>
            )}
          </Show>
          <div class="lk-actions">
            <Show
              when={notStarted()}
              fallback={
                // Cancel is disabled from the first step.
                <button type="button" disabled={working()} onClick={closeSheet}>
                  {REMOVE_DEVICE_COPY.cancel()}
                </button>
              }
            >
              <button type="button" disabled={working()} onClick={() => void onNotNow()}>
                {ROTATE_COPY.notNow()}
              </button>
            </Show>
            <button
              type="button"
              class="primary danger"
              disabled={working() || reason() === null || !gate.view().ready}
              onClick={() => void onConfirmRemove()}
            >
              {notStarted()
                ? ROTATE_COPY.retry()
                : reason() === "stolen"
                  ? ROTATE_COPY.confirm()
                  : working()
                    ? REMOVE_DEVICE_COPY.working()
                    : REMOVE_DEVICE_COPY.confirm()}
            </button>
          </div>
        </div>
      </ModalLayer>
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
  const auth = useAuth();
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
        onOwnPredecessorsStored: () => auth.rebindAfterStateBundle(props.accountIdB64),
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
  // UX 17.4: `Link device` needs the password or a passkey proven now. The
  // block sits in the confirm step; a failed proof leaves the step open
  // with the scanned code kept.
  const [proving, setProving] = createSignal(false);
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.addDeviceScan,
    passkeyFirst: true,
    busy: proving,
    sessionToken: () =>
      state().kind === "confirm" ? (auth.identitySecrets()?.sessionToken ?? null) : null,
    usernameKnown: () => props.username.trim().length > 0,
  });
  async function onLink(scannedBytes: Uint8Array): Promise<void> {
    setProving(true);
    try {
      // The proof first; nothing of the hand-over runs before it passes.
      if ((await gate.ensure()) === null) return;
    } finally {
      setProving(false);
    }
    await handleDecoded(scannedBytes);
  }
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
              deviceAuthorizeStart: (): Promise<string> => {
                const grant = auth.freshProof();
                if (grant === null) throw new FreshAuthRequiredError();
                return new AuthClient().deviceAuthorizeStart(secrets.sessionToken, grant);
              },
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
      // UX 17.5: this device let the new one in; no notice about it here.
      noteDeviceLetIn(props.accountIdB64, scanned.newDeviceKey);
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
    gate.clear();
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
                <ProofBlock gate={gate} />
                <div class="lk-actions">
                  <button
                    class="primary"
                    type="button"
                    disabled={!gate.view().ready}
                    onClick={() => {
                      if (scanned !== null) void onLink(scanned);
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
