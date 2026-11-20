// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The panel's shell: its own navigation (Queue, Appeals, Log, Moderators,
 * Numbers), not the product's. Signed out, every address shows the sign-in.
 */

import { A, Route, Router, type RouteSectionProps } from "@solidjs/router";
import { type JSX, Show, createEffect, createResource, onCleanup } from "solid-js";

import { loadModerators } from "./moderators";
import { session, signOut } from "./session";
import { Icon, Notice } from "./ui";
import { AppealsScreen } from "./screens/Appeals";
import { CaseScreen } from "./screens/Case";
import { LogScreen } from "./screens/Log";
import { ModeratorsScreen } from "./screens/Moderators";
import { NumbersScreen } from "./screens/Numbers";
import { QueueScreen } from "./screens/Queue";
import { SignInScreen } from "./screens/SignIn";

function Shell(props: RouteSectionProps): JSX.Element {
  // Sign out at the token's end rather than on the next failed call.
  createEffect(() => {
    const s = session();
    if (s === null) return;
    const left = s.expiresAtMs - Date.now();
    const timer = setTimeout(() => signOut(), Math.max(0, Math.min(left, 2 ** 31 - 1)));
    onCleanup(() => clearTimeout(timer));
  });

  const [moderators] = createResource(() => session()?.moderatorIdB64, () => loadModerators());

  return (
    <Show when={session()} fallback={<SignInScreen />}>
      {(s) => (
        <div class="app-shell mp-shell">
          <nav class="lk-rail" aria-label="Moderation">
            <A class="lk-rail-item" href="/" end>
              <Icon name="flag" />
              Queue
            </A>
            <A class="lk-rail-item" href="/appeals">
              <Icon name="envelope" />
              Appeals
            </A>
            <A class="lk-rail-item" href="/log">
              <Icon name="book" />
              Log
            </A>
            <A class="lk-rail-item" href="/moderators">
              <Icon name="shield" />
              Moderators
            </A>
            <A class="lk-rail-item" href="/numbers">
              <Icon name="chart-bar" />
              Numbers
            </A>
            <div class="lk-rail-spacer" />
            <div class="mp-rail-me">
              <span>{s().name}</span>
              <button type="button" class="ghost small" onClick={() => signOut()}>
                Sign out
              </button>
            </div>
          </nav>
          <main>
            {/* Narrow screens hide the rail's name and sign-out; this bar carries them. */}
            <div class="mp-topbar">
              <span class="lk-ellipsis">Signed in as {s().name}</span>
              <button type="button" class="ghost small" onClick={() => signOut()}>
                Sign out
              </button>
            </div>
            <article>
              <div class="mp-page lk-stack">
                <Show when={moderators()?.reportKeyMatches === false}>
                  <Notice tone="danger">
                    Your security key gave a different report key than the moderator list holds for you. Reports and
                    appeals sealed to you won't open. Ask for the list to be signed again with this key.
                  </Notice>
                </Show>
                <Show when={moderators.error !== undefined && moderators.error !== null}>
                  <Notice tone="warning">The moderator list couldn't be loaded. Decisions may fail until it loads.</Notice>
                </Show>
                {props.children}
              </div>
            </article>
          </main>
        </div>
      )}
    </Show>
  );
}

export function App(): JSX.Element {
  return (
    <Router root={Shell}>
      <Route path="/" component={QueueScreen} />
      <Route path="/case/:kind/:hex" component={CaseScreen} />
      <Route path="/appeals" component={AppealsScreen} />
      <Route path="/log" component={LogScreen} />
      <Route path="/moderators" component={ModeratorsScreen} />
      <Route path="/numbers" component={NumbersScreen} />
      <Route path="*" component={QueueScreen} />
    </Router>
  );
}
