// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  bindFailure,
  clearRenameStores,
  readRenameOwed,
  renameEmail,
  renameNeedsAddress,
  renameResume,
  replaceFailure,
  rollbackRename,
  runRename,
  writeRenameOwed,
  type Answer,
  type RenameOwed,
  type RenamePorts,
} from "./username-change";

const answer = (status: number | null, errorCode: string | null = null, retryAfterSecs: number | null = null): Answer => ({
  status,
  errorCode,
  retryAfterSecs,
});

describe("how the recovery email fares", () => {
  it("no recovery email", () => {
    expect(renameEmail(false, false)).toBe("none");
  });

  it("a recovery email and the address on this device: it moves with the name", () => {
    expect(renameEmail(true, true)).toBe("kept");
  });

  it("a recovery email and no address here: the rename turns it off", () => {
    expect(renameEmail(true, false)).toBe("dropped");
  });

  it("the screen asks for the address only when the email is the last second way in and none is held (UX 19.1c)", () => {
    expect(renameNeedsAddress("dropped", true)).toBe(true);
    // Another way stays: the rename may turn the email off, as before.
    expect(renameNeedsAddress("dropped", false)).toBe(false);
    // The device holds the address: it is carried without asking.
    expect(renameNeedsAddress("kept", true)).toBe(false);
    expect(renameNeedsAddress("none", true)).toBe(false);
  });
});

describe("reading the answers", () => {
  it("of the handle bind", () => {
    expect(bindFailure(answer(403, "fresh-auth-required"))).toEqual({ kind: "proof-required" });
    expect(bindFailure(answer(403, "address-mismatch"))).toEqual({ kind: "address-mismatch" });
    expect(bindFailure(answer(409, "handle-taken"))).toEqual({ kind: "taken" });
    // The change would drop the account's last second way in.
    expect(bindFailure(answer(409, "last-factor"))).toEqual({ kind: "last-factor" });
    expect(bindFailure(answer(429, "handle-bind-limited", 120))).toEqual({ kind: "bind-limited", retryAfterSecs: 120 });
    expect(bindFailure(answer(null))).toEqual({ kind: "offline" });
    expect(bindFailure(answer(409, "handle-mismatch"))).toEqual({ kind: "error" });
  });

  it("of the replace", () => {
    expect(replaceFailure(answer(409, "username-taken"))).toEqual({ kind: "taken" });
    expect(replaceFailure(answer(null))).toEqual({ kind: "offline" });
    expect(replaceFailure(answer(409, "no-username"))).toEqual({ kind: "error" });
  });
});

// ---------------------------------------------------------------------------

interface Harness {
  readonly ports: RenamePorts;
  readonly calls: string[];
  readonly binds: Parameters<RenamePorts["bind"]>[0][];
  readonly owed: (RenameOwed | null)[];
  readonly finished: string[];
}

function harness(script: {
  bind?: (Answer | null)[];
  replace?: Answer | "ok";
} = {}): Harness {
  const calls: string[] = [];
  const binds: Parameters<RenamePorts["bind"]>[0][] = [];
  const owed: (RenameOwed | null)[] = [];
  const finished: string[] = [];
  const bindAnswers = [...(script.bind ?? [])];
  const ports: RenamePorts = {
    handleFor: async (username) => ({ bytes: new TextEncoder().encode(username), wire: `h(${username})` }),
    bind: async (args) => {
      calls.push(`bind:${args.handle}`);
      binds.push(args);
      return bindAnswers.length > 0 ? (bindAnswers.shift() ?? null) : null;
    },
    replace: async (username) => {
      calls.push(`replace:${username}`);
      const r = script.replace ?? "ok";
      return r === "ok" ? { ok: true, username, certificate: new Uint8Array([1]) } : { ok: false, answer: r };
    },
    saveOwed: (record) => void owed.push(record),
    finishLocal: async (username) => {
      calls.push(`finish:${username}`);
      finished.push(username);
    },
  };
  return { ports, calls, binds, owed, finished };
}
const input = (email: { address: string } | null = null) => ({
  oldUsername: "alice",
  newUsername: "alicia",
  grant: "k1.grant",
  email,
});
const last = <T>(xs: T[]): T => xs[xs.length - 1] as T;

