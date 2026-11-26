// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0084 — the profile photo on the web client: the data model and its
 * round trip through the real core, the stored row, and the decrypted-avatar
 * cache.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  avatarFromJson,
  avatarFromWasm,
  avatarToWasm,
  bytesToB64,
  looksLikeJpeg,
  looksLikeMp4,
  sameAvatar,
  type AvatarData,
} from "./avatar-model";
import { AvatarCache, MemoryAvatarBackend, RETRY_AFTER_MS } from "./avatar-cache";
import { decodeProfileRow, encodeProfileRow } from "./profile-idb";
import { MemoryProfileBackend, ProfileStore } from "./profile-store";
import { sniffProfile } from "./profile-envelope";
import { composeOwnProfileForDevices } from "./own-profile-sync";

/** ADR-0101 section 8: a v6 profile envelope is signed by an account
 *  identity; this test's profiles are signed by one fixed identity. */
let profileSignerHeld: wasm.Identity | undefined;
function profileSigner(): wasm.Identity {
  return (profileSignerHeld ??= wasm.Identity.fromSeed(new Uint8Array(64).fill(0x5a)));
}

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const b64 = (n: number, len: number): string => bytesToB64(new Uint8Array(len).fill(n));

const PHOTO: AvatarData = {
  still: { blobIdB64: b64(1, 32), keyB64: b64(2, 32), nonceB64: b64(3, 12), size: 40_000, side: 640 },
  video: null,
};
const VIDEO: AvatarData = {
  still: { blobIdB64: b64(4, 32), keyB64: b64(5, 32), nonceB64: b64(6, 12), size: 30_000, side: 480 },
  video: {
    ref: { blobIdB64: b64(7, 32), keyB64: b64(8, 32), nonceB64: b64(9, 12), size: 2_000_000, side: 480 },
    durationMs: 9_500,
  },
};

function compose(avatar: AvatarData | null): Uint8Array {
  return wasm.composeProfileEnvelope(
    profileSigner(),
    "Alice",
    undefined,
    1_000n,
    undefined,
    undefined,
    undefined,
    avatar === null ? undefined : avatarToWasm<wasm.ProfileAvatar>(wasm, avatar),
  );
}

describe("the avatar through the real core", () => {
  it("round-trips a photo and a video", () => {
    for (const a of [PHOTO, VIDEO]) {
      const decoded = wasm.decodeProfileEnvelope(compose(a));
      try {
        expect(avatarFromWasm(decoded.avatar)).toEqual(a);
      } finally {
        decoded.free();
      }
    }
  });

  it("carries no avatar when none is set", () => {
    const decoded = wasm.decodeProfileEnvelope(compose(null));
    try {
      expect(avatarFromWasm(decoded.avatar)).toBeNull();
    } finally {
      decoded.free();
    }
  });

  it("is refused by the core when out of bounds", () => {
    const big: AvatarData = { ...PHOTO, still: { ...PHOTO.still, side: 700 } };
    expect(() => compose(big)).toThrow();
  });

  it("is detached by the receive sniff", () => {
    const sniff = sniffProfile(wasm as never, compose(VIDEO));
    expect(sniff.kind).toBe("profile");
    if (sniff.kind === "profile") expect(sniff.profile.avatar).toEqual(VIDEO);
  });

  it("goes to the account's other devices with the name", () => {
    const bytes = composeOwnProfileForDevices(
      wasm as never,
      profileSigner(),
      { displayName: "Alice", updatedAtMs: 5_000, avatar: PHOTO },
      null,
    );
    expect(bytes).not.toBeNull();
    const decoded = wasm.decodeProfileEnvelope(bytes!);
    try {
      expect(avatarFromWasm(decoded.avatar)).toEqual(PHOTO);
      expect(decoded.displayName).toBe("Alice");
    } finally {
      decoded.free();
    }
  });
});

