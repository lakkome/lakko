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

import type { DeviceCipher } from "../auth/device-cipher";
import { createEffect, onCleanup, untrack, type Component } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { useChats } from "../chat/ChatContext";
import {
  channelsActions,
  clearChannelsSession,
  hydrateChannelsState,
  installPublisherProviderPersister,
  markChannelsHydrationUnavailable,
} from "../state/channels-state";
import { awaitProviderSettled } from "../mls/provider-settled";
import { installPublisherDeviceSource } from "./publisher-device";
import { notePublisherCommitLanded } from "./channel-access";
import { republishChannelMeta } from "./channel-meta";
import { replayPublisherCommits } from "./copublisher";
import { withPublisherGroup } from "./publisher-group-registry";
import { settleOwedRecord } from "./owed-record";
import { afterReplayedPublisherCommits } from "./replayed-commit-followup";

export const ChannelsHydration: Component = () => {
  const auth = useAuth();
  const chats = useChats();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  let bound: string | null = null;
  let boundDevice: object | null = null;
  let uninstallPersister: (() => void) | null = null;
  onCleanup(() => uninstallPersister?.());
  // Monotone token so a hydration that resolves after the session it
  // belonged to ended does not arm write-through for the wrong account.
  let generation = 0;

  // ADR-0090: the PublisherGroups live in the DEVICE identity's provider,
  // persisted with the conversation snapshot.
  const uninstallDevice = installPublisherDeviceSource(() => chats.identity());
  onCleanup(uninstallDevice);

  createEffect(() => {
    const account = accountId();
    const identity = chats.conversationAccountIdentity();
    const device = chats.identity();
    untrack(() => {
      const key = account === null || identity === null || device === null ? null : account;
      if (key === bound && device === boundDevice) return;
      boundDevice = device;
      bound = key;
      generation += 1;
      const mine = generation;
      // Always drop the previous session's in-memory state first: a null
      // account is a logout; a different one is an account switch.
      void clearChannelsSession();
      uninstallPersister?.();
      uninstallPersister = null;
      if (key === null || identity === null || device === null) return;
      uninstallPersister = installPublisherProviderPersister(() => chats.persistSnapshotDurable(key, device));

      void (async (): Promise<void> => {
        let deviceKey: DeviceCipher | null = null;
        try {
          deviceKey = await getDeviceKey(key);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("channels hydration: getDeviceKey failed", err);
        }
        if (mine !== generation) return;
        // ADR-0090: open nothing in the device provider before the MLS
        // rehydrate restored it (`restoreStorage` replaces the provider).
        await awaitProviderSettled(device);
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
          device,
          isCurrent: (): boolean => mine === generation,
          // Read at flush time: `null` once the session has freed the
          // identity, so a late flush keeps the last good snapshot.
          snapshotProvider: (): Uint8Array | null => {
            const current = chats.conversationAccountIdentity();
            if (current === null || current !== identity) return null;
            try {
              return current.snapshotStorage();
            } catch {
              return null;
            }
          },
        });
        // G-50 — resume the PublisherGroup commits whose steps had not all
        // landed before the reload, against the provider just restored.
        if (mine !== generation) return;
        const landed = await replayPublisherCommits(identity);
        if (mine !== generation || landed.length === 0) return;
        // What the live approve, removal and rotation do after the reseal:
        // the composer's seed and the channel record under the new epoch.
        await afterReplayedPublisherCommits(landed, {
          seedOf: (id) =>
            withPublisherGroup(identity, id, async (g, d) => (g.isActive() ? g.channelSeed(d) : null)),
          landed: (id, epoch, seed) => {
            notePublisherCommitLanded(id, epoch, seed);
            channelsActions.notePublisherGroupChanged();
          },
          reissueRecord: async (id) => {
            // Owed since the commit was recorded; a failure here is retried
            // by the publisher pass.
            await settleOwedRecord(id, () => republishChannelMeta(id, identity));
          },
        });
      })();
    });
  });

  return null;
};
