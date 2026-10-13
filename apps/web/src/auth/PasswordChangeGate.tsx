// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The step in front of the new-password form on a device that holds the
 * keys (docs/ux/identity-recovery.md sections 10.3 and 17.8).
 *
 * The server authorises the password replace by the keys this device
 * holds, so a signed-in session alone would be enough for it; whoever held
 * the unlocked device could then set a password and pass every other check.
 * This client asks first for one of:
 *
 *   - a fresh-auth proof: the current password, or a passkey (the shared
 *     proof block);
 *   - three words of the recovery phrase, checked locally on this device,
 *     for the person who forgot the password (the same local budget as the
 *     three-word sign-in: five misses, then a 15-minute pause).
 *
 * Whether the form may be shown is `passwordChangeAllowed` in
 * `fresh-auth.ts`; `AuthContext.replacePassword` enforces the same rule.
 */
import {
  createEffect,
  createResource,
  createSignal,
  onCleanup,
  onMount,
  Show,
  untrack,
  type JSX,
} from "solid-js";

import { useAuth } from "./AuthContext";
import { threeWordsShown } from "./protect/account-protection";
import { phraseProtectedHere } from "./protect/protection-state";
import { screenText } from "./auth-error-copy";
import { PROOF_COPY } from "./fresh-auth";
import { listPositions } from "./phrase-confirm";
import { createProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import { WaitNotice } from "./recover/FormNotices";
import { createRetryCountdown } from "./recover/retry-countdown";
import {
  afterMiss,
  budgetAt,
  clearBudget,
  loadBudget,
  pauseRemainingMs,
  saveBudget,
  THREE_WORDS,
  triesLeft,
  type ThreeWordsBudget,
} from "./recover/three-words";
import { isBip39Word } from "../recovery/bip39-words";
import { WordSlots } from "../ui/WordSlots";
import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { loadWasm } from "../wasm";

const EMPTY: string[] = Array.from({ length: THREE_WORDS }, () => "");

function storage(): Storage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}

export function PasswordChangeGate(props: {
  /** The new-password form, shown once the gate is open. */
  readonly children: JSX.Element;
  /** A second button beside the gate's own (Cancel / Not now), when given. */
  readonly onSkip?: () => void;
  readonly skipLabel?: string;
}): JSX.Element {
  const auth = useAuth();
  // The answer depends on the clock, so it is polled: when the ten minutes
  // run out the gate comes back.
  const [allowed, setAllowed] = createSignal(auth.passwordChangeAllowed());
  const poll = setInterval(() => setAllowed(auth.passwordChangeAllowed()), 1000);
  onCleanup(() => clearInterval(poll));
  const [mode, setMode] = createSignal<"proof" | "words">("proof");
  const wordsOffered = (): boolean => {
    const s = auth.state();
    return threeWordsShown(phraseProtectedHere(s.kind === "session" ? s.account.accountId : null));
  };
  const [busy, setBusy] = createSignal(false);

  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.changePassword,
    busy,
    sessionToken: () => (allowed() ? null : (auth.identitySecrets()?.sessionToken ?? null)),
    usernameKnown: () => {
      const s = auth.state();
      return s.kind === "session" && s.account.username.trim().length > 0;
    },
  });
  // `Use a passkey instead` completes the proof on its own.
  createEffect(() => {
    if (gate.proofLive()) setAllowed(auth.passwordChangeAllowed());
  });

  async function onProve(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setBusy(true);
    try {
      if ((await gate.ensure()) !== null) setAllowed(auth.passwordChangeAllowed());
    } finally {
      setBusy(false);
    }
  }

  const skip = (): JSX.Element => (
    <Show when={props.onSkip}>
      {(onSkip) => (
        <button type="button" class="ghost" disabled={busy()} onClick={() => onSkip()()}>
          {props.skipLabel ?? t("recovery.new_password.not_now")}
        </button>
      )}
    </Show>
  );

  return (
    <Show when={!allowed()} fallback={props.children}>
      <Show
        when={mode() === "proof"}
        fallback={
          <WordsCheck
            onChecked={() => setAllowed(auth.passwordChangeAllowed())}
            onKnowPassword={() => setMode("proof")}
            skip={skip}
          />
        }
      >
        <form onSubmit={(e) => void onProve(e)} class="lk-stack">
          <ProofBlock gate={gate}>
            {/* ADR-0075 section 11: three words exist only for a
                phrase-protected account. */}
            <Show when={wordsOffered()}>
            <div>
              <button
                type="button"
                class="ghost"
                disabled={busy()}
                onClick={() => {
                  gate.clear();
                  setMode("words");
                }}
              >
                {t("proof.change_password.use_words")}
              </button>
            </div>
            </Show>
          </ProofBlock>
          <div class="lk-actions">
            <button class="primary" type="submit" disabled={busy() || !gate.view().ready}>
              {busy() ? t("common.checking") : t("common.continue")}
            </button>
            {skip()}
          </div>
        </form>
      </Show>
    </Show>
  );
}

