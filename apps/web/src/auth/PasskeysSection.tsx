// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PasskeysSection` — passkey management, under Settings > Security
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
import { AuthClient } from "./client";
import { loadWasm } from "../wasm";
import { ensurePasskeyVaultEnrolled, PASSKEY_PRF_SALT } from "../recovery/passkey-vault";
import {
  createEffect,
  createResource,
  createSignal,
  Show,
  For,
  type JSX,
} from "solid-js";
import { useAuth } from "./AuthContext";
import { removePasskeyConfirmation } from "./destructive-confirm";
import { requestConfirm } from "../ui/confirm-dialog";
import { getDeviceKey } from "./key-store";
import { enableAttestedRemovalsIfNeeded } from "../devices/attested-removal-policy";
import {
  WebauthnClient,
  WebauthnHttpError,
  WebauthnNetworkError,
  WebauthnUnsupportedError,
  WebauthnUserCancelledError,
  actionHashForDeleteCredential,
  type CredentialSummary,
} from "./webauthn-client";
import { useWebauthnClient } from "./use-webauthn-client";
import { Icon } from "../ui/icons";
import { currentLanguage, t, type L10nArgs } from "../l10n";

const SUPPORTED = WebauthnClient.isWebauthnAvailable();

export default function PasskeysSection(): JSX.Element {
  const auth = useAuth();

  const { client, username } = useWebauthnClient();

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
  // The message is a catalog key, rendered at display time so it follows a
  // language switch (ADR-0058).
  const [message, setMessage] = createSignal<
    { readonly kind: "ok" | "error"; readonly key: string; readonly args?: L10nArgs } | null
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
      setMessage({ kind: "error", key: "common.error.session_not_ready" });
      return;
    }
    if (label.length === 0) {
      setMessage({ kind: "error", key: "settings.passkeys.error.name_required" });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const registered = await c.registerPasskey(
        { user_name: name, nickname: label },
        { prfSalt: PASSKEY_PRF_SALT },
      );
      // ADR-0049 section 3: when the authenticator evaluated PRF at
      // creation, seal the phrase under it so this passkey carries the
      // identity to a new device. Authenticators that only evaluate PRF on
      // assertion get enrolled at the next passkey sign-in instead.
      {
        const s = auth.state();
        const secrets = auth.identitySecrets();
        if (registered.prfOutput !== null && s.kind === "session" && secrets !== null) {
          const wasm = await loadWasm();
          void ensurePasskeyVaultEnrolled({
            wasm,
            client: new AuthClient(),
            sessionToken: secrets.sessionToken,
            accountIdB64: s.account.accountId,
            credentialId: registered.credentialId,
            prfOutput: registered.prfOutput,
          });
        }
      }
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
      setMessage({ kind: "ok", key: "settings.passkeys.added", args: { name: label } });
      setNickname("");
      setRefreshTrigger((n) => n + 1);
    } catch (err) {
      setMessage(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(cred: CredentialSummary): Promise<void> {
    const c = client();
    if (c === null) {
      setMessage({ kind: "error", key: "common.error.session_not_ready" });
      return;
    }
    // ADR-0045 UX §14 — an in-page dialog rather than the bare global
    // `confirm`. The nickname moves out of the question and onto the action
    // button (§6.1: the button's accessible name is its visible label, so
    // that is what is announced at the point of no return); the old string
    // had to stuff it into the question because a browser dialog renders one
    // string and a browser-supplied OK/Cancel pair.
    if (!(await requestConfirm(removePasskeyConfirmation(cred)))) {
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
      setMessage({ kind: "ok", key: "settings.passkeys.removed", args: { name: cred.nickname } });
      setRefreshTrigger((n) => n + 1);
    } catch (err) {
      setMessage(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-card">
      <h2>{t("settings.passkeys.title")}</h2>
      <p class="lk-card-desc">{t("settings.passkeys.lede")}</p>

      <Show
        when={SUPPORTED}
        fallback={
          <p class="form-error">{t("settings.passkeys.unsupported_browser")}</p>
        }
      >
        <Show when={credentials() !== undefined} fallback={<p class="form-info">{t("common.loading")}</p>}>
          <Show
            when={(credentials() ?? []).length > 0}
            fallback={<p class="form-info">{t("settings.passkeys.empty")}</p>}
          >
            <div>
              <For each={credentials() ?? []}>
                {(cred) => (
                  <div class="lk-card-row">
                    <span class="lk-card-row-icon">
                      <Icon name="key" size={18} />
                    </span>
                    <div class="lk-card-row-body">
                      <span class="lk-card-row-title">{cred.nickname || t("settings.passkeys.unnamed")}</span>
                      <span class="lk-card-row-sub">
                        {cred.last_used_at_ms !== null
                          ? t("settings.passkeys.added_last_used", {
                              added: formatTimestamp(cred.created_at_ms),
                              used: formatTimestamp(cred.last_used_at_ms),
                            })
                          : t("settings.passkeys.added_never_used", {
                              added: formatTimestamp(cred.created_at_ms),
                            })}
                      </span>
                    </div>
                    <button
                      type="button"
                      class="ghost small lk-card-row-action"
                      style={{ color: "var(--danger)" }}
                      disabled={busy()}
                      onClick={() => void onDelete(cred)}
                    >
                      {t("common.remove")}
                    </button>
                  </div>
                )}
              </For>
            </div>
          </Show>

          <form onSubmit={onAdd} class="lk-actions" style={{ "flex-wrap": "nowrap" }}>
            <input
              type="text"
              placeholder={t("settings.passkeys.name.placeholder")}
              aria-label={t("settings.passkeys.name.label")}
              value={nickname()}
              onInput={(e) => setNickname(e.currentTarget.value)}
              disabled={busy()}
              maxLength={128}
              style={{ flex: 1 }}
            />
            <button class="primary" type="submit" disabled={busy()}>
              {busy() ? t("common.adding") : t("settings.passkeys.add")}
            </button>
          </form>
        </Show>

        <Show when={message()}>
          {(m) => <div class={m().kind === "ok" ? "form-info" : "form-error"}>{t(m().key, m().args)}</div>}
        </Show>
      </Show>
    </section>
  );
}

/** A timestamp in the interface language (`created_at_ms` is never null
 *  on the wire; the null arm is defensive). */
function formatTimestamp(ms: number | null): string {
  const d = ms === null ? null : new Date(ms);
  if (d === null || Number.isNaN(d.getTime())) return t("common.unknown");
  return d.toLocaleString(currentLanguage());
}

/** The catalog key for an error (ADR-0058 section 6: never the server's
 *  or the browser's prose, which goes to the log). */
function explainError(err: unknown): string {
  if (err instanceof WebauthnUserCancelledError) {
    return "settings.passkeys.error.cancelled";
  }
  if (err instanceof WebauthnUnsupportedError) {
    return "settings.passkeys.error.unsupported";
  }
  if (err instanceof WebauthnHttpError) {
    if (err.status === 503) {
      return "auth.passkey.error.server_disabled";
    }
    if (err.status === 401) {
      return "common.error.session_expired";
    }
    console.warn("[passkeys] server error", err.status, err.body);
    return "settings.passkeys.error.server_status";
  }
  if (err instanceof WebauthnNetworkError) {
    return "settings.passkeys.error.network";
  }
  console.warn("[passkeys] failed", err);
  return "settings.passkeys.error.generic";
}

function errorMessage(err: unknown): {
  readonly kind: "error";
  readonly key: string;
  readonly args?: L10nArgs;
} {
  const key = explainError(err);
  return key === "settings.passkeys.error.server_status" && err instanceof WebauthnHttpError
    ? { kind: "error", key, args: { status: String(err.status) } }
    : { kind: "error", key };
}
