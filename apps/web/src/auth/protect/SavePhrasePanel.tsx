// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (docs/ux/identity-recovery.md section 19.5,
 * ADR-0075 section 3). It is the ADR-0074 key rotation with no device
 * removed and `phrase_saved: true` on its commit: the account gets new keys
 * and a new phrase, every device stays signed in, the server deletes the
 * email vault and the account is phrase-protected from then on.
 *
 * The NEW words are shown only once the rotation is complete; the existing
 * phrase screen (the grid, the three-word check, "I wrote them down") then
 * comes up through the route guards (`auth.signupPhrase`). Until then a
 * progress line; a failure before the commit changed nothing.
 */
import { plainStore } from "./local";
import { createSignal, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { PROOF_COPY } from "../fresh-auth";
import { createProofGate } from "../proof-gate";
import { ProofBlock } from "../ProofBlock";
import { WaitNotice } from "../recover/FormNotices";
import { useKeyRotation } from "../../devices/key-rotation-runtime";
import { rotationCommitted, rotationSavesPhrase } from "../../devices/key-rotation";
import { t } from "../../l10n";
import { markOwnAction } from "./notices";
import { notePhraseSaved, refreshProtection } from "./protection-state";
import { phraseSavedBy, savePhraseView, type SavePhraseView } from "./save-phrase";

export function SavePhrasePanel(props: {
  /** The keys were changed (the words follow by themselves): the surface
   *  that hosts the panel closes. */
  readonly onSaved: () => void;
  readonly onCancel?: () => void;
  readonly cancelLabel?: string;
  /** An extra line under the effect (UX 19.6: "so whoever opened it loses
   *  it"). */
  readonly extraLine?: string;
}): JSX.Element {
  const auth = useAuth();
  const rotation = useKeyRotation();
  const [busy, setBusy] = createSignal(false);
  const [view, setView] = createSignal<SavePhraseView | null>(null);

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
    const r = rotation.record();
    if (r !== null && rotationSavesPhrase(r) && !rotationCommitted(r) && !busy()) void rotation.notNow();
  });

  /** The keys were changed on a run of this panel, or of an earlier one. */
  const committed = (): boolean => {
    const r = rotation.record();
    return r !== null && rotationSavesPhrase(r) && rotationCommitted(r);
  };

  async function onShow(): Promise<void> {
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

  const failed = (): boolean => view()?.kind === "failed";
  const finishing = (): boolean => view()?.kind === "finishing" || (committed() && view()?.kind !== "done");

  return (
    <div class="lk-stack">
      <p class="lk-muted">{t("protect.phrase.save.body")}</p>
      <p class="lk-muted">{t("protect.phrase.save.effect")}</p>
      <Show when={props.extraLine}>{(line) => <p class="lk-muted">{line()}</p>}</Show>
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
          <button
            class="primary"
            type="button"
            disabled={busy() || !gate.view().ready || gate.wait.waiting()}
            onClick={() => void onShow()}
          >
            <Show when={busy()} fallback={failed() ? t("common.try_again") : t("protect.phrase.save.show")}>
              <span class="lakko-spinner" aria-hidden="true" /> {t("protect.phrase.save.progress")}
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
      </Show>
    </div>
  );
}