describe("the rename, in order", () => {
  it("binds the new handle, replaces the name, then saves it here; the record is gone at the end", async () => {
    const h = harness();
    expect(await runRename(h.ports, input())).toEqual({ kind: "done", username: "alicia" });
    expect(h.calls).toEqual(["bind:h(alicia)", "replace:alicia", "finish:alicia"]);
    expect(h.binds[0]).toEqual({ handle: "h(alicia)", previousHandle: "h(alice)", grant: "k1.grant", email: null });
    // Written before anything is sent, moved at each step, dropped last.
    expect(h.owed.map((r) => r?.phase ?? null)).toEqual(["binding", "replaced", null]);
  });

  it("carries the address alone, for every account: no vault rides along", async () => {
    const h = harness();
    await runRename(h.ports, input({ address: "name@example.com" }));
    expect(h.calls).toEqual(["bind:h(alicia)", "replace:alicia", "finish:alicia"]);
    expect(h.binds[0]?.email).toEqual({ address: "name@example.com" });
    expect(Object.keys(h.binds[0]?.email ?? {})).toEqual(["address"]);
  });

  it("a change that would drop the last second way in is refused and changed nothing", async () => {
    const h = harness({ bind: [answer(409, "last-factor")] });
    const out = await runRename(h.ports, input());
    expect(out).toEqual({ kind: "failed", failure: { kind: "last-factor" }, rolledBack: null });
    expect(h.calls).toEqual(["bind:h(alicia)"]);
    expect(last(h.owed)).toBeNull();
  });

  it("a refused bind changed nothing: no replace, no record", async () => {
    const h = harness({ bind: [answer(403, "fresh-auth-required")] });
    const out = await runRename(h.ports, input());
    expect(out).toEqual({ kind: "failed", failure: { kind: "proof-required" }, rolledBack: null });
    expect(h.calls).toEqual(["bind:h(alicia)"]);
    expect(last(h.owed)).toBeNull();
  });

  it("a bind with no answer keeps the record: whether it landed is asked at the next landing", async () => {
    const h = harness({ bind: [answer(null)] });
    expect((await runRename(h.ports, input())).kind).toBe("failed");
    expect(last(h.owed)?.phase).toBe("binding");
    expect(h.calls.includes("replace:alicia")).toBe(false);
  });
});

describe("the rollback", () => {
  it("a taken name binds the OLD handle back, the same shape reversed, the address carried again", async () => {
    const h = harness({ replace: answer(409, "username-taken") });
    const out = await runRename(h.ports, input({ address: "name@example.com" }));
    expect(out).toEqual({ kind: "failed", failure: { kind: "taken" }, rolledBack: true });
    expect(h.calls).toEqual(["bind:h(alicia)", "replace:alicia", "bind:h(alice)"]);
    expect(h.binds[1]).toMatchObject({ handle: "h(alice)", previousHandle: "h(alicia)", grant: "k1.grant" });
    expect(h.binds[1]?.email).toEqual({ address: "name@example.com" });
    expect(h.finished).toEqual([]);
    expect(last(h.owed)).toBeNull();
  });

  it("a rollback that did not go through is owed to the person", async () => {
    const h = harness({ replace: answer(409, "username-taken"), bind: [null, answer(429, "handle-bind-limited", 60)] });
    const out = await runRename(h.ports, input());
    expect(out).toMatchObject({ kind: "failed", failure: { kind: "taken" }, rolledBack: false });
    expect(last(h.owed)).toEqual({ v: 1, oldUsername: "alice", newUsername: "alicia", phase: "rollback" });
    // The person's retry binds the old handle back.
    const again = harness();
    expect(await rollbackRename(again.ports, input())).toBe(true);
    expect(again.binds[0]).toMatchObject({ handle: "h(alice)", previousHandle: "h(alicia)" });
  });

  it("a replace with no answer does NOT roll back: it may have landed", async () => {
    const h = harness({ replace: answer(null) });
    const out = await runRename(h.ports, input());
    expect(out).toMatchObject({ kind: "failed", failure: { kind: "offline" } });
    expect(h.calls).toEqual(["bind:h(alicia)", "replace:alicia"]);
    expect(last(h.owed)?.phase).toBe("binding");
  });
});

describe("the owed record", () => {
  const record: RenameOwed = { v: 1, oldUsername: "alice", newUsername: "alicia", phase: "binding" };

  it("survives a reload, holds names only, and a damaged row is no record", () => {
    const local = new MemoryStorage();
    expect(readRenameOwed(local, "acct")).toBeNull();
    writeRenameOwed(local, "acct", record);
    expect(readRenameOwed(local, "acct")).toEqual(record);
    expect(Object.keys(record).sort()).toEqual(["newUsername", "oldUsername", "phase", "v"]);
    local.setItem("lakko:username-change-owed:acct", JSON.stringify({ ...record, phase: "odd" }));
    expect(readRenameOwed(local, "acct")).toBeNull();
    local.setItem("lakko:username-change-owed:acct", "{nope");
    expect(readRenameOwed(local, "acct")).toBeNull();
    writeRenameOwed(local, "acct", record);
    clearRenameStores(local, "acct");
    expect(local.entries()).toEqual([]);
  });

  it("at the next landing: finished, dropped, sent again, or left to the person", () => {
    // Both servers were done: only this device is to be moved.
    expect(renameResume({ ...record, phase: "replaced" }, null)).toBe("finish-local");
    // The bind never landed: nothing changed.
    expect(renameResume(record, "other")).toBe("drop");
    expect(renameResume(record, "none")).toBe("drop");
    // The bind landed; the replace is sent again.
    expect(renameResume(record, "same")).toBe("replace-again");
    // No answer: asked again later, nothing is dropped.
    expect(renameResume(record, null)).toBe("later");
    // An owed rollback needs the proof.
    expect(renameResume({ ...record, phase: "rollback" }, "same")).toBe("wait-for-person");
  });
});
