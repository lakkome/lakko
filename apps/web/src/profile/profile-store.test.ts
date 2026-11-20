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
  FOUND_NAME_STAMP,
  MemoryProfileBackend,
  ProfileStore,
  awaitsOwnProfile,
  decideIngest,
  forStateBundle,
  type ProfileRow,
  type StoredProfile,
} from "./profile-store";
import { decodeProfileRow, encodeProfileRow } from "./profile-idb";
import { resolveDisplay } from "./resolver";

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
  it("once the row is loaded, a profile is filed in the same turn (a message after it is drawn under the name)", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.open(ACCOUNT);
    const filing = store.ingest(ACCOUNT, KEY_A, profile("VG3", null, 5n));
    // No microtask has run: the receive loop draws the batch's next row now.
    expect(store.get(KEY_A)?.displayName).toBe("VG3");
    expect(await filing).toBe(true);
  });

  it("before the row is loaded, an ingest still waits for it and the newer one wins", async () => {
    const backend = new MemoryProfileBackend();
    backend.rows.set(ACCOUNT, { profiles: new Map([[KEY_A, profile("Newer", null, 9n)]]), verifications: new Map() });
    const store = new ProfileStore(backend, isNewer);
    expect(await store.ingest(ACCOUNT, KEY_A, profile("Older", null, 5n))).toBe(false);
    expect(store.get(KEY_A)?.displayName).toBe("Newer");
  });

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

  it("a newer profile with no account id keeps the id the key already had", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, { ...profile("Alice", "alice", 100n), accountId: new Uint8Array(16).fill(5) });
    expect(await store.ingest(ACCOUNT, KEY_A, profile("Alice Petrova", "alice", 200n))).toBe(true);
    expect(store.get(KEY_A)?.displayName).toBe("Alice Petrova");
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

describe("a name a search found", () => {
  const found = { displayName: "Elin Holm", username: "elin", updatedAtMs: FOUND_NAME_STAMP };
  const keyBytes = new Uint8Array(32).fill(1);
  const shown = (store: ProfileStore): string =>
    resolveDisplay({ signatureKey: keyBytes, profile: store.get(KEY_A) }).name;

  it("names the person until they say otherwise", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    expect(await store.ingest(ACCOUNT, KEY_A, found)).toBe(true);
    expect(shown(store)).toBe("Elin Holm");
  });

  it("gives way to any profile the person sends", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, found);
    expect(await store.ingest(ACCOUNT, KEY_A, profile("Elin H.", "elin", 1n))).toBe(true);
    expect(shown(store)).toBe("Elin H.");
  });

  it("never replaces a profile already held", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, profile("Elin H.", "elin", 5n));
    expect(await store.ingest(ACCOUNT, KEY_A, found)).toBe(false);
    expect(shown(store)).toBe("Elin H.");
  });

  it("survives a reload through the row the device writes", async () => {
    const backend = new MemoryProfileBackend();
    const first = new ProfileStore(backend, isNewer);
    await first.ingest(ACCOUNT, KEY_A, found);
    await first.flush();
    const row = backend.rows.get(ACCOUNT);
    expect(row).toBeDefined();
    // The encrypted row's payload, as a relaunch reads it back.
    const relaunched = new MemoryProfileBackend();
    relaunched.rows.set(ACCOUNT, decodeProfileRow(encodeProfileRow(row as ProfileRow, new Map())) as ProfileRow);
    const second = new ProfileStore(relaunched, isNewer);
    await second.open(ACCOUNT);
    expect(shown(second)).toBe("Elin Holm");
    expect(await second.ingest(ACCOUNT, KEY_A, profile("Elin H.", "elin", 1n))).toBe(true);
    expect(shown(second)).toBe("Elin H.");
  });

  it("is told apart from a profile the person sent, for the request on open", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, KEY_A, found);
    expect(awaitsOwnProfile(store.get(KEY_A))).toBe(true);
    // A person with no name yet sends their profile at this very stamp.
    const id = new Uint8Array(16).fill(0xab);
    await store.ingest(ACCOUNT, KEY_A, { ...found, accountId: id });
    expect(awaitsOwnProfile(store.get(KEY_A))).toBe(false);
    expect(awaitsOwnProfile(undefined)).toBe(false);
    expect(awaitsOwnProfile(profile("A", null, 1n))).toBe(false);
  });

  describe("a person who never set a name publishes at the same stamp", () => {
    const id = new Uint8Array(16).fill(0xab);
    const theirs = { displayName: null, username: "elin", updatedAtMs: FOUND_NAME_STAMP, accountId: id };

    it("theirs replaces the found name", async () => {
      const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
      await store.ingest(ACCOUNT, KEY_A, found);
      expect(await store.ingest(ACCOUNT, KEY_A, theirs)).toBe(true);
      expect(store.get(KEY_A)?.displayName).toBeNull();
      expect(store.get(KEY_A)?.accountIdHex).toBe("ab".repeat(16));
    });

    it("a profile saying nothing does not put the stand-in back", async () => {
      const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
      await store.ingest(ACCOUNT, KEY_A, found);
      await store.ingest(ACCOUNT, KEY_A, { ...theirs, username: null });
      expect(shown(store)).toBe("Elin Holm");
    });

    it("once theirs is held, it is an ordinary profile", async () => {
      const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
      await store.ingest(ACCOUNT, KEY_A, theirs);
      expect(await store.ingest(ACCOUNT, KEY_A, found)).toBe(false);
      expect(await store.ingest(ACCOUNT, KEY_A, { ...theirs, username: "other" })).toBe(false);
      expect(store.get(KEY_A)?.username).toBe("elin");
    });

    it("a found claim offered twice is no replacement", async () => {
      const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
      await store.ingest(ACCOUNT, KEY_A, found);
      expect(await store.ingest(ACCOUNT, KEY_A, { ...found, displayName: "Other" })).toBe(false);
      expect(shown(store)).toBe("Elin Holm");
    });
  });

  describe("in a pairing state bundle", () => {
    it("a found username does not travel: its verdict is this device's", () => {
      expect(forStateBundle(profile("Elin Holm", "elin", FOUND_NAME_STAMP))).toEqual(
        profile("Elin Holm", null, FOUND_NAME_STAMP),
      );
    });

    it("a found row that is only a username carries nothing", () => {
      expect(forStateBundle(profile(null, "elin", FOUND_NAME_STAMP))).toBeNull();
    });

    it("anything the person sent travels whole", () => {
      const theirs = { ...profile("Elin", "elin", FOUND_NAME_STAMP), accountIdHex: "ab".repeat(16) };
      expect(forStateBundle(theirs)).toBe(theirs);
      const newer = profile("Elin", "elin", 7n);
      expect(forStateBundle(newer)).toBe(newer);
    });
  });

  it("the name alone is a profile the core will carry to a new device", () => {
    const wire = wasm.composeProfileEnvelope("Elin Holm", undefined, FOUND_NAME_STAMP, undefined, undefined, [], undefined);
    expect(wire.length).toBeGreaterThan(0);
  });
});
