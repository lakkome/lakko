// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - a committer that merged the commit and owes the Welcome
 * retries when a fresh committer envelope for that person arrives: its
 * delegation may have expired. Real reach session and real intake, so the test
 * fails if an envelope stops being a change the owed-Welcome retry hears about.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

const rows = new Map<string, string>();

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));
vi.mock("./reach-store", () => ({
  loadReachRow: async (account: string, kind: string) => rows.get(`${account}/${kind}`) ?? null,
  saveReachRow: async (account: string, kind: string, value: string) => {
    rows.set(`${account}/${kind}`, value);
  },
}));
vi.mock("../contacts/address-resolver", () => ({
  resolveAccountKeyHex: (hex: string) => hex,
  personKeysHex: (hex: string) => [hex],
}));

const { onReceivedGrantsChanged, startReachSession, stopReachSession } = await import("./reach-service");
const { intakeScopedEnvelope } = await import("./scoped-intake");
const { retryOwedWelcomes } = await import("../chat/owed-welcomes");

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64");
const person = (fill: number): wasm.Identity => wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));

const ACCOUNT = "d2FrZS1hY2NvdW50";
const OWING = "b3dpbmc=";
const QUIET = "cXVpZXQ=";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

let committer: wasm.Identity;
let proposer: wasm.Identity;
let carol: wasm.Identity;
let unsubscribe: () => void;
let swept: string[];

beforeEach(async () => {
  rows.clear();
  committer = person(0x41);
  proposer = person(0x42);
  carol = person(0x43);
  swept = [];
  await startReachSession(
    ACCOUNT,
    committer.ed25519Seed,
    committer.signaturePublicKey,
    new Uint8Array(32).fill(0x55),
  );
  unsubscribe = onReceivedGrantsChanged(() => {
    void retryOwedWelcomes(
      [QUIET, OWING],
      async (g) => (g === OWING ? [{ sweepWelcomesOwed: true }] : [{}]),
      async (g) => {
        swept.push(g);
      },
    );
  });
});

afterEach(() => {
  unsubscribe();
  stopReachSession();
  committer.free();
  proposer.free();
  carol.free();
});

function envelope(): Uint8Array {
  const env = new wasm.CommitterDelegationEnvelope(carol.signaturePublicKey, wasm.signMailboxBinding(carol), []);
  try {
    return wasm.composeCommitterDelegationEnvelope(env);
  } finally {
    env.free();
  }
}

function arrive(senderKey: Uint8Array | undefined): Promise<void> {
  return intakeScopedEnvelope({
    kind: "committer-delegation",
    payload: envelope(),
    senderKeyB64: senderKey === undefined ? undefined : b64(senderKey),
    oneToOne: false,
    scopeHex: "31".repeat(32),
  });
}

describe("an arriving committer envelope", () => {
  it("wakes the groups that owe a Welcome, and only those", async () => {
    await arrive(proposer.signaturePublicKey);
    await vi.waitFor(() => expect(swept).toEqual([OWING]));
  });

  it("wakes them again for a re-sent envelope", async () => {
    await arrive(proposer.signaturePublicKey);
    await vi.waitFor(() => expect(swept).toHaveLength(1));
    await arrive(proposer.signaturePublicKey);
    await vi.waitFor(() => expect(swept).toHaveLength(2));
  });

  it("wakes them for an envelope from another device of this account", async () => {
    await arrive(committer.signaturePublicKey);
    await vi.waitFor(() => expect(swept).toEqual([OWING]));
  });

  it("wakes nothing when the envelope is dropped (its sender did not resolve)", async () => {
    await arrive(undefined);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(swept).toEqual([]);
  });
});
