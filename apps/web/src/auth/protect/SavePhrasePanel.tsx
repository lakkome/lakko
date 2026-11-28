// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (docs/ux/identity-recovery.md section 19.5 as
 * amended 2026-11-26; ADR-0075, second amendment).
 *
 * No key changes. The screen shows the 12 words this device already holds
 * (behind the proof of 17.1), checks three of them, and then publishes the
 * account's protection again with the phrase in it
 * (`auth.savePhraseSeal()`, ADR-0101 section 10). The account counts as
 * having a saved phrase only when that publish answered; a person who leaves
 * before then has saved nothing. On its failure the three-word screen stays
 * with "We could not finish. Nothing changed." and `Try again`.
 */
import { createSignal, Match, onCleanup, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { PROOF_COPY } from "../fresh-auth";
import { phraseWords } from "../phrase-confirm";
import { PhraseCheckForm } from "../PhraseCheckForm";
import { createProofGate } from "../proof-gate";
import { ProofBlock } from "../ProofBlock";
import { WaitNotice } from "../recover/FormNotices";
import { readDevicePhrase } from "../../backup/device-phrase";
import { t } from "../../l10n";
import { PhraseGrid } from "../../ui/PhraseGrid";
import { loadWasm } from "../../wasm";
import { notePhraseSaved } from "./protection-state";
import {
  phraseOpensAccount,
  savePhraseOutcome,
  wordsToSave,
  type SavePhraseStage,
} from "./save-phrase";

export function SavePhrasePanel(props: {
  /** The phrase was saved: the surface that hosts the panel closes. */
  readonly onSaved: () => void;
  readonly onCancel?: () => void;
  readonly cancelLabel?: string;
}): JSX.Element {
  const auth = useAuth();
  const [busy, setBusy] = createSignal(false);
  const [stage, setStage] = createSignal<SavePhraseStage>("intro");
  const [phrase, setPhrase] = createSignal<string | null>(null);
  // The words could not be shown (this device holds none that open the
  // account) or the publish failed: the line and `Try again`.
  const [showFailed, setShowFailed] = createSignal(false);
  const [saveFailed, setSaveFailed] = createSignal(false);

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
  // The words never outlive the surface.
  onCleanup(() => {
    gate.clear();
    setPhrase(null);
  });

  /** The proof, then the words the device holds. */
  async function onShowWords(): Promise<void> {
    const acct = account();
    const secrets = auth.identitySecrets();
    if (busy()) return;
    setShowFailed(false);
    if (acct === null || secrets === null) {
      setShowFailed(true);
      return;
    }
    setBusy(true);
    try {
      if ((await gate.ensure()) === null) return;
      const words = await wordsToSave({
        heldPhrase: () => readDevicePhrase(acct),
        opensAccount: async (candidate) =>
          phraseOpensAccount(await loadWasm(), candidate, secrets.accountIdentitySeed),
      });
      if (words === null) {
        setShowFailed(true);
        return;
      }
      setPhrase(words);
      setStage("words");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("reading the recovery phrase failed", err instanceof Error ? err.name : "unknown");
      setShowFailed(true);
    } finally {
      setBusy(false);
    }
  }

  /** The three words matched: publish the account's protection with the
   *  phrase in it. Nothing counts before this has its answer. */
  async function onWordsConfirmed(): Promise<void> {
    const acct = account();
    if (busy()) return;
    setSaveFailed(false);
    if (acct === null) {
      setSaveFailed(true);
      return;
    }
    setBusy(true);
    try {
      if (savePhraseOutcome(await auth.savePhraseSeal()) === "failed") {
        setSaveFailed(true);
        return;
      }
      notePhraseSaved(acct);
      setPhrase(null);
      props.onSaved();
    } finally {
      setBusy(false);
    }
  }

  const words = (): string[] => {
    const p = phrase();
    return p === null ? [] : phraseWords(p);
  };

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
            busy={busy()}
            onConfirmed={() => void onWordsConfirmed()}
            onBack={() => {
              setSaveFailed(false);
              setStage("words");
            }}
          />
        </Match>
        <Match when={true}>
          <p class="lk-muted">{t("protect.phrase.save.body")}</p>
          <ProofBlock gate={gate} />
          <Show when={showFailed()}>
            <div class="form-error" role="alert">
              {t("protect.phrase.save.failed")}
            </div>
          </Show>
          {/* The block draws the countdown itself while it is on screen. */}
          <Show when={!gate.view().visible}>
            <WaitNotice remainingMs={gate.wait.remainingMs} />
          </Show>
          <div class="lk-actions">
            <button
              class="primary"
              type="button"
              disabled={busy() || !gate.view().ready || gate.wait.waiting()}
              onClick={() => void onShowWords()}
            >
              <Show when={busy()} fallback={showFailed() ? t("common.try_again") : t("protect.phrase.save.show")}>
                <span class="lakko-spinner" aria-hidden="true" /> {t("protect.phrase.save.show")}
              </Show>
            </button>
            <Show when={props.onCancel}>
              {(cancel) => (
                <button type="button" class="ghost" disabled={busy()} onClick={() => cancel()()}>
                  {props.cancelLabel ?? t("common.cancel")}
                </button>
              )}
            </Show>
          </div>
        </Match>
      </Switch>
    </div>
  );
}
