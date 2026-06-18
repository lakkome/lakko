// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PasskeysSection` — passkey management UI on the Home screen
 * (Phase 2a2c1).
 *
 * Renders three things, in this order:
 *
 *   1. **Capability gate.** When the browser doesn't expose
 *      `navigator.credentials.create/get` we show a single line
 *      explaining why the section is inert. No further state.
 *
 *   2. **Credential list.** One row per stored passkey from the
 *      `/webauthn/credentials` endpoint. Each row carries the
 *      nickname, creation time, last-used time, and a Delete
 *      button. Empty list → "No passkeys yet" hint.
 *
 *   3. **Add affordance.** A nickname input + an "Add passkey"
 *      button. Click drives the full registration round-trip
 *      via `WebauthnClient.registerPasskey`: server returns the
 *      challenge → browser shows the platform sheet → server
 *      finishes the binding. On success the list refreshes.
 *
 * The "Sign in with passkey" path is wired ELSEWHERE — see
 * `AuthContext.loginWithPasskey` (the action) and `Login.tsx`
 * (the "Sign in with passkey" button on the login screen),
 * both landed in 2a2c2. That flow re-authenticates to the
 * server with the passkey, then reconstructs the session from
 * the exportKey cached under the device key (populated by a
 * prior OPAQUE login on this device); a fresh device with no
 * cached exportKey fails closed, since a WebAuthn signature
 * does not produce one. THIS section is purely about managing
 * the server-side credential set (list / add / delete).
 */
import {
  createEffect,
  createResource,
  createSignal,
  Show,
  For,
  type JSX,
} from "solid-js";
import { useAuth } from "./AuthContext";
import { getDeviceKey } from "./key-store";
import { enableAttestedRemovalsIfNeeded } from "../devices/attested-removal-policy";
import {
  WebauthnClient,
  WebauthnHttpError,
  WebauthnNetworkError,
  WebauthnUnsupportedError,
  WebauthnUserCancelledError,
  actionHashForDeleteCredential,
  actionHashForDeleteAccount,
  type CredentialSummary,
} from "./webauthn-client";
import { DiscoveryClient } from "../discovery/client";

const SUPPORTED = WebauthnClient.isWebauthnAvailable();

