// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Message-log change pub/sub (G-8).
 *
 * The persistent message log (`message-log-store`) is mutated from the chat
 * view (`Chat.tsx`) — possibly in a different tab or route than the one
 * rendering an unread badge (`Contacts.tsx`). Without a notification, a
 * badge sourced from a one-shot `loadMessageLog` on mount only refreshes on
 * the next mount. This module is the cross-context signal that closes that
 * gap: a writer calls [`publishMessageLogChanged`] after a save, and a badge
 * subscribes via [`subscribeMessageLogChanged`] to reload.
 *
 * Transport is a `BroadcastChannel`, so the signal crosses tabs / workers.
 * `BroadcastChannel` does NOT deliver a message back to the instance that
 * posted it, so [`publishMessageLogChanged`] also notifies the in-process
 * subscribers directly — covering the same-tab, different-route case. The
 * iOS / Android clients get this for free from their `@Published` /
 * `StateFlow` in-memory stores; this brings web to parity.
 */

/** Channel name — versioned so a future payload change can coexist. */
const CHANNEL_NAME = "lakko/message-log/v1";

/** Called with the affected account's base64 id when its log changes. */
type Handler = (accountIdB64: string) => void;

const handlers = new Set<Handler>();
let channel: BroadcastChannel | null = null;

/** Lazily open the shared channel (no-op when `BroadcastChannel` is absent,
 *  e.g. older browsers / SSR — the in-process path still works). */
function ensureChannel(): BroadcastChannel | null {
  if (typeof BroadcastChannel === "undefined") return null;
  if (channel === null) {
    channel = new BroadcastChannel(CHANNEL_NAME);
    channel.onmessage = (event: MessageEvent): void => {
      const account = (event.data as { accountIdB64?: unknown } | null)?.accountIdB64;
      if (typeof account === "string") {
        for (const handler of handlers) handler(account);
      }
    };
  }
  return channel;
}

/** Announce that `accountIdB64`'s message log changed. */
export function publishMessageLogChanged(accountIdB64: string): void {
  // Cross-tab / cross-context.
  ensureChannel()?.postMessage({ accountIdB64 });
  // Same context: BroadcastChannel never echoes to the posting instance, so
  // notify in-process subscribers (a badge on another route in this tab)
  // directly. Other tabs are covered by the `onmessage` handler above.
  for (const handler of handlers) handler(accountIdB64);
}

/** Subscribe to message-log changes. Returns an unsubscribe function. */
export function subscribeMessageLogChanged(handler: Handler): () => void {
  ensureChannel();
  handlers.add(handler);
  return () => {
    handlers.delete(handler);
  };
}
