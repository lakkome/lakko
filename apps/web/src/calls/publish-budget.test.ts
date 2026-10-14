// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The SFU's ceiling on this device's camera senders (`publish-budget.ts`;
 * ADR-0068, amendment of 2026-10-11): the frame, the one mapping from the
 * frame to a sender's ceiling, and its application to a sender, serialised
 * with every other write to that sender's parameters.
 */
import { describe, expect, it, vi } from "vitest";

import { CAMERA_DEGRADATION_PREFERENCE, preferCameraFramerate } from "./camera-sender";
import {
  applyEncodingCeiling,
  BUDGETED_SENDER_KINDS,
  claimsPublishBudget,
  describePublishBudgetChange,
  describeSenderCeiling,
  MAX_PUBLISH_BUDGET_KBPS,
  readEncodingCeiling,
  readPublishBudgetFrame,
  sameBudget,
  SenderCeiling,
  senderCeiling,
  type CeilingSender,
} from "./publish-budget";
import { SenderActivity } from "./sender-activity";
import { queueParameterWrite } from "./sender-parameters";

// ---- The frame -------------------------------------------------------------

describe("reading a publish-budget frame", () => {
  const parser = (values: number[] | Error) => ({
    parseSfuPublishBudget: (): Uint32Array => {
      if (values instanceof Error) throw values;
      return new Uint32Array(values);
    },
  });

  it("turns the helper's two values into the two ceilings", () => {
    expect(readPublishBudgetFrame(parser([2500, 500]), "")).toEqual({ mainKbps: 2500, middleKbps: 500 });
    expect(readPublishBudgetFrame(parser([1, MAX_PUBLISH_BUDGET_KBPS]), "")).toEqual({
      mainKbps: 1,
      middleKbps: MAX_PUBLISH_BUDGET_KBPS,
    });
  });

  it("is null when the helper throws, and never throws itself", () => {
    expect(readPublishBudgetFrame(parser(new Error("not a publish-budget frame")), "x")).toBeNull();
  });

  it("is null for anything but exactly two ceilings within the protocol's bounds", () => {
    expect(readPublishBudgetFrame(parser([]), "")).toBeNull();
    expect(readPublishBudgetFrame(parser([900]), "")).toBeNull();
    expect(readPublishBudgetFrame(parser([900, 500, 150]), "")).toBeNull();
    expect(readPublishBudgetFrame(parser([0, 500]), "")).toBeNull();
    expect(readPublishBudgetFrame(parser([900, 0]), "")).toBeNull();
    expect(readPublishBudgetFrame(parser([MAX_PUBLISH_BUDGET_KBPS + 1, 500]), "")).toBeNull();
    expect(readPublishBudgetFrame(parser([900, 4_000_000_000]), "")).toBeNull();
  });

  it("recognises a refused frame that says it is a publish-budget one", () => {
    expect(claimsPublishBudget('{"kind":"publish-budget","main_kbps":0}')).toBe(true);
    expect(claimsPublishBudget('{"kind":"video-needed","main":true,"middle":true}')).toBe(false);
    expect(claimsPublishBudget("not json")).toBe(false);
    expect(claimsPublishBudget("null")).toBe(false);
    expect(claimsPublishBudget('"publish-budget"')).toBe(false);
  });
});

// ---- The mapping -----------------------------------------------------------

describe("a sender's ceiling", () => {
  it("is not written before the first frame: each sender keeps what it was created with", () => {
    expect(senderCeiling("main", null)).toBeNull();
    expect(senderCeiling("middle", null)).toBeNull();
    expect(senderCeiling("light", null)).toBeNull();
  });

  it("is the frame's figure for the main and the middle sender, in bits a second", () => {
    const budget = { mainKbps: 1200, middleKbps: 483 };
    expect(senderCeiling("main", budget)).toBe(1_200_000);
    expect(senderCeiling("middle", budget)).toBe(483_000);
  });

  it("is never written for the light sender", () => {
    expect(senderCeiling("light", { mainKbps: 200, middleKbps: 100 })).toBeNull();
    expect(BUDGETED_SENDER_KINDS).toEqual(["main", "middle"]);
  });

  it("is the server's figure as it stands: no client-side clamp or share", () => {
    // Whatever the SFU says within the protocol's bounds is applied.
    expect(senderCeiling("middle", { mainKbps: 1, middleKbps: MAX_PUBLISH_BUDGET_KBPS })).toBe(
      MAX_PUBLISH_BUDGET_KBPS * 1000,
    );
    expect(senderCeiling("main", { mainKbps: 1, middleKbps: 1 })).toBe(1000);
  });
});

// ---- Application -----------------------------------------------------------

