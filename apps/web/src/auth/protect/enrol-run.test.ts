// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { EmailVaultRequest, EmailVaultResult } from "./email-vault-job";
import { EnrolSealError, finishEnrolment, type EnrolFinishPorts } from "./enrol-run";

const HANDLE = new Uint8Array(32).fill(7);
const SALT = new Uint8Array(16).fill(9);
const SEALED = new Uint8Array(44).fill(1);
const ADDRESS = "name@example.com";

class PhraseSaved extends Error {}

function ports(over: Partial<EnrolFinishPorts> = {}): {
  ports: EnrolFinishPorts;
  jobs: EmailVaultRequest[];
  finished: (Uint8Array | null)[];
} {
  const jobs: EmailVaultRequest[] = [];
  const finished: (Uint8Array | null)[] = [];
  return {
    jobs,
    finished,
    ports: {
      phrase: async () => "one two three",
      seal: async (job): Promise<EmailVaultResult> => {
        jobs.push(job);
        return { kind: "sealed", sealed: SEALED };
      },
      finish: async (sealed) => {
        finished.push(sealed);
        return { vault: sealed !== null };
      },
      isPhraseSaved: (err) => err instanceof PhraseSaved,
      ...over,
    },
  };
}
const input = (phraseSaved: boolean) => ({ phraseSaved, loginHandle: HANDLE, address: ADDRESS, vaultSalt: SALT });

describe("finishing the enrolment", () => {
  it("mail-protected: seals the phrase under the handle, the address and the start's salt, and sends it", async () => {
    const p = ports();
    expect(await finishEnrolment(p.ports, input(false))).toEqual({ vault: true });
    expect(p.jobs).toEqual([
      { kind: "seal", phrase: "one two three", loginHandle: HANDLE, address: ADDRESS, vaultSalt: SALT },
    ]);
    expect(p.finished).toEqual([SEALED]);
  });

  it("phrase-protected: seals nothing and sends none", async () => {
    let phraseRead = false;
    const p = ports({
      phrase: async () => {
        phraseRead = true;
        return "one two three";
      },
    });
    expect(await finishEnrolment(p.ports, input(true))).toEqual({ vault: false });
    expect(p.jobs).toEqual([]);
    expect(p.finished).toEqual([null]);
    // The phrase is not even read.
    expect(phraseRead).toBe(false);
  });

  it("a device that does not hold the phrase sends nothing at all", async () => {
    const p = ports({ phrase: async () => null });
    await expect(finishEnrolment(p.ports, input(false))).rejects.toBeInstanceOf(EnrolSealError);
    expect(p.finished).toEqual([]);
  });

  it("a seal that failed sends nothing at all", async () => {
    const p = ports({ seal: async () => ({ kind: "error", code: "kdf" }) });
    await expect(finishEnrolment(p.ports, input(false))).rejects.toMatchObject({ reason: "seal-failed" });
    expect(p.finished).toEqual([]);
  });

  it("a phrase saved on another device meanwhile: the same code is confirmed without a vault", async () => {
    const finished: (Uint8Array | null)[] = [];
    const p = ports({
      finish: async (sealed) => {
        finished.push(sealed);
        if (sealed !== null) throw new PhraseSaved();
        return { vault: false };
      },
    });
    expect(await finishEnrolment(p.ports, input(false))).toEqual({ vault: false });
    expect(finished).toEqual([SEALED, null]);
  });

  it("any other refusal is the caller's to word, and is not retried", async () => {
    let calls = 0;
    const p = ports({
      finish: async () => {
        calls += 1;
        throw new Error("code-refused");
      },
    });
    await expect(finishEnrolment(p.ports, input(false))).rejects.toThrow("code-refused");
    expect(calls).toBe(1);
  });
});
