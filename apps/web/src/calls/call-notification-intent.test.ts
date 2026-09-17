// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Accept and Decline pressed on a ringing notification, as the page reads
 * them.
 *
 * Two routes carry the press — a `postMessage` to an open tab, and the query
 * of a window the worker opened — and they must produce the same value and
 * obey the same rules, because a user cannot tell which one they used.
 *
 * The rule most of these cases exist for is the one that makes the untrusted
 * input harmless: an intent may only accept or decline a call ALREADY RINGING
 * on this device with exactly that call id. Anyone can hand a user a
 * `?callAction=accept` link, so this is a boundary, not a nicety.
 */
import { describe, expect, it } from "vitest";

import {
  CALL_ACTION_MAX_WAIT_MS,
  CALL_ACTION_MESSAGE,
  decideCallAction,
  parseCallActionMessage,
  readCallActionParams,
  stripCallActionParams,
  type CallNotificationIntent,
} from "./call-notification-intent";

const CALL_ID = "11".repeat(16);
const OTHER_CALL_ID = "22".repeat(16);
const GROUP = "aa".repeat(32);

const accept: CallNotificationIntent = {
  kind: "accept",
  callIdHex: CALL_ID,
  groupHex: GROUP,
  video: false,
};
const decline: CallNotificationIntent = { ...accept, kind: "decline" };

const ringing = (callIdHex: string | null, waitedMs = 0) => ({
  ready: true,
  ringingCallIdHex: callIdHex,
  waitedMs,
});

describe("reading an intent from an open tab", () => {
  it("reads a well-formed action", () => {
    expect(
      parseCallActionMessage({ type: CALL_ACTION_MESSAGE, intent: accept }),
    ).toEqual(accept);
  });

  it("ignores the worker's other messages", () => {
    // Neither the arrival broadcast nor the navigate-only tap may answer a
    // call: only an explicit press of a button can.
    expect(parseCallActionMessage({ type: "LAKKO_PUSH_WAKE", wake: {} })).toBeNull();
    expect(
      parseCallActionMessage({ type: "LAKKO_NOTIFICATION_TAP", groupHex: GROUP }),
    ).toBeNull();
    expect(parseCallActionMessage(null)).toBeNull();
    expect(parseCallActionMessage("accept")).toBeNull();
  });

  it("refuses a malformed intent rather than half-trusting it", () => {
    const bad = [
      { ...accept, kind: "hang-up" },
      { ...accept, callIdHex: "not hex at all!!!!!!!!!!!!!!!!!!!" },
      { ...accept, callIdHex: "11" },
      { ...accept, callIdHex: 42 },
      {},
      null,
    ];
    for (const intent of bad) {
      expect(parseCallActionMessage({ type: CALL_ACTION_MESSAGE, intent })).toBeNull();
    }
  });

  it("normalises the hex case a hand-copied value may carry", () => {
    const upper = { ...accept, callIdHex: CALL_ID.toUpperCase() };
    expect(
      parseCallActionMessage({ type: CALL_ACTION_MESSAGE, intent: upper })?.callIdHex,
    ).toBe(CALL_ID);
  });
});

describe("reading an intent from a cold-open URL", () => {
  it("reads the same value the message route produces", () => {
    const search = `?wake=${GROUP}&callAction=accept&callId=${CALL_ID}`;
    expect(readCallActionParams(search)).toEqual(accept);
  });

  it("carries the video flag", () => {
    const search = `?wake=${GROUP}&callAction=accept&callId=${CALL_ID}&callVideo=1`;
    expect(readCallActionParams(search)?.video).toBe(true);
  });

  it("is null when the query carries no action", () => {
    expect(readCallActionParams(`?wake=${GROUP}`)).toBeNull();
    expect(readCallActionParams("")).toBeNull();
  });

  it("refuses an action with no usable call id", () => {
    // Without a call id nothing can be matched, and acting on "whatever is in
    // the slot" is exactly the behaviour the id check exists to prevent.
    expect(readCallActionParams("?callAction=accept")).toBeNull();
    expect(readCallActionParams("?callAction=accept&callId=zz")).toBeNull();
  });

  it("strips its own parameters so a refresh cannot re-answer", () => {
    const stripped = stripCallActionParams(
      `https://lakko.me/?wake=${GROUP}&callAction=accept&callId=${CALL_ID}&callVideo=1`,
    );
    expect(stripped).toBe(`/?wake=${GROUP}`);
    // `?wake=` is left for `push/wake-target.ts`, which owns it.
    expect(readCallActionParams(stripped!.slice(1))).toBeNull();
  });

  it("leaves unrelated query and hash alone", () => {
    expect(
      stripCallActionParams(`https://lakko.me/x?a=1&callAction=decline&callId=${CALL_ID}#h`),
    ).toBe("/x?a=1#h");
  });

  it("returns null rather than a guess for an unparseable href", () => {
    expect(stripCallActionParams("not a url")).toBeNull();
  });
});

describe("deciding what to do with a pending intent", () => {
  it("accepts the call the notification actually named", () => {
    expect(decideCallAction(accept, ringing(CALL_ID))).toEqual({
      kind: "accept",
      withVideo: false,
    });
  });

  it("answers a video call in kind", () => {
    // The notification offers ONE Accept, so it answers the call the caller
    // placed. "Accept without video" lives on the full-screen screen.
    expect(decideCallAction({ ...accept, video: true }, ringing(CALL_ID))).toEqual({
      kind: "accept",
      withVideo: true,
    });
  });

  it("declines the call the notification named", () => {
    expect(decideCallAction(decline, ringing(CALL_ID))).toEqual({ kind: "decline" });
  });

  it("NEVER touches a different call", () => {
    // The boundary: a hostile `?callAction=` link must not answer or refuse
    // the call the user is actually being offered.
    expect(decideCallAction(accept, ringing(OTHER_CALL_ID))).toEqual({ kind: "drop" });
    expect(decideCallAction(decline, ringing(OTHER_CALL_ID))).toEqual({ kind: "drop" });
  });

  it("waits while the app is still booting", () => {
    // A cold open races its own restore; nothing is ringing yet because the
    // journal has not been polled.
    expect(
      decideCallAction(accept, { ready: false, ringingCallIdHex: null, waitedMs: 0 }),
    ).toEqual({ kind: "wait" });
    expect(decideCallAction(accept, ringing(null))).toEqual({ kind: "wait" });
  });

  it("gives up once the call could no longer be ringing", () => {
    // Bounded by the 45 s ring timeout, which this module reads and does not
    // change: past it nothing can match, so holding the intent would only
    // risk answering a LATER call.
    expect(decideCallAction(accept, ringing(null, CALL_ACTION_MAX_WAIT_MS + 1))).toEqual({
      kind: "drop",
    });
    expect(
      decideCallAction(accept, ringing(CALL_ID, CALL_ACTION_MAX_WAIT_MS + 1)),
    ).toEqual({ kind: "drop" });
  });

  it("does nothing for a body click", () => {
    // Raising the app and landing on the chat are the worker's and
    // `wake-target.ts`'s jobs; the incoming call screen is already app-wide.
    expect(decideCallAction({ ...accept, kind: "open" }, ringing(CALL_ID))).toEqual({
      kind: "drop",
    });
  });

  it("does nothing without an intent", () => {
    expect(decideCallAction(null, ringing(CALL_ID))).toEqual({ kind: "drop" });
  });
});
