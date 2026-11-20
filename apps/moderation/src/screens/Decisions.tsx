// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Deciding (UX sections 5.1 and 5.3): choose the decision, a required reason
 * and an optional internal note; read the exact sentence that will be signed;
 * confirm; touch the key. A decision is never one click. `No action` signs
 * nothing and publishes nothing, but is confirmed the same way.
 */

import { type JSX, Show, createSignal } from "solid-js";

import { api } from "../api/client";
import { errorMessage } from "../api/errors";
import { resend, signAndDecide, type DecisionResult } from "../decide";
import {
  NOTE_MAX,
  canResend,
  confirmationSentence,
  outcomeSentence,
  type DecisionOption,
} from "../model/decisions";
import { REASONS } from "../model/names";
import { BusyButton, Dialog, LakkoPicker, Notice } from "../ui";

/**
 * Decisions that end a restriction rather than impose one, or (keeping a
 * suspension on appeal) impose nothing new: their confirm button is not red.
 */
const UNDOING: ReadonlySet<string> = new Set([
  "no-action",
  "keep-suspension",
  "lift",
  "release-name",
  "show-in-search",
  "channel-restore",
  "group-restore",
]);

export function DecisionCard(props: {
  readonly title: string;
  readonly options: readonly DecisionOption[];
  /** The case `No action` closes. */
  readonly caseKind: string;
  readonly caseTargetHex: string;
  /** Why deciding is not offered now, or `null`. */
  readonly blockedBy: string | null;
  readonly onDecided: () => void;
  /** Preselect this decision (an appeal offers only the lift). */
  readonly initialOptionId?: string;
}): JSX.Element {
  const [optionId, setOptionId] = createSignal(props.initialOptionId ?? "");
  const [reason, setReason] = createSignal("");
  const [note, setNote] = createSignal("");
  const [confirming, setConfirming] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [result, setResult] = createSignal<DecisionResult | null>(null);
  /** The action of the decision in `result` (the picker is reset after signing). */
  const [resultAction, setResultAction] = createSignal<string | undefined>(undefined);
  const [noActionDone, setNoActionDone] = createSignal<number | null>(null);

  const option = (): DecisionOption | undefined => props.options.find((o) => o.id === optionId());
  const reasonCode = (): number => Number(reason() === "" ? 0 : reason());
  const ready = (): boolean => option() !== undefined && reasonCode() > 0 && note().length <= NOTE_MAX;

  const run = async (): Promise<void> => {
    const o = option();
    if (o === undefined) return;
    setBusy(true);
    setError(null);
    try {
      if (o.action === "no-action") {
        const answer = await api.noAction(props.caseKind, props.caseTargetHex, reasonCode(), note());
        setNoActionDone(answer.reports_closed);
      } else {
        const r = await signAndDecide({ option: o, reasonCode: reasonCode(), note: note() });
        setResultAction(o.action);
        setResult(r);
      }
      setConfirming(false);
      setOptionId(props.initialOptionId ?? "");
      setNote("");
      props.onDecided();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const sendAgain = async (): Promise<void> => {
    const r = result();
    if (r === null) return;
    setBusy(true);
    setError(null);
    try {
      setResult(await resend(r.body, r.issuedAtMs));
      props.onDecided();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="lk-card">
      <h2>{props.title}</h2>
      <Show when={result()}>
        {(r) => (
          <Notice tone={r().outcome.kind === "not-applied" ? "warning" : undefined}>
            <p style={{ margin: "0" }}>
              {outcomeSentence(r().outcome, { action: resultAction(), issuedAtMs: r().issuedAtMs })}
            </p>
            <Show when={r().outcome.kind === "not-applied" && canResend(r().issuedAtMs, Date.now())}>
              <div style={{ "margin-top": "8px" }}>
                <BusyButton class="small" busy={busy()} onClick={() => void sendAgain()}>
                  Send again
                </BusyButton>
              </div>
            </Show>
          </Notice>
        )}
      </Show>
      <Show when={noActionDone()}>
        {(n) => (
          <Notice>
            Closed with no action. {n() === 1 ? "1 report closed." : `${n()} reports closed.`} Nothing was published.
          </Notice>
        )}
      </Show>
      <Show
        when={props.blockedBy === null}
        fallback={<p class="mp-row-sub" style={{ margin: "0" }}>{props.blockedBy}</p>}
      >
        <div class="lk-picker-field">
          <span id="d-option">Decision</span>
          <LakkoPicker
            labelledBy="d-option"
            value={optionId()}
            placeholder="Choose a decision"
            options={props.options.map((o) => ({ value: o.id, label: o.label }))}
            onChange={(v) => setOptionId(v)}
          />
        </div>
        <div class="lk-picker-field">
          <span id="d-reason">Reason (required)</span>
          <LakkoPicker
            labelledBy="d-reason"
            value={reason()}
            placeholder="Choose a reason"
            options={REASONS.map((r) => ({ value: String(r.code), label: r.label }))}
            onChange={(v) => setReason(v)}
          />
        </div>
        <label>
          Internal note (optional, never published)
          <textarea rows={3} maxLength={NOTE_MAX} value={note()} onInput={(e) => setNote(e.currentTarget.value)} />
        </label>
        <Show when={error() !== null && !confirming()}>
          <p class="form-error" role="alert">
            {error()}
          </p>
        </Show>
        <div class="lk-actions">
          <button type="button" class="primary" disabled={!ready()} onClick={() => setConfirming(true)}>
            Review decision
          </button>
        </div>
      </Show>

      <Dialog
        open={confirming()}
        title={option()?.action === "no-action" ? "Close with no action?" : "Sign this decision?"}
        onCancel={() => {
          if (!busy()) {
            setConfirming(false);
            setError(null);
          }
        }}
      >
        <Show when={option()}>
          {(o) => (
            <>
              <p class="mp-dialog-body">{confirmationSentence(o(), reasonCode())}</p>
              <Show when={o().action !== "no-action"}>
                <p class="mp-dialog-body">After you confirm, touch your security key to sign it.</p>
              </Show>
              <Show when={note().trim() !== ""}>
                <p class="mp-dialog-body">The internal note is kept in the panel and never published.</p>
              </Show>
              <Show when={error()}>
                {(msg) => (
                  <p class="form-error" role="alert">
                    {msg()}
                  </p>
                )}
              </Show>
              <div class="mp-dialog-actions">
                <button type="button" class="ghost" data-cancel disabled={busy()} onClick={() => setConfirming(false)}>
                  Cancel
                </button>
                <BusyButton
                  class={UNDOING.has(o().action) ? "primary" : "danger filled"}
                  busy={busy()}
                  onClick={() => void run()}
                >
                  {o().action === "no-action" ? "Close with no action" : "Sign and publish"}
                </BusyButton>
              </div>
            </>
          )}
        </Show>
      </Dialog>
    </section>
  );
}