interface FakeSender extends CeilingSender {
  encodings: Array<Record<string, unknown>>;
  degradationPreference: string | undefined;
  /** What each `setParameters` call carried, in order. */
  asked: Array<{ maxBitrate: unknown; active: unknown; degradationPreference: unknown }>;
  /** Calls that were pending at the same time, at most. */
  maxPending: number;
  /** `setParameters` calls refused for a reading another call outdated. */
  stale: number;
  refuse: boolean;
  /** Resolve the pending `setParameters` calls by hand when set. */
  manual: boolean;
  release(): void;
}

function fakeSender(first: Record<string, unknown> | null = {}): FakeSender {
  let pending = 0;
  let transaction = 0;
  const waiting: Array<() => void> = [];
  const sender: FakeSender = {
    encodings: first === null ? [] : [{ ...first }],
    degradationPreference: undefined,
    asked: [],
    maxPending: 0,
    stale: 0,
    refuse: false,
    manual: false,
    release(): void {
      for (const go of waiting.splice(0)) go();
    },
    getParameters(): RTCRtpSendParameters {
      transaction += 1;
      return {
        transactionId: String(transaction),
        encodings: sender.encodings.map((e) => ({ ...e })),
        degradationPreference: sender.degradationPreference,
      } as unknown as RTCRtpSendParameters;
    },
    async setParameters(p: RTCRtpSendParameters): Promise<void> {
      // As a browser: only the newest `getParameters` reading may be set.
      if (p.transactionId !== String(transaction)) {
        sender.stale += 1;
        throw new Error("InvalidStateError: stale transaction");
      }
      const next = (p.encodings as unknown as Array<Record<string, unknown>>).map((e) => ({ ...e }));
      sender.asked.push({
        maxBitrate: next[0]?.["maxBitrate"],
        active: next[0]?.["active"],
        degradationPreference: p.degradationPreference,
      });
      pending += 1;
      sender.maxPending = Math.max(sender.maxPending, pending);
      try {
        if (sender.manual) await new Promise<void>((r) => waiting.push(r));
        else await Promise.resolve();
        if (sender.refuse) throw new Error("InvalidModificationError");
        sender.encodings = next;
        sender.degradationPreference = p.degradationPreference;
      } finally {
        pending -= 1;
      }
    },
  };
  return sender;
}

const ceilingOf = (s: FakeSender): unknown => s.encodings[0]?.["maxBitrate"];

/** Let every queued step run, releasing the fake browser's answers. */
async function drain(s: FakeSender): Promise<void> {
  for (let i = 0; i < 20; i += 1) {
    await Promise.resolve();
    s.release();
  }
}

describe("one read-modify-set of the ceiling", () => {
  it("sets nothing when the encoding already holds it", async () => {
    const s = fakeSender({ maxBitrate: 500_000 });
    expect(await applyEncodingCeiling(s, () => 500_000)).toEqual({ result: "applied" });
    expect(s.asked).toEqual([]);
  });

  it("changes only `maxBitrate`, keeping the rest of the encoding", async () => {
    const s = fakeSender({ scaleResolutionDownBy: 2, active: false, maxBitrate: 500_000 });
    expect(await applyEncodingCeiling(s, () => 483_000)).toEqual({ result: "applied" });
    expect(s.encodings[0]).toEqual({ scaleResolutionDownBy: 2, active: false, maxBitrate: 483_000 });
  });

  it("gives a sender that had no ceiling one", async () => {
    const s = fakeSender({ scalabilityMode: "L1T3" });
    await applyEncodingCeiling(s, () => 1_200_000);
    expect(s.encodings[0]).toEqual({ scalabilityMode: "L1T3", maxBitrate: 1_200_000 });
  });

  it("writes nothing when nothing is wanted", async () => {
    const s = fakeSender({});
    expect(await applyEncodingCeiling(s, () => null)).toEqual({ result: "applied" });
    expect(s.asked).toEqual([]);
  });

  it("reports a sender with no encoding, and a refusal, without throwing", async () => {
    expect((await applyEncodingCeiling(fakeSender(null), () => 1000)).result).toBe("no-encoding");
    const s = fakeSender({});
    s.refuse = true;
    const outcome = await applyEncodingCeiling(s, () => 1000);
    expect(outcome.result).toBe("refused");
    expect(ceilingOf(s)).toBeUndefined();
  });

  it("reads what a sender's encoding holds, and nothing as null", () => {
    expect(readEncodingCeiling(fakeSender({ maxBitrate: 150_000 }))).toBe(150_000);
    expect(readEncodingCeiling(fakeSender({}))).toBeNull();
    expect(readEncodingCeiling(fakeSender(null))).toBeNull();
    expect(
      readEncodingCeiling({
        getParameters: () => {
          throw new Error("closed");
        },
      }),
    ).toBeNull();
  });
});

