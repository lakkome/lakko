// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A case (UX section 5.3): the target and its standing, earlier decisions,
 * every report opened in the browser, and the decisions. Opening a case takes
 * its lock, so other moderators see "In review by {name}"; the lock is kept
 * alive while the case is open and released when the moderator leaves.
 */

import { A, useParams } from "@solidjs/router";
import { For, type JSX, Match, Show, Switch, createEffect, createResource, createSignal, on, onCleanup } from "solid-js";

import { api } from "../api/client";
import { ApiError, errorMessage } from "../api/errors";
import type { CaseDetail, HistoryEntry, Lock } from "../api/types";
import { decisionOptions } from "../model/decisions";
import { usernameLine } from "../model/evidence";
import { formatTime, untilPhrase } from "../model/format";
import { actionLabel, ageBandLabel, caseStatusLabel, reasonPhrase, targetKindLabel } from "../model/names";
import { session } from "../session";
import { Loading, Notice } from "../ui";
import { DecisionCard } from "./Decisions";
import { ReportCard } from "./Evidence";

/** How often an open case's lock is renewed (the service frees it after 30 min idle). */
const LOCK_RENEW_MS = 5 * 60_000;

type LockState =
  | { readonly kind: "pending" }
  | { readonly kind: "mine" }
  | { readonly kind: "other"; readonly lock: Lock }
  | { readonly kind: "failed"; readonly message: string };

function History(props: { readonly entries: readonly HistoryEntry[] }): JSX.Element {
  return (
    <Show
      when={props.entries.length > 0}
      fallback={<p class="mp-row-sub" style={{ margin: "0" }}>No earlier decisions.</p>}
    >
      <ul class="mp-list">
        <For each={props.entries}>
          {(h) => (
            <li class="mp-evidence-item">
              <Switch>
                <Match when={h.type === "statement" && h}>
                  {(e) => {
                    const st = (e() as Extract<HistoryEntry, { type: "statement" }>);
                    return (
                      <>
                        <strong>
                          {actionLabel(st.statement.action)}
                          {st.statement.action === "suspend" ? ` ${untilPhrase(st.statement.until_ms)}` : ""}
                          {st.reason !== null ? ` for ${reasonPhrase(st.reason)}` : ""}
                        </strong>
                        <span class="mp-row-sub">
                          {formatTime(st.statement.issued_at_ms)} · {st.statement.moderator_name ?? "a moderator no longer listed"} · log
                          entry {st.statement.log_index}
                          {st.statement.target_kind !== "account" ? ` · ${targetKindLabel(st.statement.target_kind)}` : ""}
                        </span>
                        <Show when={st.note}>
                          {(n) => (
                            <span class="mp-row-sub">
                              Note by {n().moderator_name}: {n().text}
                            </span>
                          )}
                        </Show>
                      </>
                    );
                  }}
                </Match>
                <Match when={h.type === "no-action" && h}>
                  {(e) => {
                    const na = e() as Extract<HistoryEntry, { type: "no-action" }>;
                    return (
                      <>
                        <strong>No action for {reasonPhrase(na.reason)}</strong>
                        <span class="mp-row-sub">
                          {formatTime(na.at_ms)} · {na.moderator_name} · not published
                        </span>
                        <Show when={na.note}>
                          {(n) => (
                            <span class="mp-row-sub">
                              Note by {n().moderator_name}: {n().text}
                            </span>
                          )}
                        </Show>
                      </>
                    );
                  }}
                </Match>
              </Switch>
            </li>
          )}
        </For>
      </ul>
    </Show>
  );
}

