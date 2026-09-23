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
 */

import { createMemo, For, Show, type JSX } from "solid-js";
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
import CreateCallLinkPanel from "../calls/CreateCallLinkPanel";
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
    if (entry === undefined) return "Unknown";
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
    <aside class="lk-pane" aria-label="Calls">
      <div class="lk-pane-head">
        <h1>Calls</h1>
      </div>
      <div class="lk-pane-body">
        <Show
          when={auth.state().kind === "session" && days().length > 0}
          fallback={
            <div class="lk-empty">
              <Icon name="calls" size={40} strokeWidth={1.4} />
              <strong>No calls yet</strong>
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
                          title={row.entry.video ? "Video call" : "Call"}
                          aria-label={row.entry.video ? "Video call" : "Call"}
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
        <CreateCallLinkPanel />
      </div>
    </aside>
  );
}

/** The chat route takes the base64url form of the group id. */
function base64UrlOf(groupIdB64: string): string {
  return groupIdB64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
