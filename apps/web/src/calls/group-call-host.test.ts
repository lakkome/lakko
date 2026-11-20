// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  GROUP_CALL_ALONE_END_MS,
  callStartHeldWhileBehind,
  chatCallAffordance,
  decideGroupCallEpochChange,
  groupCallAloneAction,
  groupCallStartAbandoned,
  groupCallStartCheck,
  runGroupCallStart,
  planGroupCallLeave,
  selectGroupCallBanner,
  type ChatCallAffordanceInput,
  type GroupCallBannerInput,
  type GroupCallEpochInput,
  type GroupCallLeaveInput,
  type GroupCallStartRun,
  type GroupCallStartSteps,
} from "./group-call-host";

function leave(over: Partial<GroupCallLeaveInput>): GroupCallLeaveInput {
  return {
    reason: "hungUp",
    sessionFailed: false,
    joinPublished: true,
    initiator: false,
    ownMemberKey: "own",
    recordFreshMemberKeys: new Set(["own"]),
    ...over,
  };
}

describe("planGroupCallLeave", () => {
  it("a joined participant who was the last one in publishes Leave then End", () => {
    expect(planGroupCallLeave(leave({}))).toEqual({
      leaveReason: "hungUp",
      publishLeave: true,
      publishEnd: true,
    });
  });

  it("does not end a call somebody else is still in", () => {
    const plan = planGroupCallLeave(leave({ recordFreshMemberKeys: new Set(["own", "other"]) }));
    expect(plan.publishLeave).toBe(true);
    expect(plan.publishEnd).toBe(false);
  });

  it("a joiner whose connection failed before it ever joined tells nobody anything", () => {
    // It must not end a call other members may be about to enter.
    const plan = planGroupCallLeave(
      leave({ joinPublished: false, recordFreshMemberKeys: new Set<string>(), reason: "error" }),
    );
    expect(plan.publishLeave).toBe(false);
    expect(plan.publishEnd).toBe(false);
  });

  it("an initiator whose session never connected still owes the End of the call it announced", () => {
    const plan = planGroupCallLeave(
      leave({ joinPublished: false, initiator: true, recordFreshMemberKeys: new Set<string>() }),
    );
    expect(plan.publishLeave).toBe(false);
    expect(plan.publishEnd).toBe(true);
  });

  it("a record already gone (someone else ended it) counts as nobody else joined", () => {
    expect(planGroupCallLeave(leave({ recordFreshMemberKeys: null })).publishEnd).toBe(true);
  });

  it("a failed session overrides the requested reason", () => {
    expect(planGroupCallLeave(leave({ sessionFailed: true })).leaveReason).toBe("error");
    expect(planGroupCallLeave(leave({ reason: "error" })).leaveReason).toBe("error");
    expect(planGroupCallLeave(leave({})).leaveReason).toBe("hungUp");
  });
});

function epoch(over: Partial<GroupCallEpochInput>): GroupCallEpochInput {
  return {
    callGroupIdB64: "group-a",
    settledGroupIdB64: "group-a",
    groupJoined: true,
    isActive: true,
    keyedEpoch: 5n,
    currentEpoch: 5n,
    ...over,
  };
}

describe("decideGroupCallEpochChange", () => {
  it("rekeys when the call's group drained at a different epoch", () => {
    expect(decideGroupCallEpochChange(epoch({ currentEpoch: 6n }))).toBe("rekey");
  });

  it("rekeys back on a rollback too (the ADR-0040 F1 reset moves the epoch down)", () => {
    expect(decideGroupCallEpochChange(epoch({ keyedEpoch: 6n, currentEpoch: 5n }))).toBe("rekey");
  });

  it("does nothing when the epoch has not moved — a send or a quiet poll drains the lock too", () => {
    expect(decideGroupCallEpochChange(epoch({}))).toBe("none");
  });

  it("ignores a drain of any other group, whatever happened there", () => {
    expect(
      decideGroupCallEpochChange(
        epoch({ settledGroupIdB64: "group-b", currentEpoch: 9n, isActive: false }),
      ),
    ).toBe("none");
  });

  it("does nothing without a live group call", () => {
    expect(decideGroupCallEpochChange(epoch({ callGroupIdB64: null, currentEpoch: 9n }))).toBe(
      "none",
    );
  });

  it("keeps the current key when the epoch cannot be read (fail-closed, no guess)", () => {
    expect(decideGroupCallEpochChange(epoch({ currentEpoch: undefined }))).toBe("none");
  });

  it("rekeys once the epoch becomes readable when it could not be read at start", () => {
    expect(decideGroupCallEpochChange(epoch({ keyedEpoch: null }))).toBe("rekey");
  });

  it("ends the call locally when this device is no longer in the group", () => {
    expect(decideGroupCallEpochChange(epoch({ isActive: false, currentEpoch: 6n }))).toBe(
      "end-locally",
    );
    expect(decideGroupCallEpochChange(epoch({ groupJoined: false }))).toBe("end-locally");
  });

  it("an unreadable membership flag is not an eviction", () => {
    expect(decideGroupCallEpochChange(epoch({ isActive: undefined, currentEpoch: 6n }))).toBe(
      "rekey",
    );
  });
});

