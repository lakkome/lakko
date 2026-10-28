// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §3 — the received-profile store: ordering and keying.
 *
 * The order is the REAL core's `profileIsNewer`, not a local comparison, so a
 * change to the core's rule is what these tests follow.
 *
 * ## Discrimination (verified by running the broken variants)
 *
 *   - `decideIngest` returning `store` on an EQUAL stamp fails "a replay of
 *     the held profile changes nothing" (`expected true to be false`).
 *   - Filing by the profile's claimed username instead of the sender key
 *     fails "a claim from another key never touches the first key's entry"
 *     with `expected undefined to be 100n`.
 *   - Keying a verification verdict on the key alone fails "a verdict is per
 *     (key, username)" with `expected { state: 'verified', … } to be undefined`.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  MemoryProfileBackend,
  ProfileStore,
  decideIngest,
  type ProfileRow,
  type StoredProfile,
} from "./profile-store";
import { decodeProfileRow, encodeProfileRow } from "./profile-idb";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const isNewer = (a: bigint, b: bigint): boolean => wasm.profileIsNewer(a, b);
const ACCOUNT = "acct";
const KEY_A = "a".repeat(43) + "=";
const KEY_B = "b".repeat(43) + "=";

function profile(name: string | null, username: string | null, at: bigint): StoredProfile {
  return { displayName: name, username, updatedAtMs: at, accountIdHex: null, usernameCertificateB64: null };
}

describe("decideIngest", () => {
  it("stores the first profile for a key", () => {
    expect(decideIngest(undefined, 5n, isNewer)).toBe("store");
  });
  it("stores only a strictly newer stamp", () => {
    const held = profile("A", null, 10n);
    expect(decideIngest(held, 11n, isNewer)).toBe("store");
    expect(decideIngest(held, 10n, isNewer)).toBe("keep");
    expect(decideIngest(held, 9n, isNewer)).toBe("keep");
  });
});

