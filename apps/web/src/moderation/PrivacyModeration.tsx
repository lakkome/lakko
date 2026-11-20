// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX `reporting-and-moderation.md` sections 1.1 and 2.3 - Settings > Privacy:
 * the two rows (Blocked people, Your reports) and the two screens they open.
 */

import { A } from "@solidjs/router";
import { createSignal, For, onMount, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";
import { loadWasm } from "../wasm";
import { unblockPerson } from "./block-actions";
import { blockedEntries } from "./block-registry";
import { fetchReportStatus } from "./report-api";
import { reasonKey, statusKey, type ReportStatus } from "./report-model";
import { loadSentReports, type SentReport } from "./reports-store";

/** The two rows on the Privacy page. */
export function PrivacyModerationRows(): JSX.Element {
  return (
    <section class="lk-card">
      <A href={paths.settingsSection("privacy-blocked")} class="lk-card-row lk-card-row-link">
        <span class="lk-card-row-icon">
          <Icon name="shieldAlert" size={18} />
        </span>
        <span class="lk-card-row-body">
          <span class="lk-card-row-title">{t("block.list.title")}</span>
        </span>
        <Icon name="chevron" size={18} />
      </A>
      <A href={paths.settingsSection("privacy-reports")} class="lk-card-row lk-card-row-link">
        <span class="lk-card-row-icon">
          <Icon name="flag" size={18} />
        </span>
        <span class="lk-card-row-body">
          <span class="lk-card-row-title">{t("report.list.title")}</span>
        </span>
        <Icon name="chevron" size={18} />
      </A>
    </section>
  );
}

/** Blocked people: names from contacts and profiles, Unblock per row. */
export function BlockedPeopleList(): JSX.Element {
  const profiles = useProfiles();
  const [busy, setBusy] = createSignal<string | null>(null);
  const [failed, setFailed] = createSignal<string | null>(null);
  async function unblock(keyB64: string): Promise<void> {
    setBusy(keyB64);
    setFailed(null);
    try {
      await unblockPerson(keyB64);
    } catch {
      setFailed(keyB64);
    } finally {
      setBusy(null);
    }
  }
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("block.list.title")}</h1>
      </div>
      <Show
        when={blockedEntries().length > 0}
        fallback={<p class="lk-meta lk-center">{t("block.list.empty")}</p>}
      >
        <section class="lk-card">
          <For each={blockedEntries()}>
            {(entry) => {
              const name = (): string => memberNameOf(profiles.resolve(entry.keyB64), "a blocked person row");
              return (
                <div class="lk-card-row">
                  <Avatar label={name()} id={entry.keyB64} personKey={entry.keyB64} />
                  <div class="lk-card-row-body">
                    <span class="lk-card-row-title">{name()}</span>
                    <Show when={failed() === entry.keyB64}>
                      <span class="lk-card-row-sub form-error">{t("common.error.unavailable")}</span>
                    </Show>
                  </div>
                  <button
                    type="button"
                    class="lk-card-row-action"
                    disabled={busy() !== null}
                    onClick={() => void unblock(entry.keyB64)}
                  >
                    {busy() === entry.keyB64 ? t("common.saving") : t("block.unblock")}
                  </button>
                </div>
              );
            }}
          </For>
        </section>
      </Show>
    </>
  );
}

/** Your reports: what this device sent, with a status fetched on open by
 *  each report's receipt alone. */
export function YourReportsList(): JSX.Element {
  const auth = useAuth();
  const [reports, setReports] = createSignal<readonly SentReport[] | null>(null);
  const [statuses, setStatuses] = createSignal<ReadonlyMap<string, ReportStatus>>(new Map());
  const [reasonNames, setReasonNames] = createSignal<ReadonlyMap<number, string>>(new Map());
  onMount(() => {
    void loadWasm().then((wasm) => {
      const names = new Map<number, string>();
      for (const code of Array.from(wasm.reportReasonCodes())) names.set(code, wasm.reportReasonName(code) ?? "other");
      setReasonNames(names);
    });
    const s = auth.state();
    if (s.kind !== "session") {
      setReports([]);
      return;
    }
    void loadSentReports(s.account.accountId).then((list) => {
      setReports(list);
      for (const r of list) {
        void fetchReportStatus(r.receiptId).then((status) => {
          if (status === null) return;
          setStatuses((held) => new Map(held).set(r.receiptId, status));
        });
      }
    });
  });
  const dateOf = (ms: number): string =>
    new Date(ms).toLocaleDateString(displayLocale(), { year: "numeric", month: "short", day: "numeric" });
  return (
    <>
      <div class="lk-page-head">
        <h1>{t("report.list.title")}</h1>
      </div>
      <Show when={reports()} fallback={<p class="lk-meta lk-center">{t("common.loading")}</p>}>
        {(list) => (
          <Show when={list().length > 0} fallback={<p class="lk-meta lk-center">{t("report.list.empty")}</p>}>
            <section class="lk-card">
              <For each={list()}>
                {(r) => (
                  <div class="lk-card-row">
                    <span class="lk-card-row-icon">
                      <Icon name="flag" size={18} />
                    </span>
                    <div class="lk-card-row-body">
                      <span class="lk-card-row-title">{r.targetName}</span>
                      <span class="lk-card-row-sub">
                        {t("common.fact_pair", {
                          first: t(reasonKey(reasonNames().get(r.reason) ?? "other")),
                          second: dateOf(r.sentAtMs),
                        })}
                      </span>
                    </div>
                    <Show when={statuses().get(r.receiptId)}>
                      {(status) => <span class="lk-meta">{t(statusKey(status()))}</span>}
                    </Show>
                  </div>
                )}
              </For>
            </section>
          </Show>
        )}
      </Show>
    </>
  );
}
