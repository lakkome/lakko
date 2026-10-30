// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - device B of an account proposes an Add and device A of
 * the same account sweeps: A holds no grant for the person and reaches them
 * only through B's envelope. Real intake, real reach-write planning.
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

const { reachSession, startReachSession, stopReachSession } = await import("./reach-service");
const { intakeScopedEnvelope } = await import("./scoped-intake");
const { openCommitterEnvelope } = await import("./found-grant");
const { openWelcomeBinding } = await import("./mailbox-write");
const { delegationsFor } = await import("./received-grants");

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64");
const hex = (bytes: Uint8Array): string => Buffer.from(bytes).toString("hex");
const person = (fill: number): wasm.Identity => wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));

const ACCOUNT = "b3duLWRldmljZQ==";
const GROUP = "31".repeat(32);
const OTHER_GROUP = "32".repeat(32);

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

let account: wasm.Identity;
let carol: wasm.Identity;

beforeEach(async () => {
  rows.clear();
  account = person(0x51);
  carol = person(0x53);
  await startReachSession(ACCOUNT, account.ed25519Seed, account.signaturePublicKey, new Uint8Array(32).fill(0x55));
});

afterEach(() => {
  stopReachSession();
  account.free();
  carol.free();
});

/** The envelope of an Add of a person found in Everyone (no delegation). */
function envelope(): Uint8Array {
  const env = new wasm.CommitterDelegationEnvelope(carol.signaturePublicKey, wasm.signMailboxBinding(carol), []);
  try {
    return wasm.composeCommitterDelegationEnvelope(env);
  } finally {
    env.free();
  }
}

/** The same account's other device posted it, in `scopeHex`. */
function fromOtherDevice(scopeHex: string): Promise<void> {
  return intakeScopedEnvelope({
    kind: "committer-delegation",
    payload: envelope(),
    senderKeyB64: b64(account.signaturePublicKey),
    oneToOne: false,
    scopeHex,
  });
}

const grants = () => {
  const session = reachSession();
  if (session === null) throw new Error("no session");
  return session.receivedGrants();
};

describe("a committer envelope from another device of this account", () => {
  it("is kept, and is what lets this device's sweep reach the person", async () => {
    const added = hex(carol.signaturePublicKey);
    expect(await openWelcomeBinding([added], GROUP)).toBeNull();
    await fromOtherDevice(GROUP);
    expect(delegationsFor(grants(), added)).toHaveLength(1);
    expect(await openWelcomeBinding([added], GROUP)).not.toBeNull();
    expect(await openWelcomeBinding([added], OTHER_GROUP)).toBeNull();
  });

  it("does not take the place of the envelope this device posted itself", async () => {
    const binding = b64(wasm.signMailboxBinding(carol));
    const own = await openCommitterEnvelope(GROUP, carol.signaturePublicKey, binding);
    expect(own).not.toBeNull();
    await fromOtherDevice(OTHER_GROUP);
    // Re-sending the own envelope still finds it, for its own group.
    expect(await openCommitterEnvelope(GROUP, carol.signaturePublicKey, null)).toEqual(own);
    expect(await openWelcomeBinding([hex(carol.signaturePublicKey)], GROUP)).not.toBeNull();
    expect(await openWelcomeBinding([hex(carol.signaturePublicKey)], OTHER_GROUP)).not.toBeNull();
  });

  it("is not re-sent as this device's own", async () => {
    await fromOtherDevice(GROUP);
    expect(await openCommitterEnvelope(GROUP, carol.signaturePublicKey, null)).toBeNull();
  });
});
