// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the camera background keeps on this device (ADR-0089 section 5):
 * the stored choice (read through the shared Rust rule), the own photos
 * (at most twelve, sealed under the device key, wiped with the account), and
 * the strip built from them.
 */

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import { WebDeviceCipher } from "../auth/device-cipher";
import { resetAccountStoreGateForTests, sealAccountStoresForWipe } from "../auth/account-store-gate";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { wasmSync } from "../wasm";
import {
  BACKGROUND_STORAGE_KEY,
  backgroundChoice,
  choiceForCall,
  onBackgroundChoiceChange,
  readBackgroundChoice,
  resetBackgroundChoiceForTest,
  setBackgroundChoice,
} from "./background-choice";
import {
  BackgroundPhotoStore,
  MAX_BACKGROUND_PHOTOS,
  deleteBackgroundPhotos,
  newPhotoId,
  photosToEvict,
  scaledPhotoSize,
} from "./background-photos";
import { isSelectedItem, nextStripIndex, stripItems } from "./background-strip";
import { backgroundPath } from "./background-support";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("the stored choice", () => {
  const store = (value: string | null): MemoryStorage => {
    const s = new MemoryStorage();
    if (value !== null) s.setItem(BACKGROUND_STORAGE_KEY, value);
    return s;
  };

  it("nothing stored is none", () => {
    expect(readBackgroundChoice(store(null))).toEqual({ kind: "none" });
    expect(readBackgroundChoice(null)).toEqual({ kind: "none" });
  });

  it("reads every valid choice through the Rust rule", () => {
    expect(readBackgroundChoice(store("none"))).toEqual({ kind: "none" });
    expect(readBackgroundChoice(store(""))).toEqual({ kind: "none" });
    expect(readBackgroundChoice(store("blur"))).toEqual({ kind: "blur", strong: false });
    expect(readBackgroundChoice(store("strong-blur"))).toEqual({ kind: "blur", strong: true });
    expect(readBackgroundChoice(store("builtin:dusk"))).toEqual({ kind: "builtin", id: "dusk" });
    expect(readBackgroundChoice(store("photo:a1B2_c-3"))).toEqual({ kind: "photo", id: "a1B2_c-3" });
  });

  it("an unreadable value is blur, toward hiding the room", () => {
    for (const bad of ["photo:../etc", "builtin:nowhere", "blurry", "photo:", "{json}", "builtin:"]) {
      expect(readBackgroundChoice(store(bad)), bad).toEqual({ kind: "blur", strong: false });
    }
  });

  it("a storage that throws reads as none", () => {
    const broken = {
      getItem: (): string => {
        throw new Error("denied");
      },
      setItem: (): void => undefined,
    };
    expect(readBackgroundChoice(broken)).toEqual({ kind: "none" });
  });

  it("without the core loaded, anything but none reads as blur", () => {
    const failing = (): string => {
      throw new Error("not loaded");
    };
    const fallback = (stored: string): string => {
      try {
        return failing();
      } catch {
        return stored.trim() === "" || stored.trim() === "none" ? "none" : "blur";
      }
    };
    expect(readBackgroundChoice(store("builtin:dawn"), fallback)).toEqual({ kind: "blur", strong: false });
  });

  it("a selected photo whose file is missing or unreadable is blur at call time", () => {
    expect(choiceForCall({ kind: "photo", id: "x" }, false)).toEqual({ kind: "blur", strong: false });
    expect(choiceForCall({ kind: "photo", id: "x" }, true)).toEqual({ kind: "photo", id: "x" });
    expect(choiceForCall({ kind: "builtin", id: "dawn" }, false)).toEqual({ kind: "builtin", id: "dawn" });
  });

  it("a set is stored, remembered and told, also when the same choice is picked again", () => {
    resetBackgroundChoiceForTest();
    const s = new MemoryStorage();
    const heard: string[] = [];
    const off = onBackgroundChoiceChange((c) => heard.push(c.kind));
    setBackgroundChoice({ kind: "builtin", id: "night" }, s);
    setBackgroundChoice({ kind: "builtin", id: "night" }, s);
    expect(s.getItem(BACKGROUND_STORAGE_KEY)).toBe("builtin:night");
    expect(backgroundChoice()).toEqual({ kind: "builtin", id: "night" });
    expect(heard).toEqual(["builtin", "builtin"]);
    off();
    resetBackgroundChoiceForTest();
  });
});

describe("the twelve-photo limit", () => {
  const entries = (n: number): { id: string; addedAt: number }[] =>
    Array.from({ length: n }, (_, i) => ({ id: `p${i}`, addedAt: 100 + i }));

  it("nothing goes below twelve", () => {
    expect(photosToEvict(entries(11), null)).toEqual([]);
  });

  it("a thirteenth removes the oldest one", () => {
    expect(photosToEvict(entries(12), null)).toEqual(["p0"]);
  });

  it("a thirteenth removes the oldest one that is not selected", () => {
    expect(photosToEvict(entries(12), "p0")).toEqual(["p1"]);
  });

  it("the limit is twelve", () => {
    expect(MAX_BACKGROUND_PHOTOS).toBe(12);
  });

  it("a photo id passes the shared rule", () => {
    const id = newPhotoId();
    expect(wasmSync().backgroundIsPhotoId(id)).toBe(true);
  });

  it("a big picture is scaled to 1920 on its longer side, a small one kept", () => {
    expect(scaledPhotoSize(4000, 3000)).toEqual({ width: 1920, height: 1440 });
    expect(scaledPhotoSize(1080, 2400)).toEqual({ width: 864, height: 1920 });
    expect(scaledPhotoSize(800, 600)).toEqual({ width: 800, height: 600 });
  });
});

