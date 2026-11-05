// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Runs the rule of `adopt-one-to-one.ts` — a person this account has a
 * one-to-one conversation with IS a contact — over the live conversation
 * registry.
 *
 * Mounted once for the whole session, beside the other session-wide
 * receivers in `App.tsx`, because the conversations it reads arrive while
 * the user is on any route (a Welcome opened by the mailbox loop, the
 * conversations a second device is admitted to under ADR-0050) and the
 * Contacts list must be right the first time it is opened.
 *
 * It runs once at start and again on every distinct change of the set of
 * known conversation ids, and additionally when a profile lands (ADR-0048
 * v2 names the peer's account id, which completes a row written before the
 * profile arrived). Every run is idempotent: the rule returns `null` for a
 * complete row, so a run that finds nothing to do writes nothing.
 *
 * A conversation counts when its live roster holds exactly two PEOPLE
 * (ADR-0050 collapses one person's devices into one person; a group of two
 * people that was created as a group is still two people, and the person in
 * it is still someone this account talks to). The peer is the person who is
 * not this device's own account.
 */
import { createEffect, createMemo, on, type Component } from "solid-js";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { sessionAccountOf } from "../chat/persist-hooks";
import { conversationKindOf } from "../chat/conversation-kind";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { useProfiles } from "../profile/ProfileContext";
import { foundNameForKey, retireAnsweredSentRequests } from "./contact-ack";
import { adoptablePeerOf } from "./adopt-one-to-one";
import { isOneToOneWithContact } from "./contact-conversation";
import { adoptOneToOne } from "./local-store";

export const OneToOneContactAdoption: Component = () => {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();

  // The set of known conversation ids, as one string, so the effect below
  // fires on a distinct change of the SET and not on every registry write
  // (a replaced group object under the same id is the same conversation).
  const groupIdSet = createMemo(() =>
    chats
      .groups()
      .map((g) => g.groupIdB64)
      .sort()
      .join("\n"),
  );

  createEffect(
    on([groupIdSet, profiles.version, chats.identity], () => {
      const own = ownPersonKeyB64(chats.identity());
      if (own === null) return;
      const nowMs = Date.now();
      const groups = chats.groups();
      const isOneToOne = (g: (typeof groups)[number]): boolean => conversationKindOf(g.group) === "chat";
      // A row's recorded chat still counts only while it is that person's
      // one-to-one on this device; otherwise this conversation replaces it.
      const linkHolds = (groupIdB64: string, row: Parameters<typeof isOneToOneWithContact>[0]["contact"]): boolean => {
        const linked = groups.find((g) => g.groupIdB64 === groupIdB64);
        return (
          linked !== undefined &&
          isOneToOneWithContact({
            conversation: linked,
            contact: row,
            ownKeyB64: own,
            peopleOf: (g) => personKeysOf(g.group),
            isOneToOne,
          })
        );
      };
      for (const g of groups) {
        const peer = adoptablePeerOf({ isOneToOne: isOneToOne(g), people: personKeysOf(g.group), ownKeyB64: own });
        if (peer === null) continue;
        const wrote = adoptOneToOne(
          {
            signatureKey: peer,
            accountIdHex: profiles.accountIdHexOf(peer),
            groupIdB64: g.groupIdB64,
            nowMs,
          },
          undefined,
          linkHolds,
        );
        const session = sessionAccountOf(auth.state());
        // A Welcome ahead of the ack makes the row: it is not a stand-in
        // either (`contact-ack.ts`).
        const found = wrote && session !== null ? foundNameForKey(session, peer) : null;
        if (found !== null) profiles.fileFoundName(peer, found);
        // The one-to-one is here: the Sent request it answers is answered
        // (base walk 2026-11-04: the ack alone makes no contact).
        if (session !== null) retireAnsweredSentRequests(session, peer, profiles.accountIdHexOf(peer) ?? null);
      }
    }),
  );

  return null;
};
