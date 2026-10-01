// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0056 — a fresh device names the account it wants by the login handle
 * of the typed username; nothing before a session carries the account id.
 *
 *   - the pairing payload carries the handle of the typed name;
 *   - the confirming side decodes with its OWN handle, and a payload for
 *     another name reads as the wrong-account copy;
 *   - the device request carries the handle alone, never an account id
 *     and never the pairing payload (that travels sealed, device-link.ts);
 *   - the grant redeem carries the grant alone, and the account id the fresh
 *     device ends up with is the one the redeem answered;
 *   - no request leaves the device while the fresh device prepares (no
 *     discovery lookup);
 *   - the anonymous join binds the Welcome to the redeemed account, and a
 *     Welcome for another account is refused.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { AuthClient } from "../auth/client";
import { loginHandleWith } from "../auth/login-handle";
import { deviceRestoreErrorMessage } from "../auth/auth-error-copy";
import { KeyedError } from "../l10n/keyed-error";
import {
  PAIRING_UNSUPPORTED,
  PAIRING_WRONG_ACCOUNT,
  buildPairingQrPayloadWith,
  pairingDecodeError,
  parsePairingQrPayloadWith,
  type PairingCore,
} from "./pairing";
import {
  RECOVERY_EXPORTER_KEY_LENGTH,
  RECOVERY_EXPORTER_LABEL,
  buildRecoveryPayload,
} from "./recovery-payload";

// The DS polls are the only network the join touches besides the redeem;
// they are replaced so the test drives exactly one recovery row.
const recoveryRow = vi.hoisted(() => ({ sealed: null as Uint8Array | null }));
vi.mock("./recovery-payload", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./recovery-payload")>();
  return {
    ...actual,
    pollForRecoveryPayload: vi.fn(async () =>
      recoveryRow.sealed === null
        ? { kind: "timeout" as const }
        : { kind: "received" as const, sealedBlob: recoveryRow.sealed },
    ),
  };
});
vi.mock("./welcome-courier", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./welcome-courier")>();
  return { ...actual, pollForHistoryBundle: vi.fn(async () => ({ kind: "timeout" as const })) };
});
vi.mock("./state-bundle", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./state-bundle")>();
  return { ...actual, pollForStateBundle: vi.fn(async () => ({ kind: "timeout" as const })) };
});

import {
  JOINED_WRONG_ACCOUNT,
  prepareAnonymousJoinFlow,
  runJoinerRecovery,
} from "./orchestrator";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  recoveryRow.sealed = null;
});

interface Seen {
  readonly url: string;
  readonly body: Record<string, unknown> | null;
}

function stubFetch(answer: (url: string) => { status: number; json: unknown }): Seen[] {
  const seen: Seen[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, reqInit?: RequestInit) => {
      const url = String(input);
      seen.push({
        url,
        body:
          typeof reqInit?.body === "string"
            ? (JSON.parse(reqInit.body) as Record<string, unknown>)
            : null,
      });
      const a = answer(url);
      return new Response(JSON.stringify(a.json), { status: a.status });
    }),
  );
  return seen;
}

function b64url(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const alice = (): Uint8Array => loginHandleWith(wasm, "alice").bytes;
const bob = (): Uint8Array => loginHandleWith(wasm, "bob").bytes;
const rendezvous = (): Uint8Array => new Uint8Array(32).fill(9);

function keyPackage(): Uint8Array {
  const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(4));
  try {
    return id.createKeyPackage();
  } finally {
    id.free();
  }
}

