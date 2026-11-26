// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 section 3 — a group file is kept only when it is exactly the file
 * the manifest names: fetched in ranges from `/translation-models/<sha256>`,
 * every model file in it hashing to the manifest, the files covering it
 * exactly.
 */

import { createHash } from "node:crypto";

import { describe, expect, it } from "vitest";

import { fetchGroup, type GroupSlice, MODELS_PATH, RANGE_BYTES, slicesCover, verifyGroup } from "./models-store";

const sha = (b: Uint8Array): string => createHash("sha256").update(b).digest("hex");
const NAME = "ab".repeat(32);

function groupFile(sizes: readonly number[]): { bytes: Uint8Array<ArrayBuffer>; slices: GroupSlice[] } {
  const total = sizes.reduce((a, b) => a + b, 0);
  const bytes = new Uint8Array(new ArrayBuffer(total));
  for (let i = 0; i < total; i++) bytes[i] = (i * 31 + 7) & 0xff;
  const slices: GroupSlice[] = [];
  let at = 0;
  for (const n of sizes) {
    slices.push({ offset: at, length: n, sha256: sha(bytes.subarray(at, at + n)) });
    at += n;
  }
  return { bytes, slices };
}

/** A server that answers ranges of `file`, recording what was asked. */
function server(file: Uint8Array<ArrayBuffer>, opts: { ignoreRanges?: boolean; tamperAt?: number } = {}) {
  const asked: { url: string; range: string | null; credentials?: string }[] = [];
  const impl = ((url: string, init?: RequestInit) => {
    const range = new Headers(init?.headers).get("Range");
    asked.push({ url, range, ...(init?.credentials !== undefined ? { credentials: init.credentials } : {}) });
    const body = file.slice();
    if (opts.tamperAt !== undefined) body[opts.tamperAt] = (body[opts.tamperAt] ?? 0) ^ 1;
    if (opts.ignoreRanges || range === null) return Promise.resolve(new Response(body, { status: 200 }));
    const m = /^bytes=(\d+)-(\d+)$/.exec(range);
    const first = Number(m?.[1]);
    const last = Number(m?.[2]);
    return Promise.resolve(
      new Response(body.slice(first, last + 1), {
        status: 206,
        headers: { "Content-Range": `bytes ${first}-${last}/${file.byteLength}` },
      }),
    );
  }) as unknown as typeof fetch;
  return { impl, asked };
}

describe("slicesCover", () => {
  it("accepts only an exact cover", () => {
    const s = (offset: number, length: number): GroupSlice => ({ offset, length, sha256: "" });
    expect(slicesCover([s(5, 5), s(0, 5)], 10)).toBe(true);
    expect(slicesCover([s(0, 5), s(6, 4)], 10)).toBe(false);
    expect(slicesCover([s(0, 6), s(5, 5)], 10)).toBe(false);
    expect(slicesCover([s(0, 5)], 10)).toBe(false);
    expect(slicesCover([], 0)).toBe(false);
  });
});

describe("verifyGroup", () => {
  it("checks every model file and the size", async () => {
    const { bytes, slices } = groupFile([100, 50, 7]);
    expect(await verifyGroup(new Blob([bytes]), bytes.byteLength, slices)).toBe(true);
    const bad = bytes.slice();
    bad[120] = (bad[120] ?? 0) ^ 1;
    expect(await verifyGroup(new Blob([bad]), bytes.byteLength, slices)).toBe(false);
    expect(await verifyGroup(new Blob([bytes.subarray(1)]), bytes.byteLength, slices)).toBe(false);
  });
});

describe("fetchGroup", () => {
  it("asks only for the group by name, in ranges, with no credentials", async () => {
    const { bytes, slices } = groupFile([RANGE_BYTES + 10, 2 * RANGE_BYTES - 3, 1000]);
    const srv = server(bytes);
    const progress: number[] = [];
    const blob = await fetchGroup(NAME, bytes.byteLength, slices, { onProgress: (f) => progress.push(f) }, srv.impl);
    expect(blob.size).toBe(bytes.byteLength);
    expect(srv.asked.every((a) => a.url === `${MODELS_PATH}${NAME}` && a.credentials === "omit")).toBe(true);
    expect(srv.asked.map((a) => a.range)).toEqual([
      `bytes=0-${RANGE_BYTES - 1}`,
      `bytes=${RANGE_BYTES}-${2 * RANGE_BYTES - 1}`,
      `bytes=${2 * RANGE_BYTES}-${3 * RANGE_BYTES - 1}`,
      `bytes=${3 * RANGE_BYTES}-${bytes.byteLength - 1}`,
    ]);
    expect(progress.at(-1)).toBe(1);
  });

  it("takes a whole answer from a server that ignores ranges", async () => {
    const { bytes, slices } = groupFile([300, 200]);
    const blob = await fetchGroup(NAME, bytes.byteLength, slices, {}, server(bytes, { ignoreRanges: true }).impl);
    expect(blob.size).toBe(500);
  });

  it("refuses a file that does not check, a bad name and a failed answer", async () => {
    const { bytes, slices } = groupFile([300, 200]);
    await expect(fetchGroup(NAME, 500, slices, {}, server(bytes, { tamperAt: 250 }).impl)).rejects.toThrow("does not check");
    await expect(fetchGroup("../x", 500, slices, {}, server(bytes).impl)).rejects.toThrow("bad group name");
    const failing = (() => Promise.resolve(new Response("no", { status: 404 }))) as unknown as typeof fetch;
    await expect(fetchGroup(NAME, 500, slices, {}, failing)).rejects.toThrow("404");
    // An answer for another range or another file size.
    const wrong = (() =>
      Promise.resolve(new Response(bytes.slice(0, 500), { status: 206, headers: { "Content-Range": "bytes 0-499/999" } }))) as unknown as typeof fetch;
    await expect(fetchGroup(NAME, 500, slices, {}, wrong)).rejects.toThrow();
  });
});
