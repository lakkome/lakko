// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it } from "vitest";

import { runEmailVault, setEmailVaultRuntimeForTests, type EmailVaultWorkerLike } from "./email-vault";
import {
  runEmailVaultJob,
  vaultErrorCode,
  type EmailVaultCore,
  type EmailVaultRequest,
} from "./email-vault-job";
import type { EmailVaultWorkerRequest } from "./email-vault-worker";
import { loadWasm } from "../../wasm";

const HANDLE = new Uint8Array(32).fill(3);
const SALT = new Uint8Array(16).fill(4);
const PHRASE = "alpha beta gamma";

/** A core that "seals" by recording what it was given. */
function fakeCore(over: Partial<EmailVaultCore> = {}): { core: EmailVaultCore; freed: number[] } {
  const freed: number[] = [];
  let next = 0;
  const mnemonic = (phrase: string) => {
    const id = next;
    next += 1;
    return { phrase, free: () => void freed.push(id) };
  };
  return {
    freed,
    core: {
      RecoveryMnemonic: { fromPhrase: (phrase) => mnemonic(phrase) },
      emailVaultSeal: (m, handle, address, salt) =>
        new TextEncoder().encode(`${m.phrase}|${handle.length}|${address}|${salt.length}`),
      emailVaultOpen: (_handle, address, _salt, sealed) => {
        if (address !== "name@example.com") throw new Error("not-opened: the vault did not open");
        return mnemonic(new TextDecoder().decode(sealed));
      },
      ...over,
    },
  };
}

const seal: EmailVaultRequest = {
  kind: "seal",
  phrase: PHRASE,
  loginHandle: HANDLE,
  address: "name@example.com",
  vaultSalt: SALT,
};
const open = (address: string): EmailVaultRequest => ({
  kind: "open",
  loginHandle: HANDLE,
  address,
  vaultSalt: SALT,
  sealed: new TextEncoder().encode(PHRASE),
});

describe("one email-vault job", () => {
  it("seals the phrase and frees the mnemonic", () => {
    const { core, freed } = fakeCore();
    const result = runEmailVaultJob(core, seal);
    expect(result.kind).toBe("sealed");
    expect(freed).toEqual([0]);
  });

  it("opens to the phrase the vault held", () => {
    const { core, freed } = fakeCore();
    expect(runEmailVaultJob(core, open("name@example.com"))).toEqual({ kind: "opened", phrase: PHRASE });
    expect(freed).toEqual([0]);
  });

  it("a vault that does not open is one answer, whatever the reason", () => {
    const { core } = fakeCore();
    expect(runEmailVaultJob(core, open("other@example.com"))).toEqual({ kind: "not-opened" });
  });

  it("any other refusal carries the core's code and nothing of its prose", () => {
    const { core } = fakeCore({
      emailVaultSeal: () => {
        throw new Error("bad-salt-length: the vault salt must be 16 bytes");
      },
    });
    expect(runEmailVaultJob(core, seal)).toEqual({ kind: "error", code: "bad-salt-length" });
    expect(vaultErrorCode(new Error("Something Odd happened"))).toBe("unknown");
    expect(vaultErrorCode("nope")).toBe("unknown");
  });
});

describe("running a job off the main thread", () => {
  afterEach(() => {
    setEmailVaultRuntimeForTests({ workerFactory: null, core: () => loadWasm() });
  });

  /** A worker that answers each job through `answer`. */
  function fakeWorker(
    answer: (req: EmailVaultWorkerRequest) => Parameters<NonNullable<EmailVaultWorkerLike["onmessage"]>>[0]["data"],
  ): { factory: () => EmailVaultWorkerLike; state: { posted: number; terminated: number } } {
    const state = { posted: 0, terminated: 0 };
    return {
      state,
      factory: () => {
        const worker: EmailVaultWorkerLike = {
          onmessage: null,
          onerror: null,
          postMessage(message) {
            state.posted += 1;
            queueMicrotask(() => worker.onmessage?.({ data: answer(message) }));
          },
          terminate() {
            state.terminated += 1;
          },
        };
        return worker;
      },
    };
  }

  it("uses the worker, and terminates it when it answered", async () => {
    let mainThread = 0;
    const w = fakeWorker((req) => ({ id: req.id, kind: "result", result: { kind: "opened", phrase: PHRASE } }));
    setEmailVaultRuntimeForTests({
      workerFactory: w.factory,
      core: async () => {
        mainThread += 1;
        return fakeCore().core;
      },
    });
    expect(await runEmailVault(open("name@example.com"))).toEqual({ kind: "opened", phrase: PHRASE });
    expect(w.state).toEqual({ posted: 1, terminated: 1 });
    // The main thread did none of the work.
    expect(mainThread).toBe(0);
  });

  it("falls back to the main thread when the worker cannot load the core", async () => {
    const w = fakeWorker((req) => ({ id: req.id, kind: "unavailable" }));
    setEmailVaultRuntimeForTests({ workerFactory: w.factory, core: async () => fakeCore().core });
    expect(await runEmailVault(open("name@example.com"))).toEqual({ kind: "opened", phrase: PHRASE });
    expect(w.state.terminated).toBe(1);
  });

  it("falls back when a worker cannot be started at all", async () => {
    setEmailVaultRuntimeForTests({
      workerFactory: () => {
        throw new Error("no workers here");
      },
      core: async () => fakeCore().core,
    });
    expect((await runEmailVault(seal)).kind).toBe("sealed");
  });

  it("without workers the job still runs, after a yield", async () => {
    setEmailVaultRuntimeForTests({ workerFactory: null, core: async () => fakeCore().core });
    expect(await runEmailVault(open("other@example.com"))).toEqual({ kind: "not-opened" });
  });
});
