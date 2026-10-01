// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0017 fresh-device screen, rendered whenever the auth state is
 * `needs-recovery-phrase`: the user is logged in (password, passkey or a
 * recovery code) on a device that has no cached identity seed. The identity
 * is never derived from the password and never silently re-minted (either
 * would break every contact's pin); it comes from one of three places:
 *
 *   - three words of the recovery phrase (ADR-0049), opened into the full
 *     phrase and handed to `provideRecoveryPhrase`;
 *   - the full phrase, `provideRecoveryPhrase` (verified against the
 *     published identity, fail-closed);
 *   - another device that is still logged in (ADR-0018 factor (a)), which
 *     transfers the seed end-to-end encrypted; `completeFromPairing` runs
 *     the same verification.
 *
 * The lost-everything fallback (`mintFreshIdentity`) sits behind a warning
 * at the bottom; it is the one path that publishes a new identity.
 */
import { createSignal, Match, onMount, Show, Switch, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { AuthLayout } from "../ui/AuthLayout";
import { MethodHead, MethodList } from "../ui/MethodList";
import { PhraseInput } from "../ui/PhraseInput";
import { WordSlots } from "../ui/WordSlots";
import { RESTORE_IDENTITY_METHODS, recoverMethod, type RecoverMethodId } from "./recover/methods";
import { listPositions } from "./phrase-confirm";
import { identityRestoreErrorMessage, isOffline, screenText } from "./auth-error-copy";
import { t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import { tRich } from "../l10n/rich";
import {
  WORD_VAULT_DESTRUCTION_CAP,
  wordVaultErrorMessage,
  wordsAreAnswerable,
  type WordVaultChallenge,
} from "../recovery/word-vault";
import { AuthHttpError } from "./client";
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

export default function RestoreIdentityPrompt(): JSX.Element {
  const auth = useAuth();
  const [method, setMethod] = createSignal<RecoverMethodId | null>(null);
  const [phrase, setPhrase] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // ADR-0049 three-word route.
  const [challenge, setChallenge] = createSignal<WordVaultChallenge | null>(null);
  const [challengeError, setChallengeError] = createSignal<string | null>(null);
  const [words, setWords] = createSignal<string[]>(["", "", ""]);
  const [misses, setMisses] = createSignal(0);
  const [wordsOff, setWordsOff] = createSignal(false);
  const triesLeft = (): number => WORD_VAULT_DESTRUCTION_CAP - misses();
  const wordsUnavailable = (): boolean => wordsOff() || misses() >= WORD_VAULT_DESTRUCTION_CAP;

  // ADR-0056: the other-device route addresses the account by the login
  // handle of its username. A sign-in that left no name on this device (a
  // passkey sign-in) asks for it; the handle is computed locally.
  const [typedUsername, setTypedUsername] = createSignal("");
  let pairingHandle: Uint8Array | null = null;
  const pairingUsername = (): string => {
    const acct = account();
    if (acct === null) return "";
    return acct.username.trim().length > 0 ? acct.username : typedUsername();
  };

  const methods = (): typeof RESTORE_IDENTITY_METHODS =>
    RESTORE_IDENTITY_METHODS.filter((m) => m.id !== "words" || !wordsUnavailable());

  const account = (): { username: string; accountIdB64: string } | null => {
    const s = auth.state();
    return s.kind === "needs-recovery-phrase"
      ? { username: s.account.username, accountIdB64: s.account.accountId }
      : null;
  };

  onMount(() => {
    const acct = account();
    if (acct === null) return;
    // ADR-0054 S5: the vault is addressed by the name's login handle. An
    // account with no name (a guest, or a passkey sign-in with no name on
    // this device) has nothing to address it by.
    if (acct.username.trim().length === 0) {
      setWordsOff(true);
      return;
    }
    void auth
      .wordVaultStart(acct.username)
      .then((c) => setChallenge(c))
      .catch((err: unknown) => {
        setChallengeError(wordVaultErrorMessage(err, null));
        if (err instanceof AuthHttpError && err.status === 503) setWordsOff(true);
      });
  });

  function pick(id: RecoverMethodId): void {
    setError(null);
    setMethod(id);
  }

  async function onSubmitWords(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const acct = account();
    const c = challenge();
    if (acct === null || c === null) return;
    setError(null);
    setBusy(true);
    try {
      const fullPhrase = await auth.wordVaultOpen(acct.username, c, words());
      await auth.provideRecoveryPhrase(fullPhrase);
      setWords(["", "", ""]);
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 401) {
        const next = misses() + 1;
        setMisses(next);
        setWords(["", "", ""]);
        if (next >= WORD_VAULT_DESTRUCTION_CAP) {
          setError(t("recovery.words.switched_off_restore"));
          setMethod("phrase");
        } else {
          setError(wordVaultErrorMessage(err, WORD_VAULT_DESTRUCTION_CAP - next));
        }
      } else {
        setError(wordVaultErrorMessage(err, null));
      }
    } finally {
      setBusy(false);
    }
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      await auth.provideRecoveryPhrase(phrase());
      setPhrase("");
      // No navigation: the state flips to `session` and the guard re-renders.
    } catch (err) {
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
      return {
        qrPayload: anon.qrPayload,
        awaitWelcome: (signal) => anon.awaitWelcome(signal),
        accept: async (welcome, welcomeCursor) => {
          const outcome = await anon.accept(welcome, { welcomeCursor });
          return outcome.recovery;
        },
        dispose: () => anon.dispose(),
      };
    }
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
            <p class="lk-auth-lede">
              {tRich(
                wordsUnavailable() ? "restore_identity.lede_no_words" : "restore_identity.lede",
                { username: <strong>@{account()?.username ?? ""}</strong> },
              )}
            </p>
            <MethodList methods={methods} onPick={pick} disabled={busy} />
            <Show when={challengeError() !== null && !wordsUnavailable()}>
              <p class="form-error">{challengeError()}</p>
            </Show>
          </>
        }
      >
        {(picked) => (
          <>
            <MethodHead
              method={() => recoverMethod(picked())}
              onBack={() => setMethod(null)}
              disabled={busy}
            />
            <Switch>
              <Match when={picked() === "words"}>
                <form onSubmit={onSubmitWords}>
                  <Show
                    when={challenge()}
                    fallback={
                      <p class="form-info">{challengeError() ?? t("common.loading")}</p>
                    }
                  >
                    {(c) => (
                      <>
                        <p class="lk-muted">
                          {tRich("recovery.words.prompt_restore", {
                            positions: <strong>{listPositions(c().positions)}</strong>,
                          })}
                        </p>
                        <WordSlots
                          positions={() => c().positions}
                          value={words}
                          setValue={setWords}
                          disabled={busy}
                        />
                      </>
                    )}
                  </Show>
                  <Show when={error() !== null}>
                    <div class="form-error" role="alert">
                      {error()}
                    </div>
                  </Show>
                  <button
                    class="primary"
                    type="submit"
                    disabled={busy() || challenge() === null || !wordsAreAnswerable(words())}
                  >
                    {busy() ? t("common.checking") : t("common.continue")}
                  </button>
                  <Show when={misses() > 0 && !wordsUnavailable()}>
                    <p class="form-info">{t("recovery.words.tries_left", { count: triesLeft() })}</p>
                  </Show>
                </form>
              </Match>
              <Match when={picked() === "device"}>
                <div class="lk-stack">
                  <p class="lk-muted">
                    A device that is still logged in shows a six-digit code; type it
                    here. The transfer is end-to-end encrypted; Lakko's servers
                    cannot read it. No camera needed; a QR code is there if you prefer.
                  </p>
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
                      await auth.completeFromPairing(
                        outcome.phraseSeed,
                        outcome.sessionToken ?? undefined,
                        outcome.sessionExpiresAtMs ?? undefined,
                      );
                    }}
                    disabled={busy()}
                  />
                </div>
              </Match>
              <Match when={picked() === "phrase"}>
                <form onSubmit={onSubmit}>
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
      <LostEverythingPanel busy={busy()} />
      <p class="lk-auth-foot">
        <button type="button" class="ghost small" disabled={busy()} onClick={() => void auth.logout()}>
          {t("restore_identity.log_out")}
        </button>
      </p>
    </AuthLayout>
  );
}

