// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  CLAIM_START,
  claimFailure,
  claimReduce,
  claimStart,
  claimSubmits,
  clearFirstPasswordOwed,
  isFirstPasswordOwed,
  markFirstPasswordOwed,
  usernameClaimEntry,
  usernameClaimOffered,
  type ClaimEvent,
  type ClaimState,
} from "./username-claim";

const run = (events: readonly ClaimEvent[], from: ClaimState = CLAIM_START): ClaimState =>
  events.reduce(claimReduce, from);
const read = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

describe("where Choose a username is offered", () => {
  it("only for an account that has no username", () => {
    expect(usernameClaimOffered("")).toBe(true);
    expect(usernameClaimOffered("   ")).toBe(true);
    expect(usernameClaimOffered(null)).toBe(true);
    expect(usernameClaimOffered(undefined)).toBe(true);
    expect(usernameClaimOffered("alice")).toBe(false);
  });

  it("an account whose name is saved and whose first password is owed finishes there", () => {
    expect(usernameClaimEntry("", false)).toBe("choose");
    expect(usernameClaimEntry("alice", true)).toBe("finish");
    expect(usernameClaimEntry("alice", false)).toBe("none");
  });
});

describe("the claim", () => {
  it("name, then password, then done", () => {
    const working = run([{ type: "submitted" }]);
    expect(working).toEqual({ step: "form", working: true, failure: null });
    const named = claimReduce(working, { type: "name-saved", username: "alice" });
    // The password step follows at once.
    expect(named).toEqual({ step: "password", username: "alice", working: true, failure: null });
    expect(claimReduce(named, { type: "password-saved" })).toEqual({ step: "done", username: "alice" });
  });

  it("a taken name stays on the form, and the whole claim is submitted again", () => {
    const s = run([{ type: "submitted" }, { type: "failed", failure: { kind: "taken" } }]);
    expect(s).toEqual({ step: "form", working: false, failure: { kind: "taken" } });
    expect(claimSubmits(s)).toBe("name-and-password");
  });

  it("a password that failed after the name was saved keeps the name: only the password is retried", () => {
    const s = run([
      { type: "submitted" },
      { type: "name-saved", username: "alice" },
      { type: "failed", failure: { kind: "error" } },
    ]);
    expect(s).toEqual({ step: "password", username: "alice", working: false, failure: { kind: "error" } });
    expect(claimSubmits(s)).toBe("password-only");
    const again = run([{ type: "submitted" }, { type: "password-saved" }], s);
    expect(again).toEqual({ step: "done", username: "alice" });
  });

  it("the name is never claimed twice", () => {
    const named = run([{ type: "submitted" }, { type: "name-saved", username: "alice" }]);
    expect(claimReduce(named, { type: "name-saved", username: "other" })).toBe(named);
    expect(claimSubmits(named)).toBe("password-only");
  });

  it("a password cannot be reported saved before the name is", () => {
    expect(claimReduce(CLAIM_START, { type: "password-saved" })).toBe(CLAIM_START);
  });

  it("done is final", () => {
    const done: ClaimState = { step: "done", username: "alice" };
    expect(claimReduce(done, { type: "submitted" })).toBe(done);
    expect(claimSubmits(done)).toBe("nothing");
  });

  it("after a reload with the name saved, the screen opens on the password alone", () => {
    expect(claimStart("alice", true)).toEqual({ step: "password", username: "alice", working: false, failure: null });
    expect(claimStart("", false)).toEqual(CLAIM_START);
    // A marker without a name is not a saved name.
    expect(claimStart("", true)).toEqual(CLAIM_START);
    expect(claimStart("alice", false)).toEqual(CLAIM_START);
  });

  it("reads a failed claim", () => {
    const none = { taken: false, bindLimited: false, retryAfterSecs: null, offline: false };
    expect(claimFailure({ ...none, taken: true })).toEqual({ kind: "taken" });
    expect(claimFailure({ ...none, bindLimited: true, retryAfterSecs: 120 })).toEqual({
      kind: "bind-limited",
      retryAfterSecs: 120,
    });
    expect(claimFailure({ ...none, offline: true })).toEqual({ kind: "offline" });
    expect(claimFailure(none)).toEqual({ kind: "error" });
  });
});

describe("the first password", () => {
  it("is owed from the moment the name is saved until the password is", () => {
    const local = new MemoryStorage();
    expect(isFirstPasswordOwed(local, "acct")).toBe(false);
    markFirstPasswordOwed(local, "acct");
    expect(isFirstPasswordOwed(local, "acct")).toBe(true);
    expect(isFirstPasswordOwed(local, "other")).toBe(false);
    clearFirstPasswordOwed(local, "acct");
    expect(isFirstPasswordOwed(local, "acct")).toBe(false);
    expect(isFirstPasswordOwed(undefined, "acct")).toBe(false);
  });

  it("the claim runs the sign-up's order and marks the password owed before the state moves", () => {
    const ctx = read("./AuthContext.tsx");
    const start = ctx.indexOf("    async claimGuestUsername(username) {");
    const body = ctx.slice(start, ctx.indexOf("\n    firstPasswordOwed() {", start));
    expect(start).toBeGreaterThan(0);
    // Only for an account with no username.
    expect(body.includes("!usernameClaimOffered(s.account.username)")).toBe(true);
    const bind = body.indexOf("await client.bindLoginHandle(token, handle.wire);");
    const register = body.indexOf("discovery.registerUsername(secrets.accountId, username)");
    const release = body.indexOf("await client.releaseLoginHandle(token);");
    const persisted = body.indexOf("savePersisted({ v: 1, accountIdB64, username: reg.normalisedUsername });");
    const owed = body.indexOf("markFirstPasswordOwed(plainStorage(), accountIdB64);");
    const moved = body.indexOf("setState({ kind: \"session\"");
    for (const at of [bind, register, release, persisted, owed, moved]) expect(at).toBeGreaterThan(0);
    expect(bind).toBeLessThan(register);
    expect(persisted).toBeLessThan(owed);
    expect(owed).toBeLessThan(moved);
    expect(body.includes("loginHandleSync.markBound(token, handle.wire);")).toBe(true);
    // The certificate the register call returned is kept.
    expect(body.includes("certificate: reg.usernameCertificate")).toBe(true);
  });

  it("the screen sets the password with no proof block, then publishes the profile, then offers the protect step", () => {
    const screen = read("./UsernameClaim.tsx");
    expect(screen.includes("await auth.replacePassword(password(), { firstPassword: true });")).toBe(true);
    expect(/ProofBlock|createProofGate/.test(screen)).toBe(false);
    const password = screen.indexOf("await auth.replacePassword(");
    const publish = screen.indexOf("await profiles.usernameClaimed(certificate);");
    expect(publish).toBeGreaterThan(password);
    expect(screen.includes("auth.offerProtectStep();")).toBe(true);
    expect(/window\.(confirm|alert|prompt)\(/.test(screen)).toBe(false);
  });
});