function banner(over: Partial<GroupCallBannerInput>): GroupCallBannerInput {
  return {
    live: [
      { groupIdB64: "group-a", callIdHex: "aa" },
      { groupIdB64: "group-b", callIdHex: "bb" },
    ],
    isChatOpen: () => false,
    isMember: () => true,
    inAnyCall: false,
    dismissedCallIds: new Set(),
    ...over,
  };
}

describe("selectGroupCallBanner", () => {
  it("announces the first live call when no chat is open", () => {
    expect(selectGroupCallBanner(banner({}))).toEqual({ groupIdB64: "group-a", callIdHex: "aa" });
  });

  it("leaves a call to its own chat's banner when that chat is open", () => {
    expect(selectGroupCallBanner(banner({ isChatOpen: (g) => g === "group-a" }))).toEqual({
      groupIdB64: "group-b",
      callIdHex: "bb",
    });
  });

  it("announces nothing while this device is in any call", () => {
    expect(selectGroupCallBanner(banner({ inAnyCall: true }))).toBeNull();
  });

  it("skips a dismissed call, and a group this device has left", () => {
    expect(
      selectGroupCallBanner(
        banner({ dismissedCallIds: new Set(["aa"]), isMember: (g) => g !== "group-b" }),
      ),
    ).toBeNull();
  });

  it("announces nothing when there is no live call", () => {
    expect(selectGroupCallBanner(banner({ live: [] }))).toBeNull();
  });
});

function affordance(over: Partial<ChatCallAffordanceInput>): ChatCallAffordanceInput {
  return {
    membershipEnded: false,
    callInProgress: false,
    liveCallInThisChat: false,
    isGroup: false,
    people: 2,
    ...over,
  };
}

describe("chatCallAffordance", () => {
  it("a group with nobody else in it offers no call, and a call running in it can still be joined", () => {
    expect(chatCallAffordance(affordance({ isGroup: true, people: 1 }))).toBe("hidden");
    expect(
      chatCallAffordance(affordance({ isGroup: true, people: 1, liveCallInThisChat: true })),
    ).toBe("join-group");
  });

  it("a call in progress anywhere hides the buttons, so a return to its chat never starts a second one", () => {
    expect(chatCallAffordance(affordance({ callInProgress: true, isGroup: true }))).toBe(
      "hidden",
    );
    expect(
      chatCallAffordance(affordance({ callInProgress: true, liveCallInThisChat: true })),
    ).toBe("hidden");
    expect(chatCallAffordance(affordance({ callInProgress: true }))).toBe("hidden");
  });

  it("hides them once membership has ended", () => {
    expect(
      chatCallAffordance(affordance({ membershipEnded: true, liveCallInThisChat: true })),
    ).toBe("hidden");
  });

  it("joins a call already in progress in this chat, even in a one-to-one", () => {
    expect(chatCallAffordance(affordance({ liveCallInThisChat: true }))).toBe("join-group");
    expect(chatCallAffordance(affordance({ liveCallInThisChat: true, isGroup: true }))).toBe(
      "join-group",
    );
  });

  it("otherwise starts a group call in a group, of any size, and a 1x1 call in a one-to-one", () => {
    expect(chatCallAffordance(affordance({ isGroup: true }))).toBe("start-group");
    expect(chatCallAffordance(affordance({ isGroup: false }))).toBe("start-one-to-one");
    expect(chatCallAffordance(affordance({ isGroup: true, people: 3 }))).toBe("start-group");
  });
});

