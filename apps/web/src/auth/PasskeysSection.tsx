// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PasskeysSection` — passkey management, under Settings > Security
 * (docs/ux/identity-recovery.md 10.2, amended 2026-11-11; ADR-0072
 * decision 6).
 *
 *   1. **Capability gate.** A browser without WebAuthn gets one line
 *      saying why the section is inert.
 *
 *   2. **Credential list.** One row per passkey from
 *      `/webauthn/credentials`: its name, when it was added and last used,
 *      and one of two labels, which are a state and not a setting: "Logs in
 *      and restores keys" or "Logs in only" (from `GET /vault/passkeys`). A
 *      passkey that holds no keys and is not known to lack PRF offers
 *      "Restore keys with this passkey" (the repair); every row offers
 *      Remove, which also removes what the passkey stored.
 *
 *   3. **Add.** A name plus "Add passkey". After the system sheet creates
 *      the passkey, the keys are stored for it with nothing to choose
 *      (`storeKeysForPasskey`): from the registration's PRF output, else
 *      one more passkey prompt. There is no choice screen and no "Stop
 *      restoring keys".
 *
 * Signing in with a passkey is `AuthContext.loginWithPasskey`.
 */
import { AuthClient, AuthHttpError } from "./client";
import { passkeyKeysLabel } from "../recovery/passkey-vault";
import { addPasskey } from "./passkey-add";
import { PROOF_COPY } from "./fresh-auth";
import { createProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import {
  addedPasskeyMessage,
  repairMessage,
  storeKeysForPasskey,
  type StoreKeysOutcome,
} from "./passkey-keys-store";
import { PhraseInput } from "../ui/PhraseInput";
import { addNoPrfPasskey, readNoPrfPasskeys, removeNoPrfPasskey } from "./passkey-no-prf";
import { passkeyErrorKey } from "./passkey-error-key";
import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  Show,
  For,
  type JSX,
} from "solid-js";
import { useAuth } from "./AuthContext";
import { removePasskeyConfirmation } from "./destructive-confirm";
import { requestConfirm } from "../ui/confirm-dialog";
import {
  WebauthnClient,
  WebauthnHttpError,
  actionHashForDeleteCredential,
  type CredentialSummary,
} from "./webauthn-client";
import { useWebauthnClient } from "./use-webauthn-client";
import { Icon } from "../ui/icons";
import { t, type L10nArgs } from "../l10n";
import { displayLocale } from "../l10n/display-locale";

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

  // Credentials this device found cannot carry keys (UX 10.2): kept per
  // account on the device (`passkey-no-prf.ts`), so their rows read "Logs
  // in only" with no repair action, across reloads.
  const accountIdB64 = (): string | null => {
    const st = auth.state();
    return st.kind === "session" ? st.account.accountId : null;
  };
  const [noPrfTick, setNoPrfTick] = createSignal(0);
  const noPrf = createMemo((): ReadonlySet<string> => {
    noPrfTick();
    const acct = accountIdB64();
    return new Set(acct === null ? [] : readNoPrfPasskeys(deviceStorage(), acct));
  });
  const markNoPrf = (credentialIdB64: string): void => {
    const acct = accountIdB64();
    if (acct !== null) addNoPrfPasskey(deviceStorage(), acct, credentialIdB64);
    setNoPrfTick((n) => n + 1);
  };
  const offersRepair = (cred: CredentialSummary): boolean =>
    !restoresKeys(cred) && !noPrf().has(cred.credential_id_b64);

  // The repair that is waiting for the recovery phrase (this device holds
  // none): the twelve-slot input sits under the list for that passkey.
  const [phraseFor, setPhraseFor] = createSignal<string | null>(null);
  const [typedPhrase, setTypedPhrase] = createSignal("");

  const [nickname, setNickname] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // The message is a catalog key, rendered at display time so it follows a
  // language switch (ADR-0061).
  const [message, setMessage] = createSignal<
    { readonly kind: "ok" | "error"; readonly key: string; readonly args?: L10nArgs } | null
  >(null);

  // Adding a passkey needs the password or a passkey proven now (the block
  // sits above the form; absent while a proof is live). Without it a
  // session alone could add a passkey and then use it as the proof.
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.addPasskey,
    collapsed: true,
    busy,
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    usernameKnown: () => (username() ?? "").trim().length > 0,
  });

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
    let grant: string | null = null;
    try {
      // The proof first; the registration ceremony only after it passes.
      grant = await gate.ensure();
      if (grant === null) return;
      const st = auth.state();
      const secrets = auth.identitySecrets();
      const added = await addPasskey({
        client: c,
        username: name,
        freshGrant: grant,
        nickname: label,
        account:
          st.kind === "session" && secrets !== null
            ? { accountIdBytes: secrets.accountId, accountIdB64: st.account.accountId }
            : null,
      });
      // UX 10.2 (amended 2026-11-11): the keys are stored for the new
      // passkey with nothing to choose, when the platform can protect them.
      // "signin-only" is a creation that said the passkey cannot carry keys.
      let outcome: StoreKeysOutcome = "no-prf";
      if (added.outcome === "store-keys") {
        outcome =
          st.kind === "session" && secrets !== null
            ? await storeKeysForPasskey({
                client: c,
                accountIdB64: st.account.accountId,
                accountSeed: secrets.accountIdentitySeed,
                sessionToken: secrets.sessionToken,
                credentialIdB64: added.credentialIdB64,
                prfOutput: added.prfOutput,
              })
            : "failed";
      }
      if (outcome === "no-prf") markNoPrf(added.credentialIdB64);
      const line = addedPasskeyMessage(outcome);
      setMessage(
        line.named === true
          ? { kind: line.kind, key: line.key, args: { name: label } }
          : { kind: line.kind, key: line.key },
      );
      setNickname("");
      setRefreshTrigger((n) => n + 1);
    } catch (err) {
      const failure = gate.failed(err, grant).kind;
      // A refused proof shows the block; a limit and the network are the
      // block's own lines.
      if (failure !== "required" && failure !== "rate-limited" && failure !== "offline") {
        setMessage(errorMessage(err));
      }
    } finally {
      setBusy(false);
    }
  }

  /** "Restore keys with this passkey": one passkey prompt for this
   *  credential; first the phrase when this device holds none. */
  async function onRepair(credentialIdB64: string, typed?: string): Promise<void> {
    const c = client();
    const st = auth.state();
    const secrets = auth.identitySecrets();
    if (c === null || st.kind !== "session" || secrets === null) {
      setMessage({ kind: "error", key: "common.error.session_not_ready" });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const outcome = await storeKeysForPasskey({
        client: c,
        accountIdB64: st.account.accountId,
        accountSeed: secrets.accountIdentitySeed,
        sessionToken: secrets.sessionToken,
        credentialIdB64,
        prfOutput: null,
        ...(typed !== undefined ? { typedPhrase: typed } : {}),
      });
      if (outcome === "phrase-needed") {
        setPhraseFor(credentialIdB64);
        return;
      }
      if (outcome === "no-prf") markNoPrf(credentialIdB64);
      // A wrong phrase keeps the input open for another try; every other
      // end closes it.
      if (outcome !== "phrase-mismatch" && outcome !== "invalid-phrase") {
        setPhraseFor(null);
        setTypedPhrase("");
      }
      setMessage(repairMessage(outcome));
      if (outcome === "stored") setRefreshTrigger((n) => n + 1);
    } finally {
      setBusy(false);
    }
  }

  function cancelRepair(): void {
    setPhraseFor(null);
    setTypedPhrase("");
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
      const acct = accountIdB64();
      if (acct !== null) removeNoPrfPasskey(deviceStorage(), acct, cred.credential_id_b64);
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
            fallback={
              <>
                <p class="form-info">{t("settings.passkeys.empty")}</p>
                {/* UX 15.3: the reset deleted every passkey of the account. */}
                <Show when={auth.keyResetNote()}>
                  <p class="form-info">{t("settings.passkeys.reset_note")}</p>
                </Show>
              </>
            }
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
                    <Show when={offersRepair(cred)}>
                      <button
                        type="button"
                        class="ghost small lk-card-row-action"
                        disabled={busy() || phraseFor() !== null}
                        onClick={() => void onRepair(cred.credential_id_b64)}
                      >
                        {t("settings.passkeys.keys.turn_on")}
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

          <Show when={phraseFor()}>
            {(credentialIdB64) => (
              <div class="lk-stack">
                <p>{t("settings.passkeys.keys_choice.phrase_needed")}</p>
                <PhraseInput value={typedPhrase} setValue={setTypedPhrase} disabled={busy} />
                <div class="lk-actions">
                  <button
                    type="button"
                    class="primary"
                    disabled={busy()}
                    onClick={() => void onRepair(credentialIdB64(), typedPhrase())}
                  >
                    {busy() ? t("common.checking") : t("settings.passkeys.keys.turn_on")}
                  </button>
                  <button type="button" class="ghost" disabled={busy()} onClick={cancelRepair}>
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            )}
          </Show>

          <ProofBlock gate={gate} />
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
            <button class="primary" type="submit" disabled={busy() || !gate.view().ready}>
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

/** `localStorage`, or `undefined` where this context has none or refuses
 *  access to it. */
function deviceStorage(): Storage | undefined {
  try {
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    return undefined;
  }
}

/** A timestamp in the interface language (`created_at_ms` is never null
 *  on the wire; the null arm is defensive). */
function formatTimestamp(ms: number | null): string {
  const d = ms === null ? null : new Date(ms);
  if (d === null || Number.isNaN(d.getTime())) return t("common.unknown");
  return d.toLocaleString(displayLocale());
}

function errorMessage(err: unknown): {
  readonly kind: "error";
  readonly key: string;
  readonly args?: L10nArgs;
} {
  const key = passkeyErrorKey(err);
  return key === "settings.passkeys.error.server_status" && err instanceof WebauthnHttpError
    ? { kind: "error", key, args: { status: String(err.status) } }
    : { kind: "error", key };
}
