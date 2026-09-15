// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Is this you?" — the signed-in device's side of the camera-less
 * another-device route (ADR-0049 section 2).
 *
 * While a session is open this polls `GET /device/requests`. When a fresh
 * device has posted its pairing payload, a modal shows the comparison code
 * derived from that payload and asks the user to check it against the new
 * device's screen. Confirm runs the unchanged add-device flow (`runScanFlow`
 * over the fetched payload: DeviceGroup add, Welcome, history bundle, the
 * ADR-0018 access grant and the sealed identity seed) and closes the
 * request; "It is not me" deletes it.
 *
 * What a confirmed device does NOT get today is membership in this
 * account's chat groups; see ADR-0049 section 5 for why that is blocked on
 * per-device conversation leaves.
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { useAuth } from "../auth/AuthContext";
import { AuthClient } from "../auth/client";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";
import { getDeviceKey } from "../auth/key-store";
import { base64Decode } from "../auth/account-id";
import { comparisonCode, DEVICE_REQUEST_POLL_MS } from "./device-request";
import { runScanFlow } from "./orchestrator";

export function DeviceRequestPrompt(): JSX.Element {
  const auth = useAuth();
  // ADR-0050 section 2.8 — the confirmed device is admitted to every chat.
  const receive = useBackgroundReceive();
  type Pending = {
    requestId: string;
    payload: Uint8Array;
    code: string;
  };
  const [pending, setPending] = createSignal<Pending | null>(null);
  const [busy, setBusy] = createSignal<"confirming" | null>(null);
  const [result, setResult] = createSignal<string | null>(null);
  const handled = new Set<string>();
  let timer: ReturnType<typeof setInterval> | null = null;

  async function poll(): Promise<void> {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null || busy() !== null) return;
    try {
      const req = await new AuthClient().deviceRequestsList(secrets.sessionToken);
      const shown = pending();
      // The server keeps one request per account: a newer one replaces the
      // one on screen (its code would no longer match the new device's),
      // and a vanished one (cancelled, expired) closes the prompt.
      if (req === null) {
        if (shown !== null) setPending(null);
        return;
      }
      if (handled.has(req.requestId)) {
        if (shown !== null && shown.requestId === req.requestId) setPending(null);
        return;
      }
      if (shown !== null && shown.requestId === req.requestId) return;
      setResult(null);
      setPending({
        requestId: req.requestId,
        payload: req.pairingPayload,
        code: await comparisonCode(req.pairingPayload),
      });
    } catch {
      // Polling is best-effort; the next tick retries.
    }
  }

  onMount(() => {
    timer = setInterval(() => void poll(), DEVICE_REQUEST_POLL_MS);
    void poll();
    const onVisible = (): void => void poll();
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => document.removeEventListener("visibilitychange", onVisible));
  });
  onCleanup(() => {
    if (timer !== null) clearInterval(timer);
  });

  async function decline(): Promise<void> {
    const p = pending();
    const secrets = auth.identitySecrets();
    if (p === null || secrets === null) return;
    handled.add(p.requestId);
    setPending(null);
    try {
      await new AuthClient().deviceRequestDelete(secrets.sessionToken, p.requestId);
    } catch {
      // Already gone or unreachable; the request expires on its own.
    }
    setResult("Declined. If this keeps happening, change your password.");
  }

  async function confirmRequest(): Promise<void> {
    const p = pending();
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (p === null || s.kind !== "session" || secrets === null) return;
    setBusy("confirming");
    try {
      const deviceKey = await getDeviceKey(s.account.accountId);
      if (deviceKey === null) {
        throw new Error("No device key on this browser. Sign in again.");
      }
      const client = new AuthClient();
      const scanned = await runScanFlow({
        scannedBytes: p.payload,
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
      handled.add(p.requestId);
      try {
        await client.deviceRequestDelete(secrets.sessionToken, p.requestId);
      } catch {
        // The request expires on its own.
      }
      setPending(null);
      setResult("Device added.");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("device-request: confirm failed", err);
      const detail =
        err instanceof Error
          ? err.message.length > 0
            ? err.message
            : err.name
          : String(err);
      setResult(`Could not add the device: ${detail}`);
      handled.add(p.requestId);
      setPending(null);
    } finally {
      setBusy(null);
    }
  }

  return (
    <>
      <Show when={pending() !== null}>
        <div class="lk-modal-backdrop" role="presentation">
          <div class="lk-modal" role="dialog" aria-modal="true" aria-labelledby="lk-devreq-title">
            <h2 id="lk-devreq-title">New sign-in request</h2>
            <p>Someone is signing in to your account on a new device.</p>
            <p class="form-info" style={{ "margin-bottom": "4px" }}>Comparison code</p>
            <div class="lk-sas-code">{pending()?.code}</div>
            <p>Does the new device show the same code?</p>
            <div class="lk-actions">
              <button type="button" disabled={busy() !== null} onClick={() => void decline()}>
                It is not me
              </button>
              <button class="primary" type="button" disabled={busy() !== null} onClick={() => void confirmRequest()}>
                {busy() === "confirming" ? "Adding the device…" : "Confirm"}
              </button>
            </div>
          </div>
        </div>
      </Show>
      <Show when={result() !== null}>
        <div class="lk-toast" role="status">
          <span>{result()}</span>
          <button type="button" onClick={() => setResult(null)} aria-label="Dismiss">
            Close
          </button>
        </div>
      </Show>
    </>
  );
}
