// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (docs/ux/identity-recovery.md section 19.5,
 * ADR-0075 section 3 as amended 2026-11-01). `GET /account/phrase` says which
 * of two ways this is (`save-phrase.ts`), before anything else is drawn:
 *
 *   - the words this device already holds, on an account whose phrase was
 *     never in an email vault. No key changes: the grid, the three-word
 *     check, then `POST /account/phrase/saved`. The account counts as
 *     protected only when that call answered; a person who leaves before
 *     then has saved nothing.
 *   - the ADR-0074 key rotation, with no device removed and
 *     `phrase_saved: true` on its commit: the account gets new keys and a new
 *     phrase, every device stays signed in, the server deletes the email
 *     vault and the account is phrase-protected from then on. The NEW words
 *     are shown only once the rotation is complete; the existing phrase
 *     screen comes up through the route guards (`auth.signupPhrase`). Until
 *     then a progress line; a failure before the commit changed nothing.
 */
import { plainStore } from "./local";
import { createSignal, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { PROOF_COPY } from "../fresh-auth";
import { phraseWords } from "../phrase-confirm";
import { PhraseCheckForm } from "../PhraseCheckForm";
import { createProofGate } from "../proof-gate";
import { ProofBlock } from "../ProofBlock";
import { WaitNotice } from "../recover/FormNotices";
import { readDevicePhrase } from "../../backup/device-phrase";
import { useKeyRotation } from "../../devices/key-rotation-runtime";
import { rotationCommitted, rotationSavesPhrase } from "../../devices/key-rotation";
import { t } from "../../l10n";
import { PhraseGrid } from "../../ui/PhraseGrid";
import { loadWasm } from "../../wasm";
import { consumeSelfMarks, markOwnAction, type SelfMark } from "./notices";
import { notePhraseSaved, refreshProtection } from "./protection-state";
import { RecoveryEmailClient } from "./recovery-email-client";
import {
  phraseOpensAccount,
  phraseSavedBy,
  phraseSavedLeftNoNotice,
  phraseSavedVerdict,
  readSavePath,
  savePhraseView,
  type SavePath,
  type SavePhraseView,
} from "./save-phrase";

/** Where the words path is. */
type WordsStage = "intro" | "words" | "check";

export function SavePhrasePanel(props: {
  /** The phrase was saved, or the keys were changed (the words follow by
   *  themselves): the surface that hosts the panel closes. */
  readonly onSaved: () => void;
  readonly onCancel?: () => void;
  readonly cancelLabel?: string;
  /** An extra line under the effect (UX 19.6: "so whoever opened it loses
   *  it"). It speaks of the new keys, so it goes with them. */
  readonly extraLine?: string;
}): JSX.Element {
  const auth = useAuth();
  const rotation = useKeyRotation();
  const [busy, setBusy] = createSignal(false);
  const [view, setView] = createSignal<SavePhraseView | null>(null);
  // `null` until the server has said. Never guessed.
  const [path, setPath] = createSignal<SavePath | null>(null);
  const [pathFailed, setPathFailed] = createSignal(false);
  const [stage, setStage] = createSignal<WordsStage>("intro");
  const [saveFailed, setSaveFailed] = createSignal(false);
  // The notice `POST /account/phrase/saved` writes is this device's own.
  let ownMark: SelfMark | null = null;

  const account = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.savePhrase,
    busy,
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    usernameKnown: () => {
      const s = auth.state();
      return s.kind === "session" && s.account.username.trim().length > 0;
    },
  });

  // A rotation that was started here and never reached its commit is
  // forgotten when the surface closes: nothing was changed.
  onCleanup(() => {
    gate.clear();
    setPath(null);
    const r = rotation.record();
    if (r !== null && rotationSavesPhrase(r) && !rotationCommitted(r) && !busy()) void rotation.notNow();
  });

  /** The keys were changed on a run of this panel, or of an earlier one. */
  const committed = (): boolean => {
    const r = rotation.record();
    return r !== null && rotationSavesPhrase(r) && rotationCommitted(r);
  };

  /** The phrase this device holds, when it opens the account as it is now. */
  async function heldPhrase(accountIdB64: string, accountSeed: Uint8Array): Promise<string | null> {
    const phrase = await readDevicePhrase(accountIdB64);
    if (phrase === null) return null;
    return phraseOpensAccount(await loadWasm(), phrase, accountSeed) ? phrase : null;
  }

  async function readPath(): Promise<void> {
    const acct = account();
    const secrets = auth.identitySecrets();
    // A rotation past its commit finishes whatever the server says now.
    if (committed()) {
      setPath({ kind: "rotation" });
      return;
    }
    setPathFailed(false);
    if (acct === null || secrets === null) {
      setPathFailed(true);
      return;
    }
    setBusy(true);
    try {
      setPath(
        await readSavePath({
          status: () => new RecoveryEmailClient().phraseStatus(secrets.sessionToken),
          heldPhrase: () => heldPhrase(acct, secrets.accountIdentitySeed),
        }),
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("reading how to save the phrase failed", err instanceof Error ? err.name : "unknown");
      setPathFailed(true);
    } finally {
      setBusy(false);
    }
  }
  onMount(() => void readPath());

  async function onShowRotation(): Promise<void> {
    const acct = account();
    if (acct === null || busy()) return;
    setView(null);
    setBusy(true);
    let grant: string | null = null;
    try {
      grant = await gate.ensure();
      if (grant === null) return;
      // The commit writes the "recovery phrase saved" notice: remembered
      // first, so this device is not told about its own action.
      markOwnAction(plainStore(), acct, "recovery-phrase-saved", Date.now());
      const result = await rotation.savePhrase(grant);
      const outcome = result.kind === "ran" ? result.outcome : null;
      const next = savePhraseView(outcome);
      if (result.kind === "ran" && (next.kind === "proof" || next.kind === "rate-limited")) {
        // Drops a refused grant and shows the block, or starts the countdown.
        gate.failed(result.error, grant);
      }
      if (phraseSavedBy(outcome)) {
        notePhraseSaved(acct);
        const token = auth.identitySecrets()?.sessionToken ?? null;
        if (token !== null) void refreshProtection(acct, token);
      }
      setView(next);
      if (next.kind === "done") props.onSaved();
    } finally {
      setBusy(false);
    }
  }

  /** The proof, then the words the device holds. */
  async function onShowWords(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    try {
      if ((await gate.ensure()) !== null) setStage("words");
    } finally {
      setBusy(false);
    }
  }

  /** The three words matched: tell the server. Nothing counts before this
   *  has its answer. */
  async function onWordsConfirmed(): Promise<void> {
    const acct = account();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (busy()) return;
    setSaveFailed(false);
    if (acct === null || token === null) {
      setSaveFailed(true);
      return;
    }
    setBusy(true);
    try {
      const grant = await gate.ensure();
      if (grant === null) return;
      // Remembered first, as on the rotation path; one mark however many
      // tries, since one notice is written.
      if (ownMark === null) {
        ownMark = { kind: "recovery-phrase-saved", atMs: Date.now() };
        markOwnAction(plainStore(), acct, ownMark.kind, ownMark.atMs);
      }
      let failure: unknown = null;
      try {
        await new RecoveryEmailClient().markPhraseSaved(token, grant);
      } catch (err) {
        failure = err ?? new Error("refused");
      }
      const verdict = phraseSavedVerdict(failure);
      if (phraseSavedLeftNoNotice(verdict)) {
        consumeSelfMarks(plainStore(), acct, [ownMark]);
        ownMark = null;
      }
      switch (verdict) {
        case "saved":
          notePhraseSaved(acct);
          void refreshProtection(acct, token);
          props.onSaved();
          return;
        case "needs-rotation":
          // The phrase was in a vault after all: the person is told about
          // the new keys and goes on from there.
          setStage("intro");
          setPath({ kind: "rotation" });
          return;
        case "proof":
          gate.failed(failure, grant);
          return;
        case "refused":
        case "unknown":
          gate.failed(failure, grant);
          setSaveFailed(true);
          return;
      }
    } finally {
      setBusy(false);
    }
  }

  const onShow = (): void => void (path()?.kind === "words" ? onShowWords() : onShowRotation());
  const words = (): string[] => {
    const p = path();
    return p?.kind === "words" ? phraseWords(p.phrase) : [];
  };
  const rotates = (): boolean => path()?.kind === "rotation";
  const failed = (): boolean => view()?.kind === "failed" || pathFailed();
  const finishing = (): boolean => view()?.kind === "finishing" || (committed() && view()?.kind !== "done");
  const buttonLabel = (): string =>
    failed() ? t("common.try_again") : t("protect.phrase.save.show");

  return (
    <div class="lk-stack">
      <Switch>
        <Match when={stage() === "words"}>
          <p class="lk-auth-step">{t("recovery_phrase.step", { current: 1, total: 2 })}</p>
          <p class="lk-auth-lede">{t("recovery_phrase.save.lede")}</p>
          <PhraseGrid words={words} />
          <p class="form-info">{t("recovery_phrase.save.no_screenshot")}</p>
          <div class="lk-actions">
            <button class="primary" type="button" onClick={() => setStage("check")}>
              {t("recovery_phrase.save.written")}
            </button>
            <Show when={props.onCancel}>
              {(cancel) => (
                <button type="button" class="ghost" onClick={() => cancel()()}>
                  {props.cancelLabel ?? t("common.cancel")}
                </button>
              )}
            </Show>
          </div>
        </Match>
        <Match when={stage() === "check"}>
          <PhraseCheckForm
            words={words()}
            stepLabel={t("recovery_phrase.step", { current: 2, total: 2 })}
            submitLabel={saveFailed() ? t("common.try_again") : t("common.done")}
            failure={saveFailed() ? t("protect.phrase.save.failed") : null}
            busy={busy() || gate.wait.waiting()}
            onConfirmed={() => void onWordsConfirmed()}
            onBack={() => {
              setSaveFailed(false);
              setStage("words");
            }}
          >
            <ProofBlock gate={gate} />
            <Show when={!gate.view().visible}>
              <WaitNotice remainingMs={gate.wait.remainingMs} />
            </Show>
          </PhraseCheckForm>
        </Match>
        <Match when={true}>
          <p class="lk-muted">{t("protect.phrase.save.body")}</p>
          {/* Only what is true of this account: most accounts change no key. */}
          <Show when={rotates()}>
            <p class="lk-muted">{t("protect.phrase.save.effect")}</p>
            <Show when={props.extraLine}>{(line) => <p class="lk-muted">{line()}</p>}</Show>
          </Show>
          <Show
            when={!finishing()}
            fallback={
              // The keys are changed; the rest finishes by itself and the words
              // come up when it has.
              <p class="form-info" role="status" aria-live="polite">
                <span class="lakko-spinner" aria-hidden="true" /> {t("protect.phrase.save.finishing")}
              </p>
            }
          >
            <ProofBlock gate={gate} />
            <Show when={failed()}>
              <div class="form-error" role="alert">
                {t("protect.phrase.save.failed")}
              </div>
            </Show>
            <Show when={view()?.kind === "rate-limited" && !gate.wait.waiting()}>
              <div class="form-error" role="alert">
                {t("common.error.too_many_attempts")}
              </div>
            </Show>
            {/* The block draws the countdown itself while it is on screen. */}
            <Show when={!gate.view().visible}>
              <WaitNotice remainingMs={gate.wait.remainingMs} />
            </Show>
            <div class="lk-actions">
              <Show
                when={!pathFailed()}
                fallback={
                  <button class="primary" type="button" disabled={busy()} onClick={() => void readPath()}>
                    {buttonLabel()}
                  </button>
                }
              >
                <button
                  class="primary"
                  type="button"
                  disabled={busy() || path() === null || !gate.view().ready || gate.wait.waiting()}
                  onClick={onShow}
                >
                  <Show when={busy()} fallback={buttonLabel()}>
                    <span class="lakko-spinner" aria-hidden="true" />{" "}
                    {rotates() ? t("protect.phrase.save.progress") : t("protect.phrase.save.show")}
                  </Show>
                </button>
              </Show>
              <Show when={props.onCancel}>
                {(cancel) => (
                  <button type="button" class="ghost" disabled={busy()} onClick={() => cancel()()}>
                    {props.cancelLabel ?? t("common.cancel")}
                  </button>
                )}
              </Show>
            </div>
          </Show>
        </Match>
      </Switch>
    </div>
  );
}