export default function PasskeysSection(): JSX.Element {
  const auth = useAuth();

  // Capture the username + sessionToken reactively so the section
  // can re-bind across logins / token rotations without remounting.
  const username = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : null;
  };
  const sessionToken = (): string | null => {
    const secrets = auth.identitySecrets();
    return secrets === null ? null : secrets.sessionToken;
  };

  // Build a fresh `WebauthnClient` whenever the session token
  // rotates — same shape as `ContactsClient` in `Contacts.tsx`.
  // Memo-cheap: the client itself is tiny, the only state is the
  // bearer token.
  const client = (): WebauthnClient | null => {
    const token = sessionToken();
    if (token === null) return null;
    const c = new WebauthnClient();
    c.setSessionToken(token);
    return c;
  };

  const [refreshTrigger, setRefreshTrigger] = createSignal(0);
  const [credentials] = createResource(
    () => {
      // Re-fetch when either the token changes (new login) or the
      // refresh trigger ticks (after a successful add / delete).
      const c = client();
      const tick = refreshTrigger();
      return c === null ? null : ({ c, tick } as const);
    },
    async (
      input: { readonly c: WebauthnClient; readonly tick: number } | null,
    ): Promise<readonly CredentialSummary[]> => {
      if (input === null) return [];
      const res = await input.c.listCredentials();
      return res.credentials;
    },
  );

  const [nickname, setNickname] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal<
    { readonly kind: "ok" | "error"; readonly text: string } | null
  >(null);

  // Clear messages when the form input changes — keeps stale
  // toasts from lingering after the user starts a fresh attempt.
  createEffect(() => {
    nickname();
    setMessage(null);
  });

  async function onAdd(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const c = client();
    const name = username();
    const label = nickname().trim();
    if (c === null || name === null) {
      setMessage({ kind: "error", text: "Session not ready." });
      return;
    }
    if (label.length === 0) {
      setMessage({ kind: "error", text: "Give the passkey a nickname." });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      await c.registerPasskey({ user_name: name, nickname: label });
      // G-13 (ADR-0015): now that the account has a passkey, enable attested
      // removals so device removals require a peer-verified passkey gesture.
      // Non-fatal: the policy can be enabled on a later passkey action.
      const s = auth.state();
      const secrets = auth.identitySecrets();
      if (s.kind === "session" && secrets !== null) {
        const deviceKey = await getDeviceKey(s.account.accountId);
        if (deviceKey !== null) {
          try {
            await enableAttestedRemovalsIfNeeded({
              accountIdBytes: secrets.accountId,
              accountIdB64: s.account.accountId,
              deviceKey,
            });
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("failed to enable attested removals", err);
          }
        }
      }
      setMessage({
        kind: "ok",
        text: `Passkey "${label}" added.`,
      });
      setNickname("");
      setRefreshTrigger((n) => n + 1);
    } catch (err) {
      setMessage({ kind: "error", text: explainError(err) });
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(cred: CredentialSummary): Promise<void> {
    const c = client();
    if (c === null) {
      setMessage({ kind: "error", text: "Session not ready." });
      return;
    }
    if (
      !confirm(
        `Remove passkey "${cred.nickname}"? You can't undo this; the device that owns the matching private key will no longer be able to sign in via passkey.`,
      )
    ) {
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      // G-13 (ADR-0014): require a fresh passkey gesture cryptographically
      // bound to THIS credential's removal before deleting it. A throw
      // (e.g. the user dismisses the sheet) aborts the delete — fail-closed.
      const actionHashB64 = await actionHashForDeleteCredential(
        cred.credential_id_b64,
      );
      await c.assertAction(actionHashB64);
      await c.deleteCredential(cred.credential_id_b64);
      setMessage({
        kind: "ok",
        text: `Removed "${cred.nickname}".`,
      });
      setRefreshTrigger((n) => n + 1);
    } catch (err) {
      setMessage({ kind: "error", text: explainError(err) });
    } finally {
      setBusy(false);
    }
  }

  // A3 (GDPR right-to-erasure) — delete the whole account across services +
  // wipe this device. Orchestration: auth `DELETE /account` (with a passkey
  // action gesture when the account has a passkey) -> best-effort discovery
  // purge -> local `forgetDevice` wipe + sign-out.
  async function onDeleteAccount(): Promise<void> {
    const c = client();
    const secrets = auth.identitySecrets();
    if (c === null || secrets === null) {
      setMessage({ kind: "error", text: "Session not ready." });
      return;
    }
    if (
      !window.confirm(
        "Permanently delete your account? This erases your server-side " +
          "identity (username, keys, recovery factors, backup) and signs you " +
          "out everywhere. Your local messages are wiped from this device too. " +
          "This cannot be undone.",
      )
    ) {
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      // Raw 16-byte account_id for the delete-account action hash.
      const accountId = secrets.accountId;
      // Auth tier: try the delete; if the server demands a passkey gesture
      // (the account has a passkey) it answers 403, so run the action ceremony
      // bound to this account and retry once. A throw (the user dismisses the
      // sheet) aborts — fail-closed.
      try {
        await c.deleteAccount();
      } catch (err) {
        if (err instanceof WebauthnHttpError && err.status === 403) {
          const actionHashB64 = await actionHashForDeleteAccount(accountId);
          await c.assertAction(actionHashB64);
          await c.deleteAccount();
        } else {
          throw err;
        }
      }
      // Discovery tier: best-effort (the auth tier is the critical erasure;
      // the session token is about to be invalidated anyway, and what is left
      // behind is only opaque public artefacts).
      try {
        const d = new DiscoveryClient();
        d.setSessionToken(secrets.sessionToken);
        await d.deleteAccount();
      } catch {
        /* best-effort */
      }
      // Local wipe + full sign-out on this device.
      await auth.forgetDevice();
    } catch (err) {
      setMessage({ kind: "error", text: explainError(err) });
    } finally {
      setBusy(false);
    }
  }

  // A3 (GDPR data-portability) — download a JSON of what the server holds about
  // this account + the locally-known username. The actual E2EE content
  // (messages, contacts, groups) lives only on-device and is exported via the
  // encrypted Backup, not here.
  async function onExportData(): Promise<void> {
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
          "and live only on your devices — export them via the encrypted Backup.",
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
      setMessage({ kind: "ok", text: "Exported your account data." });
    } catch (err) {
      setMessage({ kind: "error", text: explainError(err) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="passkeys-section">
      <h2 style={{ "margin-bottom": "0.5rem" }}>Passkeys</h2>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Sign in with a hardware key, Face ID, or your password manager.
        Recovery phrase always works as the last-resort path.
      </p>

      <Show
        when={SUPPORTED}
        fallback={
          <p class="form-error">
            Your browser doesn't support WebAuthn. Use a recent version
            of Safari, Chrome, Firefox, or Edge to manage passkeys.
          </p>
        }
      >
        <Show
          when={credentials() !== undefined}
          fallback={<p>Loading passkeys…</p>}
        >
          <Show
            when={(credentials() ?? []).length > 0}
            fallback={
              <p style={{ color: "var(--surface-muted)" }}>
                No passkeys yet. Add one below.
              </p>
            }
          >
            <ul class="passkeys-list" style={{ "list-style": "none", padding: 0 }}>
              <For each={credentials() ?? []}>
                {(cred) => (
                  <li
                    style={{
                      display: "flex",
                      "align-items": "center",
                      "justify-content": "space-between",
                      padding: "0.5rem 0",
                      "border-bottom": "1px solid var(--surface-border, #eee)",
                    }}
                  >
                    <div>
                      <strong>{cred.nickname || "(unnamed passkey)"}</strong>
                      <div
                        style={{
                          color: "var(--surface-muted)",
                          "font-size": "0.85em",
                        }}
                      >
                        Added {formatTimestamp(cred.created_at_ms)}
                        {cred.last_used_at_ms !== null
                          ? ` · last used ${formatTimestamp(cred.last_used_at_ms)}`
                          : " · never used"}
                      </div>
                    </div>
                    <button
                      type="button"
                      disabled={busy()}
                      onClick={() => void onDelete(cred)}
                    >
                      Delete
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>

          <form
            onSubmit={onAdd}
            style={{ display: "flex", gap: "0.5rem", "margin-top": "1rem" }}
          >
            <input
              type="text"
              placeholder="Nickname (e.g. MacBook Touch ID)"
              value={nickname()}
              onInput={(e) => setNickname(e.currentTarget.value)}
              disabled={busy()}
              maxLength={128}
              style={{ flex: 1 }}
            />
            <button class="primary" type="submit" disabled={busy()}>
              {busy() ? "Adding…" : "Add passkey"}
            </button>
          </form>
        </Show>

        {message() !== null && (
          <div
            class={message()?.kind === "ok" ? "form-info" : "form-error"}
            style={{ "margin-top": "0.75rem" }}
          >
            {message()?.text}
          </div>
        )}
      </Show>

      {/* A3 (GDPR) — data portability: download what the server holds. */}
      <div
        style={{
          "margin-top": "2rem",
          "padding-top": "1rem",
          "border-top": "1px solid var(--surface-border, #ccc)",
        }}
      >
        <h3 style={{ "margin-bottom": "0.25rem" }}>Your data</h3>
        <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
          Download a copy of what the server holds about your account. Your
          messages and contacts are end-to-end encrypted on your devices —
          export those via the encrypted Backup.
        </p>
        <button type="button" disabled={busy()} onClick={() => void onExportData()}>
          Export my data
        </button>
      </div>

      {/* A3 (GDPR) — danger zone: permanent account deletion. */}
      <div
        style={{
          "margin-top": "2rem",
          "padding-top": "1rem",
          "border-top": "1px solid var(--surface-border, #ccc)",
        }}
      >
        <h3 style={{ "margin-bottom": "0.25rem", color: "var(--error)" }}>
          Delete account
        </h3>
        <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
          Permanently erase your account from the server (username, keys,
          recovery factors, backup) and wipe this device. This cannot be undone.
        </p>
        <button
          type="button"
          class="danger"
          style={{ color: "var(--error)" }}
          disabled={busy()}
          onClick={() => void onDeleteAccount()}
        >
          Delete my account
        </button>
      </div>
    </section>
  );
}

function formatTimestamp(ms: number | null): string {
  if (ms === null) return "never";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "unknown";
  return d.toLocaleString();
}

function explainError(err: unknown): string {
  if (err instanceof WebauthnUserCancelledError) {
    return "Cancelled. No passkey was created.";
  }
  if (err instanceof WebauthnUnsupportedError) {
    return "This browser / device can't create passkeys right now.";
  }
  if (err instanceof WebauthnHttpError) {
    if (err.status === 503) {
      return "Server hasn't enabled passkey support.";
    }
    if (err.status === 401) {
      return "Session expired. Sign in again.";
    }
    return `Server error (${err.status}). ${err.body}`;
  }
  if (err instanceof WebauthnNetworkError) {
    return "Network error. Check your connection and retry.";
  }
  return err instanceof Error ? err.message : String(err);
}
