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
  callOwnsCaptureNow,
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
    callHoldsCapture(callIsLive, unansweredIncomingRing) {
      asked.push({ callIsLive, unansweredIncomingRing });
      return answer;
    },
  };
}

function host(lane: string | null, ringingCallIdHex: string | null = null): CaptureCallLane {
  return { callLaneGroupIdB64: () => lane, ringingCallIdHex: () => ringingCallIdHex };
}

describe("callOwnsCaptureNow", () => {
  it("tells the rule a ringing incoming call holds nothing", () => {
    const rules = spy(false);
    // The host reports both: the ring took the one call slot, AND the 1x1
    // call is in `ringing`.
    expect(callOwnsCaptureNow(rules, host(GROUP, "c0ffee"))).toBe(false);
    expect(rules.asked).toEqual([{ callIsLive: true, unansweredIncomingRing: true }]);
  });

  it("tells the rule a live call holds capture", () => {
    const rules = spy(true);
    expect(callOwnsCaptureNow(rules, host(GROUP))).toBe(true);
    expect(rules.asked).toEqual([{ callIsLive: true, unansweredIncomingRing: false }]);
  });

  it("reports no call at all when the slot is empty", () => {
    const empty = spy(false);
    expect(callOwnsCaptureNow(empty, host(null))).toBe(false);
    expect(empty.asked).toEqual([{ callIsLive: false, unansweredIncomingRing: false }]);
  });

  it("still blocks this chat's recorder while a call runs in another chat", () => {
    // `docs/ux/calls.md` section 4.5: a live call collapses into a bar and
    // survives every navigation, so a person can stand in chat B with a call
    // placed from chat A. The microphone is one device-wide resource and is
    // just as busy here. The earlier reading - "a call in THIS chat" - would
    // have answered `false` and handed the microphone to a second taker.
    const elsewhere = spy(true);
    expect(callOwnsCaptureNow(elsewhere, host(OTHER_GROUP))).toBe(true);
    expect(elsewhere.asked).toEqual([{ callIsLive: true, unansweredIncomingRing: false }]);
  });

  it("answers with the rule's verdict and never its own", () => {
    // A ringing call IS a call in the slot, so anything re-deciding here from
    // the slot read alone would answer `true` for the first of these - which is
    // precisely the defect.
    expect(callOwnsCaptureNow(spy(false), host(GROUP, "c0ffee"))).toBe(false);
    expect(callOwnsCaptureNow(spy(true), host(GROUP))).toBe(true);
  });

  it("falls back to the call-slot read until the rules have loaded", () => {
    // The pre-rule behaviour, which refuses rather than recording over a live
    // call. Theoretical in the running app; a fallback guessing the other way
    // would be a silent regression on a slow wasm load.
    expect(callOwnsCaptureNow(null, host(GROUP, "c0ffee"))).toBe(true);
    expect(callOwnsCaptureNow(null, host(OTHER_GROUP))).toBe(true);
    expect(callOwnsCaptureNow(null, host(null))).toBe(false);
  });
});

describe("callHoldsCapture", () => {
  it("hands both facts straight through in order", () => {
    const rules = spy(false);
    callHoldsCapture(rules, { callIsLive: false, unansweredIncomingRing: true });
    expect(rules.asked).toEqual([{ callIsLive: false, unansweredIncomingRing: true }]);
  });
});
