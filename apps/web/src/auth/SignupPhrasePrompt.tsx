// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The post-signup "Save your recovery phrase" screen, as a guided flow
 * (docs/ux/client-redesign.md section 5.6):
 *
 *   1. the 12 numbered words, reading across the rows (3 x 4 at phone
 *      width, 4 x 3 on a wide screen), Copy and a short explanation;
 *   2. a check: three words at random positions, typed back from the notes,
 *      plus the acknowledgement, before Done;
 *   3. where passkeys are supported, an offer to add one
 *      (docs/ux/identity-recovery.md section 9, step 3): "Add a passkey" or
 *      "Not now". Adding one continues into the opt-in keys choice
 *      (`PasskeyKeysChoice`) when the browser can protect keys with it. The
 *      step never blocks signup: every outcome, a failure included, leaves
 *      "Not now" available.
 *
 * The phrase is acknowledged when the last step ends, so a reload in the
 * middle shows the phrase again from step 1.
 *
 * It is rendered by the route GUARDS, from `auth.signupPhrase()`, for as long
 * as that phrase is unacknowledged, whichever route the user is on and again
 * after a reload; the decision lives in `auth/phrase-gate.ts`. The picker and
 * the checker live in `phrase-confirm.ts`.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { AuthLayout } from "../ui/AuthLayout";
