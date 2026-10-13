// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  INVITE_IDLE,
  INVITE_STATUS_HOLD_MS,
  type InCallInvite,
  copyCallLink,
  inviteBusy,
  inviteStatusHoldMs,
  inviteStatusLine,
  mintedInviteFate,
  mintedInviteUrl,
  nextInvitePress,
} from "./in-call-invite";

const URL_ = "https://lakko.me/c/AAAA";

describe("nextInvitePress", () => {
  it("mints once and then re-offers, so a second press costs no round trip", () => {
    expect(nextInvitePress(INVITE_IDLE)).toBe("mint");
    expect(nextInvitePress({ kind: "ready", url: URL_, delivery: { kind: "copied" } })).toBe("reoffer");
  });

  it("ignores a press while a mint is in flight", () => {
    expect(nextInvitePress({ kind: "minting" })).toBe("ignore");
  });

  it("lets a failure be tried again", () => {
    expect(nextInvitePress({ kind: "failed", failure: "unavailable" })).toBe("mint");
    expect(nextInvitePress({ kind: "failed", failure: "signed-out" })).toBe("mint");
  });

  it("does not mint again when only the sending failed", () => {
    const state: InCallInvite = { kind: "failed", failure: "not-sent", url: URL_ };
    expect(nextInvitePress(state)).toBe("reoffer");
    expect(mintedInviteUrl(state)).toBe(URL_);
    expect(mintedInviteUrl({ kind: "failed", failure: "unavailable" })).toBeNull();
    expect(mintedInviteUrl(INVITE_IDLE)).toBeNull();
  });
});

describe("inviteBusy", () => {
  it("is true only while minting", () => {
    expect(inviteBusy(INVITE_IDLE)).toBe(false);
    expect(inviteBusy({ kind: "minting" })).toBe(true);
    expect(inviteBusy({ kind: "ready", url: URL_, delivery: { kind: "copied" } })).toBe(false);
    expect(inviteBusy({ kind: "failed", failure: "unavailable" })).toBe(false);
  });
});

describe("inviteStatusLine", () => {
  it("says nothing before the first press", () => {
    expect(inviteStatusLine(INVITE_IDLE)).toBeNull();
  });

  it("says what the wait is, not only that there is one", () => {
    const line = inviteStatusLine({ kind: "minting" }) ?? "";
    expect(line).toContain("link");
    expect(line).toContain("few seconds");
  });

  it("names where the link went", () => {
    expect(
      inviteStatusLine({ kind: "ready", url: URL_, delivery: { kind: "sent", name: "Anna" } }),
    ).toContain("Anna");
    expect(inviteStatusLine({ kind: "ready", url: URL_, delivery: { kind: "copied" } })).toContain("copied");
    expect(inviteStatusLine({ kind: "ready", url: URL_, delivery: { kind: "shown" } })).toContain(URL_);
  });

  it("gives a failure what happened and what to do, and never ends the call", () => {
    const failed = inviteStatusLine({ kind: "failed", failure: "unavailable" }) ?? "";
    expect(failed).toContain("could not");
    expect(failed).toContain("try again");
    expect(failed).toContain("call is unaffected");
    expect(inviteStatusLine({ kind: "failed", failure: "signed-out" })).toContain("Sign in again");
  });

  it("uses no protocol vocabulary and no exclamation marks", () => {
    const states: InCallInvite[] = [
      INVITE_IDLE,
      { kind: "minting" },
      { kind: "ready", url: URL_, delivery: { kind: "sent", name: "Anna" } },
      { kind: "failed", failure: "not-sent", url: URL_ },
      { kind: "ready", url: URL_, delivery: { kind: "copied" } },
      { kind: "ready", url: URL_, delivery: { kind: "shown" } },
      { kind: "failed", failure: "signed-out" },
      { kind: "failed", failure: "unavailable" },
    ];
    const banned = [
      "MLS",
      "OPRF",
      "KeyPackage",
      "pseudonym",
      "epoch",
      "capability",
      "proof of work",
      "proof-of-work",
      "by design",
    ];
    for (const state of states) {
      const line = inviteStatusLine(state);
      if (line === null) continue;
      expect(line).not.toContain("!");
      for (const word of banned) expect(line.toLowerCase()).not.toContain(word.toLowerCase());
    }
  });
});

describe("inviteStatusHoldMs", () => {
  it("holds a finished line for a few seconds", () => {
    expect(inviteStatusHoldMs({ kind: "ready", url: URL_, delivery: { kind: "copied" } })).toBe(
      INVITE_STATUS_HOLD_MS,
    );
    expect(inviteStatusHoldMs({ kind: "failed", failure: "unavailable" })).toBe(
      INVITE_STATUS_HOLD_MS,
    );
  });

  it("never times out a wait, an empty line, or a link the person is selecting", () => {
    expect(inviteStatusHoldMs({ kind: "minting" })).toBeNull();
    expect(inviteStatusHoldMs(INVITE_IDLE)).toBeNull();
    expect(inviteStatusHoldMs({ kind: "ready", url: URL_, delivery: { kind: "shown" } })).toBeNull();
  });
});

describe("copyCallLink", () => {
  it("puts the link on the clipboard", async () => {
    const copy = vi.fn(async () => undefined);
    expect(await copyCallLink(URL_, copy)).toEqual({ kind: "copied" });
    expect(copy).toHaveBeenCalledWith(URL_);
  });

  it("shows the link when the clipboard is not reachable", async () => {
    expect(await copyCallLink(URL_, undefined)).toEqual({ kind: "shown" });
    expect(
      await copyCallLink(URL_, async () => {
        throw new Error("denied");
      }),
    ).toEqual({ kind: "shown" });
  });
});

describe("mintedInviteFate", () => {
  it("the call the mint was started for still holds the slot: the link is shown", () => {
    expect(mintedInviteFate({ mintedForCallIdHex: "aa", slotCallIdHex: "aa" })).toBe("show");
  });

  it("the call ended during the mint: the line goes back to idle", () => {
    expect(mintedInviteFate({ mintedForCallIdHex: "aa", slotCallIdHex: null })).toBe("call-over");
  });

  it("the call ended and another started during the mint: the first call's link is not shown on the second", () => {
    expect(mintedInviteFate({ mintedForCallIdHex: "aa", slotCallIdHex: "bb" })).toBe("another-call");
  });
});
