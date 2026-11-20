// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The current moderator list (UX section 5.5): names and when each was added.
 * Read-only: the list is changed with the offline tool.
 */

import { For, type JSX, Match, Show, Switch, createResource } from "solid-js";

import { errorMessage } from "../api/errors";
import { formatDay, formatTime } from "../model/format";
import { loadModerators } from "../moderators";
import { session } from "../session";
import { Loading, Notice } from "../ui";

export function ModeratorsScreen(): JSX.Element {
  const [state, { refetch }] = createResource(() => loadModerators());
  return (
    <>
      <header class="lk-page-head">
        <h1>Moderators</h1>
        <p>The list is changed with the offline tool, not here.</p>
      </header>
      <Switch>
        <Match when={state.error !== undefined && state.error !== null}>
          <Notice tone="danger">
            <p style={{ margin: "0 0 8px" }}>{errorMessage(state.error)}</p>
            <button type="button" class="small" onClick={() => void refetch()}>
              Try again
            </button>
          </Notice>
        </Match>
        <Match when={state.loading && state() === undefined}>
          <Loading label="Loading the list..." />
        </Match>
        <Match when={state()}>
          {(st) => (
            <>
              <Switch>
                <Match when={st().check.kind === "verified"}>
                  <Notice>
                    Version {st().list.list_version}, signed {formatTime(st().list.issued_at_ms)}. Checked against the
                    root key this panel was built with.
                  </Notice>
                </Match>
                <Match when={st().check.kind === "unpinned"}>
                  <Notice tone="warning">
                    Version {st().list.list_version}, signed {formatTime(st().list.issued_at_ms)}. Not checked: this
                    panel was built without the root key.
                  </Notice>
                </Match>
                <Match when={st().check.kind === "failed"}>
                  <Notice tone="danger">
                    The list didn't check out against the root key this panel was built with. Don't trust the names
                    below until the list is signed again.
                  </Notice>
                </Match>
              </Switch>
              <ul class="mp-list">
                <For each={st().list.moderators}>
                  {(m) => (
                    <li class="mp-row">
                      <div>
                        <div class="mp-row-title">
                          {m.name}
                          {m.moderator_id_b64 === session()?.moderatorIdB64 ? " (you)" : ""}
                        </div>
                        <div class="mp-row-sub">
                          Added {formatDay(m.added_at_ms)} · security key {m.key_kind === "p256" ? "P-256" : "Ed25519"}
                        </div>
                      </div>
                      <div class="mp-row-side">
                        <Show when={m.revoked_at_ms !== null} fallback={<span>Active</span>}>
                          <span>Removed {formatDay(m.revoked_at_ms ?? 0)}</span>
                        </Show>
                      </div>
                    </li>
                  )}
                </For>
              </ul>
            </>
          )}
        </Match>
      </Switch>
    </>
  );
}
