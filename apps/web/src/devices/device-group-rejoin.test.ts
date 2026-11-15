// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decision 4 - a device that lost or fell behind its DeviceGroup
 * rejoins through a sibling; re-founding is a single-device account's.
 *
 *   - "behind" is a drained read still behind the DS, with no own commit in
 *     flight;
 *   - a sibling admits a lost device (listed, bound, knocked after the last
 *     time it saw that key added);
 *   - a removed device (deleted and tombstoned in the directory, so not
 *     listed) is not admitted;
 *   - every running sibling that passes the checks admits at once (no
 *     designated admitter: the DS commit route picks one);
 *   - the device's own run knocks and waits max(4 x rtt, 1 s): a presence
 *     note means wait for the Welcome, silence means found;
 *   - a single-device account founds (`lifecycle.ts` with `mayFound`).
 */
import { describe, expect, it } from "vitest";

import {
  ANSWER_WINDOW_MIN_MS,
  NO_HANDLE_RETRY_MS,
  createNoHandleWaker,
  REKNOCK_AFTER_MS,
  RESET_REFUSALS_BEFORE_REJOIN,
  STALE_KNOCK_AT_LISTEN_START_MS,
  admissionVerdict,
  answerWindowMs,
  decodeAdmissionRecord,
  decodeRejoinState,
  encodeAdmissionRecord,
  encodeRejoinState,
  isBehind,
  journalResetUnder,
  knockScreen,
  newAdmissionRecord,
  noteAdded,
  noteHandled,
  redactIdentifiers,
  rejoinFailureCode,
  rejoinOnce,
  shouldKnockAgain,
  type AdmissionInput,
  type AnswerWatch,
  type RejoinAnswer,
  type RejoinPorts,
  type RejoinState,
} from "./device-group-rejoin";

describe("behind", () => {
  it("a drained read still behind the DS, with no own commit in flight", () => {
    expect(isBehind(5n, true, 7n, false)).toBe(true);
    expect(isBehind(5n, true, 5n, false)).toBe(false);
    // Rows still to read: they may bring the device forward.
    expect(isBehind(5n, false, 7n, false)).toBe(false);
    // The DS named no epoch (an unclaimed journal).
    expect(isBehind(5n, true, undefined, false)).toBe(false);
    // Its own commit in flight: the DS may hold its own next epoch.
    expect(isBehind(5n, true, 6n, true)).toBe(false);
  });
});

const SIBLING = "aa";
const LOST = "bb";
const OTHER = "cc";

function input(overrides: Partial<AdmissionInput> = {}): AdmissionInput {
  return {
    knockId: 10,
    knockAtMs: 1_000_000,
    keyHex: LOST,
    ownKeyHex: SIBLING,
    roster: [
      { leaf: 0, keyHex: SIBLING },
      { leaf: 1, keyHex: LOST },
      { leaf: 2, keyHex: OTHER },
    ],
    handled: new Set(),
    watchingSinceMs: 500_000,
    lastAddedAtMs: undefined,
    listed: true,
    policyRequiresAttestation: false,
    selfRemovalProofCovers: false,
    keyPackageAlreadyIn: false,
    nowMs: 1_000_100,
    ...overrides,
  };
}

