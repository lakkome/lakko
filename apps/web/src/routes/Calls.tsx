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
import { newCallId, newCallStart } from "../calls/new-call";
import { useCallHost } from "../calls/CallHostContext";
import { useChats } from "../chat/ChatContext";
import { conversationTitle } from "../chat/conversation-title";
import { conversationAvatarKeys, type ConversationAvatarKeys } from "../chat/conversation-avatar";
import { conversationKindOf } from "../chat/conversation-kind";
import { groupTitle } from "../chat/group-title";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { useGroupNames } from "../chat/GroupNameContext";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { paths } from "./paths";
import { t } from "../l10n";
import { callFailureText } from "../calls/call-errors";
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
      nameOf: (k) => memberNameOf(profiles.resolve(k), "a Calls row title"),
      groupTitle,
    });
  };

  /** ADR-0084 / ADR-0087 section 18 - what a call row's avatar shows: the
   *  other person's photo in a one-to-one, the group's own photo in a group
   *  (`chat/conversation-avatar.ts`). */
  const avatarKeysOf = (groupIdB64: string): ConversationAvatarKeys => {
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return {};
    const ownB64 = ownPersonKeyB64(chats.identity());
    return conversationAvatarKeys({
      kind: conversationKindOf(entry.group),
      groupIdB64,
      peerKeyB64: personKeysOf(entry.group).find((k) => k !== ownB64),
    });
  };

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

  /** The row opens the conversation; the button places the call. Two
   *  targets, so their hit areas must not overlap. */
  const open = (groupIdB64: string): void => {
    navigate(paths.chat(base64UrlOf(groupIdB64)));
  };

  const callBack = (row: CallHistoryRow): void => {
    // Back in the mode the row records: a video call re-dials video.
    // A call that could not start (also: no session the person's devices
    // are in exists here yet, ADR-0073 R.12) reads as it does in the chat.
    setStartError(null);
    callHost.dial(row.groupIdB64, row.entry.video).catch((err: unknown) => {
      setStartError(callFailureText(err, () => t("chat.call.error.start")));
    });
  };

  const iconFor = (row: CallHistoryRow): "incoming" | "outgoing" | "missed" => {
    switch (row.entry.outcome) {
      case "missed":
      case "declined":
      case "failed":
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
                          <Avatar
                            label={title()}
                            id={row.groupIdB64}
                            personKey={avatarKeysOf(row.groupIdB64).personKey}
                            conversationKey={avatarKeysOf(row.groupIdB64).conversationKey}
                          />
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
