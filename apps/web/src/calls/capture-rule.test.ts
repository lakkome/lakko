// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` — the composer's recorder stands aside while a
 * call HOLDS the microphone and camera, and a ringing offer holds neither.
 *
 * ## What these tests prove, and what they deliberately do not
 *
 * The rule is `crates/lakko-ui-rules/src/capture.rs` and is asserted there,
 * including that a ringing offer frees the recorder and an answered call does
 * not. Restating it here would be a second copy of the paragraph the shared
 * crate exists to have one of, and a copy that drifted would keep passing.
 *
 * So these tests cover the WIRING, which is where the defect lived: the two
 * facts this client reads off the call host, and that the rule's answer comes
 * back untouched. The outcome each pair of facts produces is the rule's.
 */
import { describe, expect, it } from "vitest";

import {
  callHoldsCapture,
  callOwnsCaptureIn,
  type CaptureCallLane,
  type CaptureFacts,
  type CaptureRules,
} from "./capture-rule";

const GROUP = "grp-anna";
const OTHER_GROUP = "grp-team";

/** A rule that records what it was asked and answers whatever it is told to. */
function spy(answer: boolean): CaptureRules & { asked: CaptureFacts[] } {
  const asked: CaptureFacts[] = [];
  return {
    asked,
    callHoldsCapture(callInThisChat, unansweredIncomingRing) {
      asked.push({ callInThisChat, unansweredIncomingRing });
      return answer;
    },
  };
}

function host(lane: string | null, ringingCallIdHex: string | null = null): CaptureCallLane {
  return { callLaneGroupIdB64: () => lane, ringingCallIdHex: () => ringingCallIdHex };
}

describe("callOwnsCaptureIn", () => {
  it("tells the rule a ringing incoming call in this chat holds nothing", () => {
    const rules = spy(false);
    // The host reports both: the ring took the call slot, so this chat's lane
    // is occupied, AND the 1x1 call is in `ringing`.
    expect(callOwnsCaptureIn(rules, host(GROUP, "c0ffee"), GROUP)).toBe(false);
    expect(rules.asked).toEqual([{ callInThisChat: true, unansweredIncomingRing: true }]);
  });

  it("tells the rule a live call in this chat holds capture", () => {
    const rules = spy(true);
    expect(callOwnsCaptureIn(rules, host(GROUP), GROUP)).toBe(true);
    expect(rules.asked).toEqual([{ callInThisChat: true, unansweredIncomingRing: false }]);
  });

  it("reports no call at all when the lane is empty or belongs to another chat", () => {
    const empty = spy(false);
    expect(callOwnsCaptureIn(empty, host(null), GROUP)).toBe(false);
    expect(empty.asked).toEqual([{ callInThisChat: false, unansweredIncomingRing: false }]);

    // A call live in a different conversation: not this chat's business, and
    // the rule answers from the first fact alone.
    const elsewhere = spy(false);
    expect(callOwnsCaptureIn(elsewhere, host(OTHER_GROUP), GROUP)).toBe(false);
    expect(elsewhere.asked).toEqual([
      { callInThisChat: false, unansweredIncomingRing: false },
    ]);
  });

  it("answers with the rule's verdict and never its own", () => {
    // A ringing call IS a call in the lane, so anything re-deciding here from
    // the lane read alone would answer `true` for the first of these - which is
    // precisely the defect.
    expect(callOwnsCaptureIn(spy(false), host(GROUP, "c0ffee"), GROUP)).toBe(false);
    expect(callOwnsCaptureIn(spy(true), host(GROUP), GROUP)).toBe(true);
  });

  it("falls back to the call-lane read until the rules have loaded", () => {
    // The pre-rule behaviour, which refuses rather than recording over a live
    // call. Theoretical in the running app; a fallback guessing the other way
    // would be a silent regression on a slow wasm load.
    expect(callOwnsCaptureIn(null, host(GROUP, "c0ffee"), GROUP)).toBe(true);
    expect(callOwnsCaptureIn(null, host(null), GROUP)).toBe(false);
  });
});

describe("callHoldsCapture", () => {
  it("hands both facts straight through in order", () => {
    const rules = spy(false);
    callHoldsCapture(rules, { callInThisChat: false, unansweredIncomingRing: true });
    expect(rules.asked).toEqual([{ callInThisChat: false, unansweredIncomingRing: true }]);
  });
});
