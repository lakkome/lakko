// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { inviterNameOf, invitePageView, type InvitePageInput } from "./invite-page";

const base: InvitePageInput = {
  link: "opened",
  expired: false,
  inviterDisplayName: "Anna",
  inviterUsername: "anna",
  label: "",
  signedIn: true,
  isOwn: false,
  existingContactKeyB64: null,
};

describe("inviterNameOf", () => {
  it("prefers the display name, then @username, then nothing", () => {
    expect(inviterNameOf("Anna", "anna")).toBe("Anna");
    expect(inviterNameOf("  ", "anna")).toBe("@anna");
    expect(inviterNameOf(null, null)).toBeNull();
    expect(inviterNameOf(undefined, "")).toBeNull();
  });
});

describe("invitePageView (docs/ux/contact-invite-link.md section 3)", () => {
  it("shows the default Add page with the stated name and username", () => {
    expect(invitePageView(base)).toEqual({
      kind: "default",
      name: "Anna",
      username: "anna",
      label: null,
    });
  });

  it("keeps the label when the inviter gave one", () => {
    const v = invitePageView({ ...base, label: "From the conference" });
    expect(v.kind === "default" ? v.label : undefined).toBe("From the conference");
  });

  it("reads an expired, gone or damaged link as no longer working, without a name", () => {
    expect(invitePageView({ ...base, expired: true })).toEqual({ kind: "dead" });
    expect(invitePageView({ ...base, link: "dead" })).toEqual({ kind: "dead" });
  });

  it("shows a signed-out visitor no inviter card: straight to sign-in, whatever the link (2026-10-19)", () => {
    expect(invitePageView({ ...base, signedIn: false })).toEqual({ kind: "sign-in" });
    expect(invitePageView({ ...base, signedIn: false, link: "dead" })).toEqual({ kind: "sign-in" });
    expect(invitePageView({ ...base, signedIn: false, link: "loading" })).toEqual({ kind: "sign-in" });
    // Once signed in the link is checked again: live -> the card, dead -> Contacts.
    expect(invitePageView({ ...base, signedIn: true }).kind).toBe("default");
    expect(invitePageView({ ...base, signedIn: true, link: "dead" })).toEqual({ kind: "dead" });
  });

  it("recognises the visitor's own link before an existing contact", () => {
    expect(invitePageView({ ...base, isOwn: true, existingContactKeyB64: "k" })).toEqual({ kind: "own" });
  });

  it("opens the chat for an inviter who is already a contact; nothing is sent", () => {
    expect(invitePageView({ ...base, existingContactKeyB64: "a2V5" })).toEqual({
      kind: "already",
      name: "Anna",
      contactKeyB64: "a2V5",
    });
  });

  it("tells a visitor who already added the inviter and waits for the answer that they are contacts (2026-10-24)", () => {
    expect(invitePageView({ ...base, pendingInviter: true })).toEqual({ kind: "pending", name: "Anna" });
    // An established contact row comes first; the own link before both.
    expect(invitePageView({ ...base, pendingInviter: true, existingContactKeyB64: "a2V5" }).kind).toBe("already");
    expect(invitePageView({ ...base, pendingInviter: true, isOwn: true }).kind).toBe("own");
  });

  it("passes loading and rate-limited through", () => {
    expect(invitePageView({ ...base, link: "loading" })).toEqual({ kind: "loading" });
    expect(invitePageView({ ...base, link: "rate-limited" })).toEqual({ kind: "rate-limited" });
  });
});

describe("inviteOpenFailure", () => {
  it("offers a rate-limited or failed fetch again, and reads everything after the fetch as dead", async () => {
    const { inviteOpenFailure } = await import("./invite-page");
    const { DiscoveryRateLimitedError, DiscoveryHttpError, isDiscoveryRateLimited } = await import(
      "../discovery/client"
    );
    const { InviteLinkDamagedError, InviteLinkUnreachableError, isInviteLinkUnreachable, classifyInviteSlug } =
      await import("./invite-link");
    const is = { rateLimited: isDiscoveryRateLimited, unreachable: isInviteLinkUnreachable };
    const limited = new DiscoveryRateLimitedError("/discovery/invite (GET)", null);
    expect(inviteOpenFailure(limited, is)).toBe("rate-limited");
    // A 429 arrives wrapped as a failure to ask the ledger: still a rate limit.
    expect(inviteOpenFailure(new InviteLinkUnreachableError(limited), is)).toBe("rate-limited");
    // A server error and the network say nothing about the link.
    const serverError = new DiscoveryHttpError(500, "/discovery/invite (GET)", "boom");
    expect(inviteOpenFailure(new InviteLinkUnreachableError(serverError), is)).toBe("open-failed");
    expect(inviteOpenFailure(new InviteLinkUnreachableError(new TypeError("Failed to fetch")), is)).toBe(
      "open-failed",
    );
    // A damaged or malformed link does not work.
    expect(inviteOpenFailure(new InviteLinkDamagedError(), is)).toBe("dead");
    let malformed: unknown = null;
    try {
      classifyInviteSlug("not-a-ledger-id!", { blobIdLen: 6, blobIdUrlLen: 8 } as never, "invite");
    } catch (err) {
      malformed = err;
    }
    expect(malformed).not.toBeNull();
    expect(inviteOpenFailure(malformed, is)).toBe("dead");
  });
});
