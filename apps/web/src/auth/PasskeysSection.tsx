// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PasskeysSection` — passkey management, under Settings > Security
 * (docs/ux/identity-recovery.md 10.2, ADR-0064 decision 6).
 *
 *   1. **Capability gate.** A browser without WebAuthn gets one line
 *      saying why the section is inert.
 *
 *   2. **Credential list.** One row per passkey from
 *      `/webauthn/credentials`: its name, when it was added and last used,
 *      and one of two labels, "Signs in and restores keys" or "Signs in
 *      only" (from `GET /vault/passkeys`). Each row offers "Turn on
 *      restoring keys" or "Stop restoring keys", and Remove.
 *
 *   3. **Add.** A name plus "Add passkey". When the platform can protect
 *      keys with the new passkey the choice follows; otherwise the result
 *      line says the passkey signs in and cannot restore keys.
 *
 *   4. **The choice** ("Let this passkey restore your keys?"). Storing the
 *      keys for a passkey is OPT-IN, per passkey, and informed: the safety
 *      of the keys then also rests on whoever syncs the passkey. Nothing is
 *      stored unless the person presses "Turn on"; the two buttons carry
 *      the same weight and neither is the default.
 *
 * The choice itself is `PasskeyKeysChoice` (shared with the passkey offer at
 * the end of signup); adding a passkey is `addPasskey`. A browser or
 * authenticator that gives no secret to protect keys with leaves the
 * passkey "Signs in only", and the section says so.
 *
 * Signing in with a passkey is `AuthContext.loginWithPasskey`.
 */
import { AuthClient, AuthHttpError } from "./client";
import { passkeyKeysLabel } from "../recovery/passkey-vault";
import { addPasskey } from "./passkey-add";
import { PasskeyKeysChoice, type KeysChoiceResult } from "./PasskeyKeysChoice";
import {
  createEffect,
  createResource,
  createSignal,
  Show,
  For,
  type JSX,
} from "solid-js";
import { useAuth } from "./AuthContext";
import { removePasskeyConfirmation, stopRestoringKeysConfirmation } from "./destructive-confirm";
import { requestConfirm } from "../ui/confirm-dialog";
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

  // The credential ids that have keys stored for them. `null` when the
  // server cannot say (no vault key, or the request failed): every passkey
  // then reads "Signs in only", which is what it can do.
  const [withKeys] = createResource(
    () => {
      const token = auth.identitySecrets()?.sessionToken ?? null;
      const tick = refreshTrigger();
      return token === null ? null : ({ token, tick } as const);
    },
    async (input): Promise<readonly string[] | null> => {
      if (input === null) return null;
      try {
        return await new AuthClient().passkeyVaultList(input.token);
      } catch (err) {
        if (!(err instanceof AuthHttpError && err.status === 503)) {
          console.warn("[passkeys] could not list the passkeys that restore keys", err);
        }
        return null;
      }
    },
  );
  const restoresKeys = (cred: CredentialSummary): boolean =>
    passkeyKeysLabel(cred.credential_id_b64, withKeys() ?? null) === "restores-keys";

  // The choice screen for one passkey. `prfOutput` is what the registration
  // yielded, when it yielded it; otherwise "Turn on" asks the passkey once
  // more. `step` is "phrase" while this device has to be given the phrase.
  const [choice, setChoice] = createSignal<{
    readonly credentialIdB64: string;
    readonly nickname: string;
    readonly prfOutput: Uint8Array | null;
  } | null>(null);

  const [nickname, setNickname] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // The message is a catalog key, rendered at display time so it follows a
  // language switch (ADR-0061).
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
      const st = auth.state();
      const secrets = auth.identitySecrets();
      const added = await addPasskey({
        client: c,
        username: name,
        nickname: label,
        account:
          st.kind === "session" && secrets !== null
            ? { accountIdBytes: secrets.accountId, accountIdB64: st.account.accountId }
            : null,
      });
      // ADR-0064 decision 6: nothing is stored for the new passkey here.
      // When the platform can protect keys with it, the person is asked;
      // otherwise the line says it signs in only.
      if (added.outcome === "offer-keys-choice") {
        setMessage({ kind: "ok", key: "settings.passkeys.added", args: { name: label } });
        openChoice(added.credentialIdB64, label, added.prfOutput);
      } else {
        setMessage({ kind: "ok", key: "settings.passkeys.added_no_keys" });
      }
      setNickname("");
      setRefreshTrigger((n) => n + 1);
    } catch (err) {
      setMessage(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  function openChoice(credentialIdB64: string, name: string, prfOutput: Uint8Array | null): void {
    setChoice({ credentialIdB64, nickname: name, prfOutput });
  }

  /** The choice ended: say what happened and refresh the labels. */
  function onChoiceDone(result: KeysChoiceResult): void {
    setChoice(null);
    setBusy(false);
    switch (result) {
      case "stored":
        setMessage({ kind: "ok", key: "settings.passkeys.keys_choice.done" });
        break;
      case "no-prf":
        setMessage({ kind: "error", key: "settings.passkeys.keys_choice.no_prf" });
        break;
      case "unavailable":
        setMessage({ kind: "error", key: "settings.passkeys.keys_choice.unavailable" });
        break;
      case "signin-only":
        break;
    }
    setRefreshTrigger((n) => n + 1);
  }

  async function onStopRestoring(cred: CredentialSummary): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setMessage({ kind: "error", key: "common.error.session_not_ready" });
      return;
    }
    if (!(await requestConfirm(stopRestoringKeysConfirmation()))) return;
    setBusy(true);
    setMessage(null);
    try {
      await new AuthClient().passkeyVaultDelete(secrets.sessionToken, cred.credential_id_b64);
      setMessage({ kind: "ok", key: "settings.passkeys.stop_restoring.done" });
      setRefreshTrigger((n) => n + 1);
    } catch (err) {
      console.warn("[passkeys] stop restoring keys failed", err);
      setMessage({ kind: "error", key: "settings.passkeys.error.generic" });
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
      <p class="lk-card-desc">{t("settings.passkeys.lede_device")}</p>

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
                        <strong>
                          {restoresKeys(cred)
                            ? t("settings.passkeys.label.restores_keys")
                            : t("settings.passkeys.label.signin_only")}
                        </strong>
                      </span>
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
                    <Show
                      when={restoresKeys(cred)}
                      fallback={
                        <button
                          type="button"
                          class="ghost small lk-card-row-action"
                          disabled={busy() || choice() !== null}
                          onClick={() => openChoice(cred.credential_id_b64, cred.nickname, null)}
                        >
                          {t("settings.passkeys.keys.turn_on")}
                        </button>
                      }
                    >
                      <button
                        type="button"
                        class="ghost small lk-card-row-action"
                        disabled={busy()}
                        onClick={() => void onStopRestoring(cred)}
                      >
                        {t("settings.passkeys.stop_restoring.action")}
                      </button>
                    </Show>
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

          <Show when={choice()}>
            {(picked) => (
              <PasskeyKeysChoice
                credentialIdB64={picked().credentialIdB64}
                nickname={picked().nickname}
                prfOutput={picked().prfOutput}
                client={client}
                onDone={onChoiceDone}
                onBusy={setBusy}
              />
            )}
          </Show>

          <form onSubmit={onAdd} class="lk-actions" style={{ "flex-wrap": "nowrap" }}>
            <input
              type="text"
              placeholder={t("settings.passkeys.name.placeholder_device")}
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

/** The catalog key for an error (ADR-0061 section 6: never the server's
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
