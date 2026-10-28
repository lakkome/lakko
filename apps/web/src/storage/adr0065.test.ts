// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 on the web client: the blob pass cache, the refusal mapping, the
 * storage client's pass handling against a fake server, the owed-delete
 * list, the keep scheduler, and the attachment failure classification.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  BlobPassCache,
  PASS_RENEW_MARGIN_MS,
  StorageLimitError,
  base64Url,
  blobRefusalOf,
  installBlobAccess,
  parseBlobPass,
  storageErrorCode,
  type BlobPassKind,
} from "./blob-access";
import { StorageClient, StorageHttpError } from "./client";
import { MAX_OWED_DELETES, drainOwedDeletes, owe, readOwedDeletes, type QueueStorage } from "./blob-delete-queue";
import {
  EMPTY_KEEP_STATE,
  KEEP_BATCH,
  KEEP_INTERVAL_MS,
  MAX_SOURCES_PER_TICK,
  markKept,
  parseKeepState,
  planKeeps,
  spreadOffsetMs,
} from "./keep-scheduler";
import { isSendLimit, openFailureOf } from "../chat/attachment-failure";
import { AttachmentMissingError } from "../chat/attachments";
import { blobIdsOfMessages, blobIdsOfRefs, joinBlobIds, splitBlobIds } from "../chat/blob-ids";

afterEach(() => {
  installBlobAccess(null);
  vi.unstubAllGlobals();
});

const ID = (n: number): string => n.toString(16).padStart(2, "0").repeat(32);

describe("the blob pass cache", () => {
  it("buys once, reuses, and renews before expiry", async () => {
    let now = 1_000_000;
    let bought = 0;
    const cache = new BlobPassCache(async (kind) => {
      bought += 1;
      return { pass: `${kind}-${bought}`, expiresAtMs: now + 3_600_000 };
    }, () => now);
    expect(await cache.get("read")).toBe("read-1");
    expect(await cache.get("read")).toBe("read-1");
    now += 3_600_000 - PASS_RENEW_MARGIN_MS + 1;
    expect(await cache.get("read")).toBe("read-2");
    expect(await cache.get("upload")).toBe("upload-3");
  });

  it("shares one purchase between concurrent requests", async () => {
    let bought = 0;
    const cache = new BlobPassCache(async () => {
      bought += 1;
      await new Promise((r) => setTimeout(r, 5));
      return { pass: "p", expiresAtMs: Date.now() + 3_600_000 };
    });
    await Promise.all([cache.get("read"), cache.get("read"), cache.get("read")]);
    expect(bought).toBe(1);
  });

  it("drops only the pass that was refused", async () => {
    let n = 0;
    const cache = new BlobPassCache(async () => ({ pass: `p${++n}`, expiresAtMs: Date.now() + 3_600_000 }));
    expect(await cache.get("read")).toBe("p1");
    cache.invalidate("read", "older");
    expect(await cache.get("read")).toBe("p1");
    cache.invalidate("read", "p1");
    expect(await cache.get("read")).toBe("p2");
  });

  it("reads the pass answer and refuses a malformed one", () => {
    expect(parseBlobPass({ pass: "x", kind: "read", expires_at_unix_secs: 10, budget: {} }, "read")).toEqual({
      pass: "x",
      expiresAtMs: 10_000,
    });
    expect(() => parseBlobPass({ pass: "x", kind: "upload", expires_at_unix_secs: 10 }, "read")).toThrow();
    expect(() => parseBlobPass({ kind: "read", expires_at_unix_secs: 10 }, "read")).toThrow();
  });
});

describe("what a refusal means", () => {
  it("maps every server code", () => {
    expect(blobRefusalOf(404, "blob-not-found")).toBe("gone");
    expect(blobRefusalOf(401, "blob-pass-invalid")).toBe("renew-pass");
    expect(blobRefusalOf(401, "blob-pass-expired")).toBe("renew-pass");
    expect(blobRefusalOf(401, "blob-pass-required")).toBe("renew-pass");
    expect(blobRefusalOf(429, "blob-read-budget-exhausted")).toBe("budget");
    expect(blobRefusalOf(429, "blob-upload-budget-exhausted")).toBe("budget");
    // nginx's own 429 carries no JSON code: the generic failure.
    expect(blobRefusalOf(429, null)).toBe("failed");
    expect(blobRefusalOf(503, "blob-node-unavailable")).toBe("failed");
  });

  it("reads the code from the body", () => {
    expect(storageErrorCode('{"error":"blob-not-found"}')).toBe("blob-not-found");
    expect(storageErrorCode("<html>429 Too Many Requests</html>")).toBeNull();
  });
});