describe("keeping a sender's encoding under the ceiling last asked", () => {
  const make = (): { ceiling: SenderCeiling; warn: ReturnType<typeof vi.fn> } => {
    const warn = vi.fn();
    return { ceiling: new SenderCeiling("main", warn), warn };
  };

  it("sets the ceiling with setParameters, and again when it changes", async () => {
    const { ceiling, warn } = make();
    const s = fakeSender({ scalabilityMode: "L1T3" });
    ceiling.bind(s);
    ceiling.want(1_200_000);
    await ceiling.settled();
    expect(ceilingOf(s)).toBe(1_200_000);
    ceiling.want(683_000);
    await ceiling.settled();
    expect(ceilingOf(s)).toBe(683_000);
    expect(s.asked.map((a) => a.maxBitrate)).toEqual([1_200_000, 683_000]);
    // Asking again for what it holds costs no call.
    ceiling.want(683_000);
    await ceiling.settled();
    expect(s.asked).toHaveLength(2);
    expect(warn).not.toHaveBeenCalled();
  });

  it("writes nothing before the first word: the plan's own ceiling stands", async () => {
    const { ceiling } = make();
    const s = fakeSender({ maxBitrate: 500_000 });
    ceiling.bind(s);
    ceiling.want(null);
    await ceiling.settled();
    expect(s.asked).toEqual([]);
    expect(ceilingOf(s)).toBe(500_000);
    expect(ceiling.lastWanted).toBeNull();
  });

  it("ends at the last word under rapid changes, with no two calls overlapping", async () => {
    const { ceiling } = make();
    const s = fakeSender({});
    s.manual = true;
    ceiling.bind(s);
    ceiling.want(2_000_000);
    await Promise.resolve();
    await Promise.resolve();
    expect(s.asked.map((a) => a.maxBitrate)).toEqual([2_000_000]);
    // Two more words while the browser has not answered the first.
    ceiling.want(1_000_000);
    ceiling.want(700_000);
    await drain(s);
    await ceiling.settled();
    expect(ceilingOf(s)).toBe(700_000);
    expect(s.maxPending).toBe(1);
    expect(s.stale).toBe(0);
    // The word in between was never set: the write reads what is wanted
    // when it runs.
    expect(s.asked.map((a) => a.maxBitrate)).toEqual([2_000_000, 700_000]);
  });

  it("asks again at every later look when the write is refused, saying so once", async () => {
    const { ceiling, warn } = make();
    const s = fakeSender({});
    s.refuse = true;
    ceiling.bind(s);
    ceiling.want(900_000);
    await ceiling.settled();
    expect(ceilingOf(s)).toBeUndefined();
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("main camera sender's bitrate ceiling could not be set");
    // The next tick asks again; still refused, not said again.
    ceiling.want(900_000);
    await ceiling.settled();
    expect(s.asked).toHaveLength(2);
    expect(warn).toHaveBeenCalledTimes(1);
    // The browser gives in.
    s.refuse = false;
    ceiling.want(900_000);
    await ceiling.settled();
    expect(ceilingOf(s)).toBe(900_000);
  });

  it("puts the ceiling back when asked again after the browser lost it", async () => {
    const { ceiling } = make();
    const s = fakeSender({});
    ceiling.bind(s);
    ceiling.want(900_000);
    await ceiling.settled();
    // A renegotiation or a replaced track reset the encoding.
    s.encodings = [{}];
    ceiling.want(900_000);
    await ceiling.settled();
    expect(ceilingOf(s)).toBe(900_000);
  });

  it("gives a sender that was created again the ceiling in force, not the one it was created with", async () => {
    const { ceiling, warn } = make();
    const first = fakeSender({ maxBitrate: 500_000 });
    ceiling.bind(first);
    ceiling.want(300_000);
    await ceiling.settled();
    expect(ceilingOf(first)).toBe(300_000);
    // The middle sender is removed and added again with the plan's ceiling.
    const second = fakeSender({ maxBitrate: 500_000 });
    ceiling.bind(second);
    ceiling.want(300_000);
    await ceiling.settled();
    expect(ceilingOf(second)).toBe(300_000);
    expect(first.asked).toHaveLength(1);
    expect(warn).not.toHaveBeenCalled();
  });

  it("does nothing without a sender, and applies the last word once there is one", async () => {
    const { ceiling } = make();
    ceiling.want(800_000);
    await ceiling.settled();
    expect(ceiling.bound).toBe(false);
    const s = fakeSender({});
    ceiling.bind(s);
    ceiling.want(ceiling.lastWanted);
    await ceiling.settled();
    expect(ceilingOf(s)).toBe(800_000);
  });

  it("does not write a sender that was replaced while the write waited its turn", async () => {
    const { ceiling, warn } = make();
    const old = fakeSender({});
    old.manual = true;
    ceiling.bind(old);
    // Something else holds the old sender's queue.
    let letGo: () => void = () => {};
    void queueParameterWrite(old, () => new Promise<void>((r) => (letGo = r)));
    ceiling.want(900_000);
    await Promise.resolve();
    const fresh = fakeSender({});
    ceiling.bind(fresh);
    letGo();
    await drain(old);
    await ceiling.settled();
    expect(old.asked).toEqual([]);
    expect(warn).not.toHaveBeenCalled();
    ceiling.want(900_000);
    await ceiling.settled();
    expect(ceilingOf(fresh)).toBe(900_000);
  });

  it("forgets everything at the end of the call", async () => {
    const { ceiling } = make();
    const s = fakeSender({});
    ceiling.bind(s);
    ceiling.want(900_000);
    await ceiling.settled();
    ceiling.reset();
    expect(ceiling.bound).toBe(false);
    expect(ceiling.lastWanted).toBeNull();
    // The next call's sender is not written before that call's first frame.
    const next = fakeSender({});
    ceiling.bind(next);
    ceiling.want(null);
    await ceiling.settled();
    expect(next.asked).toEqual([]);
  });
});

