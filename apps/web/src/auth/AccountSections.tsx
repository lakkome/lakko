// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Data and Settings > Account: the data export (GDPR
 * portability), and the session / device / account controls, with the
 * destructive ones last and behind the in-page confirmation (ADR-0045 UX §14).
 */
import { createResource, createSignal, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "./AuthContext";
import {
  deleteAccountConfirmation,
  forgetDeviceConfirmation,
} from "./destructive-confirm";
import { requestConfirm } from "../ui/confirm-dialog";
import { useWebauthnClient } from "./use-webauthn-client";
import {
  WebauthnHttpError,
  actionHashForDeleteAccount,
} from "./webauthn-client";
import { DiscoveryClient } from "../discovery/client";
import { paths } from "../routes/paths";
import { Icon } from "../ui/icons";
import { AuthClient } from "./client";

type Message = { readonly kind: "ok" | "error"; readonly text: string } | null;

/** Download a JSON of what the server holds about this account. The E2EE
 *  content (messages, contacts, groups) lives only on-device. */
export function ExportDataCard(): JSX.Element {
  const { client, username } = useWebauthnClient();
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal<Message>(null);

  async function onExport(): Promise<void> {
    const c = client();
    if (c === null) {
      setMessage({ kind: "error", text: "Session not ready." });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const server = await c.exportAccount();
      const exportDoc = {
        schema: "lakko/account-export/v1",
        exported_at: new Date().toISOString(),
        username: username(),
        server,
        note:
          "Your messages, contacts, and group state are end-to-end encrypted " +
          "and live only on your devices.",
      };
      const blob = new Blob([JSON.stringify(exportDoc, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = "lakko-account-export.json";
      anchor.click();
      URL.revokeObjectURL(url);
      setMessage({ kind: "ok", text: "Downloaded lakko-account-export.json." });
    } catch (err) {
      setMessage({ kind: "error", text: err instanceof Error ? err.message : String(err) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card">
      <h2>Your data</h2>
      <p class="lk-card-desc">
        Download a copy of what Lakko's servers hold about your account. Your
        messages and contacts are end-to-end encrypted and stay on your devices,
        so they are not part of this file.
      </p>
      <div class="lk-actions">
        <button type="button" disabled={busy()} onClick={() => void onExport()}>
          <Icon name="db" size={18} />
          {busy() ? "Preparing…" : "Export my data"}
        </button>
      </div>
      <Show when={message()}>
        {(m) => <div class={m().kind === "ok" ? "form-info" : "form-error"}>{m().text}</div>}
      </Show>
    </section>
  );
}

/** Log out · Forget this device · Delete account. */
export function AccountCard(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const { client } = useWebauthnClient();
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal<Message>(null);
  // ADR-0049: whether the three-word shortcut is set up for this account.
  const [vault] = createResource(
    () => auth.identitySecrets()?.sessionToken ?? null,
    async (token) => {
      if (token === null) return null;
      try {
        return await new AuthClient().wordVaultStatus(token);
      } catch {
        return null;
      }
    },
  );
  const recoveryLine = (): string => {
    const v = vault();
    if (v === undefined) return "Checking…";
    if (v === null) return "Three-word recovery: not available on this server. Your 24-word phrase and your other devices still work.";
    return v.enrolled && v.positions !== null
      ? `Three-word recovery: on (words ${v.positions.join(", ")}). Another signed-in device and your 24-word phrase work too.`
      : "Three-word recovery: not set up on this account yet. It is set up the next time you sign in on a device that holds your phrase.";
  };

  function onLogout(): void {
    // Keeps local stores intact: re-login on this device stays cheap.
    auth.logout();
    navigate(paths.login, { replace: true });
  }

  async function onForgetDevice(): Promise<void> {
    if (!(await requestConfirm(forgetDeviceConfirmation()))) return;
    await auth.forgetDevice();
    navigate(paths.login, { replace: true });
  }

  // GDPR erasure: auth `DELETE /account` (with a passkey gesture when the
  // account has one) -> best-effort discovery purge -> local wipe + sign-out.
  async function onDeleteAccount(): Promise<void> {
    const c = client();
    const secrets = auth.identitySecrets();
    if (c === null || secrets === null) {
      setMessage({ kind: "error", text: "Session not ready." });
      return;
    }
    if (!(await requestConfirm(deleteAccountConfirmation()))) return;
    setBusy(true);
    setMessage(null);
    try {
      try {
        await c.deleteAccount();
      } catch (err) {
        if (err instanceof WebauthnHttpError && err.status === 403) {
          const actionHashB64 = await actionHashForDeleteAccount(secrets.accountId);
          await c.assertAction(actionHashB64);
          await c.deleteAccount();
        } else {
          throw err;
        }
      }
      try {
        const d = new DiscoveryClient();
        d.setSessionToken(secrets.sessionToken);
        await d.deleteAccount();
      } catch {
        /* best-effort */
      }
      await auth.forgetDevice();
    } catch (err) {
      setMessage({ kind: "error", text: err instanceof Error ? err.message : String(err) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card danger">
      <h2>Account</h2>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="key" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">Recovery</span>
          <span class="lk-card-row-sub">{recoveryLine()}</span>
        </div>
      </div>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="logout" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">Log out</span>
          <span class="lk-card-row-sub">
            Signs you out on this device. Your data stays here for the next login.
          </span>
        </div>
        <button type="button" class="lk-card-row-action" onClick={onLogout}>
          Log out
        </button>
      </div>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="devices" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">Forget this device</span>
          <span class="lk-card-row-sub">
            Removes your account data from this browser. Your messages stay on
            your other devices; to come back here you will need your recovery
            phrase or another device.
          </span>
        </div>
        <button
          type="button"
          class="danger lk-card-row-action"
          disabled={busy()}
          onClick={() => void onForgetDevice()}
        >
          Forget device
        </button>
      </div>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="trash" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">Delete account</span>
          <span class="lk-card-row-sub">
            Permanently erases your account and everything in it, and wipes this
            device. This cannot be undone.
          </span>
        </div>
        <button
          type="button"
          class="danger lk-card-row-action"
          disabled={busy()}
          onClick={() => void onDeleteAccount()}
        >
          Delete account
        </button>
      </div>
      <Show when={message()}>
        {(m) => <div class={m().kind === "ok" ? "form-info" : "form-error"}>{m().text}</div>}
      </Show>
    </section>
  );
}
