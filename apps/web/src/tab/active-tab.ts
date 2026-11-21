// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One Lakko tab per browser runs at a time (docs/ux/one-tab.md).
 *
 * ## Why
 *
 * Every tab of a browser shares one IndexedDB but holds its own wasm provider:
 * the whole MLS state of every conversation, written back as ONE row per
 * account after every change (`chat/mls-store.ts`). A tab that is behind
 * writes its old copy over the newer one, and the other tab's own commits are
 * then undecryptable to the device forever (`CannotDecryptOwnMessage`). Seen
 * live on 2026-11-21; `group-lock.ts` had recorded the shape. No per-group lock
 * can fix it: the stale copy is the whole provider.
 *
 * ## How
 *
 * The running tab holds the Web Lock {@link ACTIVE_TAB_LOCK} for the page's
 * lifetime (the callback's promise never settles; the browser releases the
 * lock when the page goes). Another tab asks with `ifAvailable` and, refused,
 * shows the notice WITHOUT loading the app (`main.tsx` imports it only once
 * this tab runs), so nothing of the app runs there.
 *
 * Use here posts `takeover` on the channel {@link ACTIVE_TAB_CHANNEL} and
 * queues a normal request. The running tab, on `takeover`, calls `onLost`:
 * the page freezes every writer at once (`tab-state.ts`) and reloads, which
 * releases the lock and grants the queued request; the reloaded page then
 * finds the lock taken and shows the notice. A running tab that does not
 * answer within {@link TAKEOVER_STEAL_AFTER_MS} is taken from with `steal`;
 * its lock request then rejects, which is `onLost` too. Anything a stolen tab
 * still writes is refused by the store's generation check (`mls-store.ts`).
 *
 * ## Without Web Locks
 *
 * Secure pages in every supported browser have `navigator.locks`; where it is
 * missing the tabs ask each other on the channel: `probe` at start, answered
 * `active` by the running tab; silence for {@link PROBE_WAIT_MS} means run.
 * Two tabs starting in the same instant can both run there (the store backstop
 * then refuses the older one's writes). With neither API, every tab runs.
 *
 * Pure apart from the injected effects, so it is testable without a browser.
 */

/** The Web Lock the running tab holds. */
export const ACTIVE_TAB_LOCK = "lakko-active-tab";
/** The channel the tabs talk on. */
export const ACTIVE_TAB_CHANNEL = "lakko-active-tab";
/** How long Use here waits for the running tab before taking the lock.
 *  Chosen, not measured: a live tab answers in well under a second. */
export const TAKEOVER_STEAL_AFTER_MS = 10_000;
/** How long a start without Web Locks waits for a running tab to answer.
 *  Chosen, not measured. */
export const PROBE_WAIT_MS = 300;

/** The messages on the channel. */
export type ActiveTabMessage = { readonly type: "takeover" | "probe" | "active" };

/** The part of `LockManager` this uses. */
export interface LockManagerLike {
  request(
    name: string,
    options: { ifAvailable?: boolean; steal?: boolean; signal?: AbortSignal },
    callback: (lock: unknown) => unknown,
  ): Promise<unknown>;
}

/** The part of `BroadcastChannel` this uses. */
export interface ChannelLike {
  postMessage(message: ActiveTabMessage): void;
  addEventListener(type: "message", fn: (ev: { data: unknown }) => void): void;
}

export interface ActiveTabDeps {
  /** `navigator.locks`, or null where missing. */
  readonly locks: LockManagerLike | null;
  /** Opens the channel, or null where `BroadcastChannel` is missing. */
  readonly openChannel: (() => ChannelLike) | null;
  readonly setTimer: (fn: () => void, ms: number) => unknown;
  readonly clearTimer: (handle: unknown) => void;
  /**
   * Another tab took over: save what must not be lost (`before-yield.ts`),
   * freeze every writer and reload. Called once. `takeover`: the other tab
   * asked and waits for this page to go; `stolen`: the lock was already
   * taken, so nothing more may be written.
   */
  readonly onLost: (reason: "takeover" | "stolen") => void;
}

/** How a tab starts. */
export type ActiveTabClaim =
  | { readonly kind: "active" }
  | {
      readonly kind: "inactive";
      /** Use here: resolves once this tab runs. */
      takeOver(): Promise<void>;
    };

function isMessage(data: unknown, type: ActiveTabMessage["type"]): boolean {
  return typeof data === "object" && data !== null && (data as { type?: unknown }).type === type;
}

/** Decide whether this tab runs (see the module doc). */
export async function claimActiveTab(deps: ActiveTabDeps): Promise<ActiveTabClaim> {
  let lost = false;
  // Running: answer probes, yield to a takeover.
  let running = false;
  const lose = (reason: "takeover" | "stolen"): void => {
    if (lost) return;
    lost = true;
    running = false;
    deps.onLost(reason);
  };
  let channel: ChannelLike | null = null;
  try {
    channel = deps.openChannel?.() ?? null;
  } catch {
    channel = null;
  }
  channel?.addEventListener("message", (ev) => {
    if (!running) return;
    if (isMessage(ev.data, "takeover")) lose("takeover");
    else if (isMessage(ev.data, "probe")) channel?.postMessage({ type: "active" });
  });
  const run = (): ActiveTabClaim => {
    running = true;
    return { kind: "active" };
  };

  const locks = deps.locks;
  if (locks !== null) {
    /** Resolves true once granted (held for the page lifetime), false when
     *  `ifAvailable` found it taken. A later rejection of the held request
     *  (stolen) is {@link lose}. */
    const acquire = (options: { ifAvailable?: boolean; steal?: boolean; signal?: AbortSignal }) =>
      new Promise<boolean>((resolve, reject) => {
        let granted = false;
        locks
          .request(ACTIVE_TAB_LOCK, options, (lock) => {
            if (lock === null) {
              resolve(false);
              return undefined;
            }
            granted = true;
            resolve(true);
            return new Promise<void>(() => {
              /* held until the page goes */
            });
          })
          .catch((err: unknown) => {
            if (granted) lose("stolen");
            else reject(err);
          });
      });

    if (await acquire({ ifAvailable: true })) return run();
    return {
      kind: "inactive",
      takeOver: async () => {
        channel?.postMessage({ type: "takeover" });
        const queued = new AbortController();
        let stealTimer: unknown = null;
        const stolen = new Promise<boolean>((resolve) => {
          stealTimer = deps.setTimer(() => {
            queued.abort();
            acquire({ steal: true }).then(resolve, () => resolve(false));
          }, TAKEOVER_STEAL_AFTER_MS);
        });
        const waited = acquire({ signal: queued.signal }).catch(() => false);
        const first = await Promise.race([waited.then((ok) => (ok ? true : null)), stolen]);
        if (first !== true) {
          // The queued request was aborted for the steal: wait for the steal.
          if (!(await stolen)) throw new Error("the running tab could not be taken over");
        } else if (stealTimer !== null) {
          deps.clearTimer(stealTimer);
        }
        run();
      },
    };
  }

  if (channel === null) return run();

  // No Web Locks: ask the other tabs.
  const ch = channel;
  const answered = await new Promise<boolean>((resolve) => {
    let done = false;
    ch.addEventListener("message", (ev) => {
      if (!done && isMessage(ev.data, "active")) {
        done = true;
        resolve(true);
      }
    });
    ch.postMessage({ type: "probe" });
    deps.setTimer(() => {
      if (!done) {
        done = true;
        resolve(false);
      }
    }, PROBE_WAIT_MS);
  });
  if (!answered) return run();
  return {
    kind: "inactive",
    takeOver: async () => {
      // Answer probes at once, so the stopping tab's reloaded page finds us.
      run();
      ch.postMessage({ type: "takeover" });
      await new Promise<void>((resolve) => deps.setTimer(resolve, PROBE_WAIT_MS));
    },
  };
}

/** The browser's deps, with `onLost` supplied by the caller. */
export function browserActiveTabDeps(onLost: (reason: "takeover" | "stolen") => void): ActiveTabDeps {
  const nav = typeof navigator === "undefined" ? undefined : navigator;
  const locks = (nav as { locks?: LockManagerLike } | undefined)?.locks ?? null;
  const openChannel =
    typeof BroadcastChannel === "undefined"
      ? null
      : (): ChannelLike => new BroadcastChannel(ACTIVE_TAB_CHANNEL) as unknown as ChannelLike;
  return {
    locks,
    openChannel,
    setTimer: (fn, ms) => setTimeout(fn, ms),
    clearTimer: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
    onLost,
  };
}
