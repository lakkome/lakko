// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * F3 of the ADR-0099 security pass: the identity proof is made by the REAL
 * core, verifies as an Ed25519 signature by the recorded key over the
 * documented message, and rides each of the four auth calls that record the
 * account's identity key (genesis attest, key reset finish, key rotate
 * finish, recall finish), against a scripted `fetch`. The OPAQUE halves are
 * faked (no server to answer them here); everything else is the real wasm.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import * as real from "../../wasm/lakko_ffi_wasm";
import initWasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", async () => {
  const actual = await vi.importActual<typeof import("../../wasm/lakko_ffi_wasm")>("../../wasm/lakko_ffi_wasm");
  const fakeReg = {
    requestB64: "cmVnLXJlcXVlc3Q=",
    finish: () => ({ uploadB64: "cmVnLXVwbG9hZA==" }),
    free: () => undefined,
  };
  const fakeLogin = {
    requestB64: "bG9naW4tcmVxdWVzdA==",
    finish: () => ({ finalizeB64: "bG9naW4tZmluaXNo" }),
    free: () => undefined,
  };
  const wasm = {
    ...actual,
    OpaqueRegister: { startRecovery: () => fakeReg, startPassword: () => fakeReg },
    OpaqueLogin: { startRecovery: () => fakeLogin, startPassword: () => fakeLogin },
  };
  return {
    loadWasm: async () => wasm,
    wasmSync: () => wasm,
    wasmIfLoaded: () => wasm,
  };
});

import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "./account-id";
import { AuthClient } from "./client";
import { IDENTITY_PROOF_LEN, identityProofB64, identityProofOfSeedB64 } from "./identity-proof";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ACCOUNT_ID = new Uint8Array(16).map((_, i) => i + 1);
const ATTESTATION_B64URL = "A".repeat(194); // 145 bytes

function newSeed(): Uint8Array {
  const m = real.RecoveryMnemonic.generate();
  try {
    return m.toSeed();
  } finally {
    m.free();
  }
}

function pubkeyOf(seed: Uint8Array): Uint8Array {
  const id = real.Identity.fromSeed(seed);
  try {
    return id.signaturePublicKey;
  } finally {
    id.free();
  }
}

/** Whether `proof` is an Ed25519 signature by `pubkey` over
 *  `"lakko/auth/identity-proof/v1" ‖ 0x00 ‖ accountId ‖ pubkey`. */
async function verifies(proof: Uint8Array, accountId: Uint8Array, pubkey: Uint8Array): Promise<boolean> {
  const domain = new TextEncoder().encode("lakko/auth/identity-proof/v1");
  const message = new Uint8Array(domain.length + 1 + 16 + 32);
  message.set(domain, 0);
  message[domain.length] = 0;
  message.set(accountId, domain.length + 1);
  message.set(pubkey, domain.length + 17);
  const key = await crypto.subtle.importKey("raw", pubkey.slice(), { name: "Ed25519" }, false, ["verify"]);
  return crypto.subtle.verify({ name: "Ed25519" }, key, proof.slice(), message);
}

describe("the identity proof", () => {
  it("is a signature by the recorded key over the account id and that key", async () => {
    const seed = newSeed();
    const pubkey = pubkeyOf(seed);
    const id = real.Identity.fromSeed(seed);
    const proof = base64UrlDecodeNoPad(identityProofB64(real, id, ACCOUNT_ID));
    id.free();
    expect(proof).toHaveLength(IDENTITY_PROOF_LEN);
    expect(await verifies(proof, ACCOUNT_ID, pubkey)).toBe(true);
    const other = new Uint8Array(16).fill(9);
    expect(await verifies(proof, other, pubkey)).toBe(false);
    expect(await verifies(proof, ACCOUNT_ID, pubkeyOf(newSeed()))).toBe(false);
  });

  it("is refused from a seed whose key is not the key being recorded", () => {
    const seed = newSeed();
    expect(() => identityProofOfSeedB64(real, seed, ACCOUNT_ID, pubkeyOf(newSeed()))).toThrow();
    expect(identityProofOfSeedB64(real, seed, ACCOUNT_ID, pubkeyOf(seed))).toMatch(/^[A-Za-z0-9_-]{86}$/);
  });

  it("is refused for an account id of the wrong length", () => {
    const seed = newSeed();
    expect(() => identityProofOfSeedB64(real, seed, new Uint8Array(15), pubkeyOf(seed))).toThrow();
  });
});

interface Sent {
  readonly path: string;
  readonly body: Record<string, unknown>;
}