describe("ProfileStore", () => {
  it("a newer profile replaces, an older one does not", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    expect(await store.ingest(ACCOUNT, KEY_A, profile("Alice", "alice", 100n))).toBe(true);
    expect(await store.ingest(ACCOUNT, KEY_A, profile("Old", null, 50n))).toBe(false);
    expect(store.get(KEY_A)?.displayName).toBe("Alice");
    expect(await store.ingest(ACCOUNT, KEY_A, profile("Alice P.", "alice", 101n))).toBe(true);
    expect(store.get(KEY_A)?.displayName).toBe("Alice P.");
  });

  it("a replay of the held profile changes nothing", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, profile("Alice", "alice", 100n));
    expect(await store.ingest(ACCOUNT, KEY_A, profile("Mallory", "mallory", 100n))).toBe(false);
    expect(store.get(KEY_A)?.displayName).toBe("Alice");
  });

  it("a same-stamp re-send fills in a missing certificate for the same username and checks the claim again", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    // A contact request's profile that arrived without the certificate,
    // checked and cached as a mismatch.
    await store.ingest(ACCOUNT, KEY_A, profile("Alice", "alice", 100n));
    store.recordVerification(ACCOUNT, KEY_A, "alice", { state: "mismatch", checkedAtMs: 1 });
    const cert = new Uint8Array(153).fill(7);
    // Another username's certificate is not taken.
    expect(
      await store.ingest(ACCOUNT, KEY_A, { ...profile("Alice", "mallory", 100n), usernameCertificate: cert }),
    ).toBe(false);
    expect(store.get(KEY_A)?.usernameCertificateB64).toBeNull();
    expect(store.verification(KEY_A, "alice")?.state).toBe("mismatch");
    // The same profile again, now with it.
    expect(
      await store.ingest(ACCOUNT, KEY_A, { ...profile("Alice", "alice", 100n), usernameCertificate: cert }),
    ).toBe(true);
    expect(store.get(KEY_A)?.usernameCertificateB64).not.toBeNull();
    expect(store.get(KEY_A)?.displayName).toBe("Alice");
    expect(store.verification(KEY_A, "alice")).toBeUndefined();
  });

  it("a newer profile with another certificate drops the verdict reached without it", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, profile("Alice", "alice", 100n));
    store.recordVerification(ACCOUNT, KEY_A, "alice", { state: "mismatch", checkedAtMs: 1 });
    await store.ingest(ACCOUNT, KEY_A, {
      ...profile("Alice", "alice", 101n),
      usernameCertificate: new Uint8Array(153).fill(7),
    });
    expect(store.verification(KEY_A, "alice")).toBeUndefined();
  });

  it("a same-stamp re-send fills in a missing account id and nothing else", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, profile("Alice", "alice", 100n));
    const id = new Uint8Array(16).fill(5);
    expect(
      await store.ingest(ACCOUNT, KEY_A, {
        displayName: "Mallory",
        username: "mallory",
        updatedAtMs: 100n,
        accountId: id,
      }),
    ).toBe(true);
    expect(store.get(KEY_A)?.displayName).toBe("Alice");
    expect(store.get(KEY_A)?.accountIdHex).toBe("05".repeat(16));
    // An older stamp learns nothing; a known id is never replaced.
    const other = new Uint8Array(16).fill(6);
    expect(
      await store.ingest(ACCOUNT, KEY_A, { displayName: "Alice", username: "alice", updatedAtMs: 99n, accountId: other }),
    ).toBe(false);
    expect(
      await store.ingest(ACCOUNT, KEY_A, { displayName: "Alice", username: "alice", updatedAtMs: 100n, accountId: other }),
    ).toBe(false);
    expect(store.get(KEY_A)?.accountIdHex).toBe("05".repeat(16));
  });

  it("a claim from another key never touches the first key's entry", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, profile("Alice", "alice", 100n));
    // Key B claims Alice's name and username with a far newer stamp.
    await store.ingest(ACCOUNT, KEY_B, profile("Alice", "alice", 999n));
    expect(store.get(KEY_A)?.updatedAtMs).toBe(100n);
    expect(store.get(KEY_B)?.updatedAtMs).toBe(999n);
    expect(store.entries().size).toBe(2);
  });

  it("two profiles from one batch decide against current state", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    const [first, second] = await Promise.all([
      store.ingest(ACCOUNT, KEY_A, profile("New", null, 200n)),
      store.ingest(ACCOUNT, KEY_A, profile("Stale", null, 150n)),
    ]);
    expect([first, second]).toEqual([true, false]);
    expect(store.get(KEY_A)?.displayName).toBe("New");
  });

  it("persists and reloads profiles and verifications", async () => {
    const backend = new MemoryProfileBackend();
    const store = new ProfileStore(backend, isNewer);
    await store.ingest(ACCOUNT, KEY_A, profile("Alice", "alice", 100n));
    store.recordVerification(ACCOUNT, KEY_A, "alice", { state: "verified", checkedAtMs: 7 });
    await store.flush();

    const reopened = new ProfileStore(backend, isNewer);
    await reopened.open(ACCOUNT);
    expect(reopened.get(KEY_A)).toEqual(profile("Alice", "alice", 100n));
    expect(reopened.verification(KEY_A, "alice")).toEqual({ state: "verified", checkedAtMs: 7 });
  });

  it("an ingest racing the load is ordered against the loaded row", async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const row: ProfileRow = {
      profiles: new Map([[KEY_A, profile("Old from disk", null, 10n)]]),
      verifications: new Map(),
    };
    const store = new ProfileStore(
      {
        load: async () => {
          await gate;
          return row;
        },
        save: async () => {},
      },
      isNewer,
    );
    const opening = store.open(ACCOUNT);
    const newer = store.ingest(ACCOUNT, KEY_A, profile("Fresh", null, 20n));
    const older = store.ingest(ACCOUNT, KEY_A, profile("Older than disk", null, 5n));
    release();
    await opening;
    expect(await newer).toBe(true);
    expect(await older).toBe(false);
    expect(store.get(KEY_A)?.displayName).toBe("Fresh");
  });

  it("a verdict is per (key, username): a changed claim has none yet", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.open(ACCOUNT);
    store.recordVerification(ACCOUNT, KEY_A, "alice", { state: "verified", checkedAtMs: 1 });
    expect(store.verification(KEY_A, "alice")?.state).toBe("verified");
    expect(store.verification(KEY_A, "bob")).toBeUndefined();
    expect(store.verification(KEY_B, "alice")).toBeUndefined();
  });

  it("is bound to one account at a time", async () => {
    const backend = new MemoryProfileBackend();
    const store = new ProfileStore(backend, isNewer);
    await store.ingest("one", KEY_A, profile("Alice", null, 1n));
    await store.open("two");
    expect(store.get(KEY_A)).toBeUndefined();
  });
});

