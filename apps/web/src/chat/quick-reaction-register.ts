// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The person's quick reaction, as the account's devices agree on it
 * (`docs/ux/reactions-and-emoji.md` section 6, D3: the choice belongs to the
 * account and follows the person to their other devices).
 *
 * The register lives in the `lakko-sync` synced state, carried over the
 * DeviceGroup like the own-username register. The provider
 * (`DeviceGroupSyncContext.tsx`) publishes what the fold holds here on every
 * refresh and installs the write port once the synced state is loaded; the
 * screens read {@link quickReactionOf} and write through
 * {@link chooseQuickReaction}. A write made before the synced state is loaded
 * waits and is applied when it is.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";

/** The emoji a person has until they pick another (`defaultQuickReaction()`
 *  in the core). Restated so a screen can draw before the wasm is loaded; the
 *  value is the core's `QUICK_REACTIONS[0]`. */
export const DEFAULT_QUICK_REACTION = "\u{1F44D}";

export interface QuickReactionPort {
  readonly accountIdB64: string;
  /** Write the register and send it to the account's other devices. Never
   *  throws. */
  readonly set: (emoji: string) => Promise<void>;
}

interface Held {
  readonly accountIdB64: string;
  readonly emoji: string | null;
}

let port: QuickReactionPort | null = null;
const [held, setHeld] = createSignal<Held | null>(null);
const [pending, setPending] = createSignal<{ readonly accountIdB64: string; readonly emoji: string } | null>(
  null,
);

/** The account's quick reaction: a write still waiting wins, then the
 *  register, then the default. Reactive. */
export function quickReactionOf(accountIdB64: string | null): string {
  const p = pending();
  if (p !== null && p.accountIdB64 === accountIdB64) return p.emoji;
  const h = held();
  if (h !== null && h.accountIdB64 === accountIdB64 && h.emoji !== null && h.emoji.length > 0) {
    return h.emoji;
  }
  return DEFAULT_QUICK_REACTION;
}

/** The register as the fold holds it now (the provider calls this on every
 *  refresh). */
export function publishQuickReaction(accountIdB64: string, emoji: string | null): void {
  const current = held();
  if (current === null || current.accountIdB64 !== accountIdB64 || current.emoji !== emoji) {
    setHeld({ accountIdB64, emoji });
  }
}

/** The synced state is loaded: a waiting write is applied now. */
export function installQuickReactionRegister(next: QuickReactionPort): () => void {
  port = next;
  const waiting = pending();
  if (waiting !== null && waiting.accountIdB64 === next.accountIdB64) {
    void next.set(waiting.emoji).finally(() => setPending(null));
  }
  return () => {
    if (port === next) port = null;
    setHeld(null);
  };
}

/** Settings > Appearance > Quick reaction: a pick. */
export function chooseQuickReaction(accountIdB64: string, emoji: string): void {
  if (port !== null && port.accountIdB64 === accountIdB64) {
    setPending({ accountIdB64, emoji });
    void port.set(emoji).finally(() => {
      const p = pending();
      if (p !== null && p.accountIdB64 === accountIdB64 && p.emoji === emoji) setPending(null);
    });
    return;
  }
  setPending({ accountIdB64, emoji });
}

registerAccountMemory("quick-reaction-register", (ids) => {
  const p = pending();
  if (p !== null && ids.includes(p.accountIdB64)) setPending(null);
  const h = held();
  if (h !== null && ids.includes(h.accountIdB64)) setHeld(null);
});
