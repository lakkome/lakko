// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which camera senders encode and send (`sender-activity.ts`): the reading
 * of the SFU's `video-needed` frame, the decision in every combination, and
 * its application to a sender. No DOM: the sender is a stand-in.
 */
import { describe, expect, it, vi } from "vitest";

import {
  applyEncodingActive,
  CAMERA_SENDER_KINDS,
  claimsVideoNeeded,
  describeSenderActivity,
  describeVideoNeededChange,
  neededField,
  neededWord,
  readVideoNeededFrame,
  SenderActivity,
  senderActive,
  sendersActive,
  type ActivitySender,
  type CameraSenderKind,
  type VideoNeeded,
} from "./sender-activity";

// ---- The frame -------------------------------------------------------------

describe("reading a video-needed frame", () => {
  const parser = (bytes: number[] | Error) => ({
    parseSfuVideoNeeded: (): Uint8Array => {
      if (bytes instanceof Error) throw bytes;
      return new Uint8Array(bytes);
    },
  });

  it("turns the helper's two bytes into the two flags", () => {
    expect(readVideoNeededFrame(parser([1, 1]), "")).toEqual({ main: true, middle: true });
    expect(readVideoNeededFrame(parser([0, 1]), "")).toEqual({ main: false, middle: true });
    expect(readVideoNeededFrame(parser([1, 0]), "")).toEqual({ main: true, middle: false });
    expect(readVideoNeededFrame(parser([0, 0]), "")).toEqual({ main: false, middle: false });
  });

  it("is null when the helper throws, and never throws itself", () => {
    expect(readVideoNeededFrame(parser(new Error("not a video-needed frame")), "x")).toBeNull();
  });

  it("is null for anything but exactly two bytes of 0 or 1", () => {
    expect(readVideoNeededFrame(parser([]), "")).toBeNull();
    expect(readVideoNeededFrame(parser([1]), "")).toBeNull();
    expect(readVideoNeededFrame(parser([1, 1, 1]), "")).toBeNull();
    expect(readVideoNeededFrame(parser([2, 0]), "")).toBeNull();
    expect(readVideoNeededFrame(parser([0, 255]), "")).toBeNull();
  });

  it("recognises a refused frame that says it is a video-needed one", () => {
    expect(claimsVideoNeeded('{"kind":"video-needed","main":1}')).toBe(true);
    expect(claimsVideoNeeded('{"kind":"video-given","given":[]}')).toBe(false);
    expect(claimsVideoNeeded("not json")).toBe(false);
    expect(claimsVideoNeeded("null")).toBe(false);
    expect(claimsVideoNeeded('"video-needed"')).toBe(false);
  });
});

// ---- The decision ----------------------------------------------------------

describe("the SFU's word for a sender", () => {
  it("is none before the first frame, and never any for the light sender", () => {
    for (const kind of CAMERA_SENDER_KINDS) expect(neededWord(kind, null)).toBeNull();
    expect(neededWord("light", { main: false, middle: false })).toBeNull();
    expect(neededWord("light", { main: true, middle: true })).toBeNull();
  });

  it("is the frame's flag for the main and the middle sender", () => {
    expect(neededWord("main", { main: false, middle: true })).toBe(false);
    expect(neededWord("middle", { main: false, middle: true })).toBe(true);
  });
});