import { PhraseGrid } from "../ui/PhraseGrid";
import { WordSlots } from "../ui/WordSlots";
import {
  confirmationComplete,
  confirmationErrorMessage,
  confirmationMismatches,
  listPositions,
  phraseWords,
  pickConfirmationPositions,
} from "./phrase-confirm";
import { t } from "../l10n";
import { addPasskey, type AddedPasskey } from "./passkey-add";
import { PROOF_COPY } from "./fresh-auth";
import { createProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import { PasskeyKeysChoice, type KeysChoiceResult } from "./PasskeyKeysChoice";
import { useWebauthnClient } from "./use-webauthn-client";
import { WebauthnClient, WebauthnUserCancelledError } from "./webauthn-client";

type Step = "show" | "confirm" | "passkey";

const PASSKEYS_SUPPORTED = WebauthnClient.isWebauthnAvailable();

/**
 * Render the pending signup phrase. Acknowledging it clears the phrase; the
 * guard that rendered this then re-decides what to show (the route it wraps,
 * or the `?return=` target on a public auth page).
 */
export default function SignupPhrasePrompt(): JSX.Element {
  const auth = useAuth();
  const words = (): string[] => phraseWords(auth.signupPhrase() ?? "");
  const [step, setStep] = createSignal<Step>("show");
  const [positions, setPositions] = createSignal<number[]>([]);
  const [typed, setTyped] = createSignal<string[]>(["", "", ""]);
  const [acknowledged, setAcknowledged] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // The passkey step needs a browser with passkeys and an account with a
  // username (a passkey is registered under it; a guest account has none).
  const offerPasskey = (): boolean => {
    const s = auth.state();
    return PASSKEYS_SUPPORTED && s.kind === "session" && s.account.username.trim().length > 0;
  };
  const totalSteps = (): number => (offerPasskey() ? 3 : 2);

  function toConfirm(): void {
    setPositions(pickConfirmationPositions(words().length));
    setTyped(["", "", ""]);
    setError(null);
    setStep("confirm");
  }

  function toWords(): void {
    setError(null);
    setStep("show");
  }

  function onDone(e: SubmitEvent): void {
    e.preventDefault();
    const misses = confirmationMismatches(words(), positions(), typed());
    if (misses.length > 0) {
      setError(confirmationErrorMessage(misses));
      setTyped(["", "", ""]);
      return;
    }
    // The words are confirmed. Where passkeys work, offer one before the
    // app opens; otherwise this was the last step.
    if (offerPasskey()) {
      setError(null);
      setStep("passkey");
      return;
    }
    auth.acknowledgeSignupPhrase();
  }

  return (
    <AuthLayout>
      <Show when={step() !== "passkey"} fallback={<PasskeyOffer onDone={() => auth.acknowledgeSignupPhrase()} />}>
      <Show
        when={step() === "confirm"}
        fallback={
          <>
            <h1>{t("recovery_phrase.save.title")}</h1>
            <p class="lk-auth-step">{t("recovery_phrase.step", { current: 1, total: totalSteps() })}</p>
            <p class="lk-auth-lede">{t("recovery_phrase.save.lede")}</p>
            <PhraseGrid words={words} />
            <p class="form-info">{t("recovery_phrase.save.no_screenshot")}</p>
            <button class="primary" type="button" onClick={toConfirm}>
              {t("recovery_phrase.save.written")}
            </button>
          </>
        }
      >
        <h1>{t("recovery_phrase.check.title")}</h1>
        <p class="lk-auth-step">{t("recovery_phrase.step", { current: 2, total: totalSteps() })}</p>
        <p class="lk-auth-lede">
          {t("recovery_phrase.check.lede", { positions: listPositions(positions()) })}
        </p>
        <form onSubmit={onDone}>
          <WordSlots
            positions={positions}
            value={typed}
            setValue={setTyped}
            allowPhrasePaste={false}
          />
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <label class="lk-check">
            <input
              type="checkbox"
              checked={acknowledged()}
              onChange={(e) => setAcknowledged(e.currentTarget.checked)}
            />
            <span>{t("recovery_phrase.check.acknowledge")}</span>
          </label>
          <button
            class="primary"
            type="submit"
            disabled={!acknowledged() || !confirmationComplete(positions(), typed())}
          >
            {offerPasskey() ? t("common.continue") : t("common.done")}
          </button>
          <button type="button" class="ghost" onClick={toWords}>
            {t("recovery_phrase.check.back")}
          </button>
        </form>
      </Show>
      </Show>
    </AuthLayout>
  );
}

/**
 * Step 3: "Add a passkey so you can sign in with your face, fingerprint or
 * screen lock." Skippable, and nothing here can fail the signup: the account
 * exists and its phrase is confirmed.
 */
function PasskeyOffer(props: { readonly onDone: () => void }): JSX.Element {
  const auth = useAuth();
  const { client, username } = useWebauthnClient();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // After the passkey exists: the keys choice, or the line that says it
  // signs in only, or what the choice ended with.
  const [added, setAdded] = createSignal<AddedPasskey | null>(null);
  const [resultKey, setResultKey] = createSignal<string | null>(null);

  // Adding a passkey needs a fresh proof. The signup finish minted one, so
  // nothing is asked here unless the person took more than ten minutes over
  // the phrase; then the password field appears.
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.addPasskey,
    offerPasskey: false,
    collapsed: true,
    busy,
    sessionToken: () => null,
    usernameKnown: () => (username() ?? "").trim().length > 0,
  });

  async function onAdd(): Promise<void> {
    const c = client();
    const name = username();
    const st = auth.state();
    const secrets = auth.identitySecrets();
    if (c === null || name === null || name.length === 0) {
      setError(t("common.error.session_not_ready"));
      return;
    }
    setError(null);
    setBusy(true);
    let grant: string | null = null;
    try {
      grant = await gate.ensure();
      if (grant === null) return;
      const passkey = await addPasskey({
        client: c,
        username: name,
        freshGrant: grant,
        nickname: t("auth.signup.passkey.default_name"),
        account:
          st.kind === "session" && secrets !== null
            ? { accountIdBytes: secrets.accountId, accountIdB64: st.account.accountId }
            : null,
      });
      if (passkey.outcome === "offer-keys-choice") setAdded(passkey);
      else setResultKey("settings.passkeys.added_no_keys");
    } catch (err) {
      const failure = gate.failed(err, grant).kind;
      if (
        !(err instanceof WebauthnUserCancelledError) &&
        failure !== "required" &&
        failure !== "rate-limited" &&
        failure !== "offline"
      ) {
        // eslint-disable-next-line no-console
        console.warn("[signup] adding a passkey failed", err);
        setError(t("auth.signup.passkey.error"));
      }
    } finally {
      setBusy(false);
    }
  }

  function onChoice(result: KeysChoiceResult): void {
    setAdded(null);
    switch (result) {
      case "stored":
        setResultKey("settings.passkeys.keys_choice.done");
        break;
      case "no-prf":
        setResultKey("settings.passkeys.keys_choice.no_prf");
        break;
      case "unavailable":
        setResultKey("settings.passkeys.keys_choice.unavailable");
        break;
      case "signin-only":
        props.onDone();
        break;
    }
  }

  return (
    <>
      <h1>{t("auth.signup.passkey.title")}</h1>
      <p class="lk-auth-step">{t("recovery_phrase.step", { current: 3, total: 3 })}</p>
      <Show
        when={resultKey()}
        fallback={
          <Show
            when={added()}
            fallback={
              <>
                <p class="lk-auth-lede">{t("auth.signup.passkey.lede")}</p>
                <Show when={error() !== null}>
                  <div class="form-error" role="alert">
                    {error()}
                  </div>
                </Show>
                <ProofBlock gate={gate} />
                <button
                  class="primary"
                  type="button"
                  disabled={busy() || !gate.view().ready}
                  onClick={() => void onAdd()}
                >
                  {busy() ? t("recovery.passkey.waiting") : t("auth.signup.passkey.add")}
                </button>
                <button type="button" class="ghost" disabled={busy()} onClick={props.onDone}>
                  {t("common.not_now")}
                </button>
              </>
            }
          >
            {(passkey) => (
              <PasskeyKeysChoice
                credentialIdB64={passkey().credentialIdB64}
                nickname=""
                prfOutput={passkey().prfOutput}
                client={client}
                onDone={onChoice}
              />
            )}
          </Show>
        }
      >
        {(key) => (
          <>
            <p class="form-info" role="status">
              {t(key())}
            </p>
            <button class="primary" type="button" onClick={props.onDone}>
              {t("common.continue")}
            </button>
          </>
        )}
      </Show>
    </>
  );
}