describe("planGroupCallLeave and the person's other device", () => {
  it("excludes only this device's key, so another device of the same person keeps the call", () => {
    const plan = planGroupCallLeave(
      leave({ recordFreshMemberKeys: new Set(["own", "own-person-laptop"]) }),
    );
    expect(plan.publishLeave).toBe(true);
    expect(plan.publishEnd).toBe(false);
  });
});

describe("groupCallAloneAction", () => {
  it("pins the shared constant", () => {
    expect(GROUP_CALL_ALONE_END_MS).toBe(5_000);
  });

  it("arms only when a remote was seen and the roster is now empty", () => {
    expect(groupCallAloneAction(0, true, false)).toBe("arm-end");
  });

  it("never arms for a call that never had a remote (initiator waiting)", () => {
    expect(groupCallAloneAction(0, false, false)).toBe("none");
    expect(groupCallAloneAction(0, false, true)).toBe("none");
  });

  it("disarms when someone comes back while armed", () => {
    expect(groupCallAloneAction(1, true, true)).toBe("disarm");
    expect(groupCallAloneAction(3, true, true)).toBe("disarm");
  });

  it("does nothing when already armed and still alone", () => {
    expect(groupCallAloneAction(0, true, true)).toBe("none");
  });

  it("does nothing while others are present and nothing is armed", () => {
    expect(groupCallAloneAction(2, true, false)).toBe("none");
    expect(groupCallAloneAction(1, false, false)).toBe("none");
  });
});

describe("groupCallStartAbandoned", () => {
  it("a session this device hung up and took out of the slot was left, not failed", () => {
    // End pressed while the SFU session was connecting: no failure is shown.
    expect(groupCallStartAbandoned({ sessionState: "closed", stillInSlot: false })).toBe(true);
  });

  it("a session that failed is a failure, in or out of the slot", () => {
    // The ended-phase effect may already have released a failed session.
    expect(groupCallStartAbandoned({ sessionState: "failed", stillInSlot: false })).toBe(false);
    expect(groupCallStartAbandoned({ sessionState: "failed", stillInSlot: true })).toBe(false);
  });

  it("a start that stopped before or while its session was up is a failure", () => {
    expect(groupCallStartAbandoned({ sessionState: null, stillInSlot: false })).toBe(false);
    expect(groupCallStartAbandoned({ sessionState: "idle", stillInSlot: true })).toBe(false);
    expect(groupCallStartAbandoned({ sessionState: "connecting", stillInSlot: true })).toBe(false);
    expect(groupCallStartAbandoned({ sessionState: "connected", stillInSlot: true })).toBe(false);
  });

  it("a closed session still in the slot is not called abandoned", () => {
    // Nothing took it out of the slot, so nobody chose to leave it.
    expect(groupCallStartAbandoned({ sessionState: "closed", stillInSlot: true })).toBe(false);
  });
});

describe("groupCallStartCheck", () => {
  const at = { current: true, announced: false, handedOver: false, callStillLive: null };

  it("the current attempt goes on", () => {
    expect(groupCallStartCheck(at)).toEqual({ kind: "continue" });
    expect(groupCallStartCheck({ ...at, callStillLive: true })).toEqual({ kind: "continue" });
    expect(groupCallStartCheck({ ...at, announced: true })).toEqual({ kind: "continue" });
  });

  it("an attempt the person left before announcing stops and owes nothing", () => {
    expect(groupCallStartCheck({ ...at, current: false })).toEqual({
      kind: "stop",
      outcome: "left",
      takeBackAnnouncement: false,
      releaseSlot: false,
    });
  });

  it("an attempt left after its Start was published takes the announcement back", () => {
    expect(groupCallStartCheck({ ...at, current: false, announced: true })).toEqual({
      kind: "stop",
      outcome: "left",
      takeBackAnnouncement: true,
      releaseSlot: false,
    });
  });

  it("a call already handed to the slot is leaving's to tell the group about", () => {
    const check = groupCallStartCheck({ ...at, current: false, announced: true, handedOver: true });
    expect(check).toMatchObject({ outcome: "left", takeBackAnnouncement: false });
  });

  it("an attempt that is no longer current never frees the slot, live record or not", () => {
    for (const callStillLive of [null, true, false]) {
      const check = groupCallStartCheck({ ...at, current: false, callStillLive });
      expect(check).toMatchObject({ kind: "stop", outcome: "left", releaseSlot: false });
    }
  });

  it("a joiner whose call ended in the meantime stops as ended and frees its own slot", () => {
    expect(groupCallStartCheck({ ...at, callStillLive: false })).toEqual({
      kind: "stop",
      outcome: "ended",
      takeBackAnnouncement: false,
      releaseSlot: true,
    });
  });
});