describe("whether a sender's encoding is active", () => {
  const words: Array<VideoNeeded | null> = [
    null,
    { main: true, middle: true },
    { main: true, middle: false },
    { main: false, middle: true },
    { main: false, middle: false },
  ];
  const bools = [true, false];

  it("is exactly: camera on, sender exists, people count (middle only), and the SFU's word not false; in every combination", () => {
    let checked = 0;
    for (const kind of CAMERA_SENDER_KINDS) {
      for (const cameraOn of bools) {
        for (const exists of bools) {
          for (const middleWanted of bools) {
            for (const needed of words) {
              const word = kind === "main" ? needed?.main : kind === "middle" ? needed?.middle : undefined;
              const expected =
                cameraOn && exists && (kind !== "middle" || middleWanted) && word !== false;
              expect(
                senderActive(kind, { cameraOn, exists, middleWanted, needed }),
                `${kind} cameraOn=${cameraOn} exists=${exists} middleWanted=${middleWanted} needed=${JSON.stringify(needed)}`,
              ).toBe(expected);
              checked += 1;
            }
          }
        }
      }
    }
    expect(checked).toBe(3 * 2 * 2 * 2 * 5);
  });

  it("takes no word yet as needed", () => {
    const on = { cameraOn: true, exists: true, middleWanted: true, needed: null };
    for (const kind of CAMERA_SENDER_KINDS) expect(senderActive(kind, on)).toBe(true);
  });

  it("never lets the SFU's word switch the light sender off", () => {
    const input = { cameraOn: true, exists: true, middleWanted: false, needed: { main: false, middle: false } };
    expect(senderActive("light", input)).toBe(true);
    expect(senderActive("main", input)).toBe(false);
    expect(senderActive("middle", input)).toBe(false);
  });

  it("keeps the middle sender off under three people whatever the SFU says", () => {
    const needed = { main: true, middle: true };
    expect(senderActive("middle", { cameraOn: true, exists: true, middleWanted: false, needed })).toBe(false);
    expect(senderActive("middle", { cameraOn: true, exists: true, middleWanted: true, needed })).toBe(true);
  });

  it("answers for every sender at once", () => {
    expect(
      sendersActive(
        { cameraOn: true, middleWanted: true, needed: { main: false, middle: true } },
        (kind) => kind !== "middle",
      ),
    ).toEqual({ main: false, middle: false, light: true });
  });
});

// ---- Application -----------------------------------------------------------

interface FakeSender extends ActivitySender {
  encodings: Array<Record<string, unknown>>;
  /** The `active` each `setParameters` call asked for, in order. */
  asked: boolean[];
  /** Calls that were pending at the same time, at most. */
  maxPending: number;
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
    asked: [],
    maxPending: 0,
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
      } as unknown as RTCRtpSendParameters;
    },
    async setParameters(p: RTCRtpSendParameters): Promise<void> {
      // As a browser: only the newest `getParameters` reading may be set.
      if (p.transactionId !== String(transaction)) throw new Error("InvalidStateError: stale transaction");
      const next = (p.encodings as unknown as Array<Record<string, unknown>>).map((e) => ({ ...e }));
      sender.asked.push((next[0]?.["active"] ?? true) === true);
      pending += 1;
      sender.maxPending = Math.max(sender.maxPending, pending);
      try {
        if (sender.manual) await new Promise<void>((r) => waiting.push(r));
        else await Promise.resolve();
        if (sender.refuse) throw new Error("InvalidModificationError");
        sender.encodings = next;
      } finally {
        pending -= 1;
      }
    },
  };
  return sender;
}

const isActive = (s: FakeSender): boolean => (s.encodings[0]?.["active"] ?? true) === true;

describe("one read-modify-set of the flag", () => {
  it("sets nothing when the encoding already says so (unset is active)", async () => {
    const s = fakeSender({ maxBitrate: 500_000 });
    expect(await applyEncodingActive(s, true)).toEqual({ result: "applied" });
    expect(s.asked).toEqual([]);
  });

  it("changes only `active`, keeping the rest of the encoding", async () => {
    const s = fakeSender({ maxBitrate: 500_000, scaleResolutionDownBy: 2 });
    expect((await applyEncodingActive(s, false)).result).toBe("applied");
    expect(s.encodings).toEqual([{ maxBitrate: 500_000, scaleResolutionDownBy: 2, active: false }]);
  });

  it("reports a sender with no encoding, and a refusal, without throwing", async () => {
    expect((await applyEncodingActive(fakeSender(null), false)).result).toBe("no-encoding");
    const s = fakeSender();
    s.refuse = true;
    const out = await applyEncodingActive(s, false);
    expect(out.result).toBe("refused");
    expect(out.error).toBeInstanceOf(Error);
  });
});