describe("a sibling's admission rule", () => {
  it("admits a lost device whose previous leaf is still in the roster", () => {
    expect(admissionVerdict(input())).toBe("admit");
  });

  it("admits a device whose leaf is gone too", () => {
    expect(
      admissionVerdict(input({ roster: [{ leaf: 0, keyHex: SIBLING }, { leaf: 2, keyHex: OTHER }] })),
    ).toBe("admit");
  });

  it("never admits a removed device: deleted from the directory and tombstoned there", () => {
    expect(admissionVerdict(input({ listed: false }))).toBe("not-listed");
  });

  it("does not admit twice: a knock older than the key's last addition is answered", () => {
    expect(admissionVerdict(input({ lastAddedAtMs: 1_000_050 }))).toBe("already-admitted");
    expect(admissionVerdict(input({ handled: new Set([10]) }))).toBe("handled");
    // A knock after the last addition (the device lost its state again) is new.
    expect(admissionVerdict(input({ lastAddedAtMs: 900_000 }))).toBe("admit");
  });

  it("ignores a knock from before it started watching (it cannot know whether it was answered)", () => {
    expect(admissionVerdict(input({ watchingSinceMs: 1_000_001 }))).toBe("before-watching");
  });

  it("every running sibling admits at once, not only the lowest leaf (ADR-0081 third amendment)", () => {
    // OTHER is not the lowest other leaf, and the knock is fresh: it admits
    // anyway; the DS commit route lets one of the commits win.
    expect(admissionVerdict(input({ ownKeyHex: OTHER, nowMs: 1_000_001 }))).toBe("admit");
    expect(admissionVerdict(input({ ownKeyHex: SIBLING, nowMs: 1_000_001 }))).toBe("admit");
  });

  it("the knocking key itself never admits", () => {
    const roster = [
      { leaf: 0, keyHex: LOST },
      { leaf: 1, keyHex: SIBLING },
    ];
    expect(admissionVerdict(input({ roster }))).toBe("admit");
    expect(admissionVerdict(input({ roster, ownKeyHex: LOST }))).toBe("own-key");
  });

  it("the screen before the presence note needs no directory: an open knock passes it", () => {
    const base = input();
    expect(knockScreen(base)).toBeNull();
    // A knock from an unlisted key is still answered with a presence note;
    // only the admission refuses it.
    expect(knockScreen(input({ listed: false }))).toBeNull();
    expect(admissionVerdict(input({ listed: false }))).toBe("not-listed");
    expect(knockScreen(input({ handled: new Set([10]) }))).toBe("handled");
    expect(knockScreen(input({ ownKeyHex: LOST }))).toBe("own-key");
    expect(knockScreen(input({ watchingSinceMs: 1_000_001 }))).toBe("before-watching");
    expect(knockScreen(input({ keyPackageAlreadyIn: true }))).toBe("already-admitted");
  });

  it("a knock already waiting when listening starts is answered only when at most 30 s old", () => {
    // knockAtMs 1 000 000.
    expect(knockScreen(input({ listeningSinceMs: 1_000_000 + STALE_KNOCK_AT_LISTEN_START_MS }))).toBeNull();
    expect(knockScreen(input({ listeningSinceMs: 1_000_001 + STALE_KNOCK_AT_LISTEN_START_MS }))).toBe("stale");
    expect(admissionVerdict(input({ listeningSinceMs: 1_000_001 + STALE_KNOCK_AT_LISTEN_START_MS }))).toBe(
      "stale",
    );
    // A knock that arrives while listening is newer than the start.
    expect(knockScreen(input({ listeningSinceMs: 999_000 }))).toBeNull();
  });

  it("under the attested-removal policy a self-removal proof that covers the replacement admits (ADR-0081 amendment)", () => {
    expect(
      admissionVerdict(input({ policyRequiresAttestation: true, selfRemovalProofCovers: true })),
    ).toBe("admit");
  });

  it("a knock whose KeyPackage is already a leaf was answered, whatever the roster's keys say", () => {
    expect(admissionVerdict(input({ keyPackageAlreadyIn: true }))).toBe("already-admitted");
    expect(
      admissionVerdict(input({ keyPackageAlreadyIn: true, selfRemovalProofCovers: true })),
    ).toBe("already-admitted");
  });

  it("under the attested-removal policy a present previous leaf without a covering proof waits for the person", () => {
    expect(admissionVerdict(input({ policyRequiresAttestation: true }))).toBe("needs-attestation");
    expect(
      admissionVerdict(
        input({ policyRequiresAttestation: true, roster: [{ leaf: 0, keyHex: SIBLING }] }),
      ),
    ).toBe("admit");
  });
});

describe("the sibling's record", () => {
  it("keeps the latest addition per key and a bounded handled set", () => {
    let r = newAdmissionRecord(5);
    r = noteAdded(r, LOST, 100);
    r = noteAdded(r, LOST, 50);
    expect(r.lastAddedAtMs[LOST]).toBe(100);
    for (let i = 0; i < 300; i += 1) r = noteHandled(r, i);
    expect(r.handled).toHaveLength(256);
    expect(r.handled[0]).toBe(44);
    expect(decodeAdmissionRecord(encodeAdmissionRecord(r))).toEqual(r);
    expect(decodeAdmissionRecord(new TextEncoder().encode('{"v":2}'))).toBeNull();
  });
});

