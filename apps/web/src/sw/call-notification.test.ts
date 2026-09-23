// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two decisions a ringing web notification embodies
 * (`public/sw-call-notification.js`): what it looks like, and what each of its
 * buttons does.
 *
 * Both are pure, which is why they live outside `sw.js` — the service worker
 * is copied verbatim into `dist/` and is unreachable from vitest, so anything
 * decided inside it is untestable by construction. `docs/ux/calls.md` §2.1
 * fixes the web behaviour ("a notification with Accept and Decline; clicking
 * it focuses the tab and opens the call") and §2.2 fixes the wording, so these
 * cases are the spec read back.
 *
 * The one rule NOT pinned here is `sw.js`'s inline `data.call.intents[action]`
 * lookup, which cannot import this module without risking the user activation
 * `openWindow` needs. `intentForAction` is its canonical definition and the
 * round-trip case below is what keeps the two honest.
 */
import { describe, expect, it } from "vitest";

import {
  CALL_ACTION_ACCEPT,
  CALL_ACTION_BODY,
  CALL_ACTION_DECLINE,
  callClickPlan,
  callIntentUrl,
  callNotificationIntents,
  callNotificationTag,
  callRingNotification,
  callSilentNotification,
  intentForAction,
  isCallNotificationTag,
  // @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
} from "../../public/sw-call-notification.js";

const CALL = {
  callIdHex: "11".repeat(16),
  groupHex: "aa".repeat(32),
  video: false,
};

describe("the ringing notification's shape", () => {
  it("says which kind of call it is, in the caller's words", () => {
    // UX §2.2: one line saying `Voice call` or `Video call`, and §1: "the word
    // on the callee's screen matches what the caller chose, always" — so the
    // flag comes off the offer, never from anything local.
    expect(callRingNotification({ ...CALL, callerName: "Anna" }).options.body).toBe(
      "Voice call",
    );
    expect(
      callRingNotification({ ...CALL, video: true, callerName: "Anna" }).options.body,
    ).toBe("Video call");
  });

  it("titles the ring with the caller's name", () => {
    expect(callRingNotification({ ...CALL, callerName: "Anna" }).title).toBe("Anna");
  });

  it("never invents a caller", () => {
    // An unknown sender keeps the generic title — the same fallback the
    // message path uses. Naming the wrong person on a call screen is worse
    // than naming nobody.
    for (const name of [null, undefined, ""]) {
      expect(callRingNotification({ ...CALL, callerName: name }).title).toBe("Lakko");
    }
  });

  it("stays on screen until the user acts", () => {
    // A ring that auto-dismisses after a few seconds IS a missed call.
    const { options } = callRingNotification({ ...CALL, callerName: "Anna" });
    expect(options.requireInteraction).toBe(true);
    expect(Array.isArray(options.vibrate)).toBe(true);
    expect((options.vibrate as number[]).length).toBeGreaterThan(1);
  });

  it("offers exactly Accept and Decline", () => {
    const { options } = callRingNotification({ ...CALL, callerName: "Anna" });
    expect(options.actions.map((a: { action: string }) => a.action)).toEqual([
      CALL_ACTION_ACCEPT,
      CALL_ACTION_DECLINE,
    ]);
    expect(options.actions.map((a: { title: string }) => a.title)).toEqual([
      "Accept",
      "Decline",
    ]);
  });

  it("tags on the CALL, so a repeat replaces rather than stacks", () => {
    const { options } = callRingNotification({ ...CALL, callerName: "Anna" });
    expect(options.tag).toBe(callNotificationTag(CALL.callIdHex));
    // Two wakes for the same call collapse; two different calls do not.
    expect(callNotificationTag("aa")).not.toBe(callNotificationTag("bb"));
    // ...and a ring's tag is never a message tag (which is the group hex), so
    // cancelling a ring cannot close a chat notification.
    expect(isCallNotificationTag(options.tag)).toBe(true);
    expect(isCallNotificationTag(CALL.groupHex)).toBe(false);
    // A replacement must still re-alert: an unanswered ring is still asking.
    expect(options.renotify).toBe(true);
  });

  it("carries the per-button meaning with it", () => {
    const { options } = callRingNotification({ ...CALL, callerName: "Anna" });
    expect(options.data.call.callIdHex).toBe(CALL.callIdHex);
    expect(options.data.call.intents[CALL_ACTION_ACCEPT].kind).toBe("accept");
  });
});

describe("action to intent", () => {
  it("maps each button, and a body click, to its own meaning", () => {
    const intents = callNotificationIntents(CALL);
    expect(intents[CALL_ACTION_ACCEPT].kind).toBe("accept");
    expect(intents[CALL_ACTION_DECLINE].kind).toBe("decline");
    // A click on the body is NOT an answer — it shows the user who is calling
    // and lets them decide.
    expect(intents[CALL_ACTION_BODY].kind).toBe("open");
  });

  it("every intent names the call and the group it belongs to", () => {
    const intents = callNotificationIntents({ ...CALL, video: true });
    for (const action of [CALL_ACTION_BODY, CALL_ACTION_ACCEPT, CALL_ACTION_DECLINE]) {
      expect(intents[action].callIdHex).toBe(CALL.callIdHex);
      expect(intents[action].groupHex).toBe(CALL.groupHex);
      expect(intents[action].video).toBe(true);
    }
  });

  it("round-trips through the notification data the click handler reads", () => {
    // This is the case that keeps `sw.js`'s inline lookup honest: the key set
    // this builds is exactly the `event.action` values the browser reports.
    const { options } = callRingNotification({ ...CALL, callerName: "Anna" });
    for (const action of [CALL_ACTION_BODY, CALL_ACTION_ACCEPT, CALL_ACTION_DECLINE]) {
      expect(intentForAction(action, options.data)).toEqual(
        options.data.call.intents[action],
      );
      // ...and the inline expression `sw.js` actually runs agrees.
      expect(options.data.call.intents[action]).toBeTruthy();
    }
  });

  it("is null for a notification that is not a ring", () => {
    // A message notification's data carries `wake` / `tapGroupHex` and no
    // `call`, so the click handler falls through to the pre-existing tap path.
    expect(intentForAction(CALL_ACTION_BODY, { wake: {}, tapGroupHex: "aa" })).toBeNull();
    expect(intentForAction(CALL_ACTION_BODY, {})).toBeNull();
    expect(intentForAction(CALL_ACTION_BODY, undefined)).toBeNull();
    expect(intentForAction("something-else", { call: { intents: {} } })).toBeNull();
  });
});

describe("what a click should actually do", () => {
  const intents = callNotificationIntents(CALL);

  it("declining never raises or opens a window when a page exists", () => {
    // Refusing a call is a request to be left alone. A page is still needed to
    // publish the decline (the worker is read-only, C-59), so it is told —
    // but not raised.
    const plan = callClickPlan(intents[CALL_ACTION_DECLINE], true);
    expect(plan).toEqual({ post: true, focus: false, openUrl: null });
  });

  it("declining with no page at all has to open one", () => {
    const plan = callClickPlan(intents[CALL_ACTION_DECLINE], false);
    expect(plan.post).toBe(false);
    expect(plan.focus).toBe(false);
    expect(plan.openUrl).toContain("callAction=decline");
  });

  it("accepting raises the existing page", () => {
    expect(callClickPlan(intents[CALL_ACTION_ACCEPT], true)).toEqual({
      post: true,
      focus: true,
      openUrl: null,
    });
  });

  it("accepting with no page opens one that joins the call", () => {
    const plan = callClickPlan(intents[CALL_ACTION_ACCEPT], false);
    expect(plan.openUrl).toContain("callAction=accept");
    expect(plan.openUrl).toContain(`callId=${CALL.callIdHex}`);
    // ...and carries the pre-existing deep link, so the user also lands in
    // the right chat.
    expect(plan.openUrl).toContain(`wake=${CALL.groupHex}`);
  });

  it("a body click raises without answering", () => {
    expect(callClickPlan(intents[CALL_ACTION_BODY], true)).toEqual({
      post: true,
      focus: true,
      openUrl: null,
    });
    // Cold: the wake deep link only — nothing that would answer the call.
    const cold = callClickPlan(intents[CALL_ACTION_BODY], false);
    expect(cold.openUrl).toBe(`/?wake=${CALL.groupHex}`);
    expect(cold.openUrl).not.toContain("callAction");
  });

  it("does nothing at all for a missing intent", () => {
    expect(callClickPlan(null, true)).toEqual({ post: false, focus: false, openUrl: null });
    expect(callClickPlan(undefined, false)).toEqual({
      post: false,
      focus: false,
      openUrl: null,
    });
  });

  it("carries the video flag into a cold-open accept", () => {
    const videoIntents = callNotificationIntents({ ...CALL, video: true });
    expect(callIntentUrl(videoIntents[CALL_ACTION_ACCEPT])).toContain("callVideo=1");
    expect(callIntentUrl(intents[CALL_ACTION_ACCEPT])).not.toContain("callVideo");
  });
});

/** `docs/ux/calls.md` section 2.1 — "a muted chat never rings". */
describe("the muted conversation's notification", () => {
  const silent = callSilentNotification({ ...CALL, callerName: "Anna" });
  const ring = callRingNotification({ ...CALL, callerName: "Anna" });

  it("says the same thing a ring says", () => {
    // Mute withholds the interruption, not the fact. The person still learns
    // who called and what kind of call it was.
    expect(silent.title).toBe(ring.title);
    expect(silent.options.body).toBe(ring.options.body);
  });

  it("withholds every part of the interruption", () => {
    expect(silent.options.silent).toBe(true);
    expect(silent.options.vibrate).toBeUndefined();
    expect(silent.options.requireInteraction).toBe(false);
    expect(silent.options.actions).toBeUndefined();
    // A repeat wake for the same call must not re-alert, which is exactly
    // what a ring DOES do.
    expect(silent.options.renotify).toBe(false);
    expect(ring.options.renotify).toBe(true);
  });

  it("shares the ring's tag, so one end wake takes either shape down", () => {
    expect(silent.options.tag).toBe(callNotificationTag(CALL.callIdHex));
  });

  it("offers no way to answer or refuse from the notification", () => {
    // There are no buttons, and the one intent it carries can never act on a
    // call: `open` raises the app and nothing more.
    expect(intentForAction(CALL_ACTION_BODY, silent.options.data).kind).toBe("open");
    expect(intentForAction(CALL_ACTION_ACCEPT, silent.options.data)).toBeNull();
    expect(intentForAction(CALL_ACTION_DECLINE, silent.options.data)).toBeNull();
  });

  it("names the call it belongs to, so the app can find it", () => {
    expect(silent.options.data.call.callIdHex).toBe(CALL.callIdHex);
    expect(silent.options.data.call.groupHex).toBe(CALL.groupHex);
  });
});
