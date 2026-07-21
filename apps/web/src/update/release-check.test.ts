// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0038 S5 (web) — shared "update available" verify-core tests.
 *
 * The wasm surface exposes VERIFY but not SIGN (a release manifest is signed
 * offline by the dedicated publisher tool, `tools/release-manifest`), so we
 * cannot mint a validly-signed manifest here. Instead the tests pin the two
 * things that ARE exercisable end-to-end:
 *
 *   1. The 0/1/2 -> string status mapping, driving the REAL wasm
 *      `releaseUpdateStatus` with fixed integers for every band (so the band
 *      logic under test is the audited Rust `release_status_for`, not a JS
 *      re-implementation), then through `mapUpdateStatus`.
 *   2. The fail-toward-"up-to-date" branches of `checkForUpdate`:
 *      unpinned key, fetch error, non-2xx, empty/over-large body, and a verify
 *      throw on a (real-wasm) garbage manifest.
 *
 * The global `fetch` is replaced per-test with a capturing mock; the release key
 * env var is stubbed via `vi.stubEnv`.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  checkForUpdate,
  currentRelease,
  manifestUrl,
  mapUpdateStatus,
  pinnedReleaseKey,
} from "./release-check";

// A valid-length (32-byte) base64url key so `pinnedReleaseKey()` returns
// non-null. Its bytes are irrelevant to the fail-safe tests — verify throws on
// any garbage manifest regardless of the key.
const TEST_KEY_B64 = base64UrlEncodeNoPad(new Uint8Array(32).fill(7));

const realFetch = globalThis.fetch;

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  (globalThis as unknown as { fetch: typeof fetch }).fetch = realFetch;
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

/** Install a capturing fetch mock; `responder` decides each `Response`. */
function installFetch(
  responder: (url: string) => Response | Promise<Response>,
): { urls: string[] } {
  const urls: string[] = [];
  const fetchMock = vi.fn((input: string | URL) => {
    urls.push(String(input));
    return Promise.resolve(responder(String(input)));
  });
  (globalThis as unknown as { fetch: typeof fetch }).fetch =
    fetchMock as unknown as typeof fetch;
  return { urls };
}

describe("mapUpdateStatus (pure)", () => {
  it("maps the wasm 0/1/2 enum to the string status, unknown -> up-to-date", () => {
    expect(mapUpdateStatus(0)).toBe("up-to-date");
    expect(mapUpdateStatus(1)).toBe("update-available");
    expect(mapUpdateStatus(2)).toBe("update-required");
    // Unknown / out-of-range codes fail toward no-nag.
    expect(mapUpdateStatus(99)).toBe("up-to-date");
    expect(mapUpdateStatus(-1)).toBe("up-to-date");
  });
});

describe("releaseUpdateStatus band mapping (real wasm)", () => {
  it("classifies every band and maps it to the string status", () => {
    // wasm was initialised in beforeAll; `releaseUpdateStatus` is pure.

    // current < min_supported -> UpdateRequired (2).
    expect(wasm.releaseUpdateStatus(29n, 42n, 30n)).toBe(2);
    expect(mapUpdateStatus(wasm.releaseUpdateStatus(29n, 42n, 30n))).toBe(
      "update-required",
    );

    // min_supported <= current < latest -> UpdateAvailable (1).
    expect(wasm.releaseUpdateStatus(30n, 42n, 30n)).toBe(1);
    expect(wasm.releaseUpdateStatus(41n, 42n, 30n)).toBe(1);
    expect(mapUpdateStatus(wasm.releaseUpdateStatus(30n, 42n, 30n))).toBe(
      "update-available",
    );

    // current >= latest -> UpToDate (0).
    expect(wasm.releaseUpdateStatus(42n, 42n, 30n)).toBe(0);
    expect(wasm.releaseUpdateStatus(100n, 42n, 30n)).toBe(0);
    expect(mapUpdateStatus(wasm.releaseUpdateStatus(42n, 42n, 30n))).toBe(
      "up-to-date",
    );

    // min-supported precedence: current < min < latest -> UpdateRequired.
    expect(wasm.releaseUpdateStatus(10n, 50n, 40n)).toBe(2);
    expect(mapUpdateStatus(wasm.releaseUpdateStatus(10n, 50n, 40n))).toBe(
      "update-required",
    );
  });
});

