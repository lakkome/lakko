// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { EmailVaultRequest } from "./email-vault-job";
import {
  resealAttemptKey,
  resealNeeded,
  resealOutcome,
  RESEAL_SALT_LEN,
  runReseal,
  type ResealPorts,
} from "./reseal";

describe("when a device puts the vault back by itself", () => {
  it("the email is on, its vault is gone, no phrase is saved, and the device holds the address", () => {
    expect(resealNeeded(true, false, false, true)).toBe(true);
  });

  it("not otherwise", () => {
    // No address on this device: "Add your email again" instead.
    expect(resealNeeded(true, false, false, false)).toBe(false);
    // The vault is there.
    expect(resealNeeded(true, true, false, true)).toBe(false);
    // A phrase-protected account never has a vault.
    expect(resealNeeded(true, false, true, true)).toBe(false);
    // No stored key at the server (after a recall): the ordinary add state.
    expect(resealNeeded(false, false, false, true)).toBe(false);
  });
});

describe("reading PUT /recovery-email/vault", () => {
  it("stored", () => {
    expect(resealOutcome(200, null)).toBe("done");
  });

  it("already there, or a phrase saved meanwhile, is as good as done: read the status again", () => {
    expect(resealOutcome(409, "vault-exists")).toBe("reread");
    expect(resealOutcome(409, "phrase-saved")).toBe("reread");
  });

  it("an address that is not the account's any more is cleared", () => {
    expect(resealOutcome(403, "address-mismatch")).toBe("clear-address");
  });

  it("everything else waits for the next landing", () => {
    expect(resealOutcome(null, null)).toBe("later");
    expect(resealOutcome(429, "too-many-attempts")).toBe("later");
    expect(resealOutcome(503, "recovery-email-disabled")).toBe("later");
    expect(resealOutcome(409, "no-recovery-email")).toBe("later");
    expect(resealOutcome(409, "handle-mismatch")).toBe("later");
    expect(resealOutcome(409, "username-required")).toBe("later");
    expect(resealOutcome(400, "address-invalid")).toBe("later");
    expect(resealOutcome(403, "something-else")).toBe("later");
  });
});

describe("one attempt", () => {
  const HANDLE = { bytes: new Uint8Array(32).fill(5), wire: "h".repeat(43) };
  const SALT = new Uint8Array(RESEAL_SALT_LEN).fill(8);
  const SEALED = new Uint8Array(44).fill(2);

  function ports(over: Partial<ResealPorts> = {}): {
    ports: ResealPorts;
    jobs: EmailVaultRequest[];
    puts: Parameters<ResealPorts["put"]>[0][];
  } {
    const jobs: EmailVaultRequest[] = [];
    const puts: Parameters<ResealPorts["put"]>[0][] = [];
    return {
      jobs,
      puts,
      ports: {
        phrase: async () => "one two three",
        handle: async () => HANDLE,
        randomSalt: () => SALT,
        seal: async (job) => {
          jobs.push(job);
          return { kind: "sealed", sealed: SEALED };
        },
        put: async (body) => {
          puts.push(body);
          return { status: 200, errorCode: null };
        },
        ...over,
      },
    };
  }

  it("seals the CURRENT phrase under the handle, the held address and a fresh salt, and sends it once", async () => {
    const p = ports();
    expect(await runReseal(p.ports, "name@example.com")).toBe("done");
    expect(p.jobs).toEqual([
      { kind: "seal", phrase: "one two three", loginHandle: HANDLE.bytes, address: "name@example.com", vaultSalt: SALT },
    ]);
    expect(p.puts).toEqual([
      { handle: HANDLE.wire, address: "name@example.com", vaultSalt: SALT, sealed: SEALED },
    ]);
  });

  it("sends nothing when this device holds no phrase, or the seal failed", async () => {
    const noPhrase = ports({ phrase: async () => null });
    expect(await runReseal(noPhrase.ports, "name@example.com")).toBe("later");
    expect(noPhrase.puts).toEqual([]);
    const failed = ports({ seal: async () => ({ kind: "error", code: "kdf" }) });
    expect(await runReseal(failed.ports, "name@example.com")).toBe("later");
    expect(failed.puts).toEqual([]);
    const badSalt = ports({ randomSalt: () => new Uint8Array(8) });
    expect(await runReseal(badSalt.ports, "name@example.com")).toBe("later");
    expect(badSalt.puts).toEqual([]);
  });

  it("never sends twice, whatever the answer", async () => {
    for (const answer of [
      { status: 429, errorCode: "too-many-attempts" },
      { status: null, errorCode: null },
      { status: 409, errorCode: "vault-exists" },
      { status: 403, errorCode: "address-mismatch" },
    ]) {
      const p = ports({ put: async () => answer });
      let calls = 0;
      const counted: ResealPorts = { ...p.ports, put: async (b) => ((calls += 1), p.ports.put(b)) };
      await runReseal(counted, "name@example.com");
      expect(calls).toBe(1);
    }
  });

  it("maps the answer", async () => {
    expect(await runReseal(ports({ put: async () => ({ status: 409, errorCode: "phrase-saved" }) }).ports, "a@b.c")).toBe("reread");
    expect(await runReseal(ports({ put: async () => ({ status: 403, errorCode: "address-mismatch" }) }).ports, "a@b.c")).toBe("clear-address");
  });
});

describe("one attempt per account, key generation and address in a page", () => {
  it("the same reading is the same key; a new generation or a new address is a new one", () => {
    const k = resealAttemptKey("acct", "3", "a@b.c");
    expect(resealAttemptKey("acct", "3", "a@b.c")).toBe(k);
    expect(resealAttemptKey("acct", "4", "a@b.c")).not.toBe(k);
    expect(resealAttemptKey("acct", "3", "x@b.c")).not.toBe(k);
    expect(resealAttemptKey("other", "3", "a@b.c")).not.toBe(k);
  });
});
