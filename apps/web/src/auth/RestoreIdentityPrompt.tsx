// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Restore your keys" (docs/ux/identity-recovery.md section 8), rendered
 * whenever the auth state is `needs-recovery-phrase`: the person is signed
 * in (password, passkey or an authenticator code) on a device
 * that does not hold the account's keys. The keys are never derived from
 * the password and never silently re-minted (either would break every
 * contact's pin); they come from one of three places, in this order
 * (ADR-0072 decision 7):
 *
 *   - another device that is still signed in (ADR-0018 factor (a)), which
 *     transfers the recovery phrase (as its entropy) end-to-end encrypted;
 *     `completeFromPairing` checks the keys it gives against the published
 *     identity before storing the keys and the phrase;
 *   - a passkey that carries the keys (ADR-0072 decision 6),
 *     `restoreKeysWithPasskey`; the row is shown only while the account has
 *     such a passkey;
 *   - the 12-word phrase, `provideRecoveryPhrase` (checked against the
 *     published identity, fail-closed).
 *
 * The authenticator code is NOT here: it signs in and cannot restore keys,
 * and offering it would promise what it does not do.
 *
 * An optional first line says how the person got here when a passkey just
 * signed them in without bringing keys (`auth.restoreNotice`).
 *
 * The lost-everything fallback (`mintFreshIdentity`) sits behind a warning
 * at the bottom; it is the one path that publishes a new identity.
 */
import type { DeviceCipher } from "./device-cipher";
import { isAbandoned } from "./binding-epoch";
import {
  createResource,
  createSignal,
  Match,
  Show,
  Switch,
  type JSX,
} from "solid-js";
import { useAuth } from "./AuthContext";
import { refreshProtection } from "./protect/protection-state";
import { EmailRecover } from "./recover/EmailRecover";
import { AuthLayout } from "../ui/AuthLayout";
import { MethodHead, MethodList } from "../ui/MethodList";
import { PhraseInput } from "../ui/PhraseInput";
import {
  restoreIdentityMethods,
  restoreMethod,
  type RecoverMethod,
  type RecoverMethodId,
} from "./recover/methods";
import {
  identityRestoreErrorMessage,
  passkeyErrorMessage,
  screenText,
} from "./auth-error-copy";
import { t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import { tRich } from "../l10n/rich";
import { AuthClient } from "./client";
import { WebauthnClient } from "./webauthn-client";
import { base64Decode } from "./account-id";
import { getDeviceKey } from "./key-store";
import {
  prepareAnonymousJoinFlow,
  prepareJoinFlow,
} from "../devices/orchestrator";
import RestoreViaDevicePanel, {
  type DeviceRestoreFlowHandle,
} from "../devices/RestoreViaDevicePanel";
import { loginHandleFor } from "./login-handle";
import { UsernameField } from "./recover/UsernameField";
import { RESET_CONFIRM_WORD, resetConfirmed } from "./reset-confirm";
import { PROOF_COPY, resetCanSubmit, resetSurfaceView } from "./fresh-auth";
import { createProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";

/** The catalog key of the prompt's first line for a restore notice. */
const NOTICE_KEYS = {
  "passkey-no-keys": "restore_identity.from_passkey_no_keys",
  "passkey-unusable": "restore_identity.from_passkey_unusable",
} as const;

export default function RestoreIdentityPrompt(): JSX.Element {
  const auth = useAuth();
  const [method, setMethod] = createSignal<RecoverMethodId | null>(null);
  const [phrase, setPhrase] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [passkeyBusy, setPasskeyBusy] = createSignal(false);

  // ADR-0056: the other-device route addresses the account by the login
  // handle of its username. A sign-in that left no name on this device (a
  // passkey sign-in) asks for it; the handle is computed locally.
  const [typedUsername, setTypedUsername] = createSignal("");
  let pairingHandle: Uint8Array | null = null;
  // The anonymous join's own state (its in-memory device seed, the
  // DeviceGroup snapshot, the history and state bundles), landed under the
  // device key `completeFromPairing` bootstraps. `null` on the route that
  // already held a device key: that join wrote as it went.
  let persistAnonymousJoin: ((deviceKey: DeviceCipher) => Promise<void>) | null = null;
  const pairingUsername = (): string => {
    const acct = account();
    if (acct === null) return "";
    return acct.username.trim().length > 0 ? acct.username : typedUsername();
  };

  const account = (): { username: string; accountIdB64: string } | null => {
    const s = auth.state();
    return s.kind === "needs-recovery-phrase"
      ? { username: s.account.username, accountIdB64: s.account.accountId }
      : null;
  };

  // How many of the account's passkeys carry keys (the device is signed
  // in, so it may ask). `null` while unknown or when the server cannot say,
  // which keeps the passkey row.
  const [passkeysWithKeys] = createResource(
    () => account()?.accountIdB64 ?? null,
    async (): Promise<number | null> => {
      const token = auth.pendingSessionToken();
      if (token === null) return null;
      try {
        return (await new AuthClient().passkeyVaultList(token)).length;
      } catch {
        return null;
      }
    },
  );
  // ADR-0075 (alignment J): how the account is protected, read with the
  // session this device holds. It decides the phrase row and the email row,
  // and keeps the device's "phrase-protected" flag in step, which is what
  // decides whether new words are shown should the keys be reset from here.
  const [protection] = createResource(
    () => account()?.accountIdB64 ?? null,
    async (accountIdB64) => {
      const token = auth.pendingSessionToken();
      return token === null ? null : refreshProtection(accountIdB64, token);
    },
  );
  const methods = (): readonly RecoverMethod[] =>
    restoreIdentityMethods({
      passkeysSupported: WebauthnClient.isWebauthnAvailable(),
      passkeysWithKeys: passkeysWithKeys() ?? null,
      protection: protection() ?? null,
    });
  // The email flow's username field, pre-filled with the account's.
  const [emailUsername, setEmailUsername] = createSignal(account()?.username ?? "");

  function pick(id: RecoverMethodId): void {
    setError(null);
    setMethod(id);
  }

  // Section 8: the passkey row runs the ceremony again and asks the passkey
  // for the keys. `restored` flips the state to `session`; anything else
  // leaves this screen up with its first line saying what happened.
  async function onPasskey(): Promise<void> {
    setError(null);
    setPasskeyBusy(true);
    try {
      const outcome = await auth.restoreKeysWithPasskey();
      if (outcome !== "restored") setMethod(null);
    } catch (err) {
      // The prompt's account is no longer the one bound here: nothing to say.
      if (isAbandoned(err)) return;
      if (!(err instanceof KeyedError && err.key === "auth.passkey.error.cancelled")) {
        setError(
          err instanceof KeyedError ? passkeyErrorMessage(err) : identityRestoreErrorMessage(err),
        );
      }
    } finally {
      setPasskeyBusy(false);
    }
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      // Typed by the person: an owed phrase screen is not shown back.
      await auth.provideRecoveryPhrase(phrase(), "typed");
      setPhrase("");
      // No navigation: the state flips to `session` and the guard re-renders.
    } catch (err) {
      if (isAbandoned(err)) return;
      setError(identityRestoreErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0018 factor (a), path (a). A browser that already holds a device key
  // (a password or passkey login ran here before) uses the authenticated join
  // flow; a fresh browser (access came from a recovery code) uses the
  // anonymous one, which mints an in-memory device seed. Either way the seed
  // and the session land via `completeFromPairing`.
  async function prepareDeviceFlow(): Promise<DeviceRestoreFlowHandle> {
    const acct = account();
    if (acct === null) {
      throw new KeyedError("restore_identity.error.nothing");
    }
    const name = pairingUsername().trim();
    if (name.length === 0) {
      throw new KeyedError("recovery.username_required");
    }
    // Throws the username rule's copy for a name it refuses.
    const loginHandle = (await loginHandleFor(name)).bytes;
    pairingHandle = loginHandle;
    const accountIdBytes = base64Decode(acct.accountIdB64);
    const deviceKey = await getDeviceKey(acct.accountIdB64);
    if (deviceKey === null) {
      const anon = await prepareAnonymousJoinFlow({
        loginHandle,
        known: { accountIdBytes, accountIdB64: acct.accountIdB64 },
      });
      // No device key yet: the join holds its device seed, the history
      // bundle and the pairing state bundle in memory until
      // `completeFromPairing` has bootstrapped the key and calls this.
      persistAnonymousJoin = (key) => anon.persistTo(key);
      return {
        qrPayload: anon.qrPayload,
        awaitWelcome: (signal) => anon.awaitWelcome(signal),
        accept: async (welcome, welcomeCursor) => {
          const outcome = await anon.accept(welcome, { welcomeCursor });
          return outcome.recovery;
        },
        dispose: () => {
          anon.dispose();
          persistAnonymousJoin = null;
        },
      };
    }
    persistAnonymousJoin = null;
    const session = await prepareJoinFlow({
      accountIdBytes,
      accountIdB64: acct.accountIdB64,
      deviceKey,
      loginHandle,
    });
    return {
      qrPayload: session.qrPayload,
      awaitWelcome: (signal) => session.awaitWelcome(signal),
      accept: async (welcome, welcomeCursor) => {
        const outcome = await session.accept(welcome, {
          welcomeCursor,
          recovery: true,
        });
        return outcome.recovery;
      },
      dispose: () => session.dispose(),
    };
  }

  return (
    <AuthLayout>
      <Show
        when={method()}
        fallback={
          <>
            <h1>{t("restore_identity.title")}</h1>
            <Show when={auth.restoreNotice()}>
              {(notice) => (
                <div class="lk-alert info" role="status">
                  {t(NOTICE_KEYS[notice()])}
                </div>
              )}
            </Show>
            <p class="lk-auth-lede">
              {tRich("restore_identity.lede", {
                username: <strong>@{account()?.username ?? ""}</strong>,
              })}
            </p>
            <MethodList methods={methods} onPick={pick} disabled={busy} />
          </>
        }
      >
        {(picked) => (
          <>
            <MethodHead
              method={() => restoreMethod(picked())}
              onBack={() => setMethod(null)}
              disabled={() => busy() || passkeyBusy()}
            />
            <Switch>
              <Match when={picked() === "email"}>
                {/* The same email flow as on Get back in. It ends in the
                    phrase restore, which lands the session with the keys. */}
                <EmailRecover
                  username={emailUsername}
                  setUsername={setEmailUsername}
                  onBack={() => setMethod(null)}
                  onDone={() => undefined}
                />
              </Match>
              <Match when={picked() === "device"}>
                <div class="lk-stack">
                  <p class="lk-muted">{t("restore_identity.device_intro")}</p>
                  <Show when={(account()?.username.trim().length ?? 0) === 0}>
                    <div>
                      <UsernameField
                        value={typedUsername}
                        setValue={setTypedUsername}
                        disabled={busy}
                      />
                    </div>
                  </Show>
                  <RestoreViaDevicePanel
                    prepare={prepareDeviceFlow}
                    requestHandle={() => pairingHandle}
                    onRestored={async (outcome) => {
                      // The panel disposes the flow only after this resolves,
                      // so the anonymous join is still alive for its persist.
                      await auth.completeFromPairing(
                        outcome.phraseEntropy,
                        outcome.session ?? undefined,
                        // ADR-0074: the state bundle's earlier account keys
                        // are stored before the session's identities exist.
                        persistAnonymousJoin ?? undefined,
                      );
                    }}
                    disabled={busy()}
                  />
                </div>
              </Match>
              <Match when={picked() === "passkey"}>
                <div class="lk-stack">
                  <p class="lk-muted">{t("recovery.passkey.intro")}</p>
                  <Show when={error() !== null}>
                    <div class="form-error" role="alert">
                      {error()}
                    </div>
                  </Show>
                  <button
                    class="primary"
                    type="button"
                    disabled={passkeyBusy()}
                    onClick={() => void onPasskey()}
                  >
                    {passkeyBusy() ? t("recovery.passkey.waiting") : t("recovery.passkey.submit")}
                  </button>
                </div>
              </Match>
              <Match when={picked() === "phrase"}>
                <form onSubmit={onSubmit}>
                  <p class="lk-muted">{t("restore_identity.phrase_intro")}</p>
                  <PhraseInput value={phrase} setValue={setPhrase} disabled={busy} />
                  <Show when={error() !== null}>
                    <div class="form-error" role="alert">
                      {error()}
                    </div>
                  </Show>
                  <button class="primary" type="submit" disabled={busy()}>
                    {busy() ? t("restore_identity.submitting") : t("restore_identity.submit")}
                  </button>
                </form>
              </Match>
            </Switch>
          </>
        )}
      </Show>
      <LostEverythingPanel
        busy={busy() || passkeyBusy()}
        usernameKnown={(account()?.username.trim().length ?? 0) > 0}
      />
      <p class="lk-auth-foot">
        <button
          type="button"
          class="ghost small"
          disabled={busy() || passkeyBusy()}
          onClick={() => void auth.logout()}
        >
          {t("restore_identity.log_out")}
        </button>
      </p>
    </AuthLayout>
  );
}

/**
 * "I have none of these" (docs/ux/identity-recovery.md sections 8 and 15.1,
 * ADR-0073): for a person who is signed in but has no other device, no
 * passkey that restores keys and no phrase. The one path that gives the
 * account a new identity, so it is collapsed behind a warning that states
 * what it does (other devices signed out and removed, passkeys deleted,
 * contacts re-verify, old messages unreadable, a new phrase; the password
 * and the authenticator keep working), gated by the typed word RESET, and
 * its button is not the primary action. Nothing is sent until the button is
 * tapped; a failure before anything changed is shown under the button and
 * leaves the panel open (a failure after the server step is the phrase
 * screen's, never this panel's).
 *
 * Sections 15.1a and 17.2: the server refuses a reset without a fresh-auth
 * proof. While one is live (a password or passkey sign-in less than ten
 * minutes ago) the panel is exactly as above. Otherwise the shared proof
 * block sits above the confirmation word: the password, or `Use a passkey
 * instead` when the account has one this browser can use. The submit
 * obtains the proof first and resets second, under one `Resetting...`
 * state. A person who came in with the authenticator code and has no
 * usable passkey sees the "cannot reset" panel first.
 */
function LostEverythingPanel(props: { busy: boolean; usernameKnown: boolean }): JSX.Element {
  const auth = useAuth();
  const [busy, setBusy] = createSignal(false);
  const [typed, setTyped] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [knowsPassword, setKnowsPassword] = createSignal(false);
  const [noPassword, setNoPassword] = createSignal(false);
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.reset,
    busy: () => busy() || props.busy,
    sessionToken: () => auth.pendingSessionToken(),
    usernameKnown: () => props.usernameKnown,
  });

  const surface = () =>
    resetSurfaceView({
      proofLive: gate.proofLive(),
      passkeyUsable: gate.passkeyUsable(),
      landing: auth.sessionLanding(),
      knowsPassword: knowsPassword(),
      noPassword: noPassword(),
    });
  const canSubmit = (): boolean =>
    resetCanSubmit({
      confirmed: resetConfirmed(typed()),
      proofReady: gate.view().ready,
      cannot: surface().cannot,
      blocked: busy() || props.busy,
    });

  async function onReset(): Promise<void> {
    if (!canSubmit()) return;
    setError(null);
    setBusy(true);
    let grant: string | null = null;
    try {
      // The proof when none is live, the server reset, then the publish;
      // flips the state to `save-fresh-phrase`, which the guard renders as
      // the phrase screen.
      grant = await gate.ensure();
      if (grant === null) return;
      await auth.mintFreshIdentity();
    } catch (err) {
      // Signed out, or another account landed, while the keys were being
      // reset: no error. If the server reset already happened, the new keys
      // and their phrase are stored for this account's next sign-in on this
      // browser.
      if (isAbandoned(err)) return;
      switch (gate.failed(err, grant).kind) {
        case "required":
        case "rate-limited":
          // The block (or its countdown) says it; nothing was reset.
          break;
        case "offline":
          setError(t("restore_identity.reset.error.offline"));
          break;
        default:
          setError(screenText(err) ?? t("restore_identity.reset.error.generic"));
          break;
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <details class="lk-disclosure">
      <summary>{t("restore_identity.lost.summary")}</summary>
      <div class="lk-disclosure-body lk-stack">
        <div class="lk-alert danger" role="alert">
          <p>{t("restore_identity.reset.lede")}</p>
          <p>{t("restore_identity.reset.what_it_does")}</p>
          <ul>
            <li>{t("restore_identity.reset.effect_devices")}</li>
            <li>{t("restore_identity.reset.effect_passkeys")}</li>
            <li>{t("restore_identity.reset.effect_contacts")}</li>
            <li>{t("restore_identity.reset.effect_messages")}</li>
            <li>{t("restore_identity.reset.effect_password")}</li>
          </ul>
        </div>
        <Show
          when={!surface().cannot}
          fallback={
            <div class="lk-stack">
              <strong>{t("proof.reset.cannot.title")}</strong>
              <p>{t("proof.reset.cannot.body")}</p>
              <p>{t("proof.reset.cannot.what_you_can_do")}</p>
              <ul>
                <li>{t("proof.reset.cannot.option_device")}</li>
                <li>{t("proof.reset.cannot.option_phrase")}</li>
                <li>{t("proof.reset.cannot.option_passkey")}</li>
              </ul>
              <p>{t("proof.reset.cannot.otherwise")}</p>
              <div class="lk-actions">
                <button
                  type="button"
                  class="ghost"
                  onClick={() => {
                    setNoPassword(false);
                    setKnowsPassword(true);
                  }}
                >
                  {t("proof.know_password_link")}
                </button>
              </div>
            </div>
          }
        >
          <ProofBlock gate={gate}>
            <Show when={surface().showNoPasswordLink}>
              <div>
                <button
                  type="button"
                  class="ghost"
                  disabled={busy() || props.busy}
                  onClick={() => {
                    gate.clear();
                    setKnowsPassword(false);
                    setNoPassword(true);
                  }}
                >
                  {t("proof.no_password_link")}
                </button>
              </div>
            </Show>
          </ProofBlock>
          <label>
            {t("restore_identity.reset.confirm_label", { word: RESET_CONFIRM_WORD })}
            <input
              type="text"
              autocomplete="off"
              autocapitalize="off"
              autocorrect="off"
              spellcheck={false}
              value={typed()}
              onInput={(e) => setTyped(e.currentTarget.value)}
              disabled={busy() || props.busy}
            />
          </label>
          <Show when={error() !== null}>
            <div class="form-error" role="alert">{error()}</div>
          </Show>
          <div class="lk-actions">
            <button
              type="button"
              class="danger"
              disabled={!canSubmit()}
              onClick={() => void onReset()}
            >
              {busy() ? t("restore_identity.reset.resetting") : t("restore_identity.lost.create")}
            </button>
          </div>
        </Show>
      </div>
    </details>
  );
}