describe("config helpers", () => {
  it("pinnedReleaseKey returns null when unset / blank / wrong length", () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", "");
    expect(pinnedReleaseKey()).toBeNull();

    // A short (non-32-byte) key is rejected.
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", base64UrlEncodeNoPad(new Uint8Array(8)));
    expect(pinnedReleaseKey()).toBeNull();
  });

  it("pinnedReleaseKey returns the 32-byte key when pinned", () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", TEST_KEY_B64);
    const key = pinnedReleaseKey();
    expect(key).not.toBeNull();
    expect(key?.length).toBe(32);
  });

  it("manifestUrl defaults to the pinned same-origin path", () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_MANIFEST_URL", "");
    expect(manifestUrl()).toBe("/.well-known/lakko-release");
  });

  it("manifestUrl honours an override", () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_MANIFEST_URL", "/custom/release");
    expect(manifestUrl()).toBe("/custom/release");
  });

  it("currentRelease defaults to 1 and parses / floors bad values", () => {
    vi.stubEnv("VITE_LAKKO_CURRENT_RELEASE", "");
    expect(currentRelease()).toBe(1);
    vi.stubEnv("VITE_LAKKO_CURRENT_RELEASE", "7");
    expect(currentRelease()).toBe(7);
    vi.stubEnv("VITE_LAKKO_CURRENT_RELEASE", "not-a-number");
    expect(currentRelease()).toBe(1);
    vi.stubEnv("VITE_LAKKO_CURRENT_RELEASE", "-3");
    expect(currentRelease()).toBe(1);
  });
});

describe("checkForUpdate (fail toward up-to-date)", () => {
  it("is dormant (up-to-date) and never fetches when the key is unpinned", async () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", "");
    const { urls } = installFetch(() => new Response(new Uint8Array(89)));

    const result = await checkForUpdate();

    expect(result).toEqual({ status: "up-to-date" });
    expect(urls).toHaveLength(0); // no fetch when dormant
  });

  it("resolves up-to-date when the fetch throws", async () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", TEST_KEY_B64);
    installFetch(() => {
      throw new Error("network down");
    });

    expect(await checkForUpdate()).toEqual({ status: "up-to-date" });
  });

  it("resolves up-to-date on a non-2xx response", async () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", TEST_KEY_B64);
    const { urls } = installFetch(
      () => new Response("nope", { status: 404 }),
    );

    expect(await checkForUpdate()).toEqual({ status: "up-to-date" });
    // It did reach the fetch (key was pinned) and used the default path.
    expect(urls).toEqual(["/.well-known/lakko-release"]);
  });

  it("resolves up-to-date on an empty body", async () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", TEST_KEY_B64);
    installFetch(() => new Response(new Uint8Array(0)));

    expect(await checkForUpdate()).toEqual({ status: "up-to-date" });
  });

  it("resolves up-to-date on an over-large body (hostile origin)", async () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", TEST_KEY_B64);
    installFetch(() => new Response(new Uint8Array(5000)));

    expect(await checkForUpdate()).toEqual({ status: "up-to-date" });
  });

  it("resolves up-to-date when the manifest fails to verify (real wasm)", async () => {
    vi.stubEnv("VITE_LAKKO_RELEASE_KEY", TEST_KEY_B64);
    // A well-sized but garbage 89-byte blob: the real `verifyReleaseManifest`
    // throws (bad version / signature), which the check swallows.
    installFetch(() => new Response(new Uint8Array(89).fill(9)));

    expect(await checkForUpdate()).toEqual({ status: "up-to-date" });
  });
});