describe("the photo store", () => {
  let idb: FakeIndexedDb;
  beforeEach(() => {
    idb = new FakeIndexedDb();
    (globalThis as Record<string, unknown>)["indexedDB"] = idb;
    resetAccountStoreGateForTests();
  });
  afterEach(() => {
    delete (globalThis as Record<string, unknown>)["indexedDB"];
    resetAccountStoreGateForTests();
  });

  async function cipher(): Promise<WebDeviceCipher> {
    const key = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
    return new WebDeviceCipher(key);
  }

  it("keeps twelve, never the selected one out, newest first, sealed", async () => {
    const key = await cipher();
    let now = 1000;
    const store = new BackgroundPhotoStore("ACCT", () => Promise.resolve(key), () => (now += 1));
    const ids: string[] = [];
    for (let i = 0; i < 12; i++) ids.push((await store.add(new Uint8Array([i, 1, 2, 3]), null)).id);
    const oldest = ids[0] as string;
    const added = await store.add(new Uint8Array([99]), oldest);
    expect(added.evicted).toEqual([ids[1]]);
    const list = await store.list();
    expect(list).toHaveLength(12);
    expect(list[0]?.id).toBe(added.id);
    expect(list.map((p) => p.id)).toContain(oldest);
    expect(list.map((p) => p.id)).not.toContain(ids[1]);
    expect(await store.get(added.id)).toEqual(new Uint8Array([99]));
    // On disk the picture is sealed: the stored bytes are not the picture.
    const rows = [...(idb.dbs.get("lakko-call-backgrounds")?.stores.get("photos")?.rows.values() ?? [])].map(
      (r) => r.value as { ciphertext: Uint8Array },
    );
    expect(rows).toHaveLength(12);
    expect(rows.every((r) => r.ciphertext.length >= 12 + 16)).toBe(true);
  });

  it("a missing photo reads as nothing", async () => {
    const key = await cipher();
    const store = new BackgroundPhotoStore("ACCT", () => Promise.resolve(key));
    expect(await store.get("nope")).toBeNull();
  });

  it("the wipe removes this account's photos and nobody else's", async () => {
    const key = await cipher();
    const a = new BackgroundPhotoStore("AAA", () => Promise.resolve(key));
    const b = new BackgroundPhotoStore("BBB", () => Promise.resolve(key));
    await a.add(new Uint8Array([1]), null);
    await a.add(new Uint8Array([2]), null);
    const kept = await b.add(new Uint8Array([3]), null);
    await deleteBackgroundPhotos("AAA");
    expect(await a.list()).toEqual([]);
    expect((await b.list()).map((p) => p.id)).toEqual([kept.id]);
  });

  it("a sealed account takes no photo", async () => {
    const key = await cipher();
    const store = new BackgroundPhotoStore("SEALED", () => Promise.resolve(key));
    sealAccountStoresForWipe(["SEALED"]);
    await expect(store.add(new Uint8Array([1]), null)).rejects.toThrow();
    expect(await store.list()).toEqual([]);
  });
});

describe("the strip", () => {
  const ids = ["dawn", "morning", "meadow", "harbor", "dusk", "night"];

  it("None, Blur, Strong blur, the six, own photos newest first, then Add photo", () => {
    const items = stripItems(ids, [
      { id: "new", addedAt: 2 },
      { id: "old", addedAt: 1 },
    ]);
    expect(items.map((i) => i.key)).toEqual([
      "none",
      "blur",
      "strong-blur",
      ...ids.map((id) => `builtin:${id}`),
      "photo:new",
      "photo:old",
      "add",
    ]);
    expect(ids).toEqual(wasmSync().backgroundBuiltinIds());
  });

  it("exactly one item is selected", () => {
    const items = stripItems(ids, []);
    expect(items.filter((i) => isSelectedItem(i, { kind: "builtin", id: "dusk" }))).toHaveLength(1);
    expect(items.filter((i) => isSelectedItem(i, { kind: "none" }))).toHaveLength(1);
  });

  it("arrow keys move along it, mirrored right to left", () => {
    expect(nextStripIndex(0, "ArrowRight", 5)).toBe(1);
    expect(nextStripIndex(4, "ArrowRight", 5)).toBe(4);
    expect(nextStripIndex(0, "ArrowLeft", 5)).toBe(0);
    expect(nextStripIndex(2, "ArrowLeft", 5, true)).toBe(3);
    expect(nextStripIndex(2, "End", 5)).toBe(4);
    expect(nextStripIndex(2, "Home", 5)).toBe(0);
    expect(nextStripIndex(2, "Enter", 5)).toBeNull();
  });
});

describe("which way a browser runs a background", () => {
  const all = { wasmSimd: true, webgl2: true, frameStreams: true, canvasCapture: true };
  it("frame streams first, then the canvas, else none", () => {
    expect(backgroundPath(all)).toBe("frames");
    expect(backgroundPath({ ...all, frameStreams: false })).toBe("canvas");
    expect(backgroundPath({ ...all, frameStreams: false, canvasCapture: false })).toBeNull();
    expect(backgroundPath({ ...all, wasmSimd: false })).toBeNull();
    expect(backgroundPath({ ...all, webgl2: false })).toBeNull();
  });
});