describe("the answer window", () => {
  it("is four round trips of the knock, and never under a second", () => {
    expect(answerWindowMs(100)).toBe(ANSWER_WINDOW_MIN_MS);
    expect(answerWindowMs(250)).toBe(1_000);
    expect(answerWindowMs(400)).toBe(1_600);
    expect(answerWindowMs(3_000)).toBe(12_000);
    expect(answerWindowMs(0)).toBe(1_000);
    expect(answerWindowMs(-5)).toBe(1_000);
    expect(answerWindowMs(Number.NaN)).toBe(1_000);
  });
});

describe("a journal reset under this device", () => {
  it("is read after a run of refused reads, not from one", () => {
    expect(journalResetUnder(RESET_REFUSALS_BEFORE_REJOIN - 1)).toBe(false);
    expect(journalResetUnder(RESET_REFUSALS_BEFORE_REJOIN)).toBe(true);
    expect(journalResetUnder(0)).toBe(false);
  });
});

describe("what a failed rejoin logs", () => {
  it("names the failure without identifiers", () => {
    expect(rejoinFailureCode({ name: "DsHttpError", status: 401, message: "ds /ds/group/abcd → 401" })).toBe(
      "DsHttpError 401",
    );
    expect(rejoinFailureCode({ name: "DsHttpError", status: 401, clockSkewed: true })).toBe(
      "DsHttpError 401 clock-skewed",
    );
    expect(rejoinFailureCode({ name: "DsNetworkError", message: "ds /ds/rendezvous/00ff network error" })).toBe(
      "DsNetworkError",
    );
    const hex = "0123456789abcdef0123456789abcdef";
    expect(rejoinFailureCode(new Error(`group ${hex} refused`))).toBe("Error: group <id> refused");
    expect(rejoinFailureCode(new Error("pairing: no username yet"))).toBe("Error: pairing: no username yet");
    expect(rejoinFailureCode("x")).toBe("string");
    expect(redactIdentifiers("token AAAAAAAAAAAAAAAAAAAAAAAA end")).toBe("token <id> end");
  });
});

/** A rendezvous scripted by time: each answer becomes readable at its time. */
function scriptedWatch(
  clock: { now: number },
  answers: readonly { readonly atMs: number; readonly answer: RejoinAnswer }[],
): AnswerWatch & { opened: number; closed: boolean } {
  const pending = [...answers].sort((a, b) => a.atMs - b.atMs);
  const w = {
    opened: clock.now,
    closed: false,
    async next(deadline: number): Promise<RejoinAnswer | null> {
      const first = pending[0];
      if (first !== undefined && first.atMs <= deadline) {
        pending.shift();
        clock.now = Math.max(clock.now, first.atMs);
        return first.answer;
      }
      clock.now = Math.max(clock.now, deadline);
      return null;
    },
    close() {
      w.closed = true;
    },
  };
  return w;
}

