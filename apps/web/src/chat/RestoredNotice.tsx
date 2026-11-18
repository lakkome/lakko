// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the chat list of a device restored from the phrase or a passkey says
 * until its first conversation arrives (`docs/ux/identity-recovery.md`
 * section 7.2): a card on the empty list when this device founded the
 * account's DeviceGroup because no other device answered its rejoin knock.
 * When another device answered, or the backup named no conversation, the
 * list is the ordinary one: a person cannot act on work another device does
 * in the background. Whether another device is merely listed in the directory
 * is not asked: a lost device stays listed. The card is not a loading state,
 * so it has no spinner, and it goes for good with the first conversation.
 */
import { createEffect, type Accessor, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import {
  clearRestoreMarker,
  isRestoreMarked,
  restoreAnswer,
  restoredConversationCount,
  restoredListState,
  restoredMarkerVersion,
  type RestoredListState,
} from "../backup/restored-marker";
import { t } from "../l10n";
import { paths } from "../routes/paths";

/** The restored state of the list. The marker goes the moment a conversation
 *  is there, and never comes back. */
export function useRestoredList(conversationCount: Accessor<number>): Accessor<RestoredListState> {
  const auth = useAuth();
  const account = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  const marked = (): boolean => {
    restoredMarkerVersion();
    const id = account();
    return id !== null && isRestoreMarked(id);
  };

  createEffect(() => {
    const id = account();
    if (id !== null && conversationCount() > 0 && marked()) clearRestoreMarker(id);
  });

  return () => {
    restoredMarkerVersion();
    const id = account();
    return restoredListState({
      marked: marked(),
      knownConversations: id === null ? 0 : restoredConversationCount(id),
      conversationCount: conversationCount(),
      answer: id === null ? null : restoreAnswer(id),
    });
  };
}

export function RestoredNotice(): JSX.Element {
  const navigate = useNavigate();
  return (
    <div class="lk-empty">
      <span>{t("chats.restored.card")}</span>
      <button type="button" class="primary" onClick={() => navigate(paths.contacts)}>
        {t("nav.contacts")}
      </button>
    </div>
  );
}
