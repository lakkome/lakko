// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../test/fake-storage";
import {
  bindFailure,
  clearRenameStores,
  readRenamedAt,
  readRenameOwed,
  renameAllowedAtMs,
  renameEmail,
  renameResume,
  renameSealsVault,
  renameWarningKeys,
  RENAME_INTERVAL_MS,
  replaceFailure,
  rollbackRename,
  runRename,
  tooSoonUntilMs,
  writeRenamedAt,
  writeRenameOwed,
  type Answer,
  type RenameOwed,
  type RenamePorts,
} from "./username-change";

const NOW = 1_800_000_000_000;
const answer = (status: number | null, errorCode: string | null = null, retryAfterSecs: number | null = null): Answer => ({
  status,
  errorCode,
  retryAfterSecs,
});

describe("what the screen shows", () => {
  it("no recovery email: the two lines about the name", () => {
    expect(renameEmail(false, false)).toBe("none");
    expect(renameWarningKeys("none")).toEqual(["username.change.body", "username.change.next_change"]);
  });

  it("a recovery email and the address on this device: it moves with the name, nothing more is said", () => {
    expect(renameEmail(true, true)).toBe("kept");
    expect(renameWarningKeys("kept")).toEqual(["username.change.body", "username.change.next_change"]);
  });

  it("a recovery email and no address here: the rename turns it off, and the screen says so", () => {
    expect(renameEmail(true, false)).toBe("dropped");
    expect(renameWarningKeys("dropped")).toEqual([
      "username.change.body",
      "username.change.next_change",
      "recovery_email.username_change",
    ]);
  });

  it("a vault is sealed only when the email is kept and no phrase is saved", () => {
    expect(renameSealsVault("kept", false)).toBe(true);
    expect(renameSealsVault("kept", true)).toBe(false);
    expect(renameSealsVault("dropped", false)).toBe(false);
    expect(renameSealsVault("none", false)).toBe(false);
  });
});

describe("too soon", () => {
  it("a change under thirty days ago names when the next is allowed", () => {
    expect(renameAllowedAtMs(NOW - 1_000, NOW)).toBe(NOW - 1_000 + RENAME_INTERVAL_MS);
    expect(renameAllowedAtMs(NOW - RENAME_INTERVAL_MS, NOW)).toBeNull();
    expect(renameAllowedAtMs(null, NOW)).toBeNull();
    // A stamp in the future is not trusted.
    expect(renameAllowedAtMs(NOW + 5_000, NOW)).toBeNull();
    expect(RENAME_INTERVAL_MS).toBe(30 * 24 * 60 * 60 * 1000);
  });

  it("the server's answer names the date", () => {
    expect(tooSoonUntilMs(3_600, NOW)).toBe(NOW + 3_600_000);
    expect(replaceFailure(answer(429, "rename-too-soon", 86_400), NOW)).toEqual({
      kind: "too-soon",
      untilMs: NOW + 86_400_000,
    });
  });

  it("the stamp is kept per account", () => {
    const local = new MemoryStorage();
    expect(readRenamedAt(local, "acct")).toBeNull();
    writeRenamedAt(local, "acct", NOW);
    expect(readRenamedAt(local, "acct")).toBe(NOW);
    expect(readRenamedAt(local, "other")).toBeNull();
  });
});