describe("keeping a sender's encoding at what was last asked", () => {
  const make = (): { activity: SenderActivity; warn: ReturnType<typeof vi.fn> } => {
    const warn = vi.fn();
    return { activity: new SenderActivity("main", warn), warn };
  };

  it("stops and starts the sender with setParameters", async () => {
    const { activity, warn } = make();
    const s = fakeSender({ scalabilityMode: "L1T3" });
    activity.bind(s);
    activity.want(false);
    await activity.settled();
    expect(isActive(s)).toBe(false);
    activity.want(true);
    await activity.settled();
    expect(isActive(s)).toBe(true);
    expect(s.asked).toEqual([false, true]);
    expect(s.encodings[0]?.["scalabilityMode"]).toBe("L1T3");
    expect(warn).not.toHaveBeenCalled();
  });

  it("ends at the last word under a rapid true, false, true, with no two calls overlapping", async () => {
    const { activity } = make();
    const s = fakeSender({ active: false });
    s.manual = true;
    activity.bind(s);
    activity.want(true);
    // The first call is out and unanswered when the next two words arrive.
    await Promise.resolve();
    await Promise.resolve();
    expect(s.asked).toEqual([true]);
    activity.want(false);
    activity.want(true);
    s.release();
    for (let i = 0; i < 10; i += 1) {
      await Promise.resolve();
      s.release();
    }
    await activity.settled();
    expect(isActive(s)).toBe(true);
    expect(s.maxPending).toBe(1);
    // Never out of order: no `false` is set after the last `true` was asked.
    expect(s.asked.at(-1)).toBe(true);
  });

  it("ends at the last word under a rapid false, true, false as well", async () => {
    const { activity } = make();
    const s = fakeSender();
    s.manual = true;
    activity.bind(s);
    activity.want(false);
    await Promise.resolve();
    await Promise.resolve();
    activity.want(true);
    activity.want(false);
    for (let i = 0; i < 10; i += 1) {
      s.release();
      await Promise.resolve();
    }
    await activity.settled();
    expect(isActive(s)).toBe(false);
    expect(s.maxPending).toBe(1);
  });

  it("applies each word in turn when the browser answers between them", async () => {
    const { activity } = make();
    const s = fakeSender();
    activity.bind(s);
    for (const word of [false, true, false, true, false]) {
      activity.want(word);
      await activity.settled();
    }
    expect(s.asked).toEqual([false, true, false, true, false]);
    expect(s.maxPending).toBe(1);
  });

  it("never uses a stale getParameters reading", async () => {
    // The fake refuses a stale transaction as a browser does; many words in
    // one turn must still land without a refusal.
    const { activity, warn } = make();
    const s = fakeSender();
    activity.bind(s);
    for (let i = 0; i < 20; i += 1) activity.want(i % 2 === 1);
    await activity.settled();
    expect(isActive(s)).toBe(true);
    expect(warn).not.toHaveBeenCalled();
  });

  it("puts the flag back when asked again after the browser reset it", async () => {
    const { activity } = make();
    const s = fakeSender();
    activity.bind(s);
    activity.want(false);
    await activity.settled();
    // A renegotiation or a replaced track that reset the parameters.
    s.encodings = [{}];
    activity.want(false);
    await activity.settled();
    expect(isActive(s)).toBe(false);
    expect(s.asked).toEqual([false, false]);
  });

  it("gives a replaced sender the flag the old one had", async () => {
    const { activity } = make();
    const old = fakeSender();
    activity.bind(old);
    activity.want(false);
    await activity.settled();
    const fresh = fakeSender();
    activity.bind(fresh);
    activity.want(false);
    await activity.settled();
    expect(isActive(fresh)).toBe(false);
    expect(old.asked).toEqual([false]);
    expect(fresh.asked).toEqual([false]);
  });

  it("leaves the sender sending when a switch-off is refused, says so once, and does not ask again", async () => {
    const { activity, warn } = make();
    const s = fakeSender();
    s.refuse = true;
    activity.bind(s);
    activity.want(false);
    await activity.settled();
    expect(isActive(s)).toBe(true);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("the main camera sender could not be stopped");
    activity.want(false);
    activity.want(false);
    await activity.settled();
    expect(s.asked).toEqual([false]);
    expect(warn).toHaveBeenCalledTimes(1);
    // A later change of mind is asked afresh.
    s.refuse = false;
    activity.want(true);
    await activity.settled();
    activity.want(false);
    await activity.settled();
    expect(isActive(s)).toBe(false);
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it("asks again at every later look when a switch-on is refused, saying so once", async () => {
    const { activity, warn } = make();
    const s = fakeSender({ active: false });
    s.refuse = true;
    activity.bind(s);
    activity.want(true);
    await activity.settled();
    activity.want(true);
    await activity.settled();
    expect(isActive(s)).toBe(false);
    expect(s.asked).toEqual([true, true]);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain("the main camera sender could not be started");
    s.refuse = false;
    activity.want(true);
    await activity.settled();
    expect(isActive(s)).toBe(true);
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it("does nothing without a sender, and applies the last word once there is one", async () => {
    const { activity } = make();
    activity.want(false);
    await activity.settled();
    expect(activity.bound).toBe(false);
    const s = fakeSender();
    activity.bind(s);
    activity.want(activity.lastWanted ?? true);
    await activity.settled();
    expect(isActive(s)).toBe(false);
  });

  it("does not touch a sender that was replaced while the browser answered", async () => {
    const { activity, warn } = make();
    const old = fakeSender();
    old.manual = true;
    old.refuse = true;
    activity.bind(old);
    activity.want(false);
    await Promise.resolve();
    await Promise.resolve();
    activity.bind(fakeSender());
    old.release();
    await activity.settled();
    // The refusal was the old sender's: nothing is said of the new one.
    expect(warn).not.toHaveBeenCalled();
  });

  it("forgets everything at the end of the call", async () => {
    const { activity } = make();
    const s = fakeSender();
    activity.bind(s);
    activity.want(false);
    await activity.settled();
    activity.reset();
    expect(activity.bound).toBe(false);
    expect(activity.lastWanted).toBeNull();
  });
});

// ---- Diagnostics -----------------------------------------------------------

describe("the diagnostics", () => {
  it("name the SFU's word yes, no or -", () => {
    expect(neededField(true)).toBe("yes");
    expect(neededField(false)).toBe("no");
    expect(neededField(null)).toBe("-");
  });

  it("give a send video line its needed= and active= tail", () => {
    expect(describeSenderActivity({ needed: null, active: true })).toBe(" needed=- active=yes");
    expect(describeSenderActivity({ needed: false, active: false })).toBe(" needed=no active=no");
    expect(describeSenderActivity({ needed: true, active: false })).toBe(" needed=yes active=no");
  });

  const all = (main: boolean, middle: boolean, light: boolean): Record<CameraSenderKind, boolean> => ({
    main,
    middle,
    light,
  });

  it("say one line when the SFU's word changed what is active", () => {
    expect(
      describeVideoNeededChange({ main: false, middle: true }, all(true, true, true), all(false, true, true)),
    ).toBe("call video needed main=no middle=yes active main=no middle=yes light=yes");
  });

  it("say nothing when the word changed nothing that is active", () => {
    // The middle sender was off for the people count already.
    expect(
      describeVideoNeededChange({ main: true, middle: false }, all(true, false, true), all(true, false, true)),
    ).toBeNull();
  });
});
