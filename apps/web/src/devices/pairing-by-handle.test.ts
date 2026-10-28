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
import { WebDeviceCipher } from "../auth/device-cipher";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { AuthClient } from "../auth/client";
import type { SessionPair } from "../auth/session-pair";
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
// The state row the scanner published, when a test drives one.
const stateRow = vi.hoisted(() => ({ sealed: null as Uint8Array | null }));
vi.mock("./state-bundle", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./state-bundle")>();
  return {
    ...actual,
    pollForStateBundle: vi.fn(async () =>
      stateRow.sealed === null
        ? { kind: "timeout" as const }
        : { kind: "received" as const, sealed: stateRow.sealed },
    ),
  };
});

import {
  JOINED_WRONG_ACCOUNT,
  prepareAnonymousJoinFlow,
  runJoinerRecovery,
} from "./orchestrator";
import {
  STATE_EXPORTER_KEY_LENGTH,
  STATE_EXPORTER_LABEL,
  encodeStateBundle,
  loadKnownGroupIds,
  type StateBundleV2,
} from "./state-bundle";
import { loadAccepted } from "../contacts/local-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  recoveryRow.sealed = null;
  stateRow.sealed = null;
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
session_token: "v2.tok",
        session_expires_at_ms: 123,
        refresh_token: "r1.tok",
        refresh_expires_at_ms: 456,
        key_generation: 1,
      },
    }));
    const result = await new AuthClient("/api/auth").deviceAuthorizeRedeem("Z3JhbnQ");
    expect(seen.map((s) => s.url)).toEqual(["/api/auth/device/authorize/redeem"]);
    expect(seen[0]?.body).toEqual({ grant_token_b64: "Z3JhbnQ" });
    expect(result.accountId).toEqual(account);
    expect(result.session.sessionToken).toBe("v2.tok");
    expect(result.session.refreshToken).toBe("r1.tok");
    expect(result.session.keyGeneration).toBe(1n);
  });
});

const PAIR: SessionPair = {
  sessionToken: "v2.tok",
  sessionExpiresAtMs: 1,
  refreshToken: "r1.tok",
  refreshExpiresAtMs: 2,
  keyGeneration: 1n,
};