describe("the pairing payload names the account by handle", () => {
  it("carries the handle of the typed name and decodes only under it", () => {
    const kp = keyPackage();
    const wire = buildPairingQrPayloadWith(wasm, alice(), kp, rendezvous());
    const decoded = parsePairingQrPayloadWith(wasm, wire, alice());
    expect(decoded.keyPackage).toEqual(kp);
    expect(decoded.rendezvousPseudonym).toEqual(rendezvous());
    // The same name typed differently is the same account.
    expect(() =>
      parsePairingQrPayloadWith(wasm, wire, loginHandleWith(wasm, "  @Alice ").bytes),
    ).not.toThrow();
  });

  it("a payload for another name reads as the wrong-account copy", () => {
    const wire = buildPairingQrPayloadWith(wasm, alice(), keyPackage(), rendezvous());
    expect(() => parsePairingQrPayloadWith(wasm, wire, bob())).toThrow(PAIRING_WRONG_ACCOUNT);
  });

  it("the confirming side passes its own handle to the core", () => {
    const seen: Uint8Array[] = [];
    const spy: PairingCore = {
      encodePairingQr: () => new Uint8Array(),
      decodePairingQr: (_bytes, expected) => {
        seen.push(expected);
        return { keyPackage: new Uint8Array([1]), rendezvousPseudonym: rendezvous() };
      },
    };
    parsePairingQrPayloadWith(spy, new Uint8Array([1, 2, 3]), alice());
    expect(seen).toEqual([alice()]);
  });

  it("an account id in the handle's place never leaves the device", () => {
    expect(() =>
      buildPairingQrPayloadWith(wasm, new Uint8Array(16), keyPackage(), rendezvous()),
    ).toThrow(/32 bytes/);
    expect(() => parsePairingQrPayloadWith(wasm, new Uint8Array([1]), new Uint8Array(16))).toThrow(
      /32 bytes/,
    );
  });

  it("an unsupported version reads as the update copy", () => {
    const err = pairingDecodeError(
      new Error("unsupported: pairing payload version is not supported by this client"),
    );
    expect(err.message).toBe(PAIRING_UNSUPPORTED);
    expect(pairingDecodeError(new Error("PairingQr decode: bad")).message).toBe(
      "PairingQr decode: bad",
    );
  });
});

describe("the auth routes of the fresh device", () => {
  it("the device request carries the handle and no account id", async () => {
    const seen = stubFetch(() => ({
      status: 200,
      json: { request_id_b64: "r", expires_in_secs: 600 },
    }));
    await new AuthClient("/api/auth").deviceRequestCreate(alice());
    expect(seen.map((s) => s.url)).toEqual(["/api/auth/device/request"]);
    // ADR-0049 amended 2026-09-28: the handle alone; the pairing payload
    // travels later, sealed inside the device link.
    expect(seen[0]?.body).toEqual({ handle: b64url(alice()) });
  });

  it("the redeem sends the grant alone and returns the account id it answered", async () => {
    const account = new Uint8Array(16).fill(0x42);
    const seen = stubFetch(() => ({
      status: 200,
      json: {
        account_id_b64: b64url(account),
        session_token: "v1.tok",
        session_expires_at_ms: 123,
      },
    }));
    const result = await new AuthClient("/api/auth").deviceAuthorizeRedeem("Z3JhbnQ");
    expect(seen.map((s) => s.url)).toEqual(["/api/auth/device/authorize/redeem"]);
    expect(seen[0]?.body).toEqual({ grant_token_b64: "Z3JhbnQ" });
    expect(result.accountId).toEqual(account);
    expect(result.sessionToken).toBe("v1.tok");
  });
});

describe("the joiner's recovery", () => {
  const KEY = new Uint8Array(RECOVERY_EXPORTER_KEY_LENGTH).fill(3);
  const exporter = {
    exportSecret: (_i: WasmIdentity, label: string, _c: Uint8Array, length: number): Uint8Array => {
      expect(label).toBe(RECOVERY_EXPORTER_LABEL);
      return KEY.slice(0, length);
    },
  };
  const grant = new Uint8Array(32).fill(0x11);
  const seed = new Uint8Array(64).fill(0x22);

  it("learns the account id from the redeem, which carries only the grant", async () => {
    recoveryRow.sealed = wasm.sealWithKey(KEY, buildRecoveryPayload(grant, seed));
    const account = new Uint8Array(16).fill(0x42);
    const redeem = vi.fn(async (_g: string) => ({
      accountId: account,
      sessionToken: "v1.tok",
      sessionExpiresAtMs: 1,
    }));
    const outcome = await runJoinerRecovery({
      exporter,
      identity: {} as WasmIdentity,
      rendezvous: rendezvous(),
      welcomeCursor: 0,
      expectedAccountIdBytes: null,
      redeem,
    });
    expect(redeem).toHaveBeenCalledWith(b64url(grant));
    expect(outcome?.accountIdBytes).toEqual(account);
    expect(outcome?.sessionToken).toBe("v1.tok");
    expect(outcome?.phraseSeed).toEqual(seed);
  });

  it("drops a redeemed session that answers for another account than the known one", async () => {
    recoveryRow.sealed = wasm.sealWithKey(KEY, buildRecoveryPayload(grant, seed));
    const known = new Uint8Array(16).fill(0x01);
    const outcome = await runJoinerRecovery({
      exporter,
      identity: {} as WasmIdentity,
      rendezvous: rendezvous(),
      welcomeCursor: 0,
      expectedAccountIdBytes: known,
      redeem: async () => ({
        accountId: new Uint8Array(16).fill(0x02),
        sessionToken: "v1.other",
        sessionExpiresAtMs: 1,
      }),
    });
    expect(outcome?.sessionToken).toBeNull();
    expect(outcome?.accountIdBytes).toEqual(known);
  });
});