describe("the ceiling and the other writes to one sender's parameters", () => {
  it("never overlap, never use an outdated reading, and none undoes another", async () => {
    const s = fakeSender({ scalabilityMode: "L1T3" });
    s.manual = true;
    const activity = new SenderActivity("main", vi.fn());
    const ceiling = new SenderCeiling("main", vi.fn());
    activity.bind(s);
    ceiling.bind(s);
    // The sender's set-up, the switch-off and the ceiling, all in one turn,
    // as after a mid-call camera-on under a budget.
    const preferred = preferCameraFramerate(s as unknown as RTCRtpSender);
    activity.want(false);
    ceiling.want(900_000);
    await drain(s);
    await Promise.all([preferred, activity.settled(), ceiling.settled()]);
    expect(s.maxPending).toBe(1);
    expect(s.stale).toBe(0);
    expect(s.encodings[0]).toEqual({ scalabilityMode: "L1T3", active: false, maxBitrate: 900_000 });
    expect(s.degradationPreference).toBe(CAMERA_DEGRADATION_PREFERENCE);
    // Each write carried what the ones before it had set.
    expect(s.asked).toHaveLength(3);
    expect(s.asked.at(-1)?.degradationPreference).toBe(CAMERA_DEGRADATION_PREFERENCE);
  });

  it("run in the order they were asked, and a failed one does not hold the rest back", async () => {
    const sender = {};
    const order: string[] = [];
    const first = queueParameterWrite(sender, async () => {
      await Promise.resolve();
      order.push("first");
      throw new Error("refused");
    });
    const second = queueParameterWrite(sender, async () => {
      order.push("second");
      return 2;
    });
    await expect(first).rejects.toThrow("refused");
    await expect(second).resolves.toBe(2);
    expect(order).toEqual(["first", "second"]);
    // Another sender's queue is its own.
    const other = queueParameterWrite({}, async () => "other");
    await expect(other).resolves.toBe("other");
  });

  it("start a write on an idle sender in the same turn", () => {
    let ran = false;
    void queueParameterWrite({}, async () => {
      ran = true;
    });
    expect(ran).toBe(true);
  });
});

// ---- The diagnostics -------------------------------------------------------

describe("the diagnostics", () => {
  it("say one line when the budget changed, and nothing when a frame repeats it", () => {
    expect(describePublishBudgetChange(null, { mainKbps: 1922, middleKbps: 500 })).toBe(
      "call publish budget main=1922 middle=500",
    );
    expect(
      describePublishBudgetChange({ mainKbps: 1922, middleKbps: 500 }, { mainKbps: 200, middleKbps: 483 }),
    ).toBe("call publish budget main=200 middle=483");
    expect(
      describePublishBudgetChange({ mainKbps: 200, middleKbps: 483 }, { mainKbps: 200, middleKbps: 483 }),
    ).toBeNull();
    expect(sameBudget(null, null)).toBe(true);
    expect(sameBudget(null, { mainKbps: 1, middleKbps: 1 })).toBe(false);
  });

  it("give a send video line its ceiling= tail in kbit/s", () => {
    expect(describeSenderCeiling(1_200_000)).toBe(" ceiling=1200");
    expect(describeSenderCeiling(150_999)).toBe(" ceiling=150");
    expect(describeSenderCeiling(null)).toBe(" ceiling=-");
    expect(describeSenderCeiling(undefined)).toBe("");
  });
});
