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

  it("passes loading and rate-limited through", () => {
    expect(invitePageView({ ...base, link: "loading" })).toEqual({ kind: "loading" });
    expect(invitePageView({ ...base, link: "rate-limited" })).toEqual({ kind: "rate-limited" });
  });
});
