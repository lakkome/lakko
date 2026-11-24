// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * 2026-11-23 — a GIF in a message shows no "GIF" label, under the picture or
 * on it: the moving picture says what it is. Screen readers still hear "GIF"
 * from the image's alt text.
 */
import { beforeAll, describe, expect, it } from "vitest";

import { GifBubble } from "./MediaBubbles";
import { createMediaHub } from "./media-hub";
import type { AttachmentRefBytes } from "../chat/attachments";
import { initWasm, installStylesheet, one, renderUi } from "../test/ui";

beforeAll(async () => {
  await initWasm();
});

const gif: AttachmentRefBytes = {
  blobId: new Uint8Array(32).fill(0x47),
  key: new Uint8Array(32),
  nonce: new Uint8Array(12),
  mime: "image/gif",
  size: 2048,
  name: undefined,
  kind: "gif",
  width: 240,
  height: 180,
};

describe("a GIF bubble", () => {
  it("draws the picture alone, with no visible GIF label", async () => {
    installStylesheet();
    const hub = createMediaHub({ voiceQueue: () => [] });
    // The bytes are already here, so nothing reaches the network.
    hub.fileLocal("local:gif", new Blob([new Uint8Array(16)], { type: "image/gif" }), "image/gif");
    const root = renderUi(() => (
      <GifBubble attachment={gif} hub={hub} mediaKey="local:gif" incoming={true} upload={undefined} onRetry={() => {}} />
    ));
    await Promise.resolve();
    const picture = one(".lakko-gif");
    expect(document.querySelector(".lakko-gif-label")).toBeNull();
    // No element outside the image itself carries the word.
    const bubble = picture.parentElement ?? root;
    expect(bubble.textContent ?? "").not.toContain("GIF");
    const img = picture.querySelector("img");
    if (img !== null) expect(img.getAttribute("alt")).toBe("GIF");
  });
});
