// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX `reporting-and-moderation.md` 2.2 and 2.3 - the report sheet, one on the
 * page, mounted in the router's root layout. A dialog-sized card on wide web,
 * a bottom sheet on a phone width (`.lk-report-sheet`). Every rule is
 * `report-model.ts`; the draft is `report-session.ts`; this holds markup.
 */

import { createEffect, createMemo, createSignal, For, on, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { ownPersonKeyB64 } from "../chat/person-key";
import { useChats } from "../chat/ChatContext";
import { shortTime } from "../chat/timeline-layout";
import { displayLocale } from "../l10n/display-locale";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { ModalLayer } from "../ui/ModalLayer";
import { t } from "../l10n";
import { loadWasm } from "../wasm";
import { blockPerson } from "./block-actions";
import {
  afterSend,
  canSend,
  includedCounts,
  includedLine,
  namesTheReported,
  pickCandidatesFor,
  reasonKey,
  REPORT_COMMENT_MAX,
  REPORT_MAX_MESSAGES,
  type PickCandidate,
  showsEmptyHint,
  withComment,
  withReason,
} from "./report-model";
import { pickCandidates, reportPorts } from "./report-runtime";
import { availablePicks, sendReport } from "./report-send";
import {
  closeReport,
  reportSent,
  reportSentVersion,
  reportSession,
  finishPicking,
  setReportPhase,
  startPicking,
  togglePicked,
  updateDraft,
} from "./report-session";
import { recordSentReport } from "./reports-store";

/** How long the "Report sent" notice stays (chosen). */
const SENT_NOTICE_MS = 4_000;

export function ReportSheetHost(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const [reasons, setReasons] = createSignal<readonly { code: number; name: string }[]>([]);
  void loadWasm().then((wasm) => {
    const codes = Array.from(wasm.reportReasonCodes());
    setReasons(codes.map((code) => ({ code, name: wasm.reportReasonName(code) ?? "other" })));
  });

  // How many picked messages still have their evidence here (UX 2.3,
  // "Evidence can't be prepared"). Re-read whenever the picks change.
  const [available, setAvailable] = createSignal(0);
  // Keyed on the picks alone (the list is replaced only by a toggle), so a
  // keystroke in the comment reads nothing.
  createEffect(
    on(
      () => reportSession()?.draft.picked,
      (picked) => {
        const draft = reportSession()?.draft;
        if (picked === undefined || draft === undefined) return;
        void reportPorts(accountId()).then(async (ports) => {
          const kept = await availablePicks(draft, ports);
          if (reportSession()?.draft.picked === picked) setAvailable(kept);
        });
      },
    ),
  );

  const open = (): boolean => reportSession() !== null;
  const picking = (): boolean => reportSession()?.picking === true;
  const draft = () => reportSession()?.draft;
  const phase = () => reportSession()?.phase ?? "editing";
  const name = (): string => draft()?.target.name ?? "";
  const counts = createMemo(() => {
    const d = draft();
    return d === undefined ? { kept: 0, total: 0 } : includedCounts(d, available());
  });

  async function runAlso(): Promise<void> {
    const s = reportSession();
    if (s === null || !s.draft.alsoChecked || s.draft.also === null) return;
    try {
      if (s.draft.also === "block" && s.draft.target.kind === "person") {
        await blockPerson(s.draft.target.accountKeyB64);
      } else if (s.leave !== null) {
        await s.leave();
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("the report's block or leave did not apply", err instanceof Error ? err.name : "unknown");
    }
  }

  async function send(): Promise<void> {
    const s = reportSession();
    if (s === null || !canSend(s.draft, s.phase)) return;
    setReportPhase("sending");
    const outcome = await sendReport(s.draft, await reportPorts(accountId()));
    const next = afterSend(outcome);
    if (next.runsAlso) await runAlso();
    if (next.phase === "closed") {
      const account = accountId();
      if (outcome.kind === "sent" && account !== null && s.draft.reason !== null) {
        void recordSentReport(account, {
          receiptId: outcome.receiptId,
          targetName: s.draft.target.name,
          reason: s.draft.reason,
          sentAtMs: Date.now(),
        });
      }
      reportSent();
      return;
    }
    setReportPhase(next.phase);
  }

  // UX 2.2 - `Add messages` opens the "Choose messages" list inside the
  // sheet: the reported author's messages (anyone's but the reporter's for a
  // group or channel), up to the limit.
  const [candidates, setCandidates] = createSignal<readonly PickCandidate[]>([]);
  const [limitShown, setLimitShown] = createSignal(false);
  function addMessages(): void {
    const d = draft();
    if (d === undefined || d.pickFrom === null) return;
    const source = d.pickFrom;
    setCandidates([]);
    setLimitShown(false);
    startPicking();
    void pickCandidates(accountId(), source).then((all) => {
      if (reportSession()?.draft.pickFrom !== source) return;
      setCandidates(pickCandidatesFor(all, source, ownPersonKeyB64(chats.identity())));
    });
  }
  function toggle(id: number): void {
    setLimitShown(!togglePicked(id));
  }
  const candidateLine = (c: PickCandidate): string => {
    const text = c.text.replace(/\s+/g, " ").trim();
    if (text.length > 0) return text.length > 120 ? `${text.slice(0, 119)}\u2026` : text;
    return c.filesLabel;
  };
  const candidateMeta = (c: PickCandidate): string => {
    const time = shortTime(c.createdAt, displayLocale());
    if (c.authorKeyB64 === undefined) return time;
    return t("common.fact_pair", {
      first: memberNameOf(profiles.resolve(c.authorKeyB64), "a report candidate"),
      second: time,
    });
  };

  // The "Report sent" notice.
  const [noticeShown, setNoticeShown] = createSignal(false);
  createEffect(
    on(
      reportSentVersion,
      () => {
        setNoticeShown(true);
        const timer = setTimeout(() => setNoticeShown(false), SENT_NOTICE_MS);
        onCleanup(() => clearTimeout(timer));
      },
      { defer: true },
    ),
  );

  const failureLine = (): string | null => {
    switch (phase()) {
      case "failed":
        return t("report.failed");
      case "limited":
        return t("report.limited");
      case "unavailable":
        return t("report.unavailable");
      default:
        return null;
    }
  };

  return (
    <>
      <ModalLayer
        open={open()}
        title={picking() ? t("report.select.title") : t("report.title", { name: name() })}
        onClose={closeReport}
        class="lk-report-sheet"
      >
        <Show when={picking() ? draft() : undefined}>
          {(d) => (
            <div class="lk-stack lk-report-choose">
              <Show
                when={candidates().length > 0}
                fallback={<p class="lk-meta">{t("report.included_none")}</p>}
              >
                <ul class="lk-report-choose-list">
                  <For each={candidates()}>
                    {(c) => (
                      <li>
                        <label class="lk-card-row lk-privacy-choice">
                          <input
                            type="checkbox"
                            checked={d().picked.includes(c.id)}
                            onChange={() => toggle(c.id)}
                          />
                          <span class="lk-card-row-body">
                            <span class="lk-card-row-title">{candidateLine(c)}</span>
                            <span class="lk-card-row-sub">{candidateMeta(c)}</span>
                          </span>
                        </label>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
              <Show when={limitShown()}>
                <p class="lk-meta" role="status">
                  {t("report.select.limit", { n: REPORT_MAX_MESSAGES })}
                </p>
              </Show>
              <div class="lk-actions lk-report-actions">
                <button type="button" class="primary" onClick={finishPicking}>
                  {t("report.select.done")}
                </button>
              </div>
            </div>
          )}
        </Show>
        <Show when={picking() ? undefined : draft()}>
          {(d) => (
            <form
              class="lk-stack lk-report-form"
              onSubmit={(e) => {
                e.preventDefault();
                void send();
              }}
            >
              <fieldset class="lk-report-reasons">
                <legend class="lk-report-legend">{t("report.why")}</legend>
                <For each={reasons()}>
                  {(r) => (
                    <label class="lk-card-row lk-privacy-choice lk-radio-row">
                      <input
                        type="radio"
                        name="lk-report-reason"
                        checked={d().reason === r.code}
                        onChange={() => updateDraft((x) => withReason(x, r.code))}
                      />
                      <span class="lk-card-row-body">
                        <span class="lk-card-row-title">{t(reasonKey(r.name))}</span>
                      </span>
                    </label>
                  )}
                </For>
              </fieldset>

              <div class="lk-report-included">
                <span>
                  {(() => {
                    const line = includedLine(counts().kept, counts().total);
                    return t(line.key, line.args);
                  })()}
                </span>
                <Show when={d().pickFrom !== null}>
                  <button type="button" class="ghost small" onClick={addMessages}>
                    {t("report.add_messages")}
                  </button>
                </Show>
              </div>
              <Show when={showsEmptyHint(counts().kept)}>
                <p class="lk-meta">{t("report.empty_hint")}</p>
              </Show>

              <label class="lk-report-comment">
                <span class="lk-field-label">{t("report.comment")}</span>
                <textarea
                  rows={3}
                  maxLength={REPORT_COMMENT_MAX}
                  value={d().comment}
                  onInput={(e) => {
                    const value = e.currentTarget.value;
                    updateDraft((x) => withComment(x, value));
                  }}
                />
              </label>

              <Show when={d().also}>
                {(also) => (
                  <label class="lk-card-row lk-privacy-choice">
                    <input
                      type="checkbox"
                      checked={d().alsoChecked}
                      onChange={(e) => {
                        const checked = e.currentTarget.checked;
                        updateDraft((x) => ({ ...x, alsoChecked: checked }));
                      }}
                    />
                    <span class="lk-card-row-body">
                      <span class="lk-card-row-title">
                        {also() === "block"
                          ? t("report.also_block", { name: name() })
                          : also() === "leave"
                            ? t("report.also_leave")
                            : t("report.also_unsubscribe")}
                      </span>
                    </span>
                  </label>
                )}
              </Show>

              <p class="lk-meta">
                {namesTheReported(d().target)
                  ? t("common.sentence_pair", {
                      first: t("report.who_sees"),
                      second: t("report.not_told", { name: name() }),
                    })
                  : t("report.who_sees")}
              </p>
              <Show when={failureLine()}>
                {(line) => (
                  <div class="form-error" role="alert">
                    {line()}
                  </div>
                )}
              </Show>
              <div class="lk-actions lk-report-actions">
                <button type="submit" class="primary" disabled={!canSend(d(), phase())}>
                  {phase() === "sending" ? t("common.sending") : t("report.send")}
                </button>
              </div>
            </form>
          )}
        </Show>
      </ModalLayer>
      <Show when={noticeShown()}>
        <div class="lk-toast" role="status" aria-live="polite">
          {t("report.sent")}
        </div>
      </Show>
    </>
  );
}
