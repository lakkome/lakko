// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Remove recovery email?" (docs/ux/identity-recovery.md section 19.2): the
 * one confirmation, in the app's own dialog, with the proof block of
 * section 17.1 when no proof is live.
 */
import { plainStore } from "./local";
import { createSignal, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { PROOF_COPY } from "../fresh-auth";
import { createProofGate } from "../proof-gate";
import { ProofBlock } from "../ProofBlock";
import { t } from "../../l10n";
import { emailFailure, emailFailureKey } from "./email-flow";
import { markOwnAction } from "./notices";
import { refreshProtection } from "./protection-state";
import { answerOf, RecoveryEmailClient } from "./recovery-email-client";

export function RemoveRecoveryEmail(props: {
  readonly onRemoved: () => void;
  readonly onCancel: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.recoveryEmail,
    busy,
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    usernameKnown: () => {
      const s = auth.state();
      return s.kind === "session" && s.account.username.trim().length > 0;
    },
  });
  onCleanup(() => gate.clear());

  async function onRemove(): Promise<void> {
    const s = auth.state();
    const token = auth.identitySecrets()?.sessionToken ?? null;
    if (s.kind !== "session" || token === null || busy()) return;
    setError(null);
    setBusy(true);
    let grant: string | null = null;
    try {
      grant = await gate.ensure();
      if (grant === null) return;
      markOwnAction(plainStore(), s.account.accountId, "recovery-email-removed", Date.now());
      await new RecoveryEmailClient().remove(token, grant);
      void refreshProtection(s.account.accountId, token);
      props.onRemoved();
    } catch (err) {
      const answer = answerOf(err);
      const read = answer === null ? ({ kind: "error" } as const) : emailFailure("remove", answer);
      if (read.kind === "proof-required" || read.kind === "rate-limited") {
        // Drops the refused grant and shows the block, or starts the wait.
        gate.failed(err, grant);
        if (read.kind === "rate-limited" && read.retryAfterSecs === null) {
          setError(t("common.error.too_many_attempts"));
        }
        return;
      }
      const key = emailFailureKey(read);
      setError(t(key ?? "recovery_phrase.fresh.error.generic"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="lk-stack">
      <p>{t("recovery_email.remove.confirm.body")}</p>
      <ProofBlock gate={gate} />
      <Show when={error() !== null}>
        <div class="form-error" role="alert">
          {error()}
        </div>
      </Show>
      <div class="lk-actions">
        <button type="button" disabled={busy()} onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
        <button
          type="button"
          class="danger"
          disabled={busy() || !gate.view().ready || gate.wait.waiting()}
          onClick={() => void onRemove()}
        >
          {busy() ? t("common.removing") : t("recovery_email.remove")}
        </button>
      </div>
    </div>
  );
}
