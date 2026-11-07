// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it, vi } from "vitest";

import { BLOB_IDLE_DAYS_HEADER, CHANNEL_FILE_IDLE_DAYS, StorageClient } from "./client";

afterEach(() => {
  vi.unstubAllGlobals();
});

/** The headers of every upload the client sends. */
function server(): Record<string, string>[] {
  const seen: Record<string, string>[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: string, init?: RequestInit) => {
      seen.push({ ...((init?.headers ?? {}) as Record<string, string>) });
      return new Response(null, { status: 201 });
    }),
  );
  return seen;
}

describe("ADR-0087 section 13: a channel file's idle life", () => {
  const blobId = new Uint8Array(32).fill(1);

  it("a channel upload asks for 45 idle days", async () => {
    const seen = server();
    await new StorageClient("/s", CHANNEL_FILE_IDLE_DAYS).putBlob(blobId, new Uint8Array([1]));
    expect(seen[0]?.[BLOB_IDLE_DAYS_HEADER]).toBe("45");
  });

  it("any other upload names no idle life", async () => {
    const seen = server();
    await new StorageClient("/s").putBlob(blobId, new Uint8Array([1]));
    expect(seen[0]?.[BLOB_IDLE_DAYS_HEADER]).toBeUndefined();
  });
});