describe("the stored form", () => {
  it("reads back what it wrote and drops anything out of bounds", () => {
    expect(avatarFromJson(JSON.parse(JSON.stringify(VIDEO)))).toEqual(VIDEO);
    expect(avatarFromJson(null)).toBeNull();
    expect(avatarFromJson({ ...PHOTO, still: { ...PHOTO.still, side: 641 } })).toBeNull();
    expect(avatarFromJson({ ...PHOTO, still: { ...PHOTO.still, blobIdB64: b64(1, 31) } })).toBeNull();
    expect(avatarFromJson({ still: PHOTO.still, video: { ref: VIDEO.video!.ref, durationMs: 10_001 } })).toBeNull();
  });

  it("compares", () => {
    expect(sameAvatar(PHOTO, JSON.parse(JSON.stringify(PHOTO)) as AvatarData)).toBe(true);
    expect(sameAvatar(PHOTO, VIDEO)).toBe(false);
    expect(sameAvatar(null, null)).toBe(true);
    expect(sameAvatar(PHOTO, null)).toBe(false);
  });

  it("survives the encrypted profile row", () => {
    const row = {
      profiles: new Map([
        ["k1", { displayName: "A", username: null, updatedAtMs: 3n, accountIdHex: null, avatar: VIDEO }],
        ["k2", { displayName: "B", username: null, updatedAtMs: 4n, accountIdHex: null }],
      ]),
      verifications: new Map(),
    };
    const back = decodeProfileRow(encodeProfileRow(row, new Map()))!;
    expect(back.profiles.get("k1")?.avatar).toEqual(VIDEO);
    expect(back.profiles.get("k2")?.avatar).toBeUndefined();
  });

  it("is kept per signature key, and a newer profile without a photo removes it", async () => {
    const store = new ProfileStore(new MemoryProfileBackend(), (a, b) => a > b);
    await store.ingest("acct", "key", { displayName: "A", username: null, updatedAtMs: 1n, avatar: PHOTO });
    expect(store.avatarOf("key")).toEqual(PHOTO);
    expect(store.avatarOf("other")).toBeNull();
    await store.ingest("acct", "key", { displayName: "A", username: null, updatedAtMs: 2n });
    expect(store.avatarOf("key")).toBeNull();
  });
});

describe("the media sniff", () => {
  it("tells a JPEG and an MP4 apart", () => {
    expect(looksLikeJpeg(new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0]))).toBe(true);
    expect(looksLikeJpeg(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0]))).toBe(false);
    const mp4 = new Uint8Array(16);
    mp4.set([0x66, 0x74, 0x79, 0x70], 4);
    expect(looksLikeMp4(mp4)).toBe(true);
    expect(looksLikeMp4(new Uint8Array(16))).toBe(false);
  });
});

describe("the decrypted-avatar cache", () => {
  function setup(opts: { fetch?: () => Promise<Uint8Array>; verify?: boolean } = {}) {
    let now = 1_000_000;
    let fetches = 0;
    const backend = new MemoryAvatarBackend();
    const cache = new AvatarCache({
      backend,
      fetchAndDecrypt: async () => {
        fetches += 1;
        return opts.fetch !== undefined ? opts.fetch() : new Uint8Array(PHOTO.still.size);
      },
      verify: async () => opts.verify ?? true,
      now: () => now,
    });
    return {
      cache,
      backend,
      fetches: () => fetches,
      advance: (ms: number) => {
        now += ms;
      },
    };
  }

  it("downloads once and keeps the bytes", async () => {
    const t = setup();
    await t.cache.want(PHOTO.still, "still");
    await t.cache.want(PHOTO.still, "still");
    expect(t.fetches()).toBe(1);
    expect(t.cache.bytes(PHOTO.still.blobIdB64)?.length).toBe(PHOTO.still.size);
    expect(t.backend.rows.has(PHOTO.still.blobIdB64)).toBe(true);
  });

  it("reads a held blob from disk without a download", async () => {
    const t = setup();
    t.backend.rows.set(PHOTO.still.blobIdB64, new Uint8Array(PHOTO.still.size));
    await t.cache.want(PHOTO.still, "still");
    expect(t.fetches()).toBe(0);
    expect(t.cache.bytes(PHOTO.still.blobIdB64)).not.toBeNull();
  });

  it("retries a failure only after a minute", async () => {
    const t = setup({ fetch: () => Promise.reject(new Error("offline")) });
    await t.cache.want(PHOTO.still, "still");
    expect(t.cache.bytes(PHOTO.still.blobIdB64)).toBeNull();
    await t.cache.want(PHOTO.still, "still");
    expect(t.fetches()).toBe(1);
    t.advance(RETRY_AFTER_MS);
    await t.cache.want(PHOTO.still, "still");
    expect(t.fetches()).toBe(2);
  });

  it("drops bytes of the wrong size or that do not decode", async () => {
    const wrongSize = setup({ fetch: async () => new Uint8Array(3) });
    await wrongSize.cache.want(PHOTO.still, "still");
    expect(wrongSize.cache.bytes(PHOTO.still.blobIdB64)).toBeNull();
    const notImage = setup({ verify: false });
    await notImage.cache.want(PHOTO.still, "still");
    expect(notImage.cache.bytes(PHOTO.still.blobIdB64)).toBeNull();
    expect(notImage.backend.rows.size).toBe(0);
  });

  it("takes this device's own upload without a download", () => {
    const t = setup();
    t.cache.seed(PHOTO.still.blobIdB64, new Uint8Array(5));
    expect(t.cache.bytes(PHOTO.still.blobIdB64)?.length).toBe(5);
    expect(t.cache.shouldLoad(PHOTO.still.blobIdB64)).toBe(false);
  });
});