/**
 * ADR-0017 lost-everything fallback: for a user who got back in with a
 * recovery code but has neither the phrase nor another device. The only
 * path that publishes a changed identity, so it is collapsed behind a
 * warning and its button is not the primary action.
 */
function LostEverythingPanel(props: { busy: boolean }): JSX.Element {
  const auth = useAuth();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onCreate(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      // Mints and publishes the new identity and flips the state to
      // `save-fresh-phrase`, which the guard renders as the phrase screen.
      await auth.mintFreshIdentity();
    } catch (err) {
      setError(
        isOffline(err)
          ? t("common.error.offline")
          : (screenText(err) ?? t("restore_identity.lost.error.generic")),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <details class="lk-disclosure">
      <summary>{t("restore_identity.lost.summary")}</summary>
      <div class="lk-disclosure-body">
        <div class="lk-alert danger" role="alert">
          {t("restore_identity.lost.warning")}
        </div>
        <Show when={error() !== null}>
          <div class="form-error">{error()}</div>
        </Show>
        <div class="lk-actions">
          <button
            type="button"
            class="danger"
            disabled={busy() || props.busy}
            onClick={() => void onCreate()}
          >
            {busy() ? t("restore_identity.lost.creating") : t("restore_identity.lost.create")}
          </button>
        </div>
      </div>
    </details>
  );
}
