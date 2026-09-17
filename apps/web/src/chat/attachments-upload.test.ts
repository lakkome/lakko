// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The upload paths read every field of the wasm `EncryptedAttachment` handle
 * BEFORE freeing it. Reading a getter on a freed handle throws
 * "null pointer passed to rust" — the composer's photo send failed exactly
 * that way. Driven through the real wasm core with an in-memory storage stub.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import { encryptAndUploadBytes, encryptAndUploadFile } from "./attachments";
import type { StorageClient } from "../storage/client";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function memoryStorage(): { storage: StorageClient; blobs: Map<string, Uint8Array> } {
  const blobs = new Map<string, Uint8Array>();
  const put = async (blobId: Uint8Array, ciphertext: Uint8Array): Promise<void> => {
    blobs.set(Array.from(blobId).join(","), ciphertext);
  };
  const storage = { putBlob: put, putBlobWithProgress: put } as unknown as StorageClient;
  return { storage, blobs };
}

describe("attachment upload", () => {
  it("encryptAndUploadFile returns a ref whose key opens the uploaded ciphertext", async () => {
    const { storage, blobs } = memoryStorage();
    const plaintext = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
    const file = new File([plaintext], "web-photo.jpg", { type: "image/jpeg" });
    const ref = await encryptAndUploadFile(file, storage);
    expect(ref.blobId.length).toBe(32);
    expect(ref.key.length).toBe(32);
    expect(ref.nonce.length).toBe(12);
    expect(ref.mime).toBe("image/jpeg");
    expect(ref.name).toBe("web-photo.jpg");
    const ct = blobs.get(Array.from(ref.blobId).join(","));
    expect(ct).toBeDefined();
    expect(Array.from(wasm.decryptAttachment(ct as Uint8Array, ref.key, ref.nonce))).toEqual(Array.from(plaintext));
  });

  it("encryptAndUploadBytes carries the media metadata and a working key", async () => {
    const { storage, blobs } = memoryStorage();
    const plaintext = new Uint8Array([9, 9, 9]);
    const ref = await encryptAndUploadBytes(plaintext, { mime: "image/jpeg", name: undefined }, { width: 320, height: 240 }, storage);
    expect(ref.width).toBe(320);
    const ct = blobs.get(Array.from(ref.blobId).join(","));
    expect(Array.from(wasm.decryptAttachment(ct as Uint8Array, ref.key, ref.nonce))).toEqual(Array.from(plaintext));
  });
});