/** A promise settled from the test, standing in for one await of a start. */
function deferred<T>(): {
  readonly promise: Promise<T>;
  resolve(value: T): void;
  reject(err: unknown): void;
} {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Let every continuation queued so far run. */
async function settle(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

/**
 * The host's side of a start, modelled the way `CallHostContext.tsx` holds
 * it: one slot, one current attempt, and a log of what reached the outside
 * world (a session built, devices opened, bodies published).
 */
function host(): {
  attempt: object | null;
  slot: string | null;
  readonly log: string[];
  leave(): void;
  start(
    name: string,
    initiator: boolean,
  ): {
    readonly run: Promise<GroupCallStartRun<string>>;
    readonly sync: ReturnType<typeof deferred<void>>;
    readonly mint: ReturnType<typeof deferred<boolean>>;
    readonly announce: ReturnType<typeof deferred<boolean>>;
    readonly connect: ReturnType<typeof deferred<void>>;
    live: boolean;
  };
} {
  const h = {
    attempt: null as object | null,
    slot: null as string | null,
    log: [] as string[],
    leave(): void {
      h.attempt = null;
      h.slot = null;
    },
    start(name: string, initiator: boolean) {
      const attempt = {};
      h.attempt = attempt;
      h.slot = name;
      const ctl = {
        sync: deferred<void>(),
        mint: deferred<boolean>(),
        announce: deferred<boolean>(),
        connect: deferred<void>(),
        live: true,
        run: undefined as unknown as Promise<GroupCallStartRun<string>>,
      };
      const steps: GroupCallStartSteps<string, string> = {
        initiator,
        isCurrent: () => h.attempt === attempt,
        sync: () => ctl.sync.promise,
        callStillLive: () => ctl.live,
        prepare: async () =>
          (await ctl.mint.promise)
            ? { ok: true, material: name }
            : { ok: false, reason: "refused" },
        announce: (m) => {
          h.log.push(`start:${m}`);
          return ctl.announce.promise;
        },
        takeBackAnnouncement: (m, why) => h.log.push(`take-back:${m}:${why}`),
        open: (m) => h.log.push(`session:${m}`),
        connect: () => {
          h.log.push(`devices:${name}`);
          return ctl.connect.promise;
        },
      };
      // As the host's `finally`: only the attempt the slot belongs to frees it.
      ctl.run = runGroupCallStart(steps).then((run) => {
        if (h.attempt === attempt) {
          h.attempt = null;
          if (run.kind !== "connected") h.slot = null;
        }
        return run;
      });
      return ctl;
    },
  };
  return h;
}

describe("runGroupCallStart", () => {
  it("control: nobody leaves, the initiator announces, builds the session and connects", async () => {
    const h = host();
    const a = h.start("a", true);
    a.mint.resolve(true);
    await settle();
    expect(h.log).toEqual(["start:a"]);
    a.announce.resolve(true);
    await settle();
    expect(h.log).toEqual(["start:a", "session:a", "devices:a"]);
    a.connect.resolve();
    expect(await a.run).toEqual({ kind: "connected" });
    expect(h.slot).toBe("a");
    expect(h.log).toEqual(["start:a", "session:a", "devices:a"]);
  });

  it("control: a joiner syncs, never announces, and connects", async () => {
    const h = host();
    const a = h.start("a", false);
    a.sync.resolve();
    a.mint.resolve(true);
    await settle();
    a.connect.resolve();
    expect(await a.run).toEqual({ kind: "connected" });
    expect(h.log).toEqual(["session:a", "devices:a"]);
  });

  it("left during the group sync: no session, no device, nothing published", async () => {
    const h = host();
    const a = h.start("a", false);
    h.leave();
    // The slot is free at once, not when the abandoned start returns.
    expect(h.slot).toBeNull();
    a.sync.resolve();
    a.mint.resolve(true);
    expect(await a.run).toEqual({ kind: "left" });
    expect(h.log).toEqual([]);
    expect(h.slot).toBeNull();
  });

  it("left during the mint: no session, no device, nothing published", async () => {
    for (const initiator of [true, false]) {
      const h = host();
      const a = h.start("a", initiator);
      a.sync.resolve();
      await settle();
      h.leave();
      expect(h.slot).toBeNull();
      a.mint.resolve(true);
      a.announce.resolve(true);
      expect(await a.run).toEqual({ kind: "left" });
      expect(h.log).toEqual([]);
    }
  });

  it("left while the Start was being published: no session, and the announcement is taken back once", async () => {
    const h = host();
    const a = h.start("a", true);
    a.mint.resolve(true);
    await settle();
    h.leave();
    a.announce.resolve(true);
    expect(await a.run).toEqual({ kind: "left" });
    expect(h.log).toEqual(["start:a", "take-back:a:left"]);
    a.connect.resolve();
    await settle();
    expect(h.log).toEqual(["start:a", "take-back:a:left"]);
    expect(h.slot).toBeNull();
  });

  it("left while a Start that never reached the journal was in flight: nothing to take back", async () => {
    const h = host();
    const a = h.start("a", true);
    a.mint.resolve(true);
    await settle();
    h.leave();
    a.announce.resolve(false);
    expect(await a.run).toEqual({ kind: "left" });
    expect(h.log).toEqual(["start:a"]);
  });

  it("left while connecting: leaving owns the call, the start takes nothing back", async () => {
    const h = host();
    const a = h.start("a", true);
    a.mint.resolve(true);
    a.announce.resolve(true);
    await settle();
    h.leave();
    // The host hangs the session up, so its connect rejects...
    a.connect.reject(new Error("closed"));
    expect(await a.run).toEqual({ kind: "left" });
    expect(h.log).toEqual(["start:a", "session:a", "devices:a"]);
  });

  it("left while connecting, and the connect resolves anyway: still left", async () => {
    const h = host();
    const a = h.start("a", true);
    a.mint.resolve(true);
    a.announce.resolve(true);
    await settle();
    h.leave();
    a.connect.resolve();
    expect(await a.run).toEqual({ kind: "left" });
  });

  it("a second start survives the first one's late success", async () => {
    const h = host();
    const a = h.start("a", true);
    h.leave();
    const b = h.start("b", true);
    expect(h.slot).toBe("b");
    // The first start's mint and publish come back late, and succeed.
    a.mint.resolve(true);
    a.announce.resolve(true);
    expect(await a.run).toEqual({ kind: "left" });
    expect(h.slot).toBe("b");
    expect(h.attempt).not.toBeNull();
    expect(h.log).toEqual([]);
    b.mint.resolve(true);
    b.announce.resolve(true);
    await settle();
    b.connect.resolve();
    expect(await b.run).toEqual({ kind: "connected" });
    expect(h.slot).toBe("b");
    expect(h.log).toEqual(["start:b", "session:b", "devices:b"]);
  });

  it("a second start survives the first one's late failure", async () => {
    const h = host();
    const a = h.start("a", true);
    h.leave();
    const b = h.start("b", true);
    a.mint.reject(new Error("mint refused"));
    // No error is reported for a call the person already left.
    expect(await a.run).toEqual({ kind: "left" });
    expect(h.slot).toBe("b");
    expect(h.attempt).not.toBeNull();
    b.mint.resolve(true);
    b.announce.resolve(true);
    await settle();
    b.connect.resolve();
    expect(await b.run).toEqual({ kind: "connected" });
    expect(h.log).toEqual(["start:b", "session:b", "devices:b"]);
  });

  it("a second start survives the first one's Start landing late: taken back, slot untouched", async () => {
    const h = host();
    const a = h.start("a", true);
    a.mint.resolve(true);
    await settle();
    h.leave();
    const b = h.start("b", true);
    a.announce.resolve(true);
    expect(await a.run).toEqual({ kind: "left" });
    expect(h.log).toEqual(["start:a", "take-back:a:left"]);
    expect(h.slot).toBe("b");
    b.mint.resolve(true);
    b.announce.resolve(true);
    await settle();
    b.connect.resolve();
    expect(await b.run).toEqual({ kind: "connected" });
    expect(h.log.filter((l) => l.startsWith("take-back"))).toEqual(["take-back:a:left"]);
  });

  it("a joiner whose call ended during the sync or the mint is told ended, with no session", async () => {
    for (const during of ["sync", "mint"] as const) {
      const h = host();
      const a = h.start("a", false);
      if (during === "sync") a.live = false;
      a.sync.resolve();
      await settle();
      a.live = false;
      a.mint.resolve(true);
      expect(await a.run).toEqual({ kind: "ended" });
      expect(h.log).toEqual([]);
      expect(h.slot).toBeNull();
    }
  });

  it("the current attempt reports its own failures, as before", async () => {
    const h = host();
    const refused = h.start("a", true);
    refused.mint.resolve(false);
    expect(await refused.run).toEqual({ kind: "refused", reason: "refused" });
    expect(h.slot).toBeNull();

    const unannounced = h.start("b", true);
    unannounced.mint.resolve(true);
    unannounced.announce.resolve(false);
    expect(await unannounced.run).toMatchObject({ kind: "failed", handedOver: false });
    expect(h.log).toEqual(["start:b"]);

    const broken = h.start("c", true);
    const err = new Error("ice failed");
    broken.mint.resolve(true);
    broken.announce.resolve(true);
    await settle();
    broken.connect.reject(err);
    // Handed over: the host leaves the call, which is what publishes the End.
    expect(await broken.run).toEqual({ kind: "failed", error: err, handedOver: true });
    expect(h.log.filter((l) => l.startsWith("take-back"))).toEqual([]);
  });
});

describe("a call in a chat this device is behind in (ADR-0091, 2026-11-20)", () => {
  const source = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
  /** The body of `async function <name>(` up to the next top-level function. */
  const body = (text: string, name: string): string => {
    const start = text.indexOf(`  async function ${name}(`);
    expect(start).toBeGreaterThan(-1);
    const end = text.indexOf("\n  async function ", start + 10);
    return text.slice(start, end < 0 ? undefined : end);
  };

  it("is held only while behind", () => {
    expect(callStartHeldWhileBehind(true)).toBe(true);
    expect(callStartHeldWhileBehind(false)).toBe(false);
  });

  it("keeps the call buttons: the affordance does not read the behind state", () => {
    const chat = source("../routes/Chat.tsx");
    const at = chat.indexOf("chatCallAffordance({");
    const call = chat.slice(at, chat.indexOf("});", at));
    expect(call).toContain("membershipEnded: membershipEnded(),");
    expect(call).not.toContain("reentryWaiting");
    expect(call).not.toContain("behindState");
  });

  it("stops a 1x1 dial and a group start before anything is captured, taken or published", () => {
    const host = source("./CallHostContext.tsx");
    const dial = body(host, "dial");
    const held = dial.indexOf("callStartHeldWhileBehind(");
    expect(held).toBeGreaterThan(-1);
    expect(held).toBeLessThan(dial.indexOf("captureGate("));
    expect(held).toBeLessThan(dial.indexOf("session.dial("));
    // An error with no copy of its own: the chat shows its ordinary
    // could-not-start line.
    expect(dial.slice(held, dial.indexOf("}", held))).toContain("throw new ConversationBehindError()");
    const group = body(host, "startGroupCall");
    const groupHeld = group.indexOf("callStartHeldWhileBehind(");
    expect(groupHeld).toBeGreaterThan(-1);
    expect(groupHeld).toBeLessThan(group.indexOf("captureGate("));
    expect(groupHeld).toBeLessThan(group.indexOf("setGroupSlot("));
    expect(groupHeld).toBeLessThan(group.indexOf("runGroupCallStart"));
    const refusal = group.slice(groupHeld, group.indexOf("};", groupHeld));
    expect(refusal).toContain('"call.error.start_group"');
    expect(refusal).toContain('"call.error.join_group"');
    expect(refusal).not.toContain("reentry");
  });
});
