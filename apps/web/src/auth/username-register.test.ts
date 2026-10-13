// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { DiscoveryHttpError, isUsernameTaken } from "../discovery/client";
import { claimUsername, UsernameTakenError } from "./login-handle";
import {
  dropPendingUsername,
  installUsernameRegister,
  publishRegisterUsername,
  registerUsername,
  rememberUsername,
  usernameRegisterAction,
  usernameWritePending,
} from "./username-register";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));
let cleanup: (() => void) | null = null;
afterEach(() => {
  cleanup?.();
  cleanup = null;
  dropPendingUsername();
});

describe("what a device does about the own-username register", () => {
  const base = { writePending: false, localConfirmed: true };

  it("a newer name from another device is adopted", () => {
    expect(usernameRegisterAction({ ...base, local: "alice", register: "alicia" })).toEqual({
      kind: "adopt",
      username: "alicia",
    });
    // A guest whose name was chosen on another device.
    expect(usernameRegisterAction({ ...base, local: "", register: "alicia" })).toEqual({
      kind: "adopt",
      username: "alicia",
    });
  });

  it("the same name is nothing", () => {
    expect(usernameRegisterAction({ ...base, local: "alice", register: "alice" })).toEqual({ kind: "nothing" });
  });

  it("an existing account with an empty register seeds it once from its marker", () => {
    expect(usernameRegisterAction({ ...base, local: "alice", register: null })).toEqual({
      kind: "seed",
      username: "alice",
    });
    expect(usernameRegisterAction({ ...base, local: "", register: null })).toEqual({ kind: "nothing" });
  });

  it("a device whose name auth has not confirmed never seeds: a stale name must not undo a rename", () => {
    expect(
      usernameRegisterAction({ local: "oldname", register: null, writePending: false, localConfirmed: false }),
    ).toEqual({ kind: "nothing" });
  });

  it("nothing while the synced state is not loaded, or while a write of this device is on its way", () => {
    expect(usernameRegisterAction({ ...base, local: "alice", register: undefined })).toEqual({ kind: "nothing" });
    // The register still holds the old name; this device just renamed.
    expect(
      usernameRegisterAction({ local: "alicia", register: "alice", writePending: true, localConfirmed: true }),
    ).toEqual({ kind: "nothing" });
  });
});

describe("the register's port", () => {
  function port(accountIdB64: string): string[] {
    const writes: string[] = [];
    cleanup = installUsernameRegister({
      accountIdB64,
      set: async (username) => {
        writes.push(username);
        publishRegisterUsername(accountIdB64, username);
      },
    });
    return writes;
  }

  it("is unknown until the synced state is loaded, then what it holds", () => {
    expect(registerUsername("acct")).toBeUndefined();
    port("acct");
    publishRegisterUsername("acct", null);
    expect(registerUsername("acct")).toBeNull();
    publishRegisterUsername("acct", "alice");
    expect(registerUsername("acct")).toBe("alice");
    expect(registerUsername("other")).toBeUndefined();
  });

  it("a write made before the state is loaded waits, is marked pending, and is applied when it loads", async () => {
    rememberUsername("acct", "alicia");
    expect(usernameWritePending("acct")).toBe(true);
    const writes = port("acct");
    await settle();
    expect(writes).toEqual(["alicia"]);
    expect(usernameWritePending("acct")).toBe(false);
    expect(registerUsername("acct")).toBe("alicia");
  });

  it("with the state loaded a write goes through at once", async () => {
    const writes = port("acct");
    rememberUsername("acct", "alice");
    await settle();
    expect(writes).toEqual(["alice"]);
    expect(usernameWritePending("acct")).toBe(false);
  });
});

describe("the register through the real core", () => {
  it("a rename on one device reaches a sibling and a later stamp wins", () => {
    const here = new wasm.SyncedState();
    expect(here.ownUsername()).toBeUndefined();
    expect(here.setOwnUsername(10n, "alice")).toBe(true);
    const first = here.ownUsernameEnvelope() as Uint8Array;
    expect(here.setOwnUsername(20n, "alicia")).toBe(true);
    const renamed = here.ownUsernameEnvelope() as Uint8Array;
    expect(wasm.isOwnUsernameEnvelope(renamed)).toBe(true);
    expect(wasm.isRecoveryEmailEnvelope(renamed)).toBe(false);
    expect(wasm.isLineageEnvelope(renamed)).toBe(false);
    const sibling = new wasm.SyncedState();
    expect(sibling.applyOwnUsernameEnvelope(renamed)).toBe(true);
    // The older name arriving late does not bring it back.
    expect(sibling.applyOwnUsernameEnvelope(first)).toBe(false);
    expect(sibling.ownUsername()).toBe("alicia");
    // It survives the stored row.
    expect(wasm.SyncedState.fromBytes(sibling.toBytes()).ownUsername()).toBe("alicia");
  });
});

describe("the discovery 409 bodies are JSON (ADR-0076)", () => {
  it("username-taken is the name; username-already-set and no-username are not", () => {
    expect(isUsernameTaken(409, "username-taken")).toBe(true);
    expect(isUsernameTaken(409, "username-already-set")).toBe(false);
    expect(isUsernameTaken(409, "no-username")).toBe(false);
    // A 409 with no readable code reads as taken, as before.
    expect(isUsernameTaken(409, null)).toBe(true);
    expect(isUsernameTaken(429, "username-taken")).toBe(false);
  });

  const claim = (body: string): Promise<unknown> => {
    let released = 0;
    return claimUsername({
      username: "alice",
      bind: async () => undefined,
      register: async () => {
        throw new DiscoveryHttpError(409, "/discovery/register", body);
      },
      release: async () => {
        released += 1;
      },
    }).catch((err: unknown) => ({ err, released }));
  };

  it("sign-up and the guest claim read username-taken as the taken name, and release the handle", async () => {
    const out = (await claim('{"error":"username-taken"}')) as { err: unknown; released: number };
    expect(out.err).toBeInstanceOf(UsernameTakenError);
    expect(out.released).toBe(1);
  });

  it("username-already-set is not 'taken'", async () => {
    const out = (await claim('{"error":"username-already-set"}')) as { err: unknown; released: number };
    expect(out.err).toBeInstanceOf(DiscoveryHttpError);
    expect(out.err).not.toBeInstanceOf(UsernameTakenError);
    expect(out.released).toBe(1);
  });
});