/** Three words of the phrase this device holds, checked here. */
function WordsCheck(props: {
  readonly onChecked: () => void;
  readonly onKnowPassword: () => void;
  readonly skip: () => JSX.Element;
}): JSX.Element {
  const auth = useAuth();
  const accountId = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : "";
  };
  const [wasm] = createResource(loadWasm);
  const [positions, setPositions] = createSignal<number[]>([]);
  const [typed, setTyped] = createSignal<string[]>(EMPTY);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [budget, setBudget] = createSignal<ThreeWordsBudget>(
    budgetAt(loadBudget(storage(), accountId()), Date.now()),
  );
  const pause = createRetryCountdown();

  function draw(): void {
    const core = wasm();
    if (core === undefined) return;
    setPositions(Array.from(core.phraseCheckRandomPositions()));
    setTyped(EMPTY);
  }
  onMount(() => {
    const left = pauseRemainingMs(budget(), Date.now());
    if (left > 0) pause.startUntil(Date.now() + left);
  });
  createEffect(() => {
    if (wasm() !== undefined && untrack(positions).length === 0) draw();
  });
  const ready = (): boolean => positions().length === THREE_WORDS;
  const complete = (): boolean =>
    typed().length === THREE_WORDS && typed().every((w) => w.length > 0 && isBip39Word(w));

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    // Re-read the stored budget: another tab may have spent tries.
    const current = budgetAt(loadBudget(storage(), accountId()), Date.now());
    setBudget(current);
    const left = pauseRemainingMs(current, Date.now());
    if (left > 0) {
      pause.startUntil(Date.now() + left);
      return;
    }
    if (!complete()) return;
    setBusy(true);
    try {
      const matched = await auth.checkPhraseWords(Uint8Array.from(positions()), typed());
      if (!matched) {
        const next = afterMiss(budget(), Date.now());
        setBudget(next);
        saveBudget(storage(), accountId(), next);
        const wait = pauseRemainingMs(next, Date.now());
        if (wait > 0) pause.startUntil(Date.now() + wait);
        // New positions for every prompt.
        draw();
        setError(t("recovery.words.error.no_match"));
        return;
      }
      clearBudget(storage(), accountId());
      setTyped(EMPTY);
      props.onChecked();
    } catch (err) {
      // The check could not run (no phrase on this device): not a guess.
      setError(screenText(err) ?? t("recovery.words.error.unavailable"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={(e) => void onSubmit(e)} class="lk-stack">
      <Show
        when={!pause.waiting()}
        fallback={<WaitNotice remainingMs={pause.remainingMs} messageKey="recovery.words.paused" />}
      >
        <Show when={ready()} fallback={<p class="lk-muted">{t("common.loading")}</p>}>
          <p>
            {tRich("recovery.words.prompt", {
              positions: <strong>{listPositions(positions())}</strong>,
            })}
          </p>
          <WordSlots positions={positions} value={typed} setValue={setTyped} disabled={busy} />
        </Show>
        <Show when={error() !== null}>
          <div class="form-error" role="alert">
            {error()}
          </div>
        </Show>
        <Show when={budget().misses > 0}>
          <p class="lk-muted" role="status">
            {t("recovery.words.tries_left", { count: triesLeft(budget(), Date.now()) })}
          </p>
        </Show>
      </Show>
      <div>
        <button type="button" class="ghost" disabled={busy()} onClick={props.onKnowPassword}>
          {t("proof.know_password_link")}
        </button>
      </div>
      <div class="lk-actions">
        <button
          class="primary"
          type="submit"
          disabled={busy() || !ready() || !complete() || pause.waiting()}
        >
          {busy() ? t("common.checking") : t("common.continue")}
        </button>
        {props.skip()}
      </div>
    </form>
  );
}