describe("the rejoining device's run", () => {
  const WELCOME: RejoinAnswer = { kind: "welcome", bytes: new Uint8Array([1]) };
  const PRESENT: RejoinAnswer = { kind: "present" };

  /** `answers` are relative to the moment the knock was sent. `rttMs` is the
   *  knock request's round trip. */
  function ports(
    answers: readonly { readonly afterMs: number; readonly answer: RejoinAnswer }[],
    options: { rttMs?: number; noUsername?: boolean } = {},
  ): RejoinPorts & {
    log: string[];
    state: RejoinState | null;
    clock: { now: number };
    noted: { ms: number | null; kind: string | null }[];
    watches: ReturnType<typeof scriptedWatch>[];
    script: { afterMs: number; answer: RejoinAnswer }[];
  } {
    const clock = { now: 1_000 };
    const rtt = options.rttMs ?? 100;
    let sentAt: number | null = null;
    const p = {
      log: [] as string[],
      state: null as RejoinState | null,
      clock,
      noted: [] as { ms: number | null; kind: string | null }[],
      watches: [] as ReturnType<typeof scriptedWatch>[],
      /** What a watch opened from now on will read (a fresh knock's
       *  rendezvous starts with whatever this holds). */
      script: [...answers],
      async dropLocalState() {
        p.log.push("drop");
      },
      async publishDirectoryKeyPackage() {
        p.log.push("directory-kp");
      },
      async loadState() {
        return p.state;
      },
      async saveState(s: RejoinState) {
        p.log.push("save");
        p.state = s;
      },
      async prepareKnock(at: number) {
        if (options.noUsername === true) return null;
        return {
          state: { v: 1 as const, rendezvousB64: `r${at}`, joinerSnapshotB64: "s", knockedAtMs: at },
          payload: new Uint8Array([9]),
        };
      },
      watch(_s: RejoinState) {
        p.log.push("watch");
        // Answers already out (a resumed knock) are readable at once.
        const base = sentAt ?? clock.now;
        const w = scriptedWatch(
          clock,
          p.script.map((a) => ({ atMs: base + rtt + a.afterMs, answer: a.answer })),
        );
        p.watches.push(w);
        return w;
      },
      async postKnock() {
        p.log.push("knock");
        sentAt = clock.now;
        clock.now += rtt;
      },
      async join() {
        p.log.push("join");
      },
      async found() {
        p.log.push("found");
      },
      async clearState() {
        p.log.push("clear");
        p.state = null;
      },
      noteAnswer(ms: number | null, kind: string | null) {
        p.noted.push({ ms, kind });
      },
      now: () => clock.now,
    };
    return p;
  }

  it("opens the rendezvous read before it posts the knock", async () => {
    const p = ports([{ afterMs: 50, answer: WELCOME }]);
    expect(await rejoinOnce(p, true)).toBe("joined");
    expect(p.log).toEqual(["drop", "directory-kp", "save", "watch", "knock", "join", "clear"]);
    expect(p.watches.every((w) => w.closed)).toBe(true);
  });

  it("a presence note inside the window: waits for the Welcome however long it takes", async () => {
    const p = ports([
      { afterMs: 300, answer: PRESENT },
      { afterMs: 90_000, answer: WELCOME },
    ]);
    expect(await rejoinOnce(p, true)).toBe("joined");
    expect(p.log).not.toContain("found");
    expect(p.noted).toEqual([{ ms: 100 + 300, kind: "present" }]);
  });

  it("silence for max(4 x rtt, 1 s): founds and starts, never waits for a device that is not there", async () => {
    const p = ports([]);
    expect(await rejoinOnce(p, true)).toBe("founded");
    expect(p.log).toEqual(["drop", "directory-kp", "save", "watch", "knock", "found", "clear"]);
    expect(p.noted).toEqual([{ ms: null, kind: null }]);
    // Knock sent at 1 000, back at 1 100, window 1 s: founding at 2 100.
    expect(p.clock.now).toBe(2_100);
  });

  it("the window grows with a slow knock: an answer after 4 round trips still counts", async () => {
    // rtt 2 s: window 8 s; the presence note at 7.5 s after the knock came back.
    const slow = ports([{ afterMs: 7_500, answer: PRESENT }, { afterMs: 9_000, answer: WELCOME }], { rttMs: 2_000 });
    expect(await rejoinOnce(slow, true)).toBe("joined");
    // The same note 1.5 s after a fast knock is too late: it founds.
    const fast = ports([{ afterMs: 1_500, answer: PRESENT }], { rttMs: 100 });
    expect(await rejoinOnce(fast, true)).toBe("founded");
  });

  it("a presence note and then no Welcome before the re-knock time: waits, then knocks again", async () => {
    const p = ports([{ afterMs: 10, answer: PRESENT }]);
    expect(await rejoinOnce(p, true)).toBe("waiting");
    expect(p.log).not.toContain("found");
    expect(p.clock.now).toBe(1_000 + REKNOCK_AFTER_MS);
    // The next pass knocks afresh; nobody answers this time, so it founds.
    p.script = [];
    expect(await rejoinOnce(p, false)).toBe("founded");
    expect(p.log.filter((l) => l === "knock")).toHaveLength(2);
  });

  it("a reload resumes a knock already answered, and knocks again when it was not", async () => {
    const answered = ports([{ afterMs: -200, answer: PRESENT }, { afterMs: 500, answer: WELCOME }]);
    answered.state = { v: 1, rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000 };
    expect(await rejoinOnce(answered, false)).toBe("joined");
    expect(answered.log).toEqual(["watch", "join", "clear"]);

    const unanswered = ports([]);
    unanswered.state = { v: 1, rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000 };
    expect(await rejoinOnce(unanswered, false)).toBe("founded");
    expect(unanswered.log).toEqual(["watch", "save", "watch", "knock", "found", "clear"]);
  });

  it("an account with no name a sibling could match: neither knocks nor founds, and knocks once a name is known", async () => {
    // 2026-11-15, seen on an iPhone: a device with no login handle while
    // another device of the account was listed founded a second DeviceGroup.
    const p = ports([], { noUsername: true });
    expect(await rejoinOnce(p, true)).toBe("no-handle");
    expect(p.log).toEqual(["drop", "directory-kp"]);
    expect(await rejoinOnce(p, false)).toBe("no-handle");
    expect(p.log).toEqual(["drop", "directory-kp"]);
    expect(p.log).not.toContain("found");
    // The name became known (a register arrived): the next pass knocks.
    p.prepareKnock = ports([]).prepareKnock;
    p.script = [{ afterMs: 50, answer: WELCOME }];
    expect(await rejoinOnce(p, false)).toBe("joined");
    expect(p.log).toEqual(["drop", "directory-kp", "save", "watch", "knock", "join", "clear"]);
  });

  it("a name learned while it waits (the person typed it) wakes the loop: the next pass knocks at once", async () => {
    // docs/ux/identity-recovery.md section 8b: the loop is the one
    // `DeviceGroupSyncContext` runs, its wait a `NoHandleWaker` the session's
    // name ends; the name is read afresh on every pass.
    let username = "";
    const p = ports([], { noUsername: true });
    const withoutName = p.prepareKnock;
    const withName = ports([]).prepareKnock;
    p.prepareKnock = (nowMs) => (username === "" ? withoutName(nowMs) : withName(nowMs));
    p.script = [{ afterMs: 50, answer: WELCOME }];
    const waker = createNoHandleWaker();
    const abort = new AbortController();
    const outcomes: string[] = [];
    const loop = (async (): Promise<void> => {
      let first = true;
      for (;;) {
        const outcome = await rejoinOnce(p, first);
        first = false;
        outcomes.push(outcome);
        if (outcome !== "no-handle") return;
        await waker.sleep(NO_HANDLE_RETRY_MS, abort.signal);
      }
    })();
    for (let i = 0; i < 50 && outcomes.length === 0; i++) await Promise.resolve();
    expect(outcomes).toEqual(["no-handle"]);
    username = "anna";
    waker.wake();
    // Well before NO_HANDLE_RETRY_MS: the wait ended on the wake.
    const raced = await Promise.race([
      loop.then(() => "done"),
      new Promise<string>((resolve) => setTimeout(() => resolve("still waiting"), 2_000)),
    ]);
    abort.abort();
    expect(raced).toBe("done");
    expect(outcomes).toEqual(["no-handle", "joined"]);
    expect(p.log).toContain("knock");
    expect(p.log).not.toContain("found");
  });

  it("the no-handle wait ends on a wake, on an abort, or after its time, and a wake with nobody waiting is not kept", async () => {
    const waker = createNoHandleWaker();
    waker.wake();
    const abort = new AbortController();
    let ended = false;
    const waiting = waker.sleep(60_000, abort.signal).then(() => {
      ended = true;
    });
    await Promise.resolve();
    expect(ended).toBe(false);
    waker.wake();
    await waiting;
    expect(ended).toBe(true);

    const aborted = new AbortController();
    const second = waker.sleep(60_000, aborted.signal);
    aborted.abort();
    await second;

    const timed = waker.sleep(5, new AbortController().signal);
    await timed;
  });

  it("a Welcome it cannot join from clears the knock, so the next pass knocks afresh", async () => {
    const p = ports([{ afterMs: 10, answer: WELCOME }]);
    p.join = async () => {
      throw new Error("bad welcome");
    };
    await expect(rejoinOnce(p, true)).rejects.toThrow("bad welcome");
    expect(p.state).toBeNull();
  });

  it("the re-knock rule and the state codec", () => {
    const s: RejoinState = { v: 1, rendezvousB64: "r", joinerSnapshotB64: "s", knockedAtMs: 10 };
    expect(shouldKnockAgain(null, 0)).toBe(true);
    expect(shouldKnockAgain(s, 10 + REKNOCK_AFTER_MS - 1)).toBe(false);
    expect(shouldKnockAgain(s, 10 + REKNOCK_AFTER_MS)).toBe(true);
    expect(shouldKnockAgain(s, 5)).toBe(true); // a clock that went back
    expect(decodeRejoinState(encodeRejoinState(s))).toEqual(s);
    expect(decodeRejoinState(new TextEncoder().encode("[]"))).toBeNull();
  });
});
