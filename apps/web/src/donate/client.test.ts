// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The donations client verifies every statement against the pinned key
 * (`docs/ux/donations.md` section 3). The wasm surface verifies only, so the
 * statements below were signed once by the offline tool
 * (`cargo run -p lakko-donations-tool -- keygen` then `sign`) with a test key
 * made for these tests; only its public half is here. They run through the
 * real wasm `verifyDonationMethod` / `verifyDonationPhone`.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import {
  fetchDonateMethods,
  fetchDonateQr,
  pinnedDonationsKey,
  revealDonatePhone,
  verifyDonateMethods,
} from "./client";

/** Public half of the test donations key (base64url, no padding). */
const KEY_B64URL = "Aa2oo1Ax_WyUSvvLwpKvosXMZ9MBmeQodQUQ9cRFhpI";

/** sbp: link `https://example.org/pay/sbp`, a QR (hash of {@link QR_PNG_B64}), a phone. */
const SBP =
  "AQAAAaHhyfJJA3NicAEAG2h0dHBzOi8vZXhhbXBsZS5vcmcvcGF5L3NicAAAAAABTY9om8Dn/h1VongZaStJZ2Ltw5S7xRO2zK1OTWl3j2IBRiZ/imXADYiETe7DnEgGHssNMCOfZn3ma9jQiQf+T/BF+ReJHN3VbG60MRQdurvr6+Pb9K1UuCZBgiqw/MfUDw==";
/** bmc: link `https://example.org/bmc` only. */
const BMC =
  "AQAAAaHhyfJJA2JtYwMAF2h0dHBzOi8vZXhhbXBsZS5vcmcvYm1jAAAAAAAA5nZKnFPzfsZLbOjeJ3ks3ABjbMWMkeK2W9lf+aExW3c+aB93oRqpdX7pHDjhVMMtq/NEbJCsTmreDG9zvo58Cw==";
/** usdt: crypto, USDT on TRON, address `TTestAddress123`. */
const USDT =
  "AQAAAaHhyfJJBHVzZHQEAAAEVVNEVARUUk9OAA9UVGVzdEFkZHJlc3MxMjMAAAkZA3eKg6Y+KZTc4Ctf+p17h7jEsDA4LmShIFGu4RTMZ24Mhidcu/CmzztuD6MW8WZfKBgLgu6jQVJeAzmRBww=";
/** The phone statement of `sbp`: `+7 900 000-00-00`. */
const SBP_PHONE =
  "AQNzYnAQKzcgOTAwIDAwMC0wMC0wMN9JN67Dh3kDZ5H5s41EhuyEPWdloGhf46BgxODda9QSaX+mF1HOFUdjq2BrnSD0UaCBGWztppdar6NonJ74Www=";
/** The QR image `sbp`'s statement hashes. */
const QR_PNG_B64 = "iVBORw0KGgpmYWtlcXJpbWFnZWJ5dGVzLWZvci10ZXN0";

const realFetch = globalThis.fetch;

function key(): Uint8Array {
  vi.stubEnv("VITE_LAKKO_DONATIONS_KEY", KEY_B64URL);
  const k = pinnedDonationsKey();
  if (k === null) throw new Error("test key did not decode");
  return k;
}

/** Flip one bit of a base64 statement, inside the signed bytes. */
function tampered(b64: string, at: number): string {
  const bytes = base64Decode(b64);
  bytes[at] = (bytes[at] ?? 0) ^ 0x01;
  return base64Encode(bytes);
}

