// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Appeals (UX section 5.4): the appeal text (opened in the browser), the
 * decision it appeals and the case behind it. Both answers are signed
 * decisions published in the moderation log: `Keep the suspension` signs a
 * `keep-suspension` statement (it declines the appeal and changes no
 * standing), `Lift the suspension` signs a `lift`. A moderator who decided
 * the suspension is warned, never blocked.
 */

import { A } from "@solidjs/router";
import { For, type JSX, Match, Show, Switch, createResource, createSignal } from "solid-js";

import { api } from "../api/client";
import { coreErrorMessage, errorMessage } from "../api/errors";
import type { Appeal } from "../api/types";
import { b64ToBytes } from "../model/bytes";
import { keepSuspensionOption, liftOption, type DecisionOption } from "../model/decisions";
import { appealWarning, formatTime, untilPhrase } from "../model/format";
import { actionLabel, caseStatusLabel } from "../model/names";
import { requireSession } from "../session";
import { Loading, Notice } from "../ui";
import { loadWasm } from "../wasm";
import { DecisionCard } from "./Decisions";

async function openAppealText(a: Appeal): Promise<string> {
  const w = await loadWasm();
  const s = requireSession();
  try {
    return w.openModerationAppeal(b64ToBytes(a.sealed_b64), s.moderatorId, s.reportSecret);
  } catch (e) {
    throw new Error(coreErrorMessage(e, "appeal"));
  }
}

function AppealCard(props: { readonly appeal: Appeal; readonly onDone: () => void }): JSX.Element {
  const [text] = createResource(() => props.appeal, openAppealText);
  /** Which answer the moderator is preparing, or `null`. */
  const [answer, setAnswer] = createSignal<"keep" | "lift" | null>(null);
  const warning = (): string | null => appealWarning(props.appeal.decided_by_you);
  const option = (which: "keep" | "lift"): DecisionOption =>
    which === "keep" ? keepSuspensionOption(props.appeal.target_hex) : liftOption(props.appeal.target_hex);

  return (
    <section class="lk-card">
      <div style={{ display: "flex", "justify-content": "space-between", gap: "12px", "flex-wrap": "wrap" }}>
        <h2>Appeal</h2>
        <span class="mp-row-sub">Sent {formatTime(props.appeal.appeal_at_ms)}</span>
      </div>
      <Show when={warning()}>{(w) => <Notice tone="warning">{w()}</Notice>}</Show>
      <Switch>
        <Match when={text.error !== undefined && text.error !== null}>
          <Notice tone="warning">{text.error instanceof Error ? text.error.message : "The appeal couldn't be opened."}</Notice>
        </Match>
        <Match when={text.loading}>
          <p class="form-info">Opening the appeal...</p>
        </Match>
        <Match when={text() !== undefined}>
          <p class="mp-evidence-text" style={{ margin: "0" }}>
            {text()}
          </p>
        </Match>
      </Switch>
      <dl class="mp-kv">
        <dt>Decision appealed</dt>
        <dd>
          <Show
            when={props.appeal.decision}
            fallback={`A suspension ${untilPhrase(props.appeal.until_ms)} (its log entry wasn't found)`}
          >
            {(d) => (
              <>
                {actionLabel(d().action)} {untilPhrase(d().until_ms)}, {formatTime(d().issued_at_ms)} by{" "}
                {d().moderator_name ?? "a moderator no longer listed"} (log entry {d().log_index})
              </>
            )}
          </Show>
        </dd>
        <dt>Account</dt>
        <dd class="mp-mono">{props.appeal.target_hex}</dd>
        <dt>Case</dt>
        <dd>
          <Show when={props.appeal.case} fallback="No case: the suspension wasn't decided from a report.">
            {(c) => (
              <A href={`/case/${c().kind}/${c().target_hex}`}>Open the case ({caseStatusLabel(c().status)})</A>
            )}
          </Show>
        </dd>
      </dl>
      <Show
        when={answer()}
        fallback={
          <div class="lk-actions">
            <button type="button" onClick={() => setAnswer("keep")}>
              Keep the suspension
            </button>
            <button type="button" class="primary" onClick={() => setAnswer("lift")}>
              Lift the suspension
            </button>
          </div>
        }
      >
        {(which) => (
          <>
            <DecisionCard
              title={which() === "keep" ? "Keep the suspension" : "Lift the suspension"}
              options={[option(which())]}
              initialOptionId={option(which()).id}
              caseKind="account"
              caseTargetHex={props.appeal.target_hex}
              blockedBy={null}
              onDecided={() => props.onDone()}
            />
            <div class="lk-actions">
              <button type="button" class="ghost small" onClick={() => setAnswer(null)}>
                Back
              </button>
            </div>
          </>
        )}
      </Show>
    </section>
  );
}

export function AppealsScreen(): JSX.Element {
  const [appeals, { refetch }] = createResource(() => api.appeals());
  return (
    <>
      <header class="lk-page-head">
        <h1>Appeals</h1>
        <p>Oldest first.</p>
      </header>
      <Switch>
        <Match when={appeals.error !== undefined && appeals.error !== null}>
          <Notice tone="danger">
            <p style={{ margin: "0 0 8px" }}>{errorMessage(appeals.error)}</p>
            <button type="button" class="small" onClick={() => void refetch()}>
              Try again
            </button>
          </Notice>
        </Match>
        <Match when={appeals.loading && appeals() === undefined}>
          <Loading label="Loading appeals..." />
        </Match>
        <Match when={appeals()?.available === false}>
          <div class="lk-empty">Appeals aren't available: the account service isn't set up on this server.</div>
        </Match>
        <Match when={appeals()}>
          {(data) => (
            <Show when={data().appeals.length > 0} fallback={<div class="lk-empty">No appeals are waiting.</div>}>
              <For each={data().appeals}>
                {(a) => <AppealCard appeal={a} onDone={() => undefined} />}
              </For>
              <div class="lk-actions">
                <button type="button" onClick={() => void refetch()}>
                  Refresh
                </button>
              </div>
            </Show>
          )}
        </Match>
      </Switch>
    </>
  );
}
