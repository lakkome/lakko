// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  classifyWakeReach,
  createPushWakeReach,
  WAKE_REACH_TTL_MS,
  type WakeReachEnv,
  type WakeReachFacts,
} from "./wake-reach";

function facts(over: Partial<WakeReachFacts> = {}): WakeReachFacts {
  return {
    hasVapidKey: true,
    pushApiPresent: true,
    permission: "granted",
    hasSubscription: true,
    ...over,
  };
}

describe("classifyWakeReach", () => {
  it("is reachable only with a key, the API, permission and a subscription", () => {
    expect(classifyWakeReach(facts())).toBe("reachable");
  });

  it("is unreachable without a VAPID key in the build", () => {
    // `subscribeToPush` returns null, so push-gw never learns of this browser.
    expect(classifyWakeReach(facts({ hasVapidKey: false }))).toBe("unreachable");
  });

  it("is unreachable without the Push API", () => {
    expect(classifyWakeReach(facts({ pushApiPresent: false }))).toBe("unreachable");
  });

  it("is unreachable when the user denied notifications", () => {
    // The observed case: the ONLY ring path left is the in-app screen, so the
    // receive loops must not park.
    expect(classifyWakeReach(facts({ permission: "denied" }))).toBe("unreachable");
    expect(classifyWakeReach(facts({ permission: "default" }))).toBe("unreachable");
    expect(classifyWakeReach(facts({ permission: null }))).toBe("unreachable");
  });

  it("is unreachable when permission is granted but nothing is subscribed", () => {
    expect(classifyWakeReach(facts({ hasSubscription: false }))).toBe("unreachable");
  });

  it("is unknown only when every structural check passed and the read failed", () => {
    expect(classifyWakeReach(facts({ hasSubscription: null }))).toBe("unknown");
  });

  it("reports a structural no ahead of an unresolved subscription read", () => {
    // Otherwise a tab that can never be woken would sit at `unknown` for as
    // long as the probe kept failing, which reads as "maybe".
    expect(classifyWakeReach(facts({ permission: "denied", hasSubscription: null }))).toBe(
      "unreachable",
    );
  });
});

/** Let the probe's promise chain settle: it is `then` -> `catch` -> `finally`,
 *  so counting microtasks by hand is a trap. A macrotask flushes all of it. */
function flush(): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, 0);
  });
}

interface Harness {
  readonly env: WakeReachEnv;
  permission: NotificationPermission | null;
  subscription: boolean | null;
  reads: number;
  nowMs: number;
}

function harness(over: Partial<Pick<Harness, "permission" | "subscription">> = {}): Harness {
  const state: Harness = {
    permission: over.permission ?? "granted",
    subscription: over.subscription ?? true,
    reads: 0,
    nowMs: 1_000,
    env: undefined as unknown as WakeReachEnv,
  };
  (state as { env: WakeReachEnv }).env = {
    hasVapidKey: () => true,
    pushApiPresent: () => true,
    permission: () => state.permission,
    readSubscription: async () => {
      state.reads += 1;
      return state.subscription;
    },
    now: () => state.nowMs,
  };
  return state;
}

describe("createPushWakeReach", () => {
  it("answers unknown before the subscription read lands, then reachable", async () => {
    const h = harness();
    const probe = createPushWakeReach(h.env);
    // The first read cannot know yet — and `unknown` is what keeps a loop
    // receiving rather than guessing that push has it covered.
    expect(probe.read()).toBe("unknown");
    await flush();
    expect(probe.read()).toBe("reachable");
  });

  it("never spends a round trip when the synchronous half already said no", () => {
    const h = harness({ permission: "denied" });
    const probe = createPushWakeReach(h.env);
    expect(probe.read()).toBe("unreachable");
    expect(probe.read()).toBe("unreachable");
    expect(h.reads).toBe(0);
  });

  it("falls back to unknown when the subscription read throws", async () => {
    const h = harness();
    const env: WakeReachEnv = {
      ...h.env,
      readSubscription: () => Promise.reject(new Error("registration gone")),
    };
    const probe = createPushWakeReach(env);
    probe.read();
    await flush();
    expect(probe.read()).toBe("unknown");
  });

  it("reuses a resolved answer until the TTL expires", async () => {
    const h = harness();
    const probe = createPushWakeReach(h.env);
    probe.read();
    await flush();
    expect(h.reads).toBe(1);

    h.nowMs += WAKE_REACH_TTL_MS - 1;
    probe.read();
    expect(h.reads).toBe(1);

    h.nowMs += 2;
    probe.read();
    expect(h.reads).toBe(2);
  });

  it("re-reads after invalidate", async () => {
    const h = harness();
    const probe = createPushWakeReach(h.env);
    probe.read();
    await flush();
    expect(probe.read()).toBe("reachable");

    // The user revoked permission and the subscription went with it.
    h.subscription = false;
    probe.invalidate();
    expect(probe.read()).toBe("unknown");
    await flush();
    expect(probe.read()).toBe("unreachable");
  });

  it("follows a permission revocation without waiting for the TTL", async () => {
    const h = harness();
    const probe = createPushWakeReach(h.env);
    probe.read();
    await flush();
    expect(probe.read()).toBe("reachable");

    h.permission = "denied";
    expect(probe.read()).toBe("unreachable");
  });
});
