// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` sections 5 and 6 — a public group or
 * channel found by search: the row under **Groups and channels**, and the
 * card it opens (a pushed screen with a back control) with `Request to join`.
 *
 * The request is a sealed KeyPackage knocked once (ADR-0087 section 5,
 * `ContactsClient.requestToJoinSealed`); nothing to fill in. It cannot be
 * withdrawn and is never said to be refused. Subscribing to a channel stays
 * as it is today (phase B): the card of a channel opens its page.
 */
import { createSignal, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode } from "../auth/account-id";
import { useChats, type ChatApi } from "../chat/ChatContext";
import { readIsActive } from "../chat/membership-ended";
import { useLineage, type LineageView } from "../chat/use-lineage";
import { channelsState } from "../state/channels-state";
import { recordRequestSent, sentRequestFor, mayAskAgain } from "../chat/requests-sent";
import { ContactsClient } from "../contacts/client";
import { DsHttpError, DsNetworkError } from "../ds/client";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { rememberCardAvatar } from "../profile/avatar-runtime";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { PushedScreen } from "../ui/PushedScreen";
import type { FoundConversation } from "./people-search";

/** `Public group · 100+ members` / `Public channel · 10k+ subscribers`. */
export function sizeLine(c: FoundConversation): string {
  const band = BANDS[Math.max(0, Math.min(BANDS.length - 1, c.sizeBand))] ?? BANDS[0] ?? "";
  return c.kind === "channel"
    ? t("search.conversation.channel_line", { band })
    : t("search.conversation.group_line", { band });
}

const BANDS = ["<10", "10+", "100+", "1k+", "10k+", "100k+"];

/** The hex form of a found conversation's id (a channel's route id). */
export function foundIdHex(found: FoundConversation): string {
  let hex = "";
  for (const b of atob(found.idB64)) hex += b.charCodeAt(0).toString(16).padStart(2, "0");
  return hex;
}

/** Whether this account is already in the found group or channel: a group
 *  any segment of whose lineage this device holds, a channel in the channel
 *  list. Such a card offers `Open`, never a request (UX section 5.1). */
export function isMemberOf(
  found: FoundConversation,
  chats: Pick<ChatApi, "get">,
  lineage: Pick<LineageView, "resolve"> | null,
): boolean {
  if (found.kind === "channel") {
    const hex = foundIdHex(found);
    return channelsState.channels().some((c) => c.id === hex);
  }
  if (chats.get(found.idB64) !== undefined) return true;
  const view = lineage?.resolve(found.idB64);
  return view !== undefined && readIsActive(view.head.group) !== false;
}

/** The lineage view where the device-group sync is mounted (the app);
 *  `null` outside it, where only the group id itself is matched. */
export function useLineageIfMounted(): LineageView | null {
  try {
    return useLineage();
  } catch {
    return null;
  }
}

/** One row under **Groups and channels**. */
export function ConversationRow(props: {
  readonly found: FoundConversation;
  readonly onOpen: (found: FoundConversation) => void;
}): JSX.Element {
  const chats = useChats();
  const lineage = useLineageIfMounted();
  const member = (): boolean => isMemberOf(props.found, chats, lineage);
  // UX 4.5.4 — the row shows the photo of a public one.
  rememberCardAvatar(`conv:${props.found.idB64}`, props.found.avatar);
  return (
    <button type="button" class="lk-row" onClick={() => props.onOpen(props.found)}>
      <Avatar label={props.found.name} id={props.found.idB64} personKey={`conv:${props.found.idB64}`} />
      <span class="lk-row-body">
        <span class="lk-row-title">{props.found.name}</span>
        <span class="lk-row-sub">
          <span dir="ltr">{`@${props.found.handle}`}</span>
          {" · "}
          {sizeLine(props.found)}
        </span>
      </span>
      <Show when={member()}>
        <span class="lk-row-trail">{t("search.conversation.open")}</span>
      </Show>
    </button>
  );
}

type CardState =
  | { kind: "idle" }
  | { kind: "sending" }
  | { kind: "sent" }
  | { kind: "error"; message: string };

