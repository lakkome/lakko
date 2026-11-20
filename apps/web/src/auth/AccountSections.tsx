// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Data and Settings > Account: the data export (GDPR
 * portability), and the session / device / account controls, with the
 * destructive ones last and behind the in-page confirmation (ADR-0045 UX §14).
 */
import { revokeAllMintedLinks } from "../contacts/own-links";
import { base64Encode } from "./account-id";
import { createSignal, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "./AuthContext";
import {
  DELETE_ACCOUNT_BODY_KEY,
  DELETE_ACCOUNT_CONFIRM_KEY,
  DELETE_ACCOUNT_TITLE_KEY,
  forgetDeviceConfirmation,
} from "./destructive-confirm";
import { deleteAccountSteps, deleteSurfaceView, PROOF_COPY } from "./fresh-auth";
import { createProofGate, proofFailureFromError } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import { ModalLayer } from "../ui/ModalLayer";
import { requestConfirm } from "../ui/confirm-dialog";
import { useWebauthnClient } from "./use-webauthn-client";
import {
  WebauthnClient,
  WebauthnHttpError,
  WebauthnUserCancelledError,
  actionHashForDeleteAccount,
} from "./webauthn-client";
import { DiscoveryClient } from "../discovery/client";
import { StorageClient } from "../storage/client";
import { paths } from "../routes/paths";
import { Icon } from "../ui/icons";
import { t, type L10nArgs } from "../l10n";
import { deleteBlockedUntilMs } from "./recall";
import { displayLocale } from "../l10n/display-locale";

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
  const [busy] = createSignal(false);
  const [message, setMessage] = createSignal<Message>(null);
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

  // GDPR erasure. The question, the proof and the deletion all live in the
  // app's own dialog below (UX 17.3).
  const [deleting, setDeleting] = createSignal(false);

  return (
    <section class="lk-card danger">
      <h2>{t("settings.section.account")}</h2>
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
          onClick={() => {
            setMessage(null);
            setDeleting(true);
          }}
        >
          {t("settings.account.delete.confirm")}
        </button>
      </div>
      <Show when={deleting()}>
        <DeleteAccountDialog onClose={() => setDeleting(false)} />
      </Show>
      <Show when={message()}>
        {(m) => <div class={m().kind === "ok" ? "form-info" : "form-error"}>{t(m().key, m().args)}</div>}
      </Show>
    </section>
  );
}

/**
 * `Delete your account?` (docs/ux/identity-recovery.md section 17.3): the
 * app's own dialog, with the shared proof block between the body and the
 * actions. An account with a passkey is asked for the passkey gesture for
 * this deletion (ADR-0014), whatever was proven before: that one gesture
 * is the proof, and no passkey sign-in precedes it. An account without one
 * proves the password unless a proof is live.
 */
/** Whose account the dialog deletes, and with what. */
export interface DeleteAccountTarget {
  readonly accountId: Uint8Array;
  /** The bearer of every deletion request: the access token, or a suspended
   *  account's restricted session (ADR-0089), which opens auth's
   *  `DELETE /account`, discovery's `DELETE /discovery/account` and
   *  storage's `DELETE /backup` and nothing else. */
  readonly bearer: string;
  readonly usernameKnown: boolean;
  /** The local cleanup once the servers deleted the account. */
  readonly finish: () => Promise<void>;
}

