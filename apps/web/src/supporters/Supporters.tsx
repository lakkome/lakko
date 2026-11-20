// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > About > Supporters (`docs/ux/supporters.md` section 2): the
 * About row, shown only while a verified list with names is held, and the
 * screen it opens. The decisions live in `supporters-list.ts`.
 */
import { createSignal, For, Match, onMount, Switch, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { t } from "../l10n";
import { paths } from "../routes/paths";
import { Icon } from "../ui/icons";
import { pinnedReleaseKey } from "../update/release-check";
import { loadWasm } from "../wasm";
import {
  defaultSupportersStorage,
  readHeldSupporters,
  refreshSupporters,
  showsSupportersRow,
  type HeldSupporters,
} from "./supporters-list";

const [held, setHeld] = createSignal<HeldSupporters | null>(
  pinnedReleaseKey() === null ? null : readHeldSupporters(defaultSupportersStorage()),
);
/** `loading` while a fetch runs with nothing held; `failed` after one that
 *  brought no list back with nothing held. */
const [fetchState, setFetchState] = createSignal<"idle" | "loading" | "failed">("idle");
let inFlight: Promise<void> | null = null;

/** Fetch (forced when About or the screen opens, else at most daily). */
export function refreshSupportersList(force: boolean): Promise<void> {
  if (inFlight !== null) return inFlight;
  if (pinnedReleaseKey() === null) return Promise.resolve();
  if (held() === null) setFetchState("loading");
  inFlight = (async () => {
    const result = await refreshSupporters(
      {
        wasm: loadWasm,
        fetch: (...a) => fetch(...a),
        key: pinnedReleaseKey(),
        storage: defaultSupportersStorage(),
        nowMs: () => Date.now(),
      },
      force,
    );
    if (result.kind === "dormant") {
      setFetchState("idle");
      return;
    }
    setHeld(result.held);
    setFetchState(result.kind === "failed" && result.held === null ? "failed" : "idle");
  })().finally(() => {
    inFlight = null;
  });
  return inFlight;
}

/** The About row; absent without a verified list that has a name. */
export function SupportersRow(): JSX.Element {
  onMount(() => void refreshSupportersList(true));
  return (
    <Switch>
      <Match when={showsSupportersRow(held())}>
        <A href={paths.settingsSection("supporters")} class="lk-card-row lk-card-row-link">
          <span class="lk-card-row-icon">
            <Icon name="heart" size={18} />
          </span>
          <span class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.about.supporters")}</span>
          </span>
        </A>
      </Match>
    </Switch>
  );
}

/** The Supporters screen. */
export function SupportersPage(): JSX.Element {
  onMount(() => {
    if (held() === null) void refreshSupportersList(true);
  });
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("supporters.title")}</h1>
        <p>{t("supporters.body")}</p>
      </div>
      <Switch>
        <Match when={held()}>
          {(list) => (
            <section class="lk-card">
              <For each={list().names}>
                {(name) => (
                  <div class="lk-card-row">
                    <div class="lk-card-row-body">
                      <bdi class="lk-card-row-title">{name}</bdi>
                    </div>
                  </div>
                )}
              </For>
            </section>
          )}
        </Match>
        <Match when={fetchState() === "failed"}>
          <section class="lk-card lk-stack" style={{ gap: "8px", padding: "16px" }}>
            <p class="form-error" role="alert" style={{ margin: 0 }}>
              {t("supporters.failed")}
            </p>
            <button type="button" onClick={() => void refreshSupportersList(true)}>
              {t("supporters.retry")}
            </button>
          </section>
        </Match>
        <Match when={true}>
          <div role="status" aria-busy="true" style={{ display: "flex", "justify-content": "center", padding: "24px" }}>
            <span class="lakko-spinner" aria-hidden="true" />
          </div>
        </Match>
      </Switch>
    </>
  );
}
