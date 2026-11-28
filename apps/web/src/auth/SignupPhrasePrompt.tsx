// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save your recovery phrase": new recovery words, as a guided flow
 * (docs/ux/client-redesign.md section 5.6). Shown after the account keys
 * were changed on this device (a key reset, taking the account back, a key
 * rotation, "Save a recovery phrase" of ADR-0075), never by a sign-up: the
 * phrase is not shown at sign-up any more (ADR-0075 D2), and the file keeps
 * its name from before that decision.
 *
 *
 *   1. the 12 numbered words, reading across the rows (3 x 4 at phone
 *      width, 4 x 3 on a wide screen), Copy and a short explanation;
 *   2. a check: three words at random positions, typed back from the notes,
 *      plus the acknowledgement, before Done.
 *
 * No passkey offer follows: section 9's step 3 belonged to the phrase at
 * sign-up, which section 19 (ADR-0075) retired; a passkey is one of the
 * choices of "Protect your account" (19.1a, `PasskeyOffer` below).
 *
 * The phrase is acknowledged when the check ends, so a reload in the middle
 * shows the phrase again from step 1.
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
import { phraseWords } from "./phrase-confirm";
import { PhraseCheckForm } from "./PhraseCheckForm";
import { t } from "../l10n";
import { addPasskey } from "./passkey-add";
import { PROOF_COPY } from "./fresh-auth";
import { createProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import { addedPasskeyMessage, storeKeysForPasskey, type StoreKeysOutcome } from "./passkey-keys-store";
import { addNoPrfPasskey } from "./passkey-no-prf";
import { accountFactors, notePasskeyCount, refreshFactors } from "./protect/factors";
import { notePasskeyAdded } from "./protect/password-only-state";
import { useWebauthnClient } from "./use-webauthn-client";
import {
  WebauthnCredentialExistsError,
  WebauthnUserCancelledError,
} from "./webauthn-client";

type Step = "show" | "confirm";

const TOTAL_STEPS = 2;

/**
 * Render the pending new words. Acknowledging them clears the phrase; the
 * guard that rendered this then re-decides what to show (the route it wraps,
 * or the `?return=` target on a public auth page).
 */
export default function SignupPhrasePrompt(): JSX.Element {
  const auth = useAuth();
  const words = (): string[] => phraseWords(auth.signupPhrase() ?? "");
  const [step, setStep] = createSignal<Step>("show");

  return (
    <AuthLayout>
      <Show
        when={step() === "confirm"}
        fallback={
          <>
            <h1>{t("recovery_phrase.save.title")}</h1>
            <p class="lk-auth-step">{t("recovery_phrase.step", { current: 1, total: TOTAL_STEPS })}</p>
            <p class="lk-auth-lede">{t("recovery_phrase.save.lede")}</p>
            <PhraseGrid words={words} />
            <p class="form-info">{t("recovery_phrase.save.no_screenshot")}</p>
            <button class="primary" type="button" onClick={() => setStep("confirm")}>
              {t("recovery_phrase.save.written")}
            </button>
          </>
        }
      >
        <h1>{t("recovery_phrase.check.title")}</h1>
        <PhraseCheckForm
          words={words()}
          stepLabel={t("recovery_phrase.step", { current: 2, total: TOTAL_STEPS })}
          submitLabel={t("common.done")}
          onConfirmed={() => auth.acknowledgeSignupPhrase()}
          onBack={() => setStep("show")}
        />
      </Show>
    </AuthLayout>
  );
}

/**
 * "Create a passkey" of the "Protect your account" step (ADR-0075, UX
 * 19.1a): "Add a passkey so you can sign in with your face, fingerprint or
 * screen lock." Back returns to the step; nothing here can fail the account.
 */
export function PasskeyOffer(props: {
  readonly onDone: () => void;
  /** A passkey now exists for the account (whatever storing its keys
   *  then comes to). */
  readonly onAdded?: () => void;
  /** The system passkey sheet was closed: nothing was added (UX 19.1a: back
   *  on the step with `protect.passkey.cancelled`). Without it the offer
   *  simply stays as it was. */
  readonly onCancelled?: () => void;
}): JSX.Element {
  const auth = useAuth();
  const { client, username } = useWebauthnClient();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // After the passkey exists: the line that says what storing its keys
  // came to (UX 10.2).
  const [result, setResult] = createSignal<{
    readonly kind: "ok" | "error";
    readonly key: string;
    readonly named?: true;
  } | null>(null);

  // Adding a passkey needs a fresh proof. When none is live (a key change
  // spends the one it was made with) the password field appears.
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
      props.onAdded?.();
      // ADR-0101 section 10 (UX 19.1d): the account's protection is published
      // with this passkey in it when it gave a secret for that; whether it
      // keeps the chats is what the notice and the passkey's row then say.
      if (st.kind === "session") {
        const keepsChats = await auth.passkeySealAdded({
          credentialIdB64: passkey.credentialIdB64,
          sealPrfOutput: passkey.sealPrfOutput,
        });
        notePasskeyAdded(st.account.accountId, keepsChats);
        const current = accountFactors(st.account.accountId);
        notePasskeyCount(st.account.accountId, (current?.passkeys ?? 0) + 1);
        if (secrets !== null) void refreshFactors(st.account.accountId, secrets.sessionToken);
      }
      let outcome: StoreKeysOutcome = "no-prf";
      if (passkey.outcome === "store-keys") {
        outcome =
          st.kind === "session" && secrets !== null
            ? await storeKeysForPasskey({
                client: c,
                accountIdB64: st.account.accountId,
                accountSeed: secrets.accountIdentitySeed,
                sessionToken: secrets.sessionToken,
                credentialIdB64: passkey.credentialIdB64,
                prfOutput: passkey.prfOutput,
              })
            : "failed";
      }
      if (outcome === "no-prf" && st.kind === "session") {
        // Remembered on the device, so Settings does not offer the repair.
        try {
          addNoPrfPasskey(
            typeof localStorage === "undefined" ? undefined : localStorage,
            st.account.accountId,
            passkey.credentialIdB64,
          );
        } catch {
          // No storage here: Settings offers the repair, which answers the same.
        }
      }
      setResult(addedPasskeyMessage(outcome));
    } catch (err) {
      const failure = gate.failed(err, grant).kind;
      if (err instanceof WebauthnUserCancelledError) {
        props.onCancelled?.();
      } else if (err instanceof WebauthnCredentialExistsError) {
        // The provider already holds a passkey for this account (UX 10.2).
        setError(t("settings.passkeys.error.already_exists"));
      } else if (
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

  return (
    <>
      <h1>{t("auth.signup.passkey.title")}</h1>
      <Show
        when={result()}
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
              {t("common.back")}
            </button>
          </>
        }
      >
        {(line) => (
          <>
            <p class={line().kind === "ok" ? "form-info" : "form-error"} role="status">
              {line().named === true
                ? t(line().key, { name: t("auth.signup.passkey.default_name") })
                : t(line().key)}
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