describe("the storage client with a pass", () => {
  function server(answers: Array<{ status: number; body?: string; bytes?: Uint8Array }>) {
    const seen: Array<{ method: string; pass: string | null; deleteKey: string | null }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init: RequestInit) => {
        const headers = (init.headers ?? {}) as Record<string, string>;
        seen.push({
          method: init.method ?? "GET",
          pass: headers["x-lakko-blob-pass"] ?? null,
          deleteKey: headers["x-lakko-blob-delete-key"] ?? null,
        });
        const a = answers.shift() ?? { status: 500 };
        return new Response(a.bytes !== undefined ? a.bytes.slice().buffer : (a.body ?? null), { status: a.status });
      }),
    );
    return seen;
  }
  function access(): void {
    let n = 0;
    installBlobAccess({
      passes: new BlobPassCache(async (kind: BlobPassKind) => ({ pass: `${kind}${++n}`, expiresAtMs: Date.now() + 3_600_000 })),
      deleteKey: async () => new Uint8Array(32).fill(0xfb),
      signDelete: async () => ["x-lakko-storage-proof", "v1.sig"] as const,
    });
  }
  const blobId = new Uint8Array(32).fill(1);

  it("sends the pass and the delete key on an upload", async () => {
    access();
    const seen = server([{ status: 201 }]);
    await new StorageClient("/s").putBlob(blobId, new Uint8Array([1, 2, 3]));
    expect(seen[0]).toEqual({ method: "PUT", pass: "upload1", deleteKey: base64Url(new Uint8Array(32).fill(0xfb)) });
    expect(seen[0]?.deleteKey).toMatch(/^[A-Za-z0-9_-]{43}$/);
  });

  it("takes a new pass once when the server lost the old one", async () => {
    access();
    const seen = server([
      { status: 401, body: '{"error":"blob-pass-invalid"}' },
      { status: 200, bytes: new Uint8Array([9]) },
    ]);
    expect(await new StorageClient("/s").getBlob(blobId)).toEqual(new Uint8Array([9]));
    expect(seen.map((s) => s.pass)).toEqual(["read1", "read2"]);
  });

  it("a budget refused twice is the limit", async () => {
    access();
    server([
      { status: 429, body: '{"error":"blob-read-budget-exhausted"}' },
      { status: 429, body: '{"error":"blob-read-budget-exhausted"}' },
    ]);
    await expect(new StorageClient("/s").getBlob(blobId)).rejects.toBeInstanceOf(StorageLimitError);
  });

  it("a 404 is gone and an nginx page is the generic failure", async () => {
    access();
    server([{ status: 404, body: '{"error":"blob-not-found"}' }]);
    expect(await new StorageClient("/s").getBlob(blobId)).toBeNull();
    server([{ status: 429, body: "<html>slow down</html>" }]);
    await expect(new StorageClient("/s").getBlob(blobId)).rejects.toBeInstanceOf(StorageHttpError);
  });

  it("an upload budget refused twice is the sending limit", async () => {
    access();
    server([
      { status: 429, body: '{"error":"blob-upload-budget-exhausted"}' },
      { status: 429, body: '{"error":"blob-upload-budget-exhausted"}' },
    ]);
    const err = await new StorageClient("/s").putBlob(blobId, new Uint8Array([1])).catch((e: unknown) => e);
    expect(isSendLimit(err)).toBe(true);
  });

  it("a delete is done only on 204; 403 and anything else throw", async () => {
    access();
    server([{ status: 204 }]);
    await new StorageClient("/s").deleteBlob(blobId);
    server([{ status: 403, body: '{"error":"blob-delete-refused"}' }]);
    await expect(new StorageClient("/s").deleteBlob(blobId)).rejects.toMatchObject({ status: 403 });
    // Not expected from DELETE: a retry later, not "done".
    server([{ status: 404 }]);
    await expect(new StorageClient("/s").deleteBlob(blobId)).rejects.toBeInstanceOf(StorageHttpError);
  });
});

describe("the owed deletes", () => {
  function mem(): QueueStorage & { m: Map<string, string> } {
    const m = new Map<string, string>();
    return {
      m,
      getItem: (k) => m.get(k) ?? null,
      setItem: (k, v) => void m.set(k, v),
      removeItem: (k) => void m.delete(k),
    };
  }

  it("deduplicates, keeps failures, drops the deleted and the refused", async () => {
    const s = mem();
    owe("a", [ID(1), ID(2), ID(1).toUpperCase(), "bad"], s);
    expect(readOwedDeletes("a", s)).toEqual([ID(1), ID(2)]);
    owe("a", [ID(3)], s);
    const outcomes: Record<string, "deleted" | "refused" | "retry"> = { [ID(1)]: "deleted", [ID(2)]: "retry", [ID(3)]: "refused" };
    expect(await drainOwedDeletes("a", async (id) => outcomes[id] ?? "retry", s)).toEqual({ done: 2, remaining: 1 });
    expect(readOwedDeletes("a", s)).toEqual([ID(2)]);
    expect(readOwedDeletes("b", s)).toEqual([]);
  });

  it("keeps an id owed during a drain, and a throw is a retry", async () => {
    const s = mem();
    owe("a", [ID(1)], s);
    await drainOwedDeletes(
      "a",
      async () => {
        owe("a", [ID(4)], s);
        throw new Error("offline");
      },
      s,
    );
    expect(readOwedDeletes("a", s)).toEqual([ID(1), ID(4)]);
  });

  it("is bounded", () => {
    const s = mem();
    const ids = Array.from({ length: MAX_OWED_DELETES + 5 }, (_, i) => i.toString(16).padStart(64, "0"));
    owe("a", ids, s);
    const held = readOwedDeletes("a", s);
    expect(held.length).toBe(MAX_OWED_DELETES);
    expect(held[0]).toBe(ids[5]);
  });
});