describe("reading the answers", () => {
  it("of the handle bind", () => {
    expect(bindFailure(answer(403, "fresh-auth-required"))).toEqual({ kind: "proof-required" });
    expect(bindFailure(answer(403, "address-mismatch"))).toEqual({ kind: "address-mismatch" });
    expect(bindFailure(answer(409, "handle-taken"))).toEqual({ kind: "taken" });
    expect(bindFailure(answer(429, "handle-bind-limited", 120))).toEqual({ kind: "bind-limited", retryAfterSecs: 120 });
    expect(bindFailure(answer(null))).toEqual({ kind: "offline" });
    expect(bindFailure(answer(409, "handle-mismatch"))).toEqual({ kind: "error" });
  });

  it("of the replace", () => {
    expect(replaceFailure(answer(409, "username-taken"), NOW)).toEqual({ kind: "taken" });
    expect(replaceFailure(answer(null), NOW)).toEqual({ kind: "offline" });
    expect(replaceFailure(answer(409, "no-username"), NOW)).toEqual({ kind: "error" });
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
  seal?: boolean;
} = {}): Harness {
  const calls: string[] = [];
  const binds: Parameters<RenamePorts["bind"]>[0][] = [];
  const owed: (RenameOwed | null)[] = [];
  const finished: string[] = [];
  const bindAnswers = [...(script.bind ?? [])];
  let seals = 0;
  const ports: RenamePorts = {
    handleFor: async (username) => ({ bytes: new TextEncoder().encode(username), wire: `h(${username})` }),
    sealVault: async (handle, address) => {
      calls.push(`seal:${new TextDecoder().decode(handle)}`);
      if (script.seal === false) return null;
      seals += 1;
      return { salt: new Uint8Array(16).fill(seals), sealed: new TextEncoder().encode(`${address}|${seals}`) };
    },
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
    nowMs: () => NOW,
  };
  return { ports, calls, binds, owed, finished };
}
const input = (email: { address: string; sealVault: boolean } | null = null) => ({
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

  it("carries the recovery email with a vault sealed under the NEW handle when no phrase is saved", async () => {
    const h = harness();
    await runRename(h.ports, input({ address: "name@example.com", sealVault: true }));
    expect(h.calls[0]).toBe("seal:alicia");
    expect(h.binds[0]?.email?.address).toBe("name@example.com");
    expect(h.binds[0]?.email?.vault).toBeDefined();
  });

  it("carries the address alone for a phrase-protected account", async () => {
    const h = harness();
    await runRename(h.ports, input({ address: "name@example.com", sealVault: false }));
    expect(h.calls.some((c) => c.startsWith("seal"))).toBe(false);
    expect(h.binds[0]?.email).toEqual({ address: "name@example.com" });
  });

  it("sends nothing when the vault could not be sealed", async () => {
    const h = harness({ seal: false });
    const out = await runRename(h.ports, input({ address: "name@example.com", sealVault: true }));
    expect(out).toEqual({ kind: "failed", failure: { kind: "error" }, rolledBack: null });
    expect(h.binds).toEqual([]);
    expect(h.owed).toEqual([]);
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
  it("a taken name binds the OLD handle back, the same shape reversed, the vault re-sealed under the old handle", async () => {
    const h = harness({ replace: answer(409, "username-taken") });
    const out = await runRename(h.ports, input({ address: "name@example.com", sealVault: true }));
    expect(out).toEqual({ kind: "failed", failure: { kind: "taken" }, rolledBack: true });
    expect(h.calls).toEqual(["seal:alicia", "bind:h(alicia)", "replace:alicia", "seal:alice", "bind:h(alice)"]);
    expect(h.binds[1]).toMatchObject({ handle: "h(alice)", previousHandle: "h(alicia)", grant: "k1.grant" });
    // A new seal, not the one made for the new handle.
    expect(h.binds[1]?.email?.vault?.sealed).not.toEqual(h.binds[0]?.email?.vault?.sealed);
    expect(h.finished).toEqual([]);
    expect(last(h.owed)).toBeNull();
  });

  it("too soon rolls back too, and names the date", async () => {
    const h = harness({ replace: answer(429, "rename-too-soon", 3_600) });
    const out = await runRename(h.ports, input());
    expect(out).toEqual({
      kind: "failed",
      failure: { kind: "too-soon", untilMs: NOW + 3_600_000 },
      rolledBack: true,
    });
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
    writeRenamedAt(local, "acct", NOW);
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
