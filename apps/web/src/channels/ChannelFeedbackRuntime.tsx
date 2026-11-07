// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Binds the channel comments-and-reactions state to the signed-in account,
 * whatever screen is showing (ADR-0087 section 13a): reads back the kept ban
 * lists and the reactions still waiting for a token, and lets the waiting ones
 * leave once an identity and the synced state are there. Renders nothing;
 * mounted once in `App.tsx`. The channel screens bind the same way on mount,
 * so nothing depends on the mount order.
 */
import { createEffect, type Component } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { bindBanListAccount } from "./ban-list";
import { ownFeedbackPort, ownFeedbackVersion } from "./own-feedback-register";
import { bindReactionAccount, bindReactionIdentity, kickWaitingReactions } from "./post-feedback";

type AuthApi = ReturnType<typeof useAuth>;
type ChatsApi = ReturnType<typeof useChats>;

/** Bind both stores to the session's account and the reaction queue to the
 *  conversation identity. Idempotent; called from every channel feedback
 *  component and from {@link ChannelFeedbackRuntime}. */
export function bindChannelFeedback(auth: AuthApi, chats: ChatsApi): void {
  bindReactionIdentity(() => chats.conversationAccountIdentity());
  createEffect(() => {
    const s = auth.state();
    const account = s.kind === "session" ? s.account.accountId : null;
    bindBanListAccount(account);
    bindReactionAccount(account);
  });
}

export const ChannelFeedbackRuntime: Component = () => {
  const auth = useAuth();
  const chats = useChats();
  bindChannelFeedback(auth, chats);
  // An identity or the synced state arriving lets waiting reactions leave.
  let wasReady = false;
  createEffect(() => {
    void ownFeedbackVersion();
    const ready = chats.conversationAccountIdentity() !== null && ownFeedbackPort() !== null;
    if (ready && !wasReady) kickWaitingReactions();
    wasReady = ready;
  });
  return null;
};
