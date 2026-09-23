// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  INVITE_IDLE,
  INVITE_STATUS_HOLD_MS,
  type InCallInvite,
  deliverCallLink,
  inviteBusy,
  inviteStatusHoldMs,
  inviteStatusLine,
  nextInvitePress,
} from "./in-call-invite";

const URL_ = "https://lakko.me/c/AAAA";

describe("nextInvitePress", () => {
  it("mints once and then re-offers, so a second press costs no round trip", () => {
    expect(nextInvitePress(INVITE_IDLE)).toBe("mint");
    expect(nextInvitePress({ kind: "ready", url: URL_, delivery: "copied" })).toBe("reoffer");
  });

  it("ignores a press while a mint is in flight", () => {
    expect(nextInvitePress({ kind: "minting" })).toBe("ignore");
  });

  it("lets a failure be tried again", () => {
    expect(nextInvitePress({ kind: "failed", failure: "unavailable" })).toBe("mint");
    expect(nextInvitePress({ kind: "failed", failure: "signed-out" })).toBe("mint");
  });
});

describe("inviteBusy", () => {
  it("is true only while minting", () => {
    expect(inviteBusy(INVITE_IDLE)).toBe(false);
    expect(inviteBusy({ kind: "minting" })).toBe(true);
    expect(inviteBusy({ kind: "ready", url: URL_, delivery: "shared" })).toBe(false);
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
    expect(inviteStatusLine({ kind: "ready", url: URL_, delivery: "shared" })).toBe("Link shared.");
    expect(inviteStatusLine({ kind: "ready", url: URL_, delivery: "copied" })).toContain("copied");
    expect(inviteStatusLine({ kind: "ready", url: URL_, delivery: "shown" })).toContain(URL_);
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
      { kind: "ready", url: URL_, delivery: "shared" },
      { kind: "ready", url: URL_, delivery: "copied" },
      { kind: "ready", url: URL_, delivery: "shown" },
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
    expect(inviteStatusHoldMs({ kind: "ready", url: URL_, delivery: "copied" })).toBe(
      INVITE_STATUS_HOLD_MS,
    );
    expect(inviteStatusHoldMs({ kind: "failed", failure: "unavailable" })).toBe(
      INVITE_STATUS_HOLD_MS,
    );
  });

  it("never times out a wait, an empty line, or a link the person is selecting", () => {
    expect(inviteStatusHoldMs({ kind: "minting" })).toBeNull();
    expect(inviteStatusHoldMs(INVITE_IDLE)).toBeNull();
    expect(inviteStatusHoldMs({ kind: "ready", url: URL_, delivery: "shown" })).toBeNull();
  });
});

describe("deliverCallLink", () => {
  it("prefers the platform share mechanism", async () => {
    const share = vi.fn(async () => undefined);
    const copy = vi.fn(async () => undefined);
    expect(await deliverCallLink(URL_, { share, copy })).toBe("shared");
    expect(share).toHaveBeenCalledWith(URL_);
    expect(copy).not.toHaveBeenCalled();
  });

  it("copies when there is no share mechanism", async () => {
    const copy = vi.fn(async () => undefined);
    expect(await deliverCallLink(URL_, { copy })).toBe("copied");
    expect(copy).toHaveBeenCalledWith(URL_);
  });

  it("copies when the share is refused or dismissed, so the link is not lost", async () => {
    const copy = vi.fn(async () => undefined);
    const share = vi.fn(async () => {
      throw new Error("AbortError");
    });
    expect(await deliverCallLink(URL_, { share, copy })).toBe("copied");
    expect(copy).toHaveBeenCalledWith(URL_);
  });

  it("shows the link when neither mechanism is reachable", async () => {
    expect(await deliverCallLink(URL_, {})).toBe("shown");
    expect(
      await deliverCallLink(URL_, {
        copy: async () => {
          throw new Error("denied");
        },
      }),
    ).toBe("shown");
  });
});
