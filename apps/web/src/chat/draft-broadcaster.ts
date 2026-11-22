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
 *
 * `send` must keep the draft on this device before it returns (the sync
 * context's `saveDraft` folds it synchronously); its promise is the publish
 * to the other devices. A flush therefore waits for nothing on the network:
 * the takeover's bound is spent writing the state that holds the draft, not
 * on a publish that may be slow or refused.
 */
export interface DraftBroadcaster {
  update(text: string): void;
  /** Hand a pending draft to `send` now. Resolves once `send` has kept it
   *  on this device; the publish goes on, and its failure goes to `onError`. */
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

  /** Starts the send of the pending draft; the draft is kept on this device
   *  when this returns. Best-effort: a failed publish must not break the
   *  composer, and the next keystroke sends again. */
  const fire = (): void => {
    const text = pending;
    if (text === null) return;
    pending = null;
    last = text;
    let publish: Promise<void>;
    try {
      publish = opts.send(text);
    } catch (err) {
      opts.onError?.(err);
      return;
    }
    void publish.catch((err: unknown) => opts.onError?.(err));
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
        fire();
      }, opts.debounceMs);
    },
    flush() {
      stop();
      fire();
      return Promise.resolve();
    },
    dispose() {
      stop();
    },
  };
}
