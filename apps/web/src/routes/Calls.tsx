// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Calls: recent calls across every conversation, newest first.
 *
 * `docs/ux/navigation.md` section 5. The destination was removed by the
 * client redesign on the recorded ground that it held no history - only a
 * create-link panel - and that was true of what existed. It was never an
 * argument against call history, and the design system's own information
 * architecture kept "Calls: recent calls" as destination 4 throughout.
 *
 * The call log has been written per conversation all along
 * (`calls/call-log-store.ts`); what was missing was the cut across
 * conversations, which is `calls/call-history.ts` and is where the ordering
 * and the day grouping are tested. This file draws them.
 *
 * The destination's action is `New call` and nothing else
 * (`docs/ux/call-links.md` section 2). It starts a group video call and shows
 * the call screen; nothing is asked first. The create-link form that used to
 * stand here — four questions and a string of text the person then carried
 * somewhere themselves — is gone, and the link it produced is now a property
 * of a call that is already happening (`Invite`, section 3). `Join by link`
 * stays for a link that did not open by itself, without the weight of a main
 * action.
 */

import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import {
  callHistory,
  callHistoryByDay,
  callHistoryDayLabel,
  type CallHistoryRow,
} from "../calls/call-history";
import { callRowLabel } from "../calls/call-log";
import { useCallLog } from "../calls/CallLogContext";
import { callLinkTarget } from "../calls/call-link-slug";
import { newCallId, newCallStart } from "../calls/new-call";
import { useCallHost } from "../calls/CallHostContext";
import { useChats } from "../chat/ChatContext";
import { conversationTitle } from "../chat/conversation-title";
import { groupTitle } from "../chat/group-title";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { useGroupNames } from "../chat/GroupNameContext";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { paths } from "./paths";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";

