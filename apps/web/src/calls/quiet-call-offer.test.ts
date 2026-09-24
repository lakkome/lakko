// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/calls.md` section 2.1.1 — a muted conversation's call is OFFERED,
 * quietly, and stays answerable for the whole ring window.
 *
 * ## What changed, and why these tests exist
 *
 * The first pass read "a muted chat never rings" as "show nothing at all". On
 * this client that made a muted contact unanswerable, because the full-screen
 * incoming call was the only Accept there was and it is precisely the surface
 * mute withholds. The spec now rejects that reading: what mute withholds is
 * the interruption, and the call is offered on two quiet surfaces instead.
 *
 * Both of those surfaces are JSX, and vitest here runs bare node — no jsdom,
 * no Solid transform — so what they show has to be decided in a plain module
 * for a test to reach it at all.
 */
import { describe, expect, it } from "vitest";

import {
  quietCallKindLabel,
  quietCallOffer,
  quietCallOfferFor,
  quietCallOfferIsLive,
  type QuietCallOffer,
} from "./quiet-call-offer";

const NOW = Date.parse("2026-09-23T12:00:00.000Z");
const GROUP = "grp-anna";
const OTHER_GROUP = "grp-team";

function offer(over: Partial<QuietCallOffer> = {}): QuietCallOffer {
  return {
    groupIdB64: GROUP,
    callIdHex: "c0ffee",
    video: false,
    expiresAtMs: NOW + 30_000,
    ...over,
  };
}

describe("the quiet offer a muted call leaves behind", () => {
  it("is offered while the call is ringing", () => {
    // The whole point of the change: something to answer, rather than nothing.
    expect(quietCallOffer({ ringing: true, taken: offer(), nowMs: NOW })).not.toBeNull();
  });

  it("is nothing while an ordinary unmuted call rings", () => {
    // An unmuted call takes the screen (section 2.2); a second, quiet copy of
    // it in the chat and on its row would be the interruption plus clutter.
    expect(quietCallOffer({ ringing: true, taken: null, nowMs: NOW })).toBeNull();
  });

  it("is gone the moment the slot stops ringing", () => {
    // Answered, declined, or given up: all three leave `ringing`, and none of
    // them should leave an Accept button behind.
    expect(quietCallOffer({ ringing: false, taken: offer(), nowMs: NOW })).toBeNull();
  });

  it("stops being offered when the ring window runs out", () => {
    // Section 2.1.1: "Both disappear when the ring window runs out."
    const o = offer({ expiresAtMs: NOW + 1 });
    expect(quietCallOffer({ ringing: true, taken: o, nowMs: NOW })).not.toBeNull();
    expect(quietCallOffer({ ringing: true, taken: o, nowMs: NOW + 1 })).toBeNull();
    expect(quietCallOffer({ ringing: true, taken: o, nowMs: NOW + 60_000 })).toBeNull();
  });

  it("puts the deadline where the shared rule puts it", () => {
    // Exclusive at the window, the same boundary `ring_decision` draws: at the
    // deadline the caller has given up, so an answer here would connect to
    // nobody.
    const o = offer({ expiresAtMs: NOW });
    expect(quietCallOfferIsLive(o, NOW - 1)).toBe(true);
    expect(quietCallOfferIsLive(o, NOW)).toBe(false);
  });

  it("survives a clock that reads earlier than the offer", () => {
    // A deadline is a comparison, not arithmetic on a duration: a reading from
    // before the offer arrived is simply still inside the window.
    expect(quietCallOfferIsLive(offer(), NOW - 10_000)).toBe(true);
  });
});

describe("which conversation offers it", () => {
  it("is offered by the conversation the call arrived in", () => {
    expect(quietCallOfferFor(offer(), GROUP, NOW)?.callIdHex).toBe("c0ffee");
  });

  it("is offered by no other conversation", () => {
    // Section 2.1.1 names the muted conversation's OWN panel and its OWN row.
    // A second chat growing an Accept for a call that is nothing to do with it
    // would be the full-screen takeover again, wearing a smaller coat.
    expect(quietCallOfferFor(offer(), OTHER_GROUP, NOW)).toBeNull();
  });

  it("is offered by nobody when there is no quiet call", () => {
    expect(quietCallOfferFor(null, GROUP, NOW)).toBeNull();
  });

  it("is bounded by the window on every surface, not only on the first", () => {
    // The two surfaces are two components; the window has to bind both, so
    // both ask this and neither re-derives it.
    const o = offer({ expiresAtMs: NOW });
    expect(quietCallOfferFor(o, GROUP, NOW)).toBeNull();
  });
});

describe("what the surfaces say about the call", () => {
  it("says which kind of call it is, in the words the call screen uses", () => {
    expect(quietCallKindLabel(false)).toBe("Voice call");
    expect(quietCallKindLabel(true)).toBe("Video call");
  });

  it("carries the caller's choice through rather than guessing it", () => {
    // Section 1 — "the word on the callee's screen matches what the caller
    // chose, always", which holds for the quiet surfaces too.
    expect(quietCallOfferFor(offer({ video: true }), GROUP, NOW)?.video).toBe(true);
    expect(quietCallOfferFor(offer({ video: false }), GROUP, NOW)?.video).toBe(false);
  });

  it("names the call, so a surface acts on that call and not on whatever is ringing", () => {
    expect(quietCallOffer({ ringing: true, taken: offer(), nowMs: NOW })?.callIdHex).toBe(
      "c0ffee",
    );
  });
});