describe("the anonymous join (the no-password restore)", () => {
  const ACCOUNT = new Uint8Array(16).fill(0x42);

  /** The confirming device: founds the DeviceGroup of ACCOUNT, adds the
   *  device the payload names (decoded under its OWN handle), and seals a
   *  recovery row under the post-add exporter. */
  function confirmOnOtherDevice(payload: Uint8Array): { welcome: Uint8Array; seed: Uint8Array } {
    const founderIdentity = wasm.Identity.fromSeed(new Uint8Array(32).fill(7));
    const founder = wasm.DeviceGroup.createFounder(founderIdentity, ACCOUNT);
    const decoded = parsePairingQrPayloadWith(wasm, payload, alice());
    const added = founder.addDeviceFromWire(founderIdentity, decoded.keyPackage);
    const key = founder.exportSecret(
      founderIdentity,
      RECOVERY_EXPORTER_LABEL,
      decoded.rendezvousPseudonym,
      RECOVERY_EXPORTER_KEY_LENGTH,
    );
    const seed = new Uint8Array(64).fill(0x22);
    recoveryRow.sealed = wasm.sealWithKey(key, buildRecoveryPayload(new Uint8Array(32).fill(0x11), seed));
    const welcome = added.welcome;
    founder.free();
    founderIdentity.free();
    return { welcome, seed };
  }

  it("prepares with no request at all, then binds to the account the redeem answered", async () => {
    const seen = stubFetch(() => ({
      status: 200,
      json: { account_id_b64: b64url(ACCOUNT), session_token: "v1.tok", session_expires_at_ms: 9 },
    }));
    const session = await prepareAnonymousJoinFlow({ loginHandle: alice() });
    // No discovery lookup, nothing at all, before the session.
    expect(seen).toHaveLength(0);
    // The payload is the typed name's: another name's device refuses it.
    expect(() => parsePairingQrPayloadWith(wasm, session.qrPayload, bob())).toThrow(
      PAIRING_WRONG_ACCOUNT,
    );

    const { welcome } = confirmOnOtherDevice(session.qrPayload);
    const outcome = await session.accept(welcome, { welcomeCursor: 1 });
    expect(seen.map((s) => s.url)).toEqual(["/api/auth/device/authorize/redeem"]);
    expect(Object.keys(seen[0]?.body ?? {})).toEqual(["grant_token_b64"]);
    expect(outcome.recovery?.accountIdBytes).toEqual(ACCOUNT);
    expect(outcome.recovery?.sessionToken).toBe("v1.tok");
    session.dispose();
  });

  it("refuses a Welcome that is not the DeviceGroup of the redeemed account", async () => {
    stubFetch(() => ({
      status: 200,
      json: {
        account_id_b64: b64url(new Uint8Array(16).fill(0x43)),
        session_token: "v1.tok",
        session_expires_at_ms: 9,
      },
    }));
    const session = await prepareAnonymousJoinFlow({ loginHandle: alice() });
    const { welcome } = confirmOnOtherDevice(session.qrPayload);
    const err = await session.accept(welcome, { welcomeCursor: 1 }).then(
      () => null,
      (e: unknown) => e,
    );
    // ADR-0061 section 6 — keyed, so the screen renders it in the current
    // language; the English rendering is the constant.
    expect(err).toBeInstanceOf(KeyedError);
    expect((err as KeyedError).key).toBe("device_link.error.wrong_account");
    expect((err as Error).message).toBe(JOINED_WRONG_ACCOUNT);
    expect(deviceRestoreErrorMessage(err)).toBe(JOINED_WRONG_ACCOUNT);
    session.dispose();
  });
});

describe("no lookup before the session in the restore screens", () => {
  it("DeviceRecover never asks discovery", () => {
    const src = readFileSync(
      fileURLToPath(new URL("../auth/recover/DeviceRecover.tsx", import.meta.url)),
      "utf8",
    );
    expect(src).not.toMatch(/DiscoveryClient|\.lookup\(/);
    expect(src).toMatch(/nameTargetFor\(/);
    expect(src).toMatch(/requestHandle=/);
  });
});