/** Section 5.1 — the card. */
export function ConversationCard(props: {
  readonly found: FoundConversation | null;
  readonly onBack: () => void;
  /** Open a conversation this device is in. */
  readonly onOpenConversation: (idB64: string) => void;
  /** A channel: open it; `subscribe` subscribes in the same step. */
  readonly onOpenChannel?: (found: FoundConversation, subscribe: boolean) => void;
}): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const [state, setState] = createSignal<CardState>({ kind: "idle" });
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
  });

  const lineage = useLineageIfMounted();
  const member = (): boolean => props.found !== null && isMemberOf(props.found, chats, lineage);
  const alreadySent = (): boolean => {
    const f = props.found;
    return f !== null && !mayAskAgain(sentRequestFor(f.idB64), Date.now());
  };

  async function request(): Promise<void> {
    const f = props.found;
    if (f === null || f.kind !== "group") return;
    setState({ kind: "sending" });
    try {
      await client.requestToJoinSealed(
        auth.identitySecrets(),
        base64Decode(f.idB64),
        f.key,
        f.knockVersion,
      );
      let bin = "";
      for (const b of f.key) bin += String.fromCharCode(b);
      recordRequestSent({
        idB64: f.idB64,
        name: f.name,
        handle: f.handle,
        sentAtMs: Date.now(),
        avatar: f.avatar,
        knockKeyB64: btoa(bin),
        knockVersion: f.knockVersion,
      });
      setState({ kind: "sent" });
    } catch (err) {
      setState({ kind: "error", message: requestErrorText(err) });
    }
  }

  return (
    <PushedScreen open={props.found !== null} title={props.found?.name ?? ""} onBack={props.onBack}>
      <Show when={props.found}>
        {(f) => (
          <div class="lk-stack lk-conversation-card" style={{ padding: "16px", "align-items": "center" }}>
            <Avatar label={f().name} id={f().idB64} size="profile" personKey={`conv:${f().idB64}`} playVideo />
            <h2 style={{ margin: 0 }}>{f().name}</h2>
            <span dir="ltr" class="lk-card-desc">{`@${f().handle}`}</span>
            <span class="lk-card-desc">{sizeLine(f())}</span>
            <Show when={f().description.length > 0}>
              <p style={{ "white-space": "pre-wrap", "text-align": "center" }}>{f().description}</p>
            </Show>
            <Show
              when={!member()}
              fallback={
                <button
                  type="button"
                  class="primary"
                  onClick={() =>
                    f().kind === "channel" ? props.onOpenChannel?.(f(), false) : props.onOpenConversation(f().idB64)
                  }
                >
                  {t("search.conversation.open")}
                </button>
              }
            >
              <Show
                when={f().kind === "group"}
                fallback={
                  <button type="button" class="primary" onClick={() => props.onOpenChannel?.(f(), true)}>
                    {t("search.conversation.subscribe")}
                  </button>
                }
              >
                <Show
                  when={state().kind !== "sent" && !alreadySent()}
                  fallback={
                    <>
                      <button type="button" class="primary" disabled>
                        {t("search.conversation.request_sent_button")}
                      </button>
                      <p class="form-info" role="status">
                        {t("search.conversation.request_sent")}
                      </p>
                    </>
                  }
                >
                  <button
                    type="button"
                    class="primary"
                    disabled={state().kind === "sending"}
                    onClick={() => void request()}
                  >
                    <Icon name="plus" size={16} />
                    {t("search.conversation.request")}
                  </button>
                </Show>
              </Show>
            </Show>
            <Show when={state().kind === "error" ? (state() as { message: string }).message : null}>
              {(m) => (
                <p class="form-error" role="alert">
                  {m()}
                </p>
              )}
            </Show>
          </div>
        )}
      </Show>
    </PushedScreen>
  );
}

/** UX 6.1 — the outcomes a request can have. */
export function requestErrorText(err: unknown): string {
  const code = (err as { errorCode?: unknown; code?: unknown })?.errorCode ?? (err as { code?: unknown })?.code;
  if (err instanceof DsHttpError && err.status === 429) {
    return code === "knock-queue-full" || /knock-queue-full/.test(String(err.message))
      ? t("search.conversation.queue_full")
      : t("search.conversation.out_of_requests");
  }
  if (err instanceof DsNetworkError) return t("group_handle.error.offline");
  const text = err instanceof Error ? err.message : "";
  if (/knock-queue-full/.test(text)) return t("search.conversation.queue_full");
  if (/429|token/i.test(text)) return t("search.conversation.out_of_requests");
  return t("group_handle.error.offline");
}

