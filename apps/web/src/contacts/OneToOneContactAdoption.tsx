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
import { useChats } from "../chat/ChatContext";
import { oneToOnePeerKey, ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { useProfiles } from "../profile/ProfileContext";
import { adoptOneToOne } from "./local-store";

export const OneToOneContactAdoption: Component = () => {
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
      for (const g of chats.groups()) {
        const peer = oneToOnePeerKey(personKeysOf(g.group), own);
        if (peer === null) continue;
        adoptOneToOne({
          signatureKey: peer,
          accountIdHex: profiles.accountIdHexOf(peer),
          groupIdB64: g.groupIdB64,
          nowMs,
        });
      }
    }),
  );

  return null;
};
