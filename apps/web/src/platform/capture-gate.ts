// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop's refusal screen for the microphone and the camera
 * (`docs/ux/desktop.md` section 4.2).
 *
 * In a browser a refused capture is the site's permission, and the call goes
 * on with what it has (`calls/capture-fallback.ts`). On a desktop the refusal
 * is the operating system's, so a call would otherwise start silent while the
 * person has no idea which switch to look for. Before a call starts the gate
 * asks for each device once; a refusal shows the screen, and the person fixes
 * it in the system settings and tries again, or goes on without that device.
 *
 * Only `not-allowed` stops here. A missing device or a device in use is the
 * capture fallback's to report.
 */
import { captureFailureReason } from "../calls/capture-fallback";
import { t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { hostCanOpenPrivacy, openHostPrivacy, type PrivacyPage } from "./host";
import { isDesktop } from "./index";

/** The person's answer on the refusal screen. */
export type RefusalAnswer = "retry" | "without";

/** Which devices the call may go on to capture. */
export interface CapturePermits {
  readonly microphone: boolean;
  readonly camera: boolean;
}

/** One device through the gate: asks until it is allowed or given up. */
async function passDevice(
  device: PrivacyPage,
  probe: (device: PrivacyPage) => Promise<void>,
  ask: (device: PrivacyPage) => Promise<RefusalAnswer>,
): Promise<boolean> {
  for (;;) {
    try {
      await probe(device);
      return true;
    } catch (err) {
      if (captureFailureReason(err) !== "not-allowed") return true;
    }
    if ((await ask(device)) === "without") return false;
  }
}

/** The rule, with the capture and the screen passed in. */
export async function runCaptureGate(
  wanted: { readonly audio: boolean; readonly video: boolean },
  probe: (device: PrivacyPage) => Promise<void>,
  ask: (device: PrivacyPage) => Promise<RefusalAnswer>,
): Promise<CapturePermits> {
  const microphone = wanted.audio ? await passDevice("microphone", probe, ask) : false;
  const camera = wanted.video ? await passDevice("camera", probe, ask) : false;
  return { microphone, camera };
}

async function probeDevice(device: PrivacyPage): Promise<void> {
  const stream = await navigator.mediaDevices.getUserMedia(
    device === "microphone" ? { audio: true } : { video: true },
  );
  for (const track of stream.getTracks()) track.stop();
}

async function askRefused(device: PrivacyPage): Promise<RefusalAnswer> {
  const canOpen = await hostCanOpenPrivacy();
  const retry = await requestConfirm({
    title: t(device === "microphone" ? "desktop.capture.microphone.title" : "desktop.capture.camera.title"),
    body: t("desktop.capture.body"),
    confirmLabel: t("common.try_again"),
    cancelLabel: t("common.cancel"),
    tone: "neutral",
    ...(canOpen
      ? {
          extra: {
            label: t("desktop.capture.open_settings"),
            onPress: (): void => void openHostPrivacy(device),
          },
        }
      : {}),
  });
  return retry ? "retry" : "without";
}

/**
 * Ask for what a call wants. Outside the desktop app everything passes: the
 * browser's own fallback stays the rule there.
 */
export function captureGate(wanted: {
  readonly audio: boolean;
  readonly video: boolean;
}): Promise<CapturePermits> {
  if (!isDesktop()) return Promise.resolve({ microphone: wanted.audio, camera: wanted.video });
  return runCaptureGate(wanted, probeDevice, askRefused);
}
