// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { callInviteOf } from "./call-invite-link";

const ORIGIN = "https://lakko.example";

describe("callInviteOf", () => {
  it("reads a message that is one call link of this deployment as an invitation", () => {
    expect(callInviteOf(`${ORIGIN}/c/AbC_123-x#secretSECRET12`, ORIGIN)).toEqual({
      href: "/c/AbC_123-x#secretSECRET12",
    });
    expect(callInviteOf(`  ${ORIGIN}/c/abc#def \n`, ORIGIN)?.href).toBe("/c/abc#def");
  });

  it("leaves a link to another host as plain text", () => {
    expect(callInviteOf("https://lakko.example.evil.test/c/abc#def", ORIGIN)).toBeNull();
    expect(callInviteOf("http://lakko.example/c/abc#def", ORIGIN)).toBeNull();
    expect(callInviteOf("https://user@lakko.example/c/abc#def", ORIGIN)).toBeNull();
  });

  it("leaves ordinary text, and text around a link, alone", () => {
    expect(callInviteOf("", ORIGIN)).toBeNull();
    expect(callInviteOf("call me", ORIGIN)).toBeNull();
    expect(callInviteOf(`join ${ORIGIN}/c/abc#def`, ORIGIN)).toBeNull();
    expect(callInviteOf(`${ORIGIN}/c/abc#def now`, ORIGIN)).toBeNull();
  });

  it("requires the call path and the secret after the hash", () => {
    expect(callInviteOf(`${ORIGIN}/c/abc`, ORIGIN)).toBeNull();
    expect(callInviteOf(`${ORIGIN}/g/abc#def`, ORIGIN)).toBeNull();
    expect(callInviteOf(`${ORIGIN}/c/abc/extra#def`, ORIGIN)).toBeNull();
    expect(callInviteOf(`${ORIGIN}/c/abc?x=1#def`, ORIGIN)).toBeNull();
    expect(callInviteOf(`${ORIGIN}/c/a%20b#def`, ORIGIN)).toBeNull();
  });
});
