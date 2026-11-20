// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The queue (UX section 5.2): one row per case, filtered by reason, status,
 * target kind and "has checked messages"; the service's order (checked
 * messages first, then report count).
 */

import { A } from "@solidjs/router";
import { For, type JSX, Match, Show, Switch, createResource, createSignal } from "solid-js";

import { api } from "../api/client";
import { errorMessage } from "../api/errors";
import { shortHex } from "../model/bytes";
import { CASE_STATUSES, REASONS, REPORTABLE_KINDS, caseStatusLabel, targetKindLabel } from "../model/names";
import {
  NO_FILTERS,
  daySpan,
  defaultOrder,
  evidenceSummary,
  hasFilters,
  reasonsSummary,
  type QueueFilters,
} from "../model/queue";
import { session } from "../session";
import { LakkoPicker, Loading, Notice } from "../ui";

export function QueueScreen(): JSX.Element {
  const [filters, setFilters] = createSignal<QueueFilters>(NO_FILTERS);
  const [cases, { refetch }] = createResource(filters, (f) => api.cases(f));

  const set = <K extends keyof QueueFilters>(key: K, value: QueueFilters[K]): void => {
    setFilters({ ...filters(), [key]: value });
  };

  return (
    <>
      <header class="lk-page-head">
        <h1>Queue</h1>
        <p>Reports grouped by what they name.</p>
      </header>

      <div class="mp-filters">
        <div class="lk-picker-field">
          <span id="f-reason">Reason</span>
          <LakkoPicker
            labelledBy="f-reason"
            value={filters().reason}
            options={[{ value: "", label: "Any reason" }, ...REASONS.map((r) => ({ value: r.name as string, label: r.label }))]}
            onChange={(v) => set("reason", v)}
          />
        </div>
        <div class="lk-picker-field">
          <span id="f-status">Status</span>
          <LakkoPicker
            labelledBy="f-status"
            value={filters().status}
            options={[
              { value: "", label: "Any status" },
              ...CASE_STATUSES.map((s) => ({ value: s as string, label: caseStatusLabel(s) })),
            ]}
            onChange={(v) => set("status", v)}
          />
        </div>
        <div class="lk-picker-field">
          <span id="f-kind">Target</span>
          <LakkoPicker
            labelledBy="f-kind"
            value={filters().kind}
            options={[
              { value: "", label: "Any target" },
              ...REPORTABLE_KINDS.map((k) => ({ value: k as string, label: targetKindLabel(k) })),
            ]}
            onChange={(v) => set("kind", v)}
          />
        </div>
        <div class="lk-picker-field">
          <span id="f-checked">Checked messages</span>
          <LakkoPicker<"" | "yes" | "no">
            labelledBy="f-checked"
            value={filters().checked}
            options={[
              { value: "", label: "Either" },
              { value: "yes", label: "Has checked messages" },
              { value: "no", label: "Has none" },
            ]}
            onChange={(v) => set("checked", v)}
          />
        </div>
      </div>

      <Switch>
        <Match when={cases.error !== undefined && cases.error !== null}>
          <Notice tone="danger">
            <p style={{ margin: "0 0 8px" }}>{errorMessage(cases.error)}</p>
            <button type="button" class="small" onClick={() => void refetch()}>
              Try again
            </button>
          </Notice>
        </Match>
        <Match when={cases.loading && cases() === undefined}>
          <Loading label="Loading the queue..." />
        </Match>
        <Match when={cases()}>
          {(data) => (
            <Show
              when={data().cases.length > 0}
              fallback={
                <div class="lk-empty">
                  <span>{hasFilters(filters()) ? "No case matches these filters." : "No reports yet."}</span>
                  <Show when={hasFilters(filters())}>
                    <button type="button" class="primary" onClick={() => setFilters(NO_FILTERS)}>
                      Clear filters
                    </button>
                  </Show>
                </div>
              }
            >
              <p class="form-info">
                {data().total === data().cases.length
                  ? `${data().total} ${data().total === 1 ? "case" : "cases"}`
                  : `Showing ${data().cases.length} of ${data().total} cases`}
              </p>
              <ul class="mp-list">
                <For each={defaultOrder(data().cases)}>
                  {(row) => (
                    <li>
                      <A class="mp-row" href={`/case/${row.kind}/${row.target_hex}`}>
                        <div>
                          <div class="mp-row-title">
                            {targetKindLabel(row.kind)} <span class="mp-mono">{shortHex(row.target_hex)}</span>
                          </div>
                          <div class="mp-row-sub">{reasonsSummary(row.reasons)}</div>
                          <div class="mp-row-sub">{evidenceSummary(row)}</div>
                        </div>
                        <div class="mp-row-side">
                          <span class="mp-status" data-status={row.status}>
                            {caseStatusLabel(row.status)}
                          </span>
                          <Show when={row.lock !== null && row.lock.moderator_id_b64 !== session()?.moderatorIdB64}>
                            <span>In review by {row.lock?.name}</span>
                          </Show>
                          <span>
                            {row.reports} {row.reports === 1 ? "report" : "reports"}
                            {row.open_reports !== row.reports ? ` · ${row.open_reports} open` : ""}
                          </span>
                          <span>{daySpan(row.first_day, row.last_day)}</span>
                        </div>
                      </A>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          )}
        </Match>
      </Switch>
    </>
  );
}
