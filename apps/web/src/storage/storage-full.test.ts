// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/storage-full.md on the web client: a `507 storage-full` refusal is
 * its own error, never retried by the client, and reads with its own words
 * wherever the file-sending limit already has a sentence.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  BlobPassCache,
  StorageFullError,
  StorageLimitError,
  blobRefusalOf,
  installBlobAccess,
  isStorageFull,
  type BlobPassKind,
} from "./blob-access";
import { StorageClient, StorageHttpError } from "./client";
import { groupPhotoUploadErrorText, isSendLimit, sendRefusalText } from "../chat/attachment-failure";
import { channelErrorPlace, channelErrorText } from "../channels/channel-error-copy";
import { runProfileSave, type SavedFields } from "../profile/profile-edit";
import { t } from "../l10n";

afterEach(() => {
  installBlobAccess(null);
  vi.unstubAllGlobals();
});

function server(answers: Array<{ status: number; body?: string }>): string[] {
  const seen: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: string, init: RequestInit) => {
      seen.push(init.method ?? "GET");
      const a = answers.shift() ?? { status: 500 };
      return new Response(a.body ?? null, { status: a.status });
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

describe("the refusal", () => {
  it("507 storage-full is its own refusal; anything else stays generic", () => {
    expect(blobRefusalOf(507, "storage-full")).toBe("full");
    expect(blobRefusalOf(507, null)).toBe("failed");
    expect(blobRefusalOf(429, "storage-full")).toBe("failed");
  });

  it("is recognised through causes, and is not the sending limit", () => {
    expect(isStorageFull(new StorageFullError())).toBe(true);
    expect(isStorageFull(new Error("x", { cause: new StorageFullError() }))).toBe(true);
    expect(isStorageFull(new StorageLimitError("upload"))).toBe(false);
    expect(isStorageFull(null)).toBe(false);
    expect(isSendLimit(new StorageFullError())).toBe(false);
  });
});

describe("the storage client", () => {
  it("a blob upload refused for a full disk throws once and is not retried", async () => {
    access();
    const seen = server([
      { status: 507, body: '{"error":"storage-full"}' },
      { status: 201 },
    ]);
    const err = await new StorageClient("/s").putBlob(blobId, new Uint8Array([1])).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(StorageFullError);
    expect(seen).toEqual(["PUT"]);
  });

  it("the backup upload treats 507 like any other failed upload", async () => {
    const seen = server([{ status: 507, body: "storage-full" }]);
    const err = await new StorageClient("/s").putBackup(new Uint8Array([1])).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(StorageHttpError);
    expect(seen).toEqual(["PUT"]);
  });
});

describe("what the person reads", () => {
  it("the composer note", () => {
    expect(sendRefusalText(new StorageFullError())).toBe(t("chat.attachment.full.send"));
    expect(sendRefusalText(new StorageLimitError("upload"))).toBe(t("chat.attachment.limit.send"));
    expect(sendRefusalText(new TypeError("offline"))).toBeNull();
  });

  it("the group photo line", () => {
    expect(groupPhotoUploadErrorText(new StorageFullError())).toBe(t("group_photo.error.full"));
    expect(groupPhotoUploadErrorText(new StorageLimitError("upload"))).toBe(t("group_photo.error.limit"));
    expect(groupPhotoUploadErrorText(new Error("offline"))).toBe(t("group_photo.error.upload"));
  });

  it("the channel photo line", () => {
    const err = Object.assign(new Error("x"), { code: "photo-full" });
    expect(channelErrorPlace("photo-full")).toBe("photo");
    expect(channelErrorText(err, () => "fallback")).toBe(t("group_photo.error.full"));
  });

  it("the profile photo: a full server is its own save error", async () => {
    const saved: SavedFields = { name: "Ann", username: "ann", avatar: null };
    const ports = {
      uploadPhoto: () => Promise.reject(new StorageFullError()),
      rename: async (u: string) => ({ kind: "done" as const, username: u }),
      publish: async () => undefined,
    };
    const out = await runProfileSave(saved, { photo: { kind: "set", media: "m" } }, ports);
    expect(out.kind).toBe("failed");
    if (out.kind === "failed") expect(out.error).toEqual({ kind: "full" });
    const other = await runProfileSave(saved, { photo: { kind: "set", media: "m" } }, {
      ...ports,
      uploadPhoto: () => Promise.reject(new Error("net")),
    });
    if (other.kind === "failed") expect(other.error).toEqual({ kind: "upload" });
  });
});
