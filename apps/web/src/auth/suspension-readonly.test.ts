// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 step 3: read-only mode. The info blocks a suspended account is not
 * offered, and the write refusal of the channels service and storage (no
 * request other than GET or HEAD leaves while suspended).
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { sectionsWhileSuspended, SUSPENDED_HIDDEN_SECTIONS } from "../chat/conversation-info";
import {
  setAccountSuspendedFlag,
  SUSPENDED_WRITES_ALLOWED,
  writeAllowedWhileSuspended,
  writeGuardedFetch,
} from "./suspension-state";

afterEach(() => {
  setAccountSuspendedFlag(false);
  vi.unstubAllGlobals();
});

describe("sectionsWhileSuspended", () => {
  const all = ["members", "add-member", "invite-link", "invite-people", "leave-group", "call", "add-contact", "report"] as const;

  it("drops adding people, invites and calls while suspended", () => {
    expect(sectionsWhileSuspended(all, true)).toEqual(["members", "leave-group", "report"]);
    for (const s of SUSPENDED_HIDDEN_SECTIONS) expect(sectionsWhileSuspended(all, true)).not.toContain(s);
  });

  it("changes nothing otherwise", () => {
    expect(sectionsWhileSuspended(all, false)).toEqual(all);
  });
});

describe("writeGuardedFetch", () => {
  it("refuses every write while suspended, as the network would, and sends nothing", async () => {
    const fetchSpy = vi.fn(async () => new Response("{}"));
    vi.stubGlobal("fetch", fetchSpy);
    setAccountSuspendedFlag(true);
    for (const method of ["POST", "PUT", "DELETE", "PATCH"]) {
      await expect(writeGuardedFetch("/api/channels/v1/channels/x/posts", { method })).rejects.toBeInstanceOf(TypeError);
    }
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("lets reads through while suspended, and everything otherwise", async () => {
    const fetchSpy = vi.fn(async () => new Response("{}"));
    vi.stubGlobal("fetch", fetchSpy);
    setAccountSuspendedFlag(true);
    await writeGuardedFetch("/api/storage/blob/ab");
    await writeGuardedFetch("/api/storage/blob/ab", { method: "HEAD" });
    setAccountSuspendedFlag(false);
    await writeGuardedFetch("/api/storage/blob/ab", { method: "PUT" });
    expect(fetchSpy).toHaveBeenCalledTimes(3);
  });
});

describe("writeAllowedWhileSuspended", () => {
  it("is exactly the read pass and the backup deletion, besides reads", () => {
    expect(SUSPENDED_WRITES_ALLOWED).toEqual([
      "POST /api/storage/blob/pass/read",
      "DELETE /api/storage/backup",
    ]);
    expect(writeAllowedWhileSuspended("POST", "/api/storage/blob/pass/read")).toBe(true);
    expect(writeAllowedWhileSuspended("DELETE", "/api/storage/backup")).toBe(true);
    expect(writeAllowedWhileSuspended("GET", "/api/storage/blob/ab")).toBe(true);
    expect(writeAllowedWhileSuspended("HEAD", "/api/storage/blob/ab")).toBe(true);
    for (const [method, url] of [
      ["POST", "/api/storage/blob/pass/upload"],
      ["POST", "/api/storage/blob/keep"],
      ["PUT", "/api/storage/blob/ab"],
      ["DELETE", "/api/storage/blob/ab"],
      ["PUT", "/api/storage/backup"],
      ["POST", "/api/channels/v1/channels/x/posts"],
      ["DELETE", "/api/channels/v1/channels/x"],
    ] as const) {
      expect(writeAllowedWhileSuspended(method, url), `${method} ${url}`).toBe(false);
    }
  });

  it("lets the read pass out while suspended through the guarded fetch", async () => {
    const fetchSpy = vi.fn(async () => new Response("{}"));
    vi.stubGlobal("fetch", fetchSpy);
    setAccountSuspendedFlag(true);
    await writeGuardedFetch("/api/storage/blob/pass/read", { method: "POST" });
    await expect(writeGuardedFetch("/api/storage/blob/pass/upload", { method: "POST" })).rejects.toBeInstanceOf(
      TypeError,
    );
    expect(fetchSpy).toHaveBeenCalledTimes(1);
  });
});