describe("the keep scheduler", () => {
  const now = 10 * KEEP_INTERVAL_MS;

  it("spreads first keeps over the first month and sends what is due", () => {
    const first = planKeeps(EMPTY_KEEP_STATE, [{ key: "pins:a", idsHex: [ID(1)] }], now);
    expect(first.keys).toEqual([]);
    expect(first.state.due["pins:a"]).toBe(now + spreadOffsetMs("pins:a"));
    expect(spreadOffsetMs("pins:a")).toBeLessThan(KEEP_INTERVAL_MS);
    const later = planKeeps(first.state, [{ key: "pins:a", idsHex: [ID(1)] }], now + KEEP_INTERVAL_MS);
    expect(later.keys).toEqual(["pins:a"]);
    expect(later.batches).toEqual([[ID(1)]]);
    const kept = markKept(later.state, later.keys, now + KEEP_INTERVAL_MS);
    expect(kept.due["pins:a"]).toBe(now + 2 * KEEP_INTERVAL_MS);
  });

  it("sends at most a few sources a tick, deduplicates ids, batches by 256", () => {
    const sources = Array.from({ length: MAX_SOURCES_PER_TICK + 2 }, (_, i) => ({
      key: `pins:${i}`,
      idsHex: Array.from({ length: 100 }, (_, j) => (i * 1000 + j).toString(16).padStart(64, "0")),
    }));
    sources.push({ key: "saved", idsHex: [sources[0]!.idsHex[0]!] });
    const due = Object.fromEntries(sources.map((s, i) => [s.key, now - 1000 + i]));
    const plan = planKeeps({ v: 1, due }, sources, now);
    expect(plan.keys.length).toBe(MAX_SOURCES_PER_TICK);
    const all = plan.batches.flat();
    expect(new Set(all).size).toBe(all.length);
    expect(plan.batches.every((b) => b.length <= KEEP_BATCH)).toBe(true);
  });

  it("forgets sources that are gone and skips empty ones", () => {
    const plan = planKeeps({ v: 1, due: { "pins:gone": 0, profile: 0 } }, [{ key: "profile", idsHex: [] }], now);
    expect(plan.keys).toEqual([]);
    expect(plan.state.due).toEqual({});
  });

  it("spreads by FNV-1a over the key, the same value iOS and Android pin", () => {
    expect(spreadOffsetMs("saved")).toBe(1_203_033_924);
  });

  it("reads back what it wrote", () => {
    const s = { v: 1 as const, due: { saved: 5 } };
    expect(parseKeepState(JSON.stringify(s))).toEqual(s);
    expect(parseKeepState("nope")).toEqual(EMPTY_KEEP_STATE);
  });
});

describe("what a failed file is", () => {
  it("gone, limited, or failed", () => {
    expect(openFailureOf(new AttachmentMissingError(new Uint8Array(32)))).toBe("gone");
    expect(openFailureOf(new StorageLimitError("read"))).toBe("limit");
    expect(openFailureOf(new Error("x", { cause: new StorageLimitError("read") }))).toBe("limit");
    expect(openFailureOf(new StorageLimitError("upload"))).toBe("failed");
    expect(openFailureOf(new TypeError("offline"))).toBe("failed");
    expect(isSendLimit(new StorageLimitError("upload"))).toBe(true);
    expect(isSendLimit(new StorageLimitError("read"))).toBe(false);
  });
});

describe("the blobs a message names", () => {
  const ref = (n: number, thumb?: number) => ({
    blobId: new Uint8Array(32).fill(n),
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    mime: "image/jpeg",
    size: 1,
    name: undefined,
    ...(thumb === undefined
      ? {}
      : { thumb: { blobId: new Uint8Array(32).fill(thumb), key: new Uint8Array(32), nonce: new Uint8Array(12), mime: "image/jpeg", size: 1, width: 1, height: 1 } }),
  });

  it("files and thumbs, once each", () => {
    expect(blobIdsOfRefs([ref(1, 2), ref(1)])).toEqual([ID(1), ID(2)]);
    expect(splitBlobIds(joinBlobIds([ID(1), ID(2)]))).toEqual([ID(1), ID(2)]);
  });

  it("of the pinned messages in a stored chat", () => {
    const b64 = (n: number): string => btoa(String.fromCharCode(...new Uint8Array(32).fill(n)));
    const att = (n: number) => ({ blobIdB64: b64(n), keyB64: b64(0), nonceB64: btoa("x".repeat(12)), mime: null, sizeStr: "1", name: null });
    const group = {
      cursor: "0",
      messages: [
        { messageId: "5", direction: "incoming", text: "", attachments: [att(3)], createdAt: "" },
        { messageId: "6", direction: "incoming", text: "", attachments: [att(4)], createdAt: "" },
      ],
    } as never;
    expect(blobIdsOfMessages(group, new Set([6]))).toEqual([ID(4)]);
  });
});
