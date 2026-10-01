// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "New sign-in request" — the signed-in device's side of the camera-less
 * another-device route (ADR-0049 section 2 as amended 2026-09-28; UX
 * `docs/ux/identity-recovery.md` section 4).
 *
 * While a session is open this polls `GET /device/requests` and drives the
 * device link (`device-link.ts`). It first asks "Is this you?" and sends
 * nothing (the second security pass, 2026-09-28); only after "Yes, it's me"
 * does this device choose a six-digit code and answer the request, and it
 * shows the code once the answer landed; the person types it on the new
 * device. Closing the question without choosing leaves the request
 * unanswered to expire. When the new device's sealed payload
 * arrives and its confirmation verifies, the unchanged add-device flow runs
 * (`runScanFlow` over the opened payload: DeviceGroup add, Welcome, history
 * bundle, the ADR-0018 access grant and the sealed identity seed) and the
 * ADR-0050 fan-out admits the device to every chat. A wrong code pairs
 * nothing. "It is not me" deletes the request.
 */
import { authLimitMessage, pairingText } from "../auth/auth-error-copy";
import { t } from "../l10n";
import { KeyedError, keyedErrorText } from "../l10n/keyed-error";
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { useAuth } from "../auth/AuthContext";
import { AuthClient } from "../auth/client";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";
import { getDeviceKey } from "../auth/key-store";
import { base64Decode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import { DEVICE_REQUEST_POLL_MS } from "./device-request";
import {
  createDeviceLinkResponder,
  deviceLinkCoreOf,
  localStorageAnswerLog,
  type DeviceLinkResponderFlow,
  type ResponderView,
} from "./device-link";
import { runScanFlow } from "./orchestrator";
import { ownPairingHandle, parsePairingQrPayload } from "./pairing";

/** The toast for a failed payload check or pairing. ADR-0061: the detail
 *  goes to the log, never to the screen. */
function failureMessage(err: unknown): string {
  // eslint-disable-next-line no-console
  console.error("device-request: confirm failed", err);
  // ADR-0054 S4: a server limit says when to try again, never "failed".
  const limit = authLimitMessage(err);
  const own = keyedErrorText(err) ?? (err instanceof Error ? pairingText(err.message) : null);
  return limit ?? own ?? t("device_link.request.error.generic");
}

export function DeviceRequestPrompt(): JSX.Element {
  const auth = useAuth();
  // ADR-0050 section 2.8 — the confirmed device is admitted to every chat.
  const receive = useBackgroundReceive();
  const [view, setView] = createSignal<ResponderView>({ kind: "idle" });
  const client = new AuthClient();
  // One flow per signed-in account; the responder it holds lives only in
  // this page's memory (a reload loses the code, and such a request can
  // only expire or be declined).
  let flow: { accountId: string; flow: DeviceLinkResponderFlow } | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;

  async function flowFor(accountId: string): Promise<DeviceLinkResponderFlow> {
    if (flow !== null && flow.accountId === accountId) return flow.flow;
    flow?.flow.dispose();
    const core = deviceLinkCoreOf(await loadWasm());
    const token = (): string => {
      const secrets = auth.identitySecrets();
      if (secrets === null) throw new Error("signed out");
      return secrets.sessionToken;
    };
    const created = createDeviceLinkResponder({
      core,
      list: () => client.deviceRequestsList(token()),
      answer: (id, pakeB) => client.deviceRequestAnswer(token(), id, pakeB),
      result: (id, r) => client.deviceRequestResult(token(), id, r),
      decline: (id) => client.deviceRequestDelete(token(), id),
      ownLoginHandle: async () => {
        const s = auth.state();
        if (s.kind !== "session") throw new Error("signed out");
        return ownPairingHandle(s.account.username);
      },
      checkPayload: async (payload, handle) => {
        await parsePairingQrPayload(payload, handle);
      },
      pair: (payload) => pair(payload),
      failureMessage,
      answers: localStorageAnswerLog(accountId),
      now: () => Date.now(),
      onView: setView,
    });
    flow = { accountId, flow: created };
    return created;
  }

  /** The unchanged add-device flow over the opened payload. */
  async function pair(payload: Uint8Array): Promise<void> {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) throw new Error("signed out");
    const deviceKey = await getDeviceKey(s.account.accountId);
    if (deviceKey === null) {
      throw new KeyedError("device_link.request.error.no_device_key");
    }
    const scanned = await runScanFlow({
      scannedBytes: payload,
      // ADR-0056: this account's own login handle; the device link already
      // bound the exchange to it, and the codec checks it again here.
      loginHandle: await ownPairingHandle(s.account.username),
      accountIdBytes: base64Decode(s.account.accountId),
      accountIdB64: s.account.accountId,
      deviceKey,
      recovery: {
        accountIdentitySeed: secrets.accountIdentitySeed.slice(),
        deviceAuthorizeStart: (): Promise<string> =>
          client.deviceAuthorizeStart(secrets.sessionToken),
      },
    });
    // ADR-0050 section 2.8 — not awaited: it waits for the device to land
    // and publish its KeyPackage, which is minutes.
    void receive.admitDevice(scanned.newDeviceKey).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("ADR-0050 admit device: fan-out failed", err);
    });
  }

  async function poll(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session" || auth.identitySecrets() === null) return;
    try {
      await (await flowFor(s.account.accountId)).poll();
    } catch {
      // Best-effort; the next tick retries.
    }
  }

  onMount(() => {
    timer = setInterval(() => void poll(), DEVICE_REQUEST_POLL_MS);
    void poll();
    const onVisible = (): void => void poll();
    document.addEventListener("visibilitychange", onVisible);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("visibilitychange", onVisible);
      document.removeEventListener("keydown", onKey);
    });
  });
  onCleanup(() => {
    if (timer !== null) clearInterval(timer);
    flow?.flow.dispose();
  });

  const modal = (): ResponderView | null => {
    const v = view();
    return v.kind === "asking" ||
      v.kind === "answering" ||
      v.kind === "showing" ||
      v.kind === "adding" ||
      v.kind === "limited"
      ? v
      : null;
  };

  /** Closing the "Is this you?" step without choosing sends nothing. */
  function onBackdrop(e: MouseEvent): void {
    if (e.target !== e.currentTarget) return;
    if (view().kind === "asking") flow?.flow.dismiss();
  }
  function onKey(e: KeyboardEvent): void {
    if (e.key === "Escape" && view().kind === "asking") flow?.flow.dismiss();
  }
  const toast = (): string | null => {
    const v = view();
    return v.kind === "closed" ? v.message : null;
  };

  return (
    <>
      <Show when={modal()}>
        {(v) => (
          <div class="lk-modal-backdrop" role="presentation" onClick={onBackdrop}>
            <div class="lk-modal" role="dialog" aria-modal="true" aria-labelledby="lk-devreq-title">
              <h2 id="lk-devreq-title">New sign-in request</h2>
              <Show
                when={v().kind !== "asking" && v().kind !== "answering"}
                fallback={
                  <>
                    <p>Someone is signing in to your account on a new device.</p>
                    <p>{t("device_link.request.title")}</p>
                    <div class="lk-actions">
                      <button
                        class="primary"
                        type="button"
                        disabled={v().kind === "answering"}
                        onClick={() => void flow?.flow.consent()}
                      >
                        Yes, it's me
                      </button>
                      <button type="button" onClick={() => void flow?.flow.decline()}>
                        {t("device_link.request.decline")}
                      </button>
                    </div>
                  </>
                }
              >
              <Show
                when={v().kind !== "limited"}
                fallback={
                  <>
                    <p>{(v() as { message: string }).message}</p>
                    <div class="lk-actions">
                      <button class="primary" type="button" onClick={() => flow?.flow.dismiss()}>
                        {t("common.close")}
                      </button>
                    </div>
                  </>
                }
              >
                <Show
                  when={v().kind === "showing"}
                  fallback={<p>{t("device_link.request.adding")}</p>}
                >
                  <p>Enter this code on the new device:</p>
                  <div class="lk-sas-code" aria-label="Sign-in code">
                    {(v() as { code?: string }).code}
                  </div>
                  <p>Never read this code to anyone. Lakko will never ask for it.</p>
                  <div class="lk-actions">
                    <button type="button" onClick={() => void flow?.flow.decline()}>
                      {t("device_link.request.decline")}
                    </button>
                  </div>
                </Show>
              </Show>
              </Show>
            </div>
          </div>
        )}
      </Show>
      <Show when={toast()}>
        {(message) => (
          <div class="lk-toast" role="status">
            <span>{message()}</span>
            <button type="button" onClick={() => flow?.flow.dismiss()} aria-label={t("common.dismiss")}>
              {t("common.close")}
            </button>
          </div>
        )}
      </Show>
    </>
  );
}
