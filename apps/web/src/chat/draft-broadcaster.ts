// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The composer's draft, sent to the account's other devices (Phase 2a3e2b)
 * at most once per debounce, and sent at once on {@link DraftBroadcaster.flush}
 * (a takeover by another tab, docs/ux/one-tab.md section 4).
 *
 * Every keystroke calls {@link DraftBroadcaster.update}; the text is sent when
 * typing pauses for the debounce, unless it is what was sent last. A message
 * sent within the debounce leaves the composer where the last write left it:
 * the pending timer is replaced by the next update, so what was sent is never
 * saved as a draft.
 */
export interface DraftBroadcaster {
  update(text: string): void;
  /** Send a pending draft now; resolves when sent, rejects when the send did. */
  flush(): Promise<void>;
  dispose(): void;
}

export function createDraftBroadcaster(opts: {
  readonly send: (text: string) => Promise<void>;
  readonly debounceMs: number;
  readonly setTimer?: (fn: () => void, ms: number) => unknown;
  readonly clearTimer?: (handle: unknown) => void;
  readonly onError?: (err: unknown) => void;
}): DraftBroadcaster {
  const setTimer = opts.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
  const clearTimer = opts.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
  let timer: unknown = null;
  let last: string | null = null;
  let pending: string | null = null;

  const fire = async (): Promise<void> => {
    const text = pending;
    if (text === null) return;
    pending = null;
    last = text;
    await opts.send(text);
  };
  const stop = (): void => {
    if (timer !== null) {
      clearTimer(timer);
      timer = null;
    }
  };

  return {
    update(text) {
      stop();
      if (text === last) {
        pending = null;
        return;
      }
      pending = text;
      timer = setTimer(() => {
        timer = null;
        // Best-effort: a transient failure must not break the composer; the
        // next keystroke sends again.
        void fire().catch((err: unknown) => opts.onError?.(err));
      }, opts.debounceMs);
    },
    async flush() {
      stop();
      await fire();
    },
    dispose() {
      stop();
    },
  };
}
