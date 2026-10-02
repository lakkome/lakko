// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Let this passkey restore your keys?" (docs/ux/identity-recovery.md 10.2,
 * ADR-0064 decision 6): the choice shown after a passkey is added, and from
 * a passkey's row later. Used by Settings > Security > Passkeys and by the
 * passkey offer at the end of signup.
 *
 * Storing the keys for a passkey is OPT-IN, per passkey, and informed: the
 * safety of the keys then also rests on whoever syncs the passkey. Nothing
 * is stored unless the person presses "Turn on"; the two buttons carry the
 * same weight and neither is the default.
 *
 * Turning it on needs the recovery phrase on this device (read from the
 * device, else typed and checked against the identity this device holds)
 * and the passkey's secret: from the registration when the browser gave it
 * there, else from one more passkey prompt. That secret is never sent to
 * the server.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { AuthClient, AuthHttpError } from "./client";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "./account-id";
import { readDevicePhrase } from "../backup/device-phrase";
import { enrollPasskeyVault, PASSKEY_PRF_SALT } from "../recovery/passkey-vault";
import { PhraseInput } from "../ui/PhraseInput";
import { loadWasm } from "../wasm";
import { t } from "../l10n";
import { WebauthnUserCancelledError, type WebauthnClient } from "./webauthn-client";

/** How the choice ended. */
export type KeysChoiceResult =
  /** The keys are stored for the passkey. */
  | "stored"
  /** The person chose sign-in only; nothing was stored. */
  | "signin-only"
  /** The passkey gave no secret to protect keys with. */
  | "no-prf"
  /** The server does not store keys for passkeys. */
  | "unavailable";

export function PasskeyKeysChoice(props: {
  readonly credentialIdB64: string;
  readonly nickname: string;
  /** What the registration yielded, when it yielded it. */
  readonly prfOutput: Uint8Array | null;
  readonly client: () => WebauthnClient | null;
  readonly onDone: (result: KeysChoiceResult) => void;
  /** Reports the working state, so the host can disable its own controls. */
  readonly onBusy?: (busy: boolean) => void;
}): JSX.Element {
  const auth = useAuth();
  const [step, setStep] = createSignal<"choice" | "phrase">("choice");
  const [typedPhrase, setTypedPhrase] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusyRaw] = createSignal(false);
  const setBusy = (b: boolean): void => {
    setBusyRaw(b);
    props.onBusy?.(b);
  };

  // "Turn on": store the keys for this passkey. Every refusal leaves the
  // passkey "Signs in only".
  async function onTurnOn(): Promise<void> {
    const c = props.client();
    const st = auth.state();
    const secrets = auth.identitySecrets();
    if (c === null || st.kind !== "session" || secrets === null) {
      setError(t("common.error.session_not_ready"));
      return;
    }
    setError(null);
    setBusy(true);
    try {
      const wasm = await loadWasm();
      // 1. The phrase: stored on this device, else typed and checked against
      //    the identity this device holds (the account's own keys).
      let phrase = await readDevicePhrase(st.account.accountId);
      if (phrase === null) {
        if (step() !== "phrase") {
          setStep("phrase");
          return;
        }
        let seed: Uint8Array;
        try {
          const mnemonic = wasm.RecoveryMnemonic.fromPhrase(typedPhrase().trim());
          seed = mnemonic.toSeed();
          mnemonic.free();
        } catch {
          setError(t("restore_identity.error.invalid_phrase"));
          return;
        }
        const same = sameBytes(seed, secrets.accountIdentitySeed);
        seed.fill(0);
        if (!same) {
          setError(t("settings.passkeys.keys_choice.error.phrase_mismatch"));
          return;
        }
        phrase = typedPhrase().trim();
      } else {
        // A stored phrase is held to the same check as a typed one: its
        // seed must be the identity this session holds, or nothing is
        // sealed (a phrase replaced on another device may still sit here).
        let seed: Uint8Array;
        try {
          const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phrase);
          seed = mnemonic.toSeed();
          mnemonic.free();
        } catch {
          setError(t("settings.passkeys.keys_choice.error.phrase_mismatch"));
          return;
        }
        const same = sameBytes(seed, secrets.accountIdentitySeed);
        seed.fill(0);
        if (!same) {
          setError(t("settings.passkeys.keys_choice.error.phrase_mismatch"));
          return;
        }
      }
      // 2. The passkey's secret: from the registration, else one more
      //    passkey prompt for this credential only.
      let prfOutput = props.prfOutput;
      if (prfOutput === null) {
        const extras = await c.evaluatePrfFor(props.credentialIdB64, PASSKEY_PRF_SALT);
        if (base64UrlEncodeNoPad(extras.credentialId) !== props.credentialIdB64) {
          setError(t("settings.passkeys.keys_choice.error.other_passkey"));
          return;
        }
        prfOutput = extras.prfOutput;
      }
      if (prfOutput === null) {
        props.onDone("no-prf");
        return;
      }
      // 3. Seal and store.
      const stored = await enrollPasskeyVault({
        wasm,
        client: new AuthClient(),
        sessionToken: secrets.sessionToken,
        credentialId: base64UrlDecodeNoPad(props.credentialIdB64),
        prfOutput,
        phrase,
      });
      props.onDone(stored ? "stored" : "unavailable");
    } catch (err) {
      if (err instanceof WebauthnUserCancelledError) {
        // Cancelled in the browser's sheet: the passkey stays sign-in only.
        setError(t("settings.passkeys.keys_choice.cancelled"));
      } else if (err instanceof AuthHttpError && err.status === 503) {
        props.onDone("unavailable");
      } else {
        // eslint-disable-next-line no-console
        console.warn("[passkeys] turning on restoring keys failed", err);
        setError(t("settings.passkeys.error.generic"));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="lk-alert info" role="group" aria-labelledby="lk-passkey-keys-choice">
      <div class="lk-stack">
        <strong id="lk-passkey-keys-choice">{t("settings.passkeys.keys_choice.title")}</strong>
        <Show when={props.nickname.length > 0}>
          <span class="lk-muted">{props.nickname}</span>
        </Show>
        <p>{t("settings.passkeys.keys_choice.body_on")}</p>
        <p>{t("settings.passkeys.keys_choice.body_off")}</p>
        <p class="lk-muted">{t("settings.passkeys.keys_choice.later")}</p>
        <Show when={step() === "phrase"}>
          <p>{t("settings.passkeys.keys_choice.phrase_needed")}</p>
          <PhraseInput value={typedPhrase} setValue={setTypedPhrase} disabled={busy} />
        </Show>
        <Show when={error() !== null}>
          <div class="form-error" role="alert">
            {error()}
          </div>
        </Show>
        {/* Same weight, no default: the person must pick one. */}
        <div class="lk-actions">
          <button type="button" disabled={busy()} onClick={() => void onTurnOn()}>
            {busy() ? t("common.checking") : t("settings.passkeys.keys_choice.turn_on")}
          </button>
          <button type="button" disabled={busy()} onClick={() => props.onDone("signin-only")}>
            {t("settings.passkeys.keys_choice.signin_only")}
          </button>
        </div>
      </div>
    </div>
  );
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}