export function DeleteAccountDialog(props: {
  readonly onClose: () => void;
  /** ADR-0089: a suspended account on a device that holds none of its keys
   *  passes its own target; otherwise the signed-in session is the target. */
  readonly target?: DeleteAccountTarget;
}): JSX.Element {
  const auth = useAuth();
  const target = (): DeleteAccountTarget | null => {
    if (props.target !== undefined) return props.target;
    const secrets = auth.identitySecrets();
    if (secrets === null) return null;
    const s = auth.state();
    return {
      accountId: secrets.accountId,
      bearer: secrets.sessionToken,
      usernameKnown: s.kind === "session" && s.account.username.trim().length > 0,
      finish: () => auth.forgetDevice(),
    };
  };
  const client = (): WebauthnClient | null => {
    const tg = target();
    if (tg === null) return null;
    const c = new WebauthnClient();
    c.setSessionToken(tg.bearer);
    return c;
  };
  const [busy, setBusy] = createSignal(false);
  const [noPassword, setNoPassword] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.deleteAccount,
    // The passkey case is the dialog's own (the gesture for this deletion).
    offerPasskey: false,
    busy,
    sessionToken: () => target()?.bearer ?? null,
    usernameKnown: () => target()?.usernameKnown === true,
  });
  const surface = () =>
    deleteSurfaceView({
      // Unknown (the list could not be read) is drawn as "no passkey"; the
      // server still asks for the gesture when the account has one.
      hasPasskey: gate.hasPasskey() === true,
      proofLive: gate.proofLive(),
      revealed: gate.view().visible,
      noPassword: noPassword(),
    });
  const canDelete = (): boolean => {
    if (busy()) return false;
    const kind = surface().kind;
    if (kind === "cannot") return false;
    return kind === "passkey" ? true : gate.view().ready;
  };

  function close(): void {
    if (busy()) return;
    gate.clear();
    props.onClose();
  }

  /** The passkey gesture bound to this deletion (ADR-0014). */
  async function assertDeletion(accountId: Uint8Array): Promise<void> {
    const c = client();
    if (c === null) throw new Error("signed out");
    await c.assertAction(await actionHashForDeleteAccount(accountId));
  }

  async function onDelete(): Promise<void> {
    if (!canDelete()) return;
    const c = client();
    const secrets = target();
    if (c === null || secrets === null) {
      setError(t("common.error.session_not_ready"));
      return;
    }
    const kind = surface().kind;
    const steps = deleteAccountSteps(kind);
    if (steps === null) return;
    setError(null);
    setBusy(true);
    let grant: string | null = null;
    try {
      if (steps.ensureProof) {
        grant = await gate.ensure();
        if (grant === null) return;
      } else {
        // The gesture for this deletion is the proof at the server. The
        // grant header rides along only when a proof happens to be live.
        grant = auth.freshProof();
      }
      if (steps.actionGesture) await assertDeletion(secrets.accountId);
      // ADR-0078 section 10: the invite ledgers name no owner, so the
      // directory cannot delete this account's links. Revoke every live link
      // this device keeps a record of first, while the session still works;
      // one it has no record of lives until it expires or is used up.
      try {
        const links = new DiscoveryClient();
        links.setSessionToken(secrets.bearer);
        await revokeAllMintedLinks(links, base64Encode(secrets.accountId));
      } catch {
        /* best-effort: the deletion does not wait on a link */
      }
      try {
        await c.deleteAccount(grant);
      } catch (err) {
        // An account whose passkey list could not be read: the server
        // names the missing gesture with a `403` that is not the proof's.
        if (
          err instanceof WebauthnHttpError &&
          err.status === 403 &&
          proofFailureFromError(err).kind !== "required" &&
          !steps.actionGesture
        ) {
          await assertDeletion(secrets.accountId);
          await c.deleteAccount(grant);
        } else {
          throw err;
        }
      }
      try {
        const d = new DiscoveryClient();
        d.setSessionToken(secrets.bearer);
        await d.deleteAccount();
      } catch {
        /* best-effort */
      }
      // The encrypted backup goes with the account (ADR-0089: a suspended
      // account's restricted session opens this route too).
      try {
        const storage = new StorageClient();
        storage.setSessionToken(secrets.bearer);
        await storage.deleteBackup();
      } catch {
        /* best-effort */
      }
      gate.clear();
      // Wipes this device and lands on the sign-in screen.
      await secrets.finish();
    } catch (err) {
      // ADR-0074 section 1.3: while the account can still be taken back
      // after a key reset, the server refuses the deletion and names the
      // date. Nothing was deleted and the proof is not at fault.
      const blockedUntilMs =
        err instanceof WebauthnHttpError ? deleteBlockedUntilMs(err.status, err.body) : null;
      if (blockedUntilMs !== null) {
        setError(
          t("settings.account.delete.blocked", {
            date: new Date(blockedUntilMs).toLocaleDateString(displayLocale(), {
              dateStyle: "medium",
            }),
          }),
        );
        return;
      }
      const failure = gate.failed(err, grant);
      if (err instanceof WebauthnUserCancelledError || failure.kind === "cancelled") {
        setError(t("settings.account.delete.error.cancelled"));
      } else if (failure.kind === "other-account") {
        setError(t("proof.passkey.other_account"));
      } else if (failure.kind === "passkey-failed") {
        setError(t("proof.passkey.failed"));
      } else if (
        // On the passkey surface no block is drawn, so a refused proof
        // (not expected: the gesture is the proof) is worded here.
        (failure.kind !== "required" || kind === "passkey") &&
        failure.kind !== "rate-limited" &&
        failure.kind !== "offline"
      ) {
        console.warn("[account] delete failed", err instanceof Error ? err.name : "unknown");
        setError(t("settings.account.delete.error.generic"));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <ModalLayer open title={t(DELETE_ACCOUNT_TITLE_KEY)} onClose={close}>
      {/* The body precedes the actions in DOM order, which is focus order. */}
      <p>{t(DELETE_ACCOUNT_BODY_KEY)}</p>
      <Show when={surface().kind === "passkey"}>
        <p class="lk-muted">{t("proof.passkey.notice")}</p>
      </Show>
      <Show when={surface().kind === "block"}>
        <ProofBlock gate={gate}>
          <div>
            <button
              type="button"
              class="ghost"
              disabled={busy()}
              onClick={() => {
                gate.clear();
                setNoPassword(true);
              }}
            >
              {t("proof.no_password_link")}
            </button>
          </div>
        </ProofBlock>
      </Show>
      <Show when={surface().kind === "cannot"}>
        <p>{t("proof.delete.cannot")}</p>
        <div>
          <button type="button" class="ghost" onClick={() => setNoPassword(false)}>
            {t("proof.know_password_link")}
          </button>
        </div>
      </Show>
      <Show when={error() !== null}>
        <div class="form-error" role="alert">{error()}</div>
      </Show>
      <div class="lk-actions">
        <button type="button" disabled={busy()} onClick={close}>
          {t("common.cancel")}
        </button>
        <Show when={surface().kind !== "cannot"}>
          <button
            type="button"
            class="danger"
            disabled={!canDelete()}
            onClick={() => void onDelete()}
          >
            {busy() ? t("settings.account.delete.working") : t(DELETE_ACCOUNT_CONFIRM_KEY)}
          </button>
        </Show>
      </div>
    </ModalLayer>
  );
}
