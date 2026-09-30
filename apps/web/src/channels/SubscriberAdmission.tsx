// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Lets new subscribers into every channel this device can post to, while the
 * app is open, whatever screen is showing (ADR-0059 section 4). Renders
 * nothing; mounted once in `App.tsx` beside `ChannelPostSync`.
 *
 * Runs once the channel state is hydrated, then every 15 s (chosen). Unlike
 * the post sync it does not stop in a hidden tab: someone is waiting to be
 * let in, and a hidden tab's timers are already slowed by the browser. The
 * Manage screen polls the same state faster while it is open.
 */
import { createEffect, onCleanup, onMount, type Component } from "solid-js";

import { useChats } from "../chat/ChatContext";
import { channelsState } from "../state/channels-state";
import { admitNewSubscribers } from "./subscriber-admission";

/** Between passes over the owned channels (chosen). */
const ADMISSION_INTERVAL_MS = 15_000;

export const SubscriberAdmission: Component = () => {
  const chats = useChats();
  let running = false;

  const run = (): void => {
    const identity = chats.accountIdentity();
    if (identity === null || !channelsState.hydrated() || running) return;
    const owned = [...channelsState.keysByChannel().keys()];
    if (owned.length === 0) return;
    running = true;
    void (async (): Promise<void> => {
      try {
        for (const id of owned) {
          try {
            await admitNewSubscribers(id, identity);
          } catch (err) {
            // A channel the server does not answer for this time.
            // eslint-disable-next-line no-console
            console.warn("[channels] subscriber admission failed", err);
          }
        }
      } finally {
        running = false;
      }
    })();
  };

  createEffect(() => {
    if (chats.accountIdentity() !== null && channelsState.hydrated()) run();
  });

  onMount(() => {
    const timer = setInterval(run, ADMISSION_INTERVAL_MS);
    onCleanup(() => clearInterval(timer));
  });

  return null;
};
