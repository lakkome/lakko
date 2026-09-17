// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Session-scoped hydration of the channel state (`state/channels-state.ts`)
 * from the encrypted per-account channel log.
 *
 * Renders nothing. Mounted once in `App.tsx` inside the auth + chat
 * providers, beside the other whole-session receivers: it must run for
 * every signed-in session whether or not the user ever opens the
 * Channels route, because the channel list is what the route renders,
 * and it must run BEFORE any channel route opens a PublisherGroup — the
 * restore replaces the account identity's MLS provider, and a group
 * opened before it would be created fresh and then discarded (or, worse,
 * survive as a fork of the channel onto a seed no subscriber holds). The
 * channel routes await `awaitChannelsHydration()` for exactly that.
 *
 * Bound to `(session account, account identity)`: a logout or account
 * switch clears the in-memory state (the encrypted row stays, per
 * account, like the message log); `forgetDevice` deletes the row through
 * `wipeChannelsForAccount` in `AuthContext`.
 */

import { createEffect, untrack, type Component } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { useChats } from "../chat/ChatContext";
import {
  clearChannelsSession,
  hydrateChannelsState,
  markChannelsHydrationUnavailable,
} from "../state/channels-state";

export const ChannelsHydration: Component = () => {
  const auth = useAuth();
  const chats = useChats();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  let bound: string | null = null;
  // Monotone token so a hydration that resolves after the session it
  // belonged to ended does not arm write-through for the wrong account.
  let generation = 0;

  createEffect(() => {
    const account = accountId();
    const identity = chats.accountIdentity();
    untrack(() => {
      const key = account === null || identity === null ? null : account;
      if (key === bound) return;
      bound = key;
      generation += 1;
      const mine = generation;
      // Always drop the previous session's in-memory state first: a null
      // account is a logout; a different one is an account switch.
      void clearChannelsSession();
      if (key === null || identity === null) return;

      void (async (): Promise<void> => {
        let deviceKey: CryptoKey | null = null;
        try {
          deviceKey = await getDeviceKey(key);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("channels hydration: getDeviceKey failed", err);
        }
        if (mine !== generation) return;
        if (deviceKey === null) {
          // No device root key (half-reset state): nothing can be read or
          // written. Let the routes proceed with in-memory state.
          markChannelsHydrationUnavailable();
          return;
        }
        await hydrateChannelsState({
          accountIdB64: key,
          deviceKey,
          identity,
          isCurrent: (): boolean => mine === generation,
          // Read at flush time: `null` once the session has freed the
          // identity, so a late flush keeps the last good snapshot.
          snapshotProvider: (): Uint8Array | null => {
            const current = chats.accountIdentity();
            if (current === null || current !== identity) return null;
            try {
              return current.snapshotStorage();
            } catch {
              return null;
            }
          },
        });
      })();
    });
  });

  return null;
};