function installFetch(responder: (url: string, init?: RequestInit) => Response): { urls: string[] } {
  const urls: string[] = [];
  const mock = vi.fn((input: string | URL, init?: RequestInit) => {
    urls.push(String(input));
    return Promise.resolve(responder(String(input), init));
  });
  (globalThis as unknown as { fetch: typeof fetch }).fetch = mock as unknown as typeof fetch;
  return { urls };
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  (globalThis as unknown as { fetch: typeof fetch }).fetch = realFetch;
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe("verifyDonateMethods", () => {
  it("shows the verified methods, in the server's order, with the signed fields", () => {
    const methods = verifyDonateMethods(
      {
        methods: [
          { id: "sbp", statement_b64: SBP },
          { id: "bmc", statement_b64: BMC },
          { id: "usdt", statement_b64: USDT },
        ],
      },
      key(),
      wasm.verifyDonationMethod,
    );
    expect(methods.map((m) => m.id)).toEqual(["sbp", "bmc", "usdt"]);
    const [sbp, bmc, usdt] = methods;
    expect(sbp?.kind).toBe("sbp");
    expect(sbp?.link).toBe("https://example.org/pay/sbp");
    expect(sbp?.hasPhone).toBe(true);
    expect(sbp?.qrSha256?.length).toBe(32);
    expect(bmc?.link).toBe("https://example.org/bmc");
    expect(bmc?.qrSha256).toBeNull();
    expect(bmc?.hasPhone).toBe(false);
    expect(usdt?.crypto).toEqual({ coin: "USDT", network: "TRON", address: "TTestAddress123" });
    expect(usdt?.link).toBeNull();
  });

  it("drops a statement changed in any signed byte, and keeps the rest", () => {
    const len = base64Decode(BMC).length;
    for (const at of [1, 12, 20, len - 1]) {
      const methods = verifyDonateMethods(
        { methods: [{ id: "bmc", statement_b64: tampered(BMC, at) }, { id: "usdt", statement_b64: USDT }] },
        key(),
        wasm.verifyDonationMethod,
      );
      expect(methods.map((m) => m.id)).toEqual(["usdt"]);
    }
  });

  it("drops an entry whose id differs from the signed id", () => {
    const methods = verifyDonateMethods(
      { methods: [{ id: "bmc", statement_b64: SBP }, { id: "usdt", statement_b64: USDT }] },
      key(),
      wasm.verifyDonationMethod,
    );
    expect(methods.map((m) => m.id)).toEqual(["usdt"]);
  });

  it("ignores unsigned fields the server adds, and shows a method once", () => {
    const methods = verifyDonateMethods(
      {
        methods: [
          { id: "bmc", statement_b64: BMC, link: "https://attacker.example/" },
          { id: "bmc", statement_b64: BMC },
        ],
      },
      key(),
      wasm.verifyDonationMethod,
    );
    expect(methods).toHaveLength(1);
    expect(methods[0]?.link).toBe("https://example.org/bmc");
  });

  it("shows nothing under another key, or for a malformed answer", () => {
    const other = new Uint8Array(32).fill(7);
    expect(verifyDonateMethods({ methods: [{ id: "bmc", statement_b64: BMC }] }, other, wasm.verifyDonationMethod)).toEqual([]);
    expect(verifyDonateMethods(null, key(), wasm.verifyDonationMethod)).toEqual([]);
    expect(verifyDonateMethods({ methods: "no" }, key(), wasm.verifyDonationMethod)).toEqual([]);
    expect(
      verifyDonateMethods({ methods: [{ id: "bmc", statement_b64: "not base64!" }, null] }, key(), wasm.verifyDonationMethod),
    ).toEqual([]);
  });
});

describe("pinnedDonationsKey and fetchDonateMethods", () => {
  it("reads no key when unset, blank or not 32 bytes", () => {
    vi.stubEnv("VITE_LAKKO_DONATIONS_KEY", "");
    expect(pinnedDonationsKey()).toBeNull();
    vi.stubEnv("VITE_LAKKO_DONATIONS_KEY", "AAAA");
    expect(pinnedDonationsKey()).toBeNull();
  });

  it("with no pinned key shows no method and asks the server nothing", async () => {
    vi.stubEnv("VITE_LAKKO_DONATIONS_KEY", "");
    const { urls } = installFetch(() => json({ methods: [{ id: "bmc", statement_b64: BMC }] }));
    expect(await fetchDonateMethods("/api")).toEqual([]);
    expect(urls).toEqual([]);
  });

  it("with the pinned key fetches and verifies the list", async () => {
    vi.stubEnv("VITE_LAKKO_DONATIONS_KEY", KEY_B64URL);
    const { urls } = installFetch(() =>
      json({ methods: [{ id: "bmc", statement_b64: BMC }, { id: "sbp", statement_b64: tampered(SBP, 5) }] }),
    );
    const methods = await fetchDonateMethods("/api");
    expect(methods.map((m) => m.id)).toEqual(["bmc"]);
    expect(urls).toEqual(["/api/discovery/donate"]);
  });
});

describe("fetchDonateQr", () => {
  function sbpHash(): Uint8Array {
    const [sbp] = verifyDonateMethods({ methods: [{ id: "sbp", statement_b64: SBP }] }, key(), wasm.verifyDonationMethod);
    if (sbp?.qrSha256 == null) throw new Error("no qr hash");
    return sbp.qrSha256;
  }

  it("returns the image when its SHA-256 equals the signed hash", async () => {
    const hash = sbpHash();
    installFetch(() => new Response(base64Decode(QR_PNG_B64).buffer as ArrayBuffer, { status: 200 }));
    const blob = await fetchDonateQr("sbp", hash, "/api");
    expect(blob).not.toBeNull();
    expect(new Uint8Array(await (blob as Blob).arrayBuffer())).toEqual(base64Decode(QR_PNG_B64));
  });

  it("hides the QR when the image differs, the request fails, or it is too large", async () => {
    const hash = sbpHash();
    installFetch(() => new Response(new Uint8Array([1, 2, 3]), { status: 200 }));
    expect(await fetchDonateQr("sbp", hash, "/api")).toBeNull();
    installFetch(() => new Response("", { status: 404 }));
    expect(await fetchDonateQr("sbp", hash, "/api")).toBeNull();
    installFetch(() => new Response(new Uint8Array(128 * 1024 + 1), { status: 200 }));
    expect(await fetchDonateQr("sbp", hash, "/api")).toBeNull();
  });
});

describe("revealDonatePhone", () => {
  it("shows the number of a verified phone statement", async () => {
    installFetch(() => json({ statement_b64: SBP_PHONE }));
    expect(await revealDonatePhone("sbp", "/api", key())).toBe("+7 900 000-00-00");
  });

  it("refuses a tampered statement, another method's statement, or no key", async () => {
    installFetch(() => json({ statement_b64: tampered(SBP_PHONE, 8) }));
    await expect(revealDonatePhone("sbp", "/api", key())).rejects.toThrow();
    installFetch(() => json({ statement_b64: SBP_PHONE }));
    await expect(revealDonatePhone("bmc", "/api", key())).rejects.toThrow();
    installFetch(() => json({ phone: "+7 999 999-99-99" }));
    await expect(revealDonatePhone("sbp", "/api", key())).rejects.toThrow();
    await expect(revealDonatePhone("sbp", "/api", null)).rejects.toThrow();
  });
});