function Header(props: {
  readonly detail: CaseDetail;
  readonly lock: LockState;
  readonly onRelease: () => void;
}): JSX.Element {
  const c = (): CaseDetail["case"] => props.detail.case;
  // The detail was read before the lock was taken; a new case the moderator
  // now holds is in review.
  const status = (): string => (props.lock.kind === "mine" && c().status === "new" ? "in-review" : c().status);
  const auth = () => props.detail.account?.auth ?? null;
  const discovery = () => props.detail.account?.discovery ?? null;
  return (
    <section class="lk-card">
      <div style={{ display: "flex", "justify-content": "space-between", gap: "12px", "flex-wrap": "wrap" }}>
        <h2>{targetKindLabel(c().kind)}</h2>
        <span class="mp-status" data-status={status()}>
          {caseStatusLabel(status())}
        </span>
      </div>
      <dl class="mp-kv">
        <dt>Target</dt>
        <dd class="mp-mono">{c().target_hex}</dd>
        <Show when={c().kind === "account"}>
          <dt>Username</dt>
          <dd>{usernameLine(null, discovery())}</dd>
          <dt>Account</dt>
          <dd>
            <Show when={auth()} fallback="Unknown: the account service didn't answer">
              {(a) => (a().exists ? ageBandLabel(a().age_band) : "No longer exists")}
            </Show>
          </dd>
          <dt>Suspended now</dt>
          <dd>
            <Show when={auth()} fallback="Unknown">
              {(a) => (a().suspended ? `Yes, ${untilPhrase(a().until_ms)}` : "No")}
            </Show>
          </dd>
          <Show when={discovery()}>
            {(d) => (
              <>
                <dt>Findable by everyone</dt>
                <dd>{d().in_everyone ? "Yes" : "No"}</dd>
              </>
            )}
          </Show>
        </Show>
        <Show when={c().decided_at_ms}>
          {(t) => (
            <>
              <dt>Decided</dt>
              <dd>{formatTime(t())}</dd>
            </>
          )}
        </Show>
      </dl>
      <Switch>
        <Match when={props.lock.kind === "other" && props.lock}>
          {(l) => (
            <Notice tone="warning">
              In review by {(l() as Extract<LockState, { kind: "other" }>).lock.name}. You can read this case; deciding is
              left to them.
            </Notice>
          )}
        </Match>
        <Match when={props.lock.kind === "failed" && props.lock}>
          {(l) => <Notice tone="warning">{(l() as Extract<LockState, { kind: "failed" }>).message}</Notice>}
        </Match>
      </Switch>
      <Show when={props.lock.kind === "mine"}>
        <div class="lk-actions">
          <button type="button" class="ghost small" onClick={() => props.onRelease()}>
            Release the case
          </button>
        </div>
      </Show>
    </section>
  );
}

export function CaseScreen(): JSX.Element {
  const params = useParams<{ kind: string; hex: string }>();
  const key = () => ({ kind: params.kind, hex: params.hex });
  const [detail, { refetch }] = createResource(key, (k) => api.caseDetail(k.kind, k.hex));
  const [lock, setLock] = createSignal<LockState>({ kind: "pending" });

  const takeLock = async (k: { kind: string; hex: string }): Promise<void> => {
    try {
      await api.lock(k.kind, k.hex);
      setLock({ kind: "mine" });
    } catch (e) {
      if (e instanceof ApiError && e.status === 409 && e.body !== null && typeof e.body === "object") {
        const holder = (e.body as { lock?: Lock }).lock;
        if (holder !== undefined) {
          setLock({ kind: "other", lock: holder });
          return;
        }
      }
      setLock({ kind: "failed", message: `The case couldn't be reserved for you: ${errorMessage(e)}` });
    }
  };

  createEffect(
    on(key, (k) => {
      setLock({ kind: "pending" });
      void takeLock(k);
      const timer = setInterval(() => {
        if (lock().kind === "mine") void takeLock(k);
      }, LOCK_RENEW_MS);
      onCleanup(() => {
        clearInterval(timer);
        if (lock().kind === "mine" && session() !== null) void api.unlock(k.kind, k.hex).catch(() => undefined);
      });
    }),
  );

  const release = async (): Promise<void> => {
    const k = key();
    try {
      await api.unlock(k.kind, k.hex);
      setLock({ kind: "failed", message: "You released this case. Reopen it from the queue to decide." });
    } catch (e) {
      setLock({ kind: "failed", message: errorMessage(e) });
    }
  };

  const blockedBy = (): string | null => {
    const l = lock();
    if (l.kind === "pending") return "Reserving the case for you...";
    if (l.kind === "other") return `In review by ${l.lock.name}.`;
    if (l.kind === "failed") return "Deciding needs the case reserved for you.";
    return null;
  };

  return (
    <>
      <header class="lk-page-head">
        <A href="/" class="mp-row-sub">
          Back to the queue
        </A>
        <h1>Case</h1>
      </header>
      <Switch>
        <Match when={detail.error !== undefined && detail.error !== null}>
          <Notice tone="danger">
            <p style={{ margin: "0 0 8px" }}>{errorMessage(detail.error)}</p>
            <button type="button" class="small" onClick={() => void refetch()}>
              Try again
            </button>
          </Notice>
        </Match>
        <Match when={detail.loading && detail() === undefined}>
          <Loading label="Loading the case..." />
        </Match>
        <Match when={detail()}>
          {(d) => (
            <>
              <Header detail={d()} lock={lock()} onRelease={() => void release()} />
              <section class="lk-card">
                <h2>Earlier decisions</h2>
                <History entries={d().history} />
              </section>
              <For each={d().reports}>{(r, i) => <ReportCard report={r} index={i()} />}</For>
              <DecisionCard
                title="Decide"
                options={decisionOptions(d().case.kind, d().case.target_hex, d().account?.discovery ?? null)}
                caseKind={d().case.kind}
                caseTargetHex={d().case.target_hex}
                blockedBy={blockedBy()}
                onDecided={() => {
                  // A decision releases the lock; take it again for a further decision.
                  void refetch();
                  void takeLock(key());
                }}
              />
            </>
          )}
        </Match>
      </Switch>
    </>
  );
}