describe("the joiner's recovery", () => {
  const KEY = new Uint8Array(RECOVERY_EXPORTER_KEY_LENGTH).fill(3);
  const exporter = {
    exportSecret: (_i: WasmIdentity, label: string, _c: Uint8Array, length: number): Uint8Array => {
      expect(label).toBe(RECOVERY_EXPORTER_LABEL);
      return KEY.slice(0, length);
    },
  };
  const grant = new Uint8Array(32).fill(0x11);
  const seed = new Uint8Array(16).fill(0x22);

  it("learns the account id from the redeem, which carries only the grant", async () => {
    recoveryRow.sealed = wasm.sealWithKey(KEY, buildRecoveryPayload(grant, seed));
    const account = new Uint8Array(16).fill(0x42);
    const redeem = vi.fn(async (_g: string) => ({
      accountId: account,
      session: PAIR,
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
    expect(outcome?.session?.sessionToken).toBe("v2.tok");
    expect(outcome?.phraseEntropy).toEqual(seed);
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
        session: { ...PAIR, sessionToken: "v2.other" },
      }),
    });
    expect(outcome?.session).toBeNull();
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
    const seed = new Uint8Array(16).fill(0x22);
    recoveryRow.sealed = wasm.sealWithKey(key, buildRecoveryPayload(new Uint8Array(32).fill(0x11), seed));
    const welcome = added.welcome;
    founder.free();
    founderIdentity.free();
    return { welcome, seed };
  }

  it("prepares with no request at all, then binds to the account the redeem answered", async () => {
    const seen = stubFetch(() => ({
      status: 200,
      json: {
        account_id_b64: b64url(ACCOUNT),
        session_token: "v2.tok",
        session_expires_at_ms: 9,
        refresh_token: "r1.tok",
        refresh_expires_at_ms: 99,
        key_generation: 1,
      },
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
    expect(outcome.recovery?.session?.sessionToken).toBe("v2.tok");
    session.dispose();
  });

  it("refuses a Welcome that is not the DeviceGroup of the redeemed account", async () => {
    stubFetch(() => ({
      status: 200,
      json: {
        account_id_b64: b64url(new Uint8Array(16).fill(0x43)),
        session_token: "v2.tok",
        session_expires_at_ms: 9,
        refresh_token: "r1.tok",
        refresh_expires_at_ms: 99,
        key_generation: 1,
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

describe("the anonymous join applies the pairing state bundle", () => {
  const ACCOUNT = new Uint8Array(16).fill(0x42);
  const ACCOUNT_B64 = btoa(String.fromCharCode(...ACCOUNT));
  const GROUP = "Z3JvdXAtb25lLWlkLWJ5dGVzLTMyLWxvbmctIQ==";
  const BUNDLE: StateBundleV2 = {
    v: 2,
    contacts: [{ signaturePubkeyB64: "cGVlci1rZXk=", displayName: "Peer", acceptedAtMs: 5 }],
    pins: [],
    knownGroupIdsB64: [GROUP],
    ownPredecessors: [],
  };

  /** The confirming device, as in the suite above, publishing a state row. */
  function confirmWithState(payload: Uint8Array): Uint8Array {
    const founderIdentity = wasm.Identity.fromSeed(new Uint8Array(32).fill(7));
    const founder = wasm.DeviceGroup.createFounder(founderIdentity, ACCOUNT);
    const decoded = parsePairingQrPayloadWith(wasm, payload, alice());
    const added = founder.addDeviceFromWire(founderIdentity, decoded.keyPackage);
    // ADR-0050 section 2.9: the recovery row and the state bundle, each
    // sealed under its own post-add exporter, as `runScanFlow` publishes them.
    const recoveryKey = founder.exportSecret(
      founderIdentity,
      RECOVERY_EXPORTER_LABEL,
      decoded.rendezvousPseudonym,
      RECOVERY_EXPORTER_KEY_LENGTH,
    );
    recoveryRow.sealed = wasm.sealWithKey(
      recoveryKey,
      buildRecoveryPayload(new Uint8Array(32).fill(0x11), new Uint8Array(16).fill(0x22)),
    );
    const stateKey = founder.exportSecret(
      founderIdentity,
      STATE_EXPORTER_LABEL,
      decoded.rendezvousPseudonym,
      STATE_EXPORTER_KEY_LENGTH,
    );
    stateRow.sealed = wasm.sealWithKey(stateKey, encodeStateBundle(BUNDLE));
    const welcome = added.welcome;
    founder.free();
    founderIdentity.free();
    return welcome;
  }

  it("holds the bundle until the device key exists, and applies it in persistTo", async () => {
    vi.stubGlobal("localStorage", new MemoryStorage());
    vi.stubGlobal("indexedDB", new FakeIndexedDb());
    stubFetch(() => ({
      status: 200,
      json: {
        account_id_b64: b64url(ACCOUNT),
        session_token: "v2.tok",
        session_expires_at_ms: 9,
        refresh_token: "r1.tok",
        refresh_expires_at_ms: 99,
        key_generation: 1,
      },
    }));
    const session = await prepareAnonymousJoinFlow({ loginHandle: alice() });
    const welcome = confirmWithState(session.qrPayload);
    const outcome = await session.accept(welcome, { welcomeCursor: 1 });
    expect(outcome.recovery?.accountIdBytes).toEqual(ACCOUNT);
    // Fetched at accept, applied nowhere yet: there is no device key.
    expect(loadAccepted(ACCOUNT_B64)).toEqual([]);
    expect(loadKnownGroupIds(ACCOUNT_B64)).toEqual([]);

    const deviceKey = new WebDeviceCipher(await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
      "encrypt",
      "decrypt",
    ]));
    const info = console.info;
    console.info = (): void => {};
    try {
      await session.persistTo(deviceKey);
    } finally {
      console.info = info;
    }
    expect(loadAccepted(ACCOUNT_B64).map((c) => c.signaturePubkeyB64)).toEqual(["cGVlci1rZXk="]);
    expect(loadKnownGroupIds(ACCOUNT_B64)).toEqual([GROUP]);
    session.dispose();
  });

  it("a session disposed before persistTo cannot apply it", async () => {
    vi.stubGlobal("localStorage", new MemoryStorage());
    vi.stubGlobal("indexedDB", new FakeIndexedDb());
    stubFetch(() => ({
      status: 200,
      json: {
        account_id_b64: b64url(ACCOUNT),
        session_token: "v2.tok",
        session_expires_at_ms: 9,
        refresh_token: "r1.tok",
        refresh_expires_at_ms: 99,
        key_generation: 1,
      },
    }));
    const session = await prepareAnonymousJoinFlow({ loginHandle: alice() });
    await session.accept(confirmWithState(session.qrPayload), { welcomeCursor: 1 });
    session.dispose();
    const deviceKey = new WebDeviceCipher(await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
      "encrypt",
      "decrypt",
    ]));
    await expect(session.persistTo(deviceKey)).rejects.toThrow(/disposed/);
    expect(loadAccepted(ACCOUNT_B64)).toEqual([]);
  });

  // `persistTo` is the only place the anonymous join applies the bundle, so
  // every caller of `prepareAnonymousJoinFlow` must reach it, with the join
  // still alive. The screens need a Solid runtime this suite does not have;
  // the wiring is pinned on the source.
  const read = (rel: string): string =>
    readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

  it("the no-session restore screen hands persistTo to completeFromDeviceRestore", () => {
    const src = read("../auth/recover/DeviceRecover.tsx");
    expect(src).toMatch(/prepareAnonymousJoinFlow\(/);
    expect(src).toMatch(/persistJoinState: \(deviceKey: DeviceCipher\): Promise<void> => session\.persistTo\(deviceKey\)/);
  });

  it("the parked sign-in with no device key hands persistTo to completeFromPairing", () => {
    const src = read("../auth/RestoreIdentityPrompt.tsx");
    expect(src).toMatch(/prepareAnonymousJoinFlow\(/);
    expect(src).toMatch(/persistAnonymousJoin = \(key\) => anon\.persistTo\(key\)/);
    const call = src.slice(src.indexOf("await auth.completeFromPairing("));
    expect(call.slice(0, call.indexOf(");"))).toMatch(/persistAnonymousJoin \?\? undefined/);
  });

  it("both completions call the persist hook after the device key exists and before the session lands", () => {
    const auth = read("../auth/AuthContext.tsx");
    for (const [name, next] of [
      ["completeFromPairing", "completeFromDeviceRestore"],
      ["completeFromDeviceRestore", "mintFreshIdentity"],
    ] as const) {
      const start = auth.indexOf(`    async ${name}(`);
      const body = auth.slice(start, auth.indexOf(`    async ${next}(`, start));
      const key = body.indexOf("await putDeviceKey(");
      const persist = body.indexOf("await persistJoinState(deviceKey)");
      const land = body.indexOf("await finishCanonicalSession(");
      expect(key, name).toBeGreaterThan(-1);
      expect(persist, name).toBeGreaterThan(key);
      expect(land, name).toBeGreaterThan(persist);
    }
  });

  it("the panel disposes the join only after the caller's completion resolved", () => {
    const src = read("./RestoreViaDevicePanel.tsx");
    const restored = src.indexOf("await props.onRestored(recovery);");
    expect(restored).toBeGreaterThan(-1);
    expect(src.slice(restored).trimStart().split("\n")[1]?.trim()).toBe("flow.dispose();");
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