export default function Calls(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const callLog = useCallLog();
  const callHost = useCallHost();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const navigate = useNavigate();

  const days = createMemo(() => callHistoryByDay(callHistory(callLog.all())));

  /**
   * Who the call was with, named exactly as the conversation list names it.
   * The shared rule, not a second one: a chat called "Anna" over there and
   * something else here is the drift this codebase keeps paying for.
   */
  const titleOf = (groupIdB64: string): string => {
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return t("call.history.unknown_conversation");
    const ownB64 = ownPersonKeyB64(chats.identity());
    const memberKeysB64 = personKeysOf(entry.group);
    return conversationTitle({
      // Two members and no shared name reads as a one-to-one; anything else
      // is titled as a group. The row only needs a name, not a routing
      // decision, so it does not consult the kind store.
      kind: memberKeysB64.length <= 2 ? "chat" : "group",
      memberKeysB64,
      ownKeyB64: ownB64,
      fallbackLabel: entry.label,
      sharedName: groupNames.nameOf(groupIdB64),
      contactNameById: undefined,
      contactNameByMember: undefined,
      nameOf: (k) => profiles.resolve(k).name,
      groupTitle,
    });
  };

  /** The pasted link, while "Join by link" is open. */
  const [joinOpen, setJoinOpen] = createSignal(false);
  const [joinInput, setJoinInput] = createSignal("");
  const [joinError, setJoinError] = createLiveText();
  /** Why `New call` could not start, or `null`. */
  const [startError, setStartError] = createLiveText();

  /**
   * Start a group video call and land in it. No form, no media choice, no
   * participant picker — section 2.
   *
   * Nothing is minted here: the call is founded by its owner (ADR-0032), and
   * the link is paid for later by whoever presses `Invite` inside the call.
   */
  async function newCall(): Promise<void> {
    setStartError(null);
    const busy = callHost.callBusyMessage("start");
    if (busy !== null) {
      setStartError(busy);
      return;
    }
    const identity = chats.identity();
    const secrets = auth.identitySecrets();
    if (identity === null || secrets === null) {
      setStartError(() => t("call.error.signed_out_start"));
      return;
    }
    const outcome = await callHost.startLinkCall(
      newCallStart(
        { accountId: secrets.accountId, signaturePublicKey: identity.signaturePublicKey },
        newCallId(),
      ),
    );
    if (outcome.kind === "busy" || outcome.kind === "failed") setStartError(outcome.message);
  }

  /** Open a link somebody sent that did not open by itself. */
  function joinByLink(e: SubmitEvent): void {
    e.preventDefault();
    const target = callLinkTarget(joinInput());
    if (target === null) {
      setJoinError(() => t("call.history.join_invalid"));
      return;
    }
    setJoinError(null);
    navigate(`${paths.joinCall(target.slug)}${target.hash}`);
  }

  /** The row opens the conversation; the button places the call. Two
   *  targets, so their hit areas must not overlap. */
  const open = (groupIdB64: string): void => {
    navigate(paths.chat(base64UrlOf(groupIdB64)));
  };

  const callBack = (row: CallHistoryRow): void => {
    // Back in the mode the row records: a video call re-dials video.
    void callHost.dial(row.groupIdB64, row.entry.video);
  };

  const iconFor = (row: CallHistoryRow): "incoming" | "outgoing" | "missed" => {
    switch (row.entry.outcome) {
      case "missed":
      case "declined":
        return "missed";
      case "incoming":
        return "incoming";
      default:
        return "outgoing";
    }
  };

  return (
    <aside class="lk-pane" aria-label={t("nav.calls")}>
      <div class="lk-pane-head">
        <h1>{t("nav.calls")}</h1>
        <div class="lk-actions">
          {/* Section 2: `Join by link` stays but is not given the weight of a
              main action — a quiet button beside the accent one, not a second
              primary and not a panel of its own. */}
          <button
            type="button"
            class="ghost"
            onClick={() => {
              setJoinOpen(!joinOpen());
              setJoinError(null);
            }}
            aria-expanded={joinOpen()}
          >
            <Icon name="link" size={16} />
            {t("call.history.join_by_link")}
          </button>
          <button type="button" class="primary" onClick={() => void newCall()}>
            <Icon name="video" size={16} />
            {t("call.history.new_call")}
          </button>
        </div>
      </div>
      <div class="lk-pane-body">
        <Show when={startError()}>
          {(message) => <div class="form-error">{message()}</div>}
        </Show>
        <Show when={joinOpen()}>
          <form onSubmit={joinByLink} style={{ padding: "4px 8px 12px" }}>
            <label style={{ display: "block" }}>
              <span>{t("call.history.paste_label")}</span>
              <input
                type="text"
                value={joinInput()}
                onInput={(e) => {
                  setJoinInput(e.currentTarget.value);
                  setJoinError(null);
                }}
                placeholder="https://lakko.me/c/…"
                style={{ width: "100%", "margin-top": "0.25rem" }}
              />
            </label>
            <Show when={joinError()}>
              {(message) => <div class="form-error">{message()}</div>}
            </Show>
            <div class="lk-actions" style={{ "margin-top": "8px" }}>
              <button type="submit">{t("common.open")}</button>
              <button
                type="button"
                class="ghost"
                onClick={() => {
                  setJoinOpen(false);
                  setJoinInput("");
                  setJoinError(null);
                }}
              >
                {t("common.cancel")}
              </button>
            </div>
          </form>
        </Show>
        <Show
          when={auth.state().kind === "session" && days().length > 0}
          fallback={
            <div class="lk-empty">
              <Icon name="calls" size={40} strokeWidth={1.4} />
              <strong>{t("call.history.empty")}</strong>
            </div>
          }
        >
          <For each={days()}>
            {(day) => (
              <>
                <div class="lk-day-sep">{callHistoryDayLabel(day.day)}</div>
                <For each={day.rows}>
                  {(row) => {
                    const title = (): string => titleOf(row.groupIdB64);
                    return (
                      <div class="lk-row">
                        {/* The row opens the conversation and the trailing
                            button places the call: two targets, so the
                            button is a sibling rather than a child. */}
                        <button
                          type="button"
                          class="lk-row-open"
                          onClick={() => open(row.groupIdB64)}
                        >
                          <Avatar label={title()} id={row.groupIdB64} />
                          <div class="lk-row-body">
                            <div class="lk-row-line">
                              <span class="lk-row-title">
                                <span class="lk-ellipsis">{title()}</span>
                              </span>
                            </div>
                            <div class="lk-row-line">
                              <span class="lk-row-sub">
                                <Icon name={iconFor(row)} size={13} />{" "}
                                {callRowLabel(row.entry)}
                              </span>
                            </div>
                          </div>
                        </button>
                        <button
                          type="button"
                          class="lk-icon-button"
                          title={row.entry.video ? t("chat.call.video") : t("chat.call.voice")}
                          aria-label={row.entry.video ? t("chat.call.video") : t("chat.call.voice")}
                          onClick={() => callBack(row)}
                        >
                          <Icon name={row.entry.video ? "video" : "phone"} size={18} />
                        </button>
                      </div>
                    );
                  }}
                </For>
              </>
            )}
          </For>
        </Show>
      </div>
    </aside>
  );
}

/** The chat route takes the base64url form of the group id. */
function base64UrlOf(groupIdB64: string): string {
  return groupIdB64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
