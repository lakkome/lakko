// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — the tray of one conversation: items
 * added up to the limits, uploaded at once, removable, retried, and taken by a
 * send. The upload is the real `encryptAndUploadBytes` path with the storage
 * stubbed; files go as files here, so no browser decoder is needed.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./attachments", async (orig) => {
  const real = await orig<typeof import("./attachments")>();
  return {
    ...real,
    encryptAndUploadBytes: vi.fn(async (bytes: Uint8Array, file: { mime?: string; name?: string }, _meta: unknown, _s: unknown, onProgress?: (f: number) => void) => {
      onProgress?.(0.5);
      return {
        blobId: new Uint8Array(32).fill(bytes.length),
        key: new Uint8Array(32),
        nonce: new Uint8Array(12),
        mime: file.mime,
        size: bytes.length,
        name: file.name,
      };
    }),
  };
});

import { encryptAndUploadBytes } from "./attachments";
import { createTrayStore, trayFor, type TrayDeps } from "./tray-store";
import type { StorageClient } from "../storage/client";

function deps(over: Partial<TrayDeps> = {}): TrayDeps {
  return {
    storage: () => ({}) as StorageClient,
    inlineType: () => null,
    discard: vi.fn(),
    onError: vi.fn(),
    ...over,
  };
}

function file(name: string, size = 3): File {
  return new File([new Uint8Array(size)], name, { type: "application/pdf" });
}

async function settled(store: ReturnType<typeof createTrayStore>): Promise<void> {
  await vi.waitFor(() => expect(store.items().every((i) => i.status === "ready" || i.status === "failed")).toBe(true));
}

afterEach(() => {
  vi.useRealTimers();
  vi.mocked(encryptAndUploadBytes).mockClear();
});

describe("tray store", () => {
  it("uploads each item as it is added and becomes sendable", async () => {
    const store = createTrayStore(deps());
    store.add([file("a.pdf"), file("b.pdf", 5)], "file");
    expect(store.items().map((i) => i.kind)).toEqual(["file", "file"]);
    await settled(store);
    expect(store.items().map((i) => i.status)).toEqual(["ready", "ready"]);
    expect(store.items()[1]?.ref?.size).toBe(5);
  });

  it("adds what fits past the files limit and shows the line for 4 s", async () => {
    vi.useFakeTimers();
    const store = createTrayStore(deps());
    store.add(Array.from({ length: 12 }, (_, i) => file(`f${i}.pdf`)), "file");
    expect(store.items()).toHaveLength(10);
    expect(store.notice()).toBe("files");
    vi.advanceTimersByTime(4_000);
    expect(store.notice()).toBeNull();
  });

  it("deletes the upload of an item removed after it was uploaded", async () => {
    const d = deps();
    const store = createTrayStore(d);
    store.add([file("a.pdf")], "file");
    await settled(store);
    const id = store.items()[0]!.id;
    store.remove(id);
    expect(store.items()).toEqual([]);
    expect(d.discard).toHaveBeenCalledTimes(1);
  });

  it("removes the last item", async () => {
    const store = createTrayStore(deps());
    store.add([file("a.pdf"), file("b.pdf")], "file");
    store.removeLast();
    expect(store.items().map((i) => i.name)).toEqual(["a.pdf"]);
  });

  it("marks a failed upload and retries it", async () => {
    vi.mocked(encryptAndUploadBytes).mockRejectedValueOnce(new Error("offline"));
    const store = createTrayStore(deps());
    store.add([file("a.pdf")], "file");
    await settled(store);
    expect(store.items()[0]?.status).toBe("failed");
    store.retry(store.items()[0]!.id);
    await settled(store);
    expect(store.items()[0]?.status).toBe("ready");
  });

  it("refuses a file over the cap and says so", () => {
    const d = deps();
    const store = createTrayStore(d);
    const big = { name: "big.bin", size: 60 * 1024 * 1024, type: "" } as unknown as File;
    store.add([big], "file");
    expect(store.items()).toEqual([]);
    expect(d.onError).toHaveBeenCalledTimes(1);
  });

  it("hands every item to a send and empties", async () => {
    const store = createTrayStore(deps());
    store.add([file("a.pdf")], "file");
    await settled(store);
    const taken = store.take();
    expect(taken).toHaveLength(1);
    expect(store.items()).toEqual([]);
  });

  it("keeps a conversation's tray between two views of it", () => {
    const first = trayFor("acct:conv-a", deps());
    first.add([file("a.pdf")], "file");
    const again = trayFor("acct:conv-a", deps());
    expect(again).toBe(first);
    expect(again.items()).toHaveLength(1);
    expect(trayFor("acct:conv-b", deps()).items()).toHaveLength(0);
  });
});
