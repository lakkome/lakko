// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Numbers for a transparency report (UX section 5.5): reports per reason,
 * decisions per kind, appeals and their outcomes, per week. Counts only.
 */

import { For, type JSX, Match, Switch, createResource, createSignal } from "solid-js";

import { api } from "../api/client";
import { errorMessage } from "../api/errors";
import { numbersColumns } from "../model/numbers";
import { LakkoPicker, Loading, Notice } from "../ui";

export function NumbersScreen(): JSX.Element {
  const [weeks, setWeeks] = createSignal("12");
  const [numbers, { refetch }] = createResource(weeks, (w) => api.numbers(Number(w)));

  return (
    <>
      <header class="lk-page-head">
        <h1>Numbers</h1>
        <p>Counts per week, newest first. Weeks start on Monday (UTC).</p>
      </header>
      <div class="mp-filters" style={{ "max-width": "240px" }}>
        <div class="lk-picker-field">
          <span id="n-weeks">Period</span>
          <LakkoPicker
            labelledBy="n-weeks"
            value={weeks()}
            options={[
              { value: "4", label: "Last 4 weeks" },
              { value: "12", label: "Last 12 weeks" },
              { value: "26", label: "Last 26 weeks" },
              { value: "52", label: "Last 52 weeks" },
            ]}
            onChange={setWeeks}
          />
        </div>
      </div>
      <Switch>
        <Match when={numbers.error !== undefined && numbers.error !== null}>
          <Notice tone="danger">
            <p style={{ margin: "0 0 8px" }}>{errorMessage(numbers.error)}</p>
            <button type="button" class="small" onClick={() => void refetch()}>
              Try again
            </button>
          </Notice>
        </Match>
        <Match when={numbers.loading && numbers() === undefined}>
          <Loading label="Counting..." />
        </Match>
        <Match when={numbers()}>
          {(data) => {
            const cols = () => numbersColumns(data().weeks);
            return (
              <div class="mp-table-wrap">
                <table class="mp-table">
                  <thead>
                    <tr>
                      <th scope="col">Week</th>
                      <For each={cols()}>
                        {(c) => (
                          <th scope="col" class="num">
                            {c.label}
                          </th>
                        )}
                      </For>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data().weeks}>
                      {(w) => (
                        <tr>
                          <th scope="row">
                            {w.week}
                            <div class="mp-row-sub">from {w.start_day}</div>
                          </th>
                          <For each={cols()}>{(c) => <td class="num">{c.value(w)}</td>}</For>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            );
          }}
        </Match>
      </Switch>
    </>
  );
}