describe("profile row codec", () => {
  it("round-trips profiles, verifications and the rename mirror", () => {
    const row: ProfileRow = {
      profiles: new Map([
        [KEY_A, profile("Alice", "alice", 18446744073709551615n)],
        [KEY_B, profile(null, null, 0n)],
      ]),
      verifications: new Map([[`${KEY_A}|alice`, { state: "mismatch", checkedAtMs: 42 }]]),
    };
    const json = encodeProfileRow(row, new Map([[KEY_A, "Mum"], [KEY_B, "  "]]));
    const parsed = JSON.parse(json) as { r: Record<string, string> };
    expect(parsed.r).toEqual({ [KEY_A]: "Mum" });
    const back = decodeProfileRow(json);
    expect(back?.profiles.get(KEY_A)?.updatedAtMs).toBe(18446744073709551615n);
    expect(back?.profiles.get(KEY_B)).toEqual(profile(null, null, 0n));
    expect(back?.verifications.get(`${KEY_A}|alice`)).toEqual({ state: "mismatch", checkedAtMs: 42 });
  });

  it("carries the claimed account id, and reads a row written without it", () => {
    const hex = "0123456789abcdef0123456789abcdef";
    const row: ProfileRow = {
      profiles: new Map([
        [KEY_A, { ...profile("Alice", null, 1n), accountIdHex: hex }],
        [KEY_B, profile("Bob", null, 2n)],
      ]),
      verifications: new Map(),
    };
    const json = encodeProfileRow(row, new Map());
    const parsed = JSON.parse(json) as { p: Record<string, { a?: string }> };
    expect(parsed.p[KEY_A]?.a).toBe(hex);
    expect("a" in (parsed.p[KEY_B] ?? {})).toBe(false);
    const back = decodeProfileRow(json);
    expect(back?.profiles.get(KEY_A)?.accountIdHex).toBe(hex);
    expect(back?.profiles.get(KEY_B)?.accountIdHex).toBeNull();
    // A malformed id drops the field, not the profile.
    const bad = decodeProfileRow(
      JSON.stringify({ v: 1, p: { k: { d: "A", u: null, t: "1", a: "ZZ" } }, vf: {}, r: {} }),
    );
    expect(bad?.profiles.get("k")).toEqual(profile("A", null, 1n));
  });

  it("stores a 16-byte account id as hex and anything else as none", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    const id = new Uint8Array(16).fill(0xab);
    await store.ingest(ACCOUNT, KEY_A, { ...profile("Alice", null, 1n), accountId: id });
    expect(store.get(KEY_A)?.accountIdHex).toBe("ab".repeat(16));
    await store.ingest(ACCOUNT, KEY_B, { ...profile("Bob", null, 1n), accountId: new Uint8Array(15) });
    expect(store.get(KEY_B)?.accountIdHex).toBeNull();
  });

  it("drops malformed entries instead of trusting them", () => {
    const json = JSON.stringify({
      v: 1,
      p: { good: { d: "A", u: null, t: "5" }, badT: { d: "A", u: null, t: "-1" }, badD: { d: 3, u: null, t: "1" } },
      vf: { "k|u": { s: "pending", c: 1 }, "k|v": { s: "verified", c: 2 } },
      r: {},
    });
    const back = decodeProfileRow(json);
    expect([...(back?.profiles.keys() ?? [])]).toEqual(["good"]);
    expect([...(back?.verifications.keys() ?? [])]).toEqual(["k|v"]);
    expect(decodeProfileRow("{\"v\":2}")).toBeNull();
    expect(decodeProfileRow("not json")).toBeNull();
  });
});
