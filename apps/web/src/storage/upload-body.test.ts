// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * An upload with progress sends bytes. WebKit hands a Blob body to a custom
 * scheme as a stream the desktop host reads empty, so every voice and video
 * message failed with `blob-hash-mismatch` (ADR-0085 section 14).
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { StorageClient } from "./client";

afterEach(() => vi.unstubAllGlobals());

describe("putBlobWithProgress", () => {
  it("hands XMLHttpRequest the ciphertext as bytes", async () => {
    const sent: unknown[] = [];
    class FakeXhr {
      status = 201;
      responseText = "";
      upload = {};
      onload: (() => void) | null = null;
      open(): void {}
      setRequestHeader(): void {}
      send(body: unknown): void {
        sent.push(body);
        this.onload?.();
      }
    }
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    const client = new StorageClient("https://storage.invalid");

    const ciphertext = new Uint8Array([1, 2, 3, 4]);
    await client.putBlobWithProgress(new Uint8Array(32), ciphertext, () => {});

    expect(sent).toHaveLength(1);
    expect(sent[0]).toBeInstanceOf(Uint8Array);
    expect(sent[0]).toEqual(ciphertext);
  });
});
