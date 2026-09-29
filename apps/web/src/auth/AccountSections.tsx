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
import { listPositions } from "./phrase-confirm";
import { t, type L10nArgs } from "../l10n";

/** A catalog key, rendered at display time so it follows a language switch. */
type Message = { readonly kind: "ok" | "error"; readonly key: string; readonly args?: L10nArgs } | null;

const EXPORT_FILE_NAME = "lakko-account-export.json";

/** Download a JSON of what the server holds about this account. The E2EE
 *  content (messages, contacts, groups) lives only on-device. */
export function ExportDataCard(): JSX.Element {
  const { client, username } = useWebauthnClient();
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal<Message>(null);

  async function onExport(): Promise<void> {
    const c = client();
    if (c === null) {
      setMessage({ kind: "error", key: "common.error.session_not_ready" });
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
        note: t("settings.data.export_note"),
      };
      const blob = new Blob([JSON.stringify(exportDoc, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = EXPORT_FILE_NAME;
      anchor.click();
      URL.revokeObjectURL(url);
      setMessage({ kind: "ok", key: "settings.data.downloaded", args: { file: EXPORT_FILE_NAME } });
    } catch (err) {
      console.warn("[account] export failed", err);
      setMessage({ kind: "error", key: "settings.data.error.generic" });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card">
      <h2>{t("settings.data.title")}</h2>
      <p class="lk-card-desc">{t("settings.data.lede")}</p>
      <div class="lk-actions">
        <button type="button" disabled={busy()} onClick={() => void onExport()}>
          <Icon name="db" size={18} />
          {busy() ? t("common.preparing") : t("settings.data.export")}
        </button>
      </div>
      <Show when={message()}>
        {(m) => <div class={m().kind === "ok" ? "form-info" : "form-error"}>{t(m().key, m().args)}</div>}
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
    if (v === undefined) return t("common.checking");
    if (v === null) return t("settings.account.vault.unavailable");
    return v.enrolled && v.positions !== null
      ? t("settings.account.vault.on", { positions: listPositions(v.positions) })
      : t("settings.account.vault.not_set_up");
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
      setMessage({ kind: "error", key: "common.error.session_not_ready" });
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
      console.warn("[account] delete failed", err);
      setMessage({ kind: "error", key: "settings.account.delete.error.generic" });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card danger">
      <h2>{t("settings.section.account")}</h2>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="key" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("settings.recovery.title")}</span>
          <span class="lk-card-row-sub">{recoveryLine()}</span>
        </div>
      </div>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="logout" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("settings.account.log_out")}</span>
          <span class="lk-card-row-sub">{t("settings.account.log_out.sub")}</span>
        </div>
        <button type="button" class="lk-card-row-action" onClick={onLogout}>
          {t("settings.account.log_out")}
        </button>
      </div>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="devices" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("settings.account.forget_device.label")}</span>
          <span class="lk-card-row-sub">{t("settings.account.forget_device.sub")}</span>
        </div>
        <button
          type="button"
          class="danger lk-card-row-action"
          disabled={busy()}
          onClick={() => void onForgetDevice()}
        >
          {t("settings.account.forget_device.confirm")}
        </button>
      </div>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="trash" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("settings.account.delete.confirm")}</span>
          <span class="lk-card-row-sub">{t("settings.account.delete.sub")}</span>
        </div>
        <button
          type="button"
          class="danger lk-card-row-action"
          disabled={busy()}
          onClick={() => void onDeleteAccount()}
        >
          {t("settings.account.delete.confirm")}
        </button>
      </div>
      <Show when={message()}>
        {(m) => <div class={m().kind === "ok" ? "form-info" : "form-error"}>{t(m().key, m().args)}</div>}
      </Show>
    </section>
  );
}
