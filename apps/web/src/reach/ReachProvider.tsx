// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - binds the reach session and the "who can find me" setting to
 * the signed-in account.
 *
 * Mounted inside `<AuthProvider>` and `<ChatProvider>`. When the account
 * identity appears it starts the reach session (a copy of the account seed,
 * the encrypted reach book), loads the setting (the default for a new
 * account), points the session at the live group and channel lists, and
 * syncs: the account mailbox gets the keys the setting calls for and the
 * `open` flag. Every later change to the groups, the channels or the setting
 * queues another sync; a failed one is retried when the device comes back
 * online.
 */
import { createEffect, onCleanup, untrack, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { journalHex } from "../ds/access";
import { channelsState } from "../state/channels-state";
import { reachSession, startReachSession, stopReachSession } from "./reach-service";
import { clearVisibility, loadVisibility, visibilitySetting } from "./visibility-store";
import { clearJoinReplies } from "./join-replies";
import { applyHeldServiceEnvelopes, clearHeldServiceEnvelopes } from "./service-envelope-intake";
import { resetServiceEnvelopeOutbox } from "./service-envelope-outbox";

export function ReachProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  let bound: string | null = null;
  createEffect(() => {
    const account = accountId();
    const identity = chats.accountIdentity();
    untrack(() => {
      const key = account === null || identity === null ? null : `${account}|${journalHex(identity.signaturePublicKey)}`;
      if (key === bound) return;
      bound = key;
      if (account === null || identity === null) {
        stopReachSession();
        clearVisibility();
        clearJoinReplies();
        clearHeldServiceEnvelopes();
        resetServiceEnvelopeOutbox();
        return;
      }
      const seed = identity.ed25519Seed;
      const accountKey = identity.signaturePublicKey;
      void (async () => {
        await loadVisibility(account);
        const session = await startReachSession(account, seed, accountKey);
        seed.fill(0);
        if (session === null) return;
        session.setSettingProvider(() => visibilitySetting());
        session.setScopeProvider(() => ({
          groupIdsHex: chats.groups().map((g) => journalHex(base64Decode(g.groupIdB64))),
          channels: channelsState.channels().map((c) => ({
            idHex: c.id,
            epoch: channelsState.epochByChannel().get(c.id) ?? 0,
          })),
        }));
        session.requestSync();
        // A setting or key versions another device sent before this one
        // was loaded (ADR-0060 section 7).
        await applyHeldServiceEnvelopes();
      })().catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[reach] session start failed", err);
      });
    });
  });

  // Re-sync when what the wanted set depends on changes.
  createEffect(() => {
    chats.groups();
    channelsState.channels();
    channelsState.epochByChannel();
    visibilitySetting();
    untrack(() => reachSession()?.requestSync());
  });

  const onOnline = (): void => reachSession()?.requestSync();
  if (typeof window !== "undefined") window.addEventListener("online", onOnline);
  onCleanup(() => {
    if (typeof window !== "undefined") window.removeEventListener("online", onOnline);
  });

  return <>{props.children}</>;
}
