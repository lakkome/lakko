// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The public moderation log as entries, newest first, with search by target
 * (UX section 5.5). Read-only.
 */

import { For, type JSX, Match, Show, Switch, createSignal, onMount } from "solid-js";

import { api } from "../api/client";
import { errorMessage } from "../api/errors";
import type { LogEntry } from "../api/types";
import { hexToBytes, shortHex } from "../model/bytes";
import { formatTime, untilPhrase } from "../model/format";
import { actionLabel, targetKindLabel } from "../model/names";
import { BusyButton, Loading, Notice } from "../ui";

function EntryRow(props: { readonly entry: LogEntry }): JSX.Element {
  const e = props.entry;
  return (
    <li class="mp-row">
      <div>
        <Switch fallback={<div class="mp-row-title">An entry the panel doesn't know</div>}>
          <Match when={e.kind === "statement" && e}>
            {(x) => {
              const st = (x() as Extract<LogEntry, { kind: "statement" }>).statement;
              return (
                <>
                  <div class="mp-row-title">
                    {actionLabel(st.action)}
                    {st.action === "suspend" ? ` ${untilPhrase(st.until_ms)}` : ""}
                  </div>
                  <div class="mp-row-sub">
                    {targetKindLabel(st.target_kind)} <span class="mp-mono">{shortHex(st.target_hex)}</span>
                  </div>
                  <div class="mp-row-sub">
                    {formatTime(st.issued_at_ms)} · {st.moderator_name ?? "a moderator no longer listed"} · list version{" "}
                    {st.list_version}
                  </div>
                </>
              );
            }}
          </Match>
          <Match when={e.kind === "moderator-list" && e}>
            {(x) => {
              const v = (x() as Extract<LogEntry, { kind: "moderator-list" }>).list_version;
              return <div class="mp-row-title">Moderator list{v === null ? "" : `, version ${v}`}</div>;
            }}
          </Match>
          <Match when={e.kind === "snapshot-root"}>
            <div class="mp-row-title">Suspension snapshot</div>
          </Match>
        </Switch>
      </div>
      <div class="mp-row-side">Entry {e.index}</div>
    </li>
  );
}

export function LogScreen(): JSX.Element {
  const [query, setQuery] = createSignal("");
  const [applied, setApplied] = createSignal("");
  const [entries, setEntries] = createSignal<readonly LogEntry[]>([]);
  const [size, setSize] = createSignal<number | null>(null);
  const [next, setNext] = createSignal<number | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [inputError, setInputError] = createSignal<string | null>(null);

  const load = async (target: string, before: number | null): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const page = await api.log(target, before);
      setEntries(before === null ? page.entries : [...entries(), ...page.entries]);
      setSize(page.log_size);
      setNext(page.next_before);
      setApplied(target);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  };

  onMount(() => void load("", null));

  const search = (): void => {
    const q = query().trim().toLowerCase();
    if (q !== "" && hexToBytes(q) === null) {
      setInputError("Paste a target in hex, as shown on a case.");
      return;
    }
    setInputError(null);
    setEntries([]);
    void load(q, null);
  };

  return (
    <>
      <header class="lk-page-head">
        <h1>Log</h1>
        <p>The public moderation log, newest first.</p>
      </header>
      <form
        class="lk-actions"
        style={{ "align-items": "flex-end" }}
        onSubmit={(ev) => {
          ev.preventDefault();
          search();
        }}
      >
        <label style={{ flex: "1", "min-width": "240px" }}>
          Target
          <input
            class="mp-mono"
            value={query()}
            placeholder="A target in hex"
            spellcheck={false}
            onInput={(ev) => setQuery(ev.currentTarget.value)}
          />
        </label>
        <button type="submit" class="primary">
          Search
        </button>
        <Show when={applied() !== ""}>
          <button
            type="button"
            class="ghost"
            onClick={() => {
              setQuery("");
              setEntries([]);
              void load("", null);
            }}
          >
            Show all
          </button>
        </Show>
      </form>
      <Show when={inputError()}>
        {(m) => (
          <p class="form-error" role="alert">
            {m()}
          </p>
        )}
      </Show>
      <Show when={error()}>{(m) => <Notice tone="danger">{m()}</Notice>}</Show>
      <Show when={loading() && entries().length === 0}>
        <Loading label="Loading the log..." />
      </Show>
      <Show when={size() !== null}>
        <p class="form-info">
          {size()} {size() === 1 ? "entry" : "entries"} in the log
          {applied() === "" ? "" : `; showing those naming ${shortHex(applied())}`}
        </p>
      </Show>
      <Show when={!loading() && size() !== null && entries().length === 0 && error() === null}>
        <div class="lk-empty">{applied() === "" ? "The log is empty." : "No entry names this target."}</div>
      </Show>
      <ul class="mp-list">
        <For each={entries()}>{(e) => <EntryRow entry={e} />}</For>
      </ul>
      <Show when={next() !== null}>
        <div class="lk-actions">
          <BusyButton busy={loading()} onClick={() => void load(applied(), next())}>
            Load older entries
          </BusyButton>
        </div>
      </Show>
    </>
  );
}
