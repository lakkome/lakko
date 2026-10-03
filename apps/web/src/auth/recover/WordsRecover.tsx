// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Three words from your phrase" (maintainer decision 2026-10-02): sign in
 * on a device that still holds the account's keys when the password is
 * forgotten.
 *
 * The check is local, like a wallet's. This device holds the recovery
 * phrase under its device key; the form asks for the words at three
 * positions drawn at random for this prompt, in the order drawn, and
 * `AuthContext.recoverWithThreeWords` compares them with the stored phrase.
 * No server takes part in the check. On a match the device signs in with
 * the seed it already holds (the recovery login, which leaves the password
 * alone), the guard then shows "Choose a new password", and nothing here
 * navigates.
 *
 * There is no username field: the account is the one cached on this device
 * (the row is only listed when there is one, `three-words.ts`).
 *
 * A miss draws NEW positions and counts against the local budget: five
 * misses, then the route pauses for 15 minutes, with the remaining time
 * shown. The budget is kept per account in `localStorage`, so a reload does
 * not reset it; a success clears it. It is a client-side gate in front of a
 * credential this device already holds, not a server limit (see
 * `three-words.ts`).
 */
import { createEffect, createResource, createSignal, onMount, Show, untrack, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { AuthHttpError } from "../client";
import { authLimitMessage, isOffline, screenText } from "../auth-error-copy";
import { listPositions } from "../phrase-confirm";
import { isBip39Word } from "../../recovery/bip39-words";
import { WordSlots } from "../../ui/WordSlots";
import { loadWasm } from "../../wasm";
import { t } from "../../l10n";
import { tRich } from "../../l10n/rich";
import { MethodUnavailable, WaitNotice } from "./FormNotices";
import { ThreeWordsSuspendedError } from "./stale-phrase";
import { attemptsRetryAfterSecs, createRetryCountdown } from "./retry-countdown";
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
} from "./three-words";

const EMPTY: string[] = Array.from({ length: THREE_WORDS }, () => "");

function storage(): Storage | undefined {
  return typeof localStorage === "undefined" ? undefined : localStorage;
}