describe("each call that records the identity key carries the proof", () => {
  let sent: Sent[];
  const realFetch = globalThis.fetch;

  beforeEach(() => {
    sent = [];
    globalThis.fetch = (async (url: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(url), "http://x").pathname;
      const text = typeof init?.body === "string" ? init.body : "";
      sent.push({ path, body: text.length > 0 ? (JSON.parse(text) as Record<string, unknown>) : {} });
      let body: unknown;
      if (path.endsWith("/start") && path.includes("/recall/")) {
        body = {
          session_id: "attempt-1",
          login_response: "lr",
          password_response: "pr",
          recovery_response: "rr",
          server_nonce_b64: base64UrlEncodeNoPad(new Uint8Array(32).fill(3)),
        };
      } else if (path.endsWith("/start")) {
        body = { recovery_response: "rr" };
      } else if (path.endsWith("/genesis/attest")) {
        body = { genesis_attest_b64: base64UrlEncodeNoPad(new Uint8Array(153)) };
      } else {
        body = {
          key_reset_attest_b64: ATTESTATION_B64URL,
          session_token: "v3.token",
          session_expires_at_ms: Date.now() + 900_000,
          refresh_token: "r2.refresh",
          refresh_expires_at_ms: Date.now() + 2_592_000_000,
          key_generation: 4,
          fresh_grant: "k1.grant",
          fresh_grant_expires_at_ms: Date.now() + 600_000,
        };
      }
      return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
    }) as typeof fetch;
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  async function expectProof(path: string, pubkey: Uint8Array): Promise<void> {
    const call = sent.find((s) => s.path.endsWith(path));
    expect(call).toBeDefined();
    const proofB64 = call?.body.identity_proof_b64;
    expect(typeof proofB64).toBe("string");
    expect(await verifies(base64UrlDecodeNoPad(proofB64 as string), ACCOUNT_ID, pubkey)).toBe(true);
  }

  it("genesis attest", async () => {
    const seed = newSeed();
    const id = real.Identity.fromSeed(seed);
    try {
      await new AuthClient("http://auth.test").genesisAttest("v3.token", id, ACCOUNT_ID);
    } finally {
      id.free();
    }
    await expectProof("/auth/genesis/attest", pubkeyOf(seed));
  });

  it("key reset finish (and not its start)", async () => {
    const seed = newSeed();
    await new AuthClient("http://auth.test").resetAccountKeys("v3.token", "k1.grant", seed, pubkeyOf(seed), ACCOUNT_ID);
    await expectProof("/account/keys/reset/finish", pubkeyOf(seed));
    expect(sent.find((s) => s.path.endsWith("/keys/reset/start"))?.body.identity_proof_b64).toBeUndefined();
  });

  it("key rotate finish", async () => {
    const seed = newSeed();
    await new AuthClient("http://auth.test").rotateAccountKeys({
      sessionToken: "v3.token",
      freshGrant: "k1.grant",
      newSeed: seed,
      newSignaturePubkey: pubkeyOf(seed),
      accountId: ACCOUNT_ID,
      proof: new Uint8Array(145),
      revokeDeviceIds: [],
    });
    await expectProof("/account/keys/rotate/finish", pubkeyOf(seed));
  });

  it("recall finish, and the same bytes on a re-signed resend", async () => {
    const seed = newSeed();
    const client = new AuthClient("http://auth.test");
    const prepared = await client.recallPrepare({
      accountId: ACCOUNT_ID,
      oldAccountSeed: new Uint8Array(32).fill(5),
      anchorSeq: 1,
      oldPassword: new TextEncoder().encode("old"),
      newPassword: new TextEncoder().encode("new"),
      newSeed: seed,
      newSignaturePubkey: pubkeyOf(seed),
    });
    await client.recallFinish(prepared);
    await expectProof("/account/recall/finish", pubkeyOf(seed));
    expect(prepared.resign(1_000).identity_proof_b64).toBe(prepared.request.identity_proof_b64);
  });

  it("nothing is sent when the seed's key is not the key being recorded", async () => {
    await expect(
      new AuthClient("http://auth.test").resetAccountKeys(
        "v3.token",
        "k1.grant",
        newSeed(),
        pubkeyOf(newSeed()),
        ACCOUNT_ID,
      ),
    ).rejects.toThrow();
    expect(sent).toEqual([]);
  });
});

describe("auth's refusals of a proof take the ordinary failure path", () => {
  it("recall reads them as a plain error, the genesis mint as a retry", async () => {
    const { recallAttemptFailure } = await import("./recall");
    const { genesisMintOutcome } = await import("./session-device");
    for (const [status, errorCode] of [
      [400, "identity-proof-invalid"],
      [409, "identity-key-taken"],
    ] as const) {
      expect(recallAttemptFailure({ wrongPasswordLocally: false, status, errorCode, retryAfterSecs: null })).toEqual({
        kind: "error",
      });
      expect(genesisMintOutcome({ ok: false, status, errorCode })).toBe("retry");
    }
  });
});
