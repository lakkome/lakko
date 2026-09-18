// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Can a push wake actually reach this tab?
 *
 * ## Why anything asks
 *
 * Both of the web client's receive loops — the open chat's long poll
 * (`routes/Chat.tsx`) and the background receive for every other joined group
 * (`chat/BackgroundReceiveContext.tsx`) — park while
 * `document.visibilityState === "hidden"`, on the stated ground that "push
 * covers a backgrounded tab". For a message that is a delay. For a call it is
 * a defect, and the defect is total: the offer is never fetched, so nothing
 * rings, nothing publishes `End{busy}`, and the caller rings for its whole
 * 45 s window and records a missed call. The callee's tab learns of the call
 * only if it happens to become visible inside the 60 s stale-offer window.
 *
 * That reasoning holds only where push genuinely covers the tab. It does not
 * when the build ships no VAPID key, when the browser has no Push API, or —
 * the common one — when the user has denied notification permission.
 * `docs/ux/calls.md` section 2.1 says what should happen there: "A browser
 * that refuses notification permission shows the in-app banner only". The
 * in-app ring is then the ONLY path a call has, and a parked receive loop is
 * what makes it unreachable.
 *
 * So the rule the loops follow is: **park only when a push wake is KNOWN to
 * reach this tab.** Anything else — unreachable, or not yet established —
 * keeps receiving. The asymmetry is deliberate: the cost of staying awake is
 * battery, the cost of parking wrongly is a call that can never arrive.
 *
 * ## Why "unknown" is a value and not a boolean
 *
 * The decisive half of the probe (`PushManager.getSubscription()`) is async
 * and can fail. A boolean would have to pick a side at the moment the loop
 * asks, before the answer exists, and both defaults are wrong somewhere: a
 * `true` default parks a tab that can never be woken, a `false` default
 * pretends to know something it does not. Carrying `unknown` lets the caller
 * state its own failure direction once, in one place, rather than encoding it
 * in an initialiser.
 */

/** Whether a Web Push wake can be delivered to this tab. */
export type PushWakeReach =
  /** A subscription exists and the user permits notifications. */
  | "reachable"
  /** Structurally impossible: no VAPID key in this build, no Push API, no
   *  permission, or no subscription. */
  | "unreachable"
  /** Not established yet (the probe has not resolved, or it failed). */
  | "unknown";

/** What {@link classifyWakeReach} needs to know. */
export interface WakeReachFacts {
  /** The build carries `VITE_LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL`. Without
   *  it `subscribeToPush` is a no-op, so no wake can ever be sent. */
  readonly hasVapidKey: boolean;
  /** The browser exposes Service Worker + Push API. */
  readonly pushApiPresent: boolean;
  /** `Notification.permission`, or `null` when there is no Notification API. */
  readonly permission: NotificationPermission | null;
  /** Whether a `PushSubscription` exists; `null` when it could not be read
   *  (the registration is not ready yet, or `getSubscription()` threw). */
  readonly hasSubscription: boolean | null;
}

/**
 * Classify the facts. Pure.
 *
 * Every "no" is decided before the one "don't know", so a tab that cannot be
 * woken for a structural reason is never reported `unknown` just because the
 * subscription read has not landed.
 */
export function classifyWakeReach(facts: WakeReachFacts): PushWakeReach {
  if (!facts.hasVapidKey) return "unreachable";
  if (!facts.pushApiPresent) return "unreachable";
  if (facts.permission !== "granted") return "unreachable";
  if (facts.hasSubscription === null) return "unknown";
  return facts.hasSubscription ? "reachable" : "unreachable";
}

/** The environment reads {@link createPushWakeReach} makes, injectable so the
 *  probe can be driven from a test. */
export interface WakeReachEnv {
  readonly hasVapidKey: () => boolean;
  readonly pushApiPresent: () => boolean;
  readonly permission: () => NotificationPermission | null;
  /** Resolve whether a push subscription exists. `null` when unreadable. */
  readonly readSubscription: () => Promise<boolean | null>;
  readonly now: () => number;
}

/** How long a resolved answer is reused before the probe looks again. The
 *  answer changes only when the user grants or revokes permission, or a
 *  subscription is created or dropped — all rare, all user-driven. */
export const WAKE_REACH_TTL_MS = 30_000;

/** A cached, self-refreshing probe. */
export interface PushWakeReachProbe {
  /**
   * The last established answer, synchronously. Kicks off a refresh when the
   * cached answer is stale; never blocks and never throws.
   */
  read(): PushWakeReach;
  /** Drop the cache, so the next {@link read} re-probes. For the moment the
   *  answer is known to have changed (a subscribe, a sign-in). */
  invalidate(): void;
}

function browserEnv(): WakeReachEnv {
  return {
    hasVapidKey: () => {
      const key = (import.meta.env.VITE_LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL as
        | string
        | undefined) ?? "";
      return key.length > 0;
    },
    pushApiPresent: () =>
      typeof navigator !== "undefined" &&
      "serviceWorker" in navigator &&
      typeof globalThis.window !== "undefined" &&
      "PushManager" in globalThis.window,
    permission: () =>
      typeof Notification === "undefined" ? null : Notification.permission,
    readSubscription: async () => {
      try {
        const reg = await navigator.serviceWorker.ready;
        return (await reg.pushManager.getSubscription()) !== null;
      } catch {
        return null;
      }
    },
    now: () => Date.now(),
  };
}

/**
 * Build a probe over `env` (the browser by default).
 *
 * The synchronous half (VAPID key, API presence, permission) is re-read on
 * every {@link PushWakeReachProbe.read}, because it is free and because a
 * permission the user just revoked must take effect at once. Only the
 * subscription read is cached and refreshed in the background.
 */
export function createPushWakeReach(env: WakeReachEnv = browserEnv()): PushWakeReachProbe {
  let cached: boolean | null = null;
  let cachedAtMs = 0;
  let inFlight = false;

  const refresh = (): void => {
    if (inFlight) return;
    inFlight = true;
    void env
      .readSubscription()
      .then((value) => {
        cached = value;
        cachedAtMs = env.now();
      })
      .catch(() => {
        cached = null;
        cachedAtMs = env.now();
      })
      .finally(() => {
        inFlight = false;
      });
  };

  return {
    read(): PushWakeReach {
      const facts: WakeReachFacts = {
        hasVapidKey: env.hasVapidKey(),
        pushApiPresent: env.pushApiPresent(),
        permission: env.permission(),
        hasSubscription: cached,
      };
      const verdict = classifyWakeReach(facts);
      // Only worth a round trip once the synchronous half has not already
      // settled the question.
      if (verdict === "unknown" || env.now() - cachedAtMs >= WAKE_REACH_TTL_MS) {
        if (facts.hasVapidKey && facts.pushApiPresent && facts.permission === "granted") {
          refresh();
        }
      }
      return verdict;
    },
    invalidate(): void {
      cached = null;
      cachedAtMs = 0;
    },
  };
}