export function WordsRecover(props: {
  /** The account cached on this device, as `threeWordsAccount` named it. */
  readonly account: { readonly accountIdB64: string; readonly username: string };
  /** Back to the list of methods. */
  readonly onBack: () => void;
  /** Three words were suspended on this device (the stored seed did not
   *  sign in): back to the list, which no longer offers them. */
  readonly onSuspended: () => void;
}): JSX.Element {
  const auth = useAuth();
  const accountId = props.account.accountIdB64;

  const [wasm] = createResource(loadWasm);
  const [positions, setPositions] = createSignal<number[]>([]);
  const [typed, setTyped] = createSignal<string[]>(EMPTY);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [unavailable, setUnavailable] = createSignal(false);
  // Three words were just suspended on this device: the form is over.
  const [suspended, setSuspended] = createSignal(false);
  const [budget, setBudget] = createSignal<ThreeWordsBudget>(
    budgetAt(loadBudget(storage(), accountId), Date.now()),
  );
  // The local pause after five misses, and a server limit on the sign-in
  // that follows a match, are two different waits with two sentences.
  const pause = createRetryCountdown();
  const serverWait = createRetryCountdown();

  /** Draw three fresh positions, in the order drawn (not sorted). */
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

  // Positions are drawn once the core is loaded.
  createEffect(() => {
    if (wasm() !== undefined && untrack(positions).length === 0) draw();
  });
  const ready = (): boolean => positions().length === THREE_WORDS;

  const complete = (): boolean =>
    typed().length === THREE_WORDS && typed().every((w) => w.length > 0 && isBip39Word(w));

  function recordMiss(): void {
    const next = afterMiss(budget(), Date.now());
    setBudget(next);
    saveBudget(storage(), accountId, next);
    const left = pauseRemainingMs(next, Date.now());
    if (left > 0) pause.startUntil(Date.now() + left);
    // New positions for every prompt: a miss never asks the same three again
    // on purpose.
    draw();
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    // Re-read the stored budget: another tab may have spent tries.
    const current = budgetAt(loadBudget(storage(), accountId), Date.now());
    setBudget(current);
    const left = pauseRemainingMs(current, Date.now());
    if (left > 0) {
      pause.startUntil(Date.now() + left);
      return;
    }
    if (!complete()) return;
    setBusy(true);
    try {
      const matched = await auth.recoverWithThreeWords(Uint8Array.from(positions()), typed());
      if (!matched) {
        recordMiss();
        setError(t("recovery.words.error.no_match"));
        return;
      }
      // Signed in: the guard shows "Choose a new password" next.
      clearBudget(storage(), accountId);
      setTyped(EMPTY);
    } catch (err) {
      // The words matched (or the check could not run); nothing here is a
      // wrong guess, so the budget is not charged.
      const secs = attemptsRetryAfterSecs(err);
      if (err instanceof ThreeWordsSuspendedError) {
        // The phrase stored here did not sign in (most likely a new identity
        // was created on another device). Three words are suspended on this
        // device; the stored phrase itself is untouched.
        setTyped(EMPTY);
        setSuspended(true);
      } else if (secs !== null) {
        serverWait.startFor(secs);
      } else if (err instanceof AuthHttpError && err.status === 503 && err.errorCode !== "rate-limiter-unavailable") {
        setUnavailable(true);
      } else if (err instanceof AuthHttpError && (err.status === 401 || err.status === 404)) {
        // The phrase stored here is no longer the account's recovery
        // credential (a new identity was created on another device).
        setError(t("recovery.words.error.rejected"));
      } else if (isOffline(err)) {
        setError(t("common.error.offline"));
      } else {
        // eslint-disable-next-line no-console
        console.warn("[three-words] sign-in failed", err);
        setError(authLimitMessage(err) ?? screenText(err) ?? t("recovery.words.error.generic"));
      }
    } finally {
      setBusy(false);
    }
  }

  const paused = (): boolean => pause.waiting();
  const missesShown = (): boolean => budget().misses > 0 && !paused();

  return (
    <Show when={!unavailable()} fallback={<MethodUnavailable onBack={props.onBack} />}>
      <Show
        when={!suspended()}
        fallback={
          <div class="lk-stack">
            <p class="form-error" role="alert">
              {t("recovery.words.error.rejected")}
            </p>
            <button type="button" onClick={props.onSuspended}>
              {t("recovery.methods.back_to_list")}
            </button>
          </div>
        }
      >
      <form onSubmit={(e) => void onSubmit(e)}>
        <p class="lk-muted">
          {t("recovery.cached_account", { username: `@${props.account.username}` })}
        </p>
        <Show
          when={!paused()}
          fallback={
            <WaitNotice remainingMs={pause.remainingMs} messageKey="recovery.words.paused" />
          }
        >
          <Show when={ready()} fallback={<p class="lk-muted">{t("common.loading")}</p>}>
            <p>
              {tRich("recovery.words.prompt", {
                positions: <strong>{listPositions(positions())}</strong>,
              })}
            </p>
            <WordSlots positions={positions} value={typed} setValue={setTyped} disabled={busy} />
          </Show>
          <p class="form-info">{t("recovery.words.signin_only")}</p>
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <Show when={missesShown()}>
            <p class="lk-muted" role="status">
              {t("recovery.words.tries_left", { count: triesLeft(budget(), Date.now()) })}
            </p>
          </Show>
          <WaitNotice remainingMs={serverWait.remainingMs} />
          <button
            class="primary"
            type="submit"
            disabled={busy() || !ready() || !complete() || serverWait.waiting()}
          >
            {busy() ? t("common.checking") : t("recovery.words.submit")}
          </button>
        </Show>
      </form>
      </Show>
    </Show>
  );
}
